use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::domain::LaneId;

use super::transaction::restore_plan as restore_kernel_plan;
use super::{
    apply_plan as apply_kernel_plan, build_policy_commands, build_snapshot_commands,
    detect_resource_conflicts, detect_resource_conflicts_for_update, AppliedKernelState,
    CommandSpec, CompiledAction, CompiledTarget, IpFamily, KernelPlan, KernelRunner,
    KernelSnapshot, PlanHash, Program, RunnerError, SnapshotInput, TableMode, MARK_BASE, MARK_MAX,
    RULE_PRIORITY_BASE, RULE_PRIORITY_MAX, TABLE_BASE, TABLE_MAX,
};

pub const MAX_PLAN_JSON_BYTES: usize = 1024 * 1024;
const MAX_STATE_BYTES: u64 = (MAX_PLAN_JSON_BYTES as u64 * 2) + 4096;
const IP_BINARY: &str = "/usr/sbin/ip";
const NFT_BINARY: &str = "/usr/sbin/nft";
const STATE_DIRECTORY: &str = "/run/routelane";
const STATE_FILE: &str = "/run/routelane/applied-plan.json";
const O_NOFOLLOW: i32 = 0o400000;
const MAX_LANES: usize = 100;
const MAX_RULES: usize = 10_000;
const ROUTING_CRASH_MODEL_VERSION: &str = "exact-command-prefix-v3";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const COMMAND_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_COMMAND_PIPE_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
struct CommandInvocation<'a> {
    program: &'static str,
    args: Vec<&'a str>,
    stdin: Option<&'a [u8]>,
}

fn command_invocation(spec: &CommandSpec) -> Result<CommandInvocation<'_>, RunnerError> {
    match (spec.program, spec.family, spec.stdin.as_deref()) {
        (Program::Ip, Some(family), None) => {
            let family = match family {
                IpFamily::V4 => "-4",
                IpFamily::V6 => "-6",
            };
            let mut args = Vec::with_capacity(spec.args.len() + 1);
            args.push(family);
            args.extend(spec.args.iter().map(String::as_str));
            Ok(CommandInvocation {
                program: IP_BINARY,
                args,
                stdin: None,
            })
        }
        (Program::Nft, None, stdin) => Ok(CommandInvocation {
            program: NFT_BINARY,
            args: spec.args.iter().map(String::as_str).collect(),
            stdin,
        }),
        (Program::Ip, None, _) => Err(RunnerError::new("ip command is missing an address family")),
        (Program::Ip, Some(_), Some(_)) => {
            Err(RunnerError::new("ip command cannot accept standard input"))
        }
        (Program::Nft, Some(_), _) => Err(RunnerError::new(
            "nft command cannot have an IP address family",
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl CommandOutput {
    #[cfg(test)]
    fn success(stdout: Vec<u8>) -> Self {
        Self {
            success: true,
            stdout,
            stderr: vec![],
        }
    }

    #[cfg(test)]
    fn failure(stderr: impl Into<Vec<u8>>) -> Self {
        Self {
            success: false,
            stdout: vec![],
            stderr: stderr.into(),
        }
    }
}

pub trait CommandExecutor: Send {
    fn execute(&mut self, spec: &CommandSpec) -> Result<CommandOutput, RunnerError>;
}

#[derive(Debug, Default)]
pub struct ProcessCommandExecutor;

impl CommandExecutor for ProcessCommandExecutor {
    fn execute(&mut self, spec: &CommandSpec) -> Result<CommandOutput, RunnerError> {
        let invocation = command_invocation(spec)?;
        execute_invocation_with_timeout(&invocation, COMMAND_TIMEOUT)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandPipe {
    Stdout,
    Stderr,
}

impl fmt::Display for CommandPipe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

#[derive(Debug)]
enum CommandPipeError {
    Read {
        pipe: CommandPipe,
        source: std::io::Error,
    },
    OutputLimitExceeded {
        pipe: CommandPipe,
        maximum: usize,
    },
}

impl fmt::Display for CommandPipeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { pipe, source } => {
                write!(formatter, "failed to read typed command {pipe}: {source}")
            }
            Self::OutputLimitExceeded { pipe, maximum } => {
                write!(
                    formatter,
                    "typed command {pipe} exceeded the {maximum} byte output limit"
                )
            }
        }
    }
}

fn read_command_pipe(pipe: CommandPipe, reader: impl Read) -> Result<Vec<u8>, CommandPipeError> {
    let mut output = Vec::new();
    reader
        .take((MAX_COMMAND_PIPE_BYTES + 1) as u64)
        .read_to_end(&mut output)
        .map_err(|source| CommandPipeError::Read { pipe, source })?;
    if output.len() > MAX_COMMAND_PIPE_BYTES {
        return Err(CommandPipeError::OutputLimitExceeded {
            pipe,
            maximum: MAX_COMMAND_PIPE_BYTES,
        });
    }
    Ok(output)
}

fn terminate_command_group(
    child: &mut Child,
    completed_status: Option<ExitStatus>,
    context: &str,
) -> Result<ExitStatus, RunnerError> {
    let process_group = i32::try_from(child.id())
        .map_err(|_| RunnerError::new("typed command process ID exceeds the supported range"))?;
    let kill_result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    if kill_result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(RunnerError::new(format!(
                "failed to terminate {context} command group: {error}"
            )));
        }
    }
    match completed_status {
        Some(status) => Ok(status),
        None => child.wait().map_err(|error| {
            RunnerError::new(format!("failed to reap {context} command: {error}"))
        }),
    }
}

fn execute_invocation_with_timeout(
    invocation: &CommandInvocation<'_>,
    timeout: Duration,
) -> Result<CommandOutput, RunnerError> {
    let mut command = Command::new(invocation.program);
    command
        .args(&invocation.args)
        .process_group(0)
        .stdin(if invocation.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|error| RunnerError::new(format!("failed to start typed command: {error}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RunnerError::new("typed command stdout pipe is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| RunnerError::new("typed command stderr pipe is unavailable"))?;
    let stdin = if invocation.stdin.is_some() {
        Some(
            child
                .stdin
                .take()
                .ok_or_else(|| RunnerError::new("typed command stdin pipe is unavailable"))?,
        )
    } else {
        None
    };

    std::thread::scope(|scope| {
        let (output_sender, output_receiver) = std::sync::mpsc::channel();
        let stdout_sender = output_sender.clone();
        let stdout_reader = scope.spawn(move || {
            let result = read_command_pipe(CommandPipe::Stdout, stdout);
            let _ = stdout_sender.send((CommandPipe::Stdout, result));
        });
        let stderr_sender = output_sender;
        let stderr_reader = scope.spawn(move || {
            let result = read_command_pipe(CommandPipe::Stderr, stderr);
            let _ = stderr_sender.send((CommandPipe::Stderr, result));
        });
        let stdin_writer = stdin.zip(invocation.stdin).map(|(mut stdin, input)| {
            scope.spawn(move || {
                stdin.write_all(input)?;
                drop(stdin);
                Ok::<(), std::io::Error>(())
            })
        });

        let started = Instant::now();
        let mut completed_status = None;
        let mut stdout_result = None;
        let mut stderr_result = None;
        let (status, timed_out, output_failed) = loop {
            if completed_status.is_none() {
                completed_status = child.try_wait().map_err(|error| {
                    RunnerError::new(format!("failed to poll typed command: {error}"))
                })?;
            }
            while let Ok((pipe, result)) = output_receiver.try_recv() {
                match pipe {
                    CommandPipe::Stdout => stdout_result = Some(result),
                    CommandPipe::Stderr => stderr_result = Some(result),
                }
            }
            let output_failed = stdout_result.as_ref().is_some_and(Result::is_err)
                || stderr_result.as_ref().is_some_and(Result::is_err);
            if output_failed {
                let status =
                    terminate_command_group(&mut child, completed_status, "output-limited typed")?;
                break (status, false, true);
            }
            let stdin_finished = stdin_writer
                .as_ref()
                .is_none_or(std::thread::ScopedJoinHandle::is_finished);
            if let Some(status) = completed_status {
                if stdin_finished && stdout_result.is_some() && stderr_result.is_some() {
                    break (status, false, false);
                }
            }
            if started.elapsed() >= timeout {
                let status = terminate_command_group(&mut child, completed_status, "timed out")?;
                break (status, true, false);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            std::thread::sleep(COMMAND_WAIT_POLL_INTERVAL.min(remaining));
        };

        let stdin_result = stdin_writer
            .map(|writer| {
                writer
                    .join()
                    .map_err(|_| RunnerError::new("typed command stdin writer panicked"))?
                    .map_err(|error| {
                        RunnerError::new(format!("failed to write typed command stdin: {error}"))
                    })
            })
            .transpose();
        stdout_reader
            .join()
            .map_err(|_| RunnerError::new("typed command stdout reader panicked"))?;
        stderr_reader
            .join()
            .map_err(|_| RunnerError::new("typed command stderr reader panicked"))?;
        while let Ok((pipe, result)) = output_receiver.try_recv() {
            match pipe {
                CommandPipe::Stdout => stdout_result = Some(result),
                CommandPipe::Stderr => stderr_result = Some(result),
            }
        }
        let stdout = stdout_result
            .ok_or_else(|| RunnerError::new("typed command stdout reader returned no result"))?
            .map_err(|error| RunnerError::new(error.to_string()))?;
        let stderr = stderr_result
            .ok_or_else(|| RunnerError::new("typed command stderr reader returned no result"))?
            .map_err(|error| RunnerError::new(error.to_string()))?;

        if timed_out {
            return Err(RunnerError::new(format!(
                "typed command timed out after {} ms",
                timeout.as_millis()
            )));
        }
        if output_failed {
            return Err(RunnerError::new(
                "typed command output reader failed without a typed error",
            ));
        }
        stdin_result?;
        Ok(CommandOutput {
            success: status.success(),
            stdout,
            stderr,
        })
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredAppliedState {
    pub active: Option<KernelPlan>,
    pub active_nft_fingerprint: Option<String>,
    pub pending: Option<KernelPlan>,
    #[serde(default)]
    pub pending_expectations: Option<PendingExpectations>,
    #[serde(default)]
    pub cleanup_pending: bool,
    #[serde(default)]
    restore_receipt: Option<RestoreReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreReceipt {
    failed_generation: u64,
    failed_plan_hash: PlanHash,
    previous_generation: Option<u64>,
    previous_plan_hash: Option<PlanHash>,
}

impl RestoreReceipt {
    fn new(failed_candidate: &KernelPlan, previous: Option<&KernelPlan>) -> Self {
        Self {
            failed_generation: failed_candidate.generation,
            failed_plan_hash: failed_candidate.content_hash(),
            previous_generation: previous.map(|plan| plan.generation),
            previous_plan_hash: previous.map(KernelPlan::content_hash),
        }
    }

    fn matches_current(&self, current: Option<&AppliedKernelState>) -> bool {
        match (current, self.previous_generation, self.previous_plan_hash) {
            (Some(current), Some(generation), Some(hash)) => {
                current.generation == generation && current.plan_hash == hash
            }
            (None, None, None) => true,
            _ => false,
        }
    }

    fn failed_matches(&self, plan: Option<&KernelPlan>) -> bool {
        plan.is_some_and(|plan| {
            plan.generation == self.failed_generation
                && plan.content_hash() == self.failed_plan_hash
        })
    }

    fn previous_matches(&self, plan: Option<&KernelPlan>) -> bool {
        match (plan, self.previous_generation, self.previous_plan_hash) {
            (Some(plan), Some(generation), Some(hash)) => {
                plan.generation == generation && plan.content_hash() == hash
            }
            (None, None, None) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingExpectations {
    pub guard_nft_fingerprint: String,
    pub guardless_transaction_nft_fingerprint: String,
    pub activated_nft_fingerprint: String,
    pub committed_nft_fingerprint: String,
    pub committed_routing_fingerprint: String,
    pub routing_model_fingerprint: String,
}

impl StoredAppliedState {
    fn committed(
        active: Option<KernelPlan>,
        active_nft_fingerprint: Option<String>,
        restore_receipt: Option<RestoreReceipt>,
    ) -> Self {
        Self {
            active,
            active_nft_fingerprint,
            pending: None,
            pending_expectations: None,
            cleanup_pending: false,
            restore_receipt,
        }
    }
}

pub trait AppliedStateStore: Send {
    fn load(&mut self) -> Result<Option<StoredAppliedState>, PrivilegedError>;
    fn save(&mut self, state: &StoredAppliedState) -> Result<(), PrivilegedError>;
    fn clear(&mut self) -> Result<(), PrivilegedError>;
}

#[derive(Debug, Default)]
pub struct InMemoryAppliedStateStore {
    state: Option<StoredAppliedState>,
}

impl AppliedStateStore for InMemoryAppliedStateStore {
    fn load(&mut self) -> Result<Option<StoredAppliedState>, PrivilegedError> {
        Ok(self.state.clone())
    }

    fn save(&mut self, state: &StoredAppliedState) -> Result<(), PrivilegedError> {
        self.state = Some(state.clone());
        Ok(())
    }

    fn clear(&mut self) -> Result<(), PrivilegedError> {
        self.state = None;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct FileAppliedStateStore {
    directory: PathBuf,
    path: PathBuf,
}

impl Default for FileAppliedStateStore {
    fn default() -> Self {
        Self {
            directory: PathBuf::from(STATE_DIRECTORY),
            path: PathBuf::from(STATE_FILE),
        }
    }
}

#[cfg(feature = "root-integration")]
impl FileAppliedStateStore {
    pub fn isolated_root_integration(directory: PathBuf) -> Result<Self, PrivilegedError> {
        let valid_name = directory
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("routelane-test-"));
        if directory.parent() != Some(Path::new("/tmp")) || !valid_name {
            return Err(state_error(
                "root integration state directory must be an explicit /tmp/routelane-test-* path",
            ));
        }
        let metadata = fs::symlink_metadata(&directory)
            .map_err(|error| state_error(format!("inspect root integration directory: {error}")))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.mode() & 0o777 != 0o700
        {
            return Err(state_error(
                "root integration state directory must be a root-owned real 0700 directory",
            ));
        }
        let path = directory.join("applied-plan.json");
        Ok(Self { directory, path })
    }
}

impl AppliedStateStore for FileAppliedStateStore {
    fn load(&mut self) -> Result<Option<StoredAppliedState>, PrivilegedError> {
        ensure_root_owned_directory(&self.directory)?;
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(O_NOFOLLOW)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(state_error(format!(
                    "open persisted applied state: {error}"
                )));
            }
        };
        let metadata = file
            .metadata()
            .map_err(|error| state_error(format!("read state metadata: {error}")))?;
        validate_root_owned_file(&metadata)?;
        if metadata.len() > MAX_STATE_BYTES {
            return Err(state_error(
                "persisted applied state exceeds the size limit",
            ));
        }
        let mut json = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut json)
            .map_err(|error| state_error(format!("read persisted applied state: {error}")))?;
        if json.len() as u64 > MAX_STATE_BYTES {
            return Err(state_error(
                "persisted applied state exceeds the size limit",
            ));
        }
        serde_json::from_slice(&json)
            .map(Some)
            .map_err(|error| state_error(format!("decode persisted applied state: {error}")))
    }

    fn save(&mut self, state: &StoredAppliedState) -> Result<(), PrivilegedError> {
        ensure_root_owned_directory(&self.directory)?;
        let json = serde_json::to_vec(state)
            .map_err(|error| state_error(format!("encode applied state: {error}")))?;
        if json.len() as u64 > MAX_STATE_BYTES {
            return Err(state_error(
                "applied state exceeds the persistence size limit",
            ));
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| state_error(format!("read system clock: {error}")))?
            .as_nanos();
        let temporary = self
            .directory
            .join(format!(".applied-plan.{}.{nonce}.tmp", std::process::id()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|error| state_error(format!("create temporary state file: {error}")))?;
            file.write_all(&json)
                .map_err(|error| state_error(format!("write temporary state file: {error}")))?;
            file.sync_all()
                .map_err(|error| state_error(format!("sync temporary state file: {error}")))?;
            fs::rename(&temporary, &self.path).map_err(|error| {
                state_error(format!("atomically replace applied state: {error}"))
            })?;
            sync_directory(&self.directory)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn clear(&mut self) -> Result<(), PrivilegedError> {
        ensure_root_owned_directory(&self.directory)?;
        match fs::remove_file(&self.path) {
            Ok(()) => sync_directory(&self.directory),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(state_error(format!(
                "remove persisted applied state: {error}"
            ))),
        }
    }
}

fn ensure_root_owned_directory(path: &Path) -> Result<(), PrivilegedError> {
    fs::create_dir_all(path)
        .map_err(|error| state_error(format!("create state directory: {error}")))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| state_error(format!("inspect state directory: {error}")))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.uid() != 0 {
        return Err(state_error(
            "state directory must be a root-owned real directory",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| state_error(format!("secure state directory permissions: {error}")))
}

fn validate_root_owned_file(metadata: &fs::Metadata) -> Result<(), PrivilegedError> {
    validate_root_owned_file_attributes(metadata.is_file(), metadata.uid(), metadata.mode() & 0o777)
}

fn validate_root_owned_file_attributes(
    is_file: bool,
    uid: u32,
    mode: u32,
) -> Result<(), PrivilegedError> {
    if !is_file || uid != 0 || mode != 0o600 {
        return Err(state_error(
            "persisted state must be a root-owned, non-symlink 0600 file",
        ));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), PrivilegedError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| state_error(format!("sync state directory: {error}")))
}

fn state_error(message: impl Into<String>) -> PrivilegedError {
    PrivilegedError::new(format!(
        "applied-state persistence failed: {}",
        message.into()
    ))
}

pub struct CommandPrivilegedBackend<E, S = InMemoryAppliedStateStore> {
    executor: E,
    store: S,
    current: Option<AppliedKernelState>,
    current_nft_fingerprint: Option<String>,
    restore_receipt: Option<RestoreReceipt>,
    initialized: bool,
}

impl<E> CommandPrivilegedBackend<E, InMemoryAppliedStateStore> {
    pub fn new(executor: E) -> Self {
        Self::with_store(executor, InMemoryAppliedStateStore::default())
    }
}

impl<E, S> CommandPrivilegedBackend<E, S> {
    pub fn with_store(executor: E, store: S) -> Self {
        Self {
            executor,
            store,
            current: None,
            current_nft_fingerprint: None,
            restore_receipt: None,
            initialized: false,
        }
    }
}

pub type SystemPrivilegedBackend =
    CommandPrivilegedBackend<ProcessCommandExecutor, FileAppliedStateStore>;

impl Default for SystemPrivilegedBackend {
    fn default() -> Self {
        Self::with_store(ProcessCommandExecutor, FileAppliedStateStore::default())
    }
}

impl<E, S> PrivilegedBackend for CommandPrivilegedBackend<E, S>
where
    E: CommandExecutor,
    S: AppliedStateStore,
{
    fn apply(&mut self, plan: &KernelPlan) -> Result<ApplyResponse, PrivilegedError> {
        self.ensure_reconciled()?;
        self.revalidate_current_ownership()?;
        if let Some(current) = self.current.as_ref() {
            if plan.generation < current.generation {
                return Err(PrivilegedError::new(format!(
                    "requested generation {} is older than applied generation {}",
                    plan.generation, current.generation
                )));
            }
            if plan.generation == current.generation {
                if plan.content_hash() != current.plan_hash {
                    return Err(PrivilegedError::new(format!(
                        "generation {} has different policy content",
                        plan.generation
                    )));
                }
                if self.restore_receipt.is_some() {
                    persist_committed(
                        &mut self.store,
                        self.current.as_ref(),
                        self.current_nft_fingerprint.as_deref(),
                    )?;
                    self.restore_receipt = None;
                }
                return Ok(ApplyResponse::from_plan(plan, vec![]));
            }
        }
        self.transition_to(plan, None)
    }

    fn restore(
        &mut self,
        failed_candidate: &KernelPlan,
        previous: Option<&KernelPlan>,
    ) -> Result<RestoreResponse, PrivilegedError> {
        self.ensure_reconciled()?;
        self.revalidate_current_ownership()?;
        let receipt = RestoreReceipt::new(failed_candidate, previous);
        if previous.is_some_and(|plan| plan.generation >= failed_candidate.generation) {
            return Err(PrivilegedError::new(
                "restore target must be older than the failed candidate",
            ));
        }
        if self.restore_receipt.as_ref() == Some(&receipt)
            && receipt.matches_current(self.current.as_ref())
        {
            return Ok(RestoreResponse::from_plan(previous));
        }
        let current = self.current.as_ref().ok_or_else(|| {
            PrivilegedError::new("cannot restore because no candidate plan is currently applied")
        })?;
        if current.generation != failed_candidate.generation
            || current.plan_hash != failed_candidate.content_hash()
        {
            return Err(PrivilegedError::new(
                "cannot restore because the live committed plan is not the failed candidate",
            ));
        }
        match previous {
            Some(plan) => {
                self.transition_to(plan, Some(receipt))?;
                Ok(RestoreResponse::from_plan(Some(plan)))
            }
            None => {
                self.cleanup_with_receipt(Some(receipt))?;
                Ok(RestoreResponse::from_plan(None))
            }
        }
    }

    fn inspect(&mut self) -> Result<InspectResponse, PrivilegedError> {
        self.ensure_inspection_ready()?;
        let captured = capture_snapshot(&mut self.executor)
            .map_err(|error| PrivilegedError::new(error.to_string()))?;
        let nft_is_exact = match (
            self.current.as_ref(),
            self.current_nft_fingerprint.as_deref(),
        ) {
            (Some(current), Some(fingerprint)) => {
                captured.snapshot.has_nft_table("inet", "routelane")
                    && nft_marker_present(&mut self.executor, current.plan())
                        .map_err(|error| PrivilegedError::new(error.to_string()))?
                    && live_nft_fingerprint(&mut self.executor)
                        .map_err(|error| PrivilegedError::new(error.to_string()))?
                        == fingerprint
            }
            _ => false,
        };
        let snapshot = match (self.current.as_ref(), nft_is_exact) {
            (Some(current), true) => captured
                .snapshot
                .with_nft_ownership(current.generation, current.plan_hash),
            _ => captured.snapshot,
        };
        Ok(InspectResponse {
            generation: self.current.as_ref().map(|state| state.generation),
            plan_hash: self.current.as_ref().map(|state| state.plan_hash),
            applied_plan: self.current.as_ref().map(|state| state.plan().clone()),
            routelane_table_present: snapshot.has_nft_table("inet", "routelane"),
            snapshot,
            diagnostics: vec![],
        })
    }

    fn cleanup(&mut self) -> Result<CleanupResponse, PrivilegedError> {
        self.cleanup_with_receipt(None)
    }
}

impl<E, S> CommandPrivilegedBackend<E, S>
where
    E: CommandExecutor,
    S: AppliedStateStore,
{
    fn cleanup_with_receipt(
        &mut self,
        restore_receipt: Option<RestoreReceipt>,
    ) -> Result<CleanupResponse, PrivilegedError> {
        self.ensure_reconciled()?;
        let Some(current) = self.current.clone() else {
            if restore_receipt.is_none() {
                self.store.clear()?;
                self.restore_receipt = None;
            }
            return Ok(CleanupResponse {
                cleaned: false,
                diagnostics: vec!["no persisted RouteLane ownership to clean".to_owned()],
            });
        };
        verify_cleanup_table_ownership(
            &mut self.executor,
            Some(&current),
            self.current_nft_fingerprint.as_deref(),
            false,
        )?;

        self.store.save(&StoredAppliedState {
            active: Some(current.plan().clone()),
            active_nft_fingerprint: self.current_nft_fingerprint.clone(),
            pending: None,
            pending_expectations: None,
            cleanup_pending: true,
            restore_receipt: restore_receipt.clone(),
        })?;
        let mut runner = CommandKernelRunner::new(&mut self.executor, Some(current));
        runner
            .snapshot()
            .and_then(|_| runner.restore(None))
            .map_err(|error| PrivilegedError::new(error.to_string()))?;
        match restore_receipt.clone() {
            Some(receipt) => {
                self.store
                    .save(&StoredAppliedState::committed(None, None, Some(receipt)))?
            }
            None => self.store.clear()?,
        }
        self.current = None;
        self.current_nft_fingerprint = None;
        self.restore_receipt = restore_receipt;
        Ok(CleanupResponse {
            cleaned: true,
            diagnostics: vec![],
        })
    }
    fn ensure_inspection_ready(&mut self) -> Result<(), PrivilegedError> {
        if self.initialized {
            return self.ensure_reconciled();
        }
        let Some(record) = self.store.load()? else {
            self.initialized = true;
            return Ok(());
        };
        if record.pending.is_some() || record.cleanup_pending {
            return self.ensure_reconciled();
        }
        validate_stored_plan(record.active.as_ref(), "active")?;
        validate_stored_fingerprint_pair(
            record.active.as_ref(),
            record.active_nft_fingerprint.as_deref(),
        )?;
        validate_restore_receipt(&record)?;
        self.current = record.active.as_ref().map(AppliedKernelState::from_plan);
        self.current_nft_fingerprint = record.active_nft_fingerprint;
        self.restore_receipt = record.restore_receipt;
        self.initialized = true;
        Ok(())
    }

    fn transition_to(
        &mut self,
        plan: &KernelPlan,
        restore_receipt: Option<RestoreReceipt>,
    ) -> Result<ApplyResponse, PrivilegedError> {
        let previous = self.current.clone();
        let previous_nft_fingerprint = self.current_nft_fingerprint.clone();
        preflight_plan(&mut self.executor, previous.as_ref(), plan)?;
        let pending_expectations =
            build_pending_expectations(previous.as_ref().map(AppliedKernelState::plan), plan)?;
        self.store.save(&StoredAppliedState {
            active: previous.as_ref().map(|state| state.plan().clone()),
            active_nft_fingerprint: previous_nft_fingerprint.clone(),
            pending: Some(plan.clone()),
            pending_expectations: Some(pending_expectations.clone()),
            cleanup_pending: false,
            restore_receipt: restore_receipt.clone(),
        })?;
        self.restore_receipt = None;

        let mut runner = CommandKernelRunner::new(&mut self.executor, previous.clone());
        let transition = if restore_receipt.is_some() {
            restore_kernel_plan(
                &mut runner,
                previous
                    .as_ref()
                    .expect("restore transition has a failed candidate"),
                plan,
            )
        } else {
            apply_kernel_plan(&mut runner, previous.as_ref(), plan)
        };
        let applied = match transition {
            Ok(applied) => applied,
            Err(error) => {
                let rollback_succeeded =
                    error.rollback_error().is_none() && error.cleanup_error().is_none();
                let state_repair = repair_committed_journal_after_rollback(
                    &mut self.store,
                    &mut *runner.executor,
                    previous.as_ref(),
                    previous_nft_fingerprint.as_deref(),
                    rollback_succeeded,
                );
                return match state_repair {
                    Ok(()) => Err(PrivilegedError::new(error.to_string())),
                    Err(state_repair) => Err(PrivilegedError::new(format!(
                        "{error}; pending journal retained: {state_repair}"
                    ))),
                };
            }
        };

        let applied_nft_fingerprint = match live_nft_fingerprint(&mut *runner.executor) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                let rollback = runner.restore(previous.as_ref());
                let state_repair = repair_committed_journal_after_rollback(
                    &mut self.store,
                    &mut *runner.executor,
                    previous.as_ref(),
                    previous_nft_fingerprint.as_deref(),
                    rollback.is_ok(),
                );
                return Err(PrivilegedError::new(format!(
                    "kernel apply completed but exact nftables state could not be recorded: {error}; rollback: {}; state repair: {}",
                    result_label(&rollback),
                    result_label(&state_repair),
                )));
            }
        };
        let committed_routing_fingerprint = capture_snapshot(&mut *runner.executor)
            .map_err(|error| PrivilegedError::new(error.to_string()))
            .and_then(|captured| {
                live_routing_ownership(&captured)
                    .and_then(|ownership| routing_ownership_fingerprint(&ownership))
            });
        let exact_committed_state = committed_routing_fingerprint.as_deref()
            == Ok(pending_expectations.committed_routing_fingerprint.as_str())
            && applied_nft_fingerprint == pending_expectations.committed_nft_fingerprint;
        if !exact_committed_state {
            let mismatch = committed_routing_fingerprint.err().unwrap_or_else(|| {
                PrivilegedError::new(
                    "live kernel state differs from the exact journaled pending commit",
                )
            });
            let rollback = runner.restore(previous.as_ref());
            let state_repair = repair_committed_journal_after_rollback(
                &mut self.store,
                &mut *runner.executor,
                previous.as_ref(),
                previous_nft_fingerprint.as_deref(),
                rollback.is_ok(),
            );
            return Err(PrivilegedError::new(format!(
                "{mismatch}; rollback: {}; state repair: {}",
                result_label(&rollback),
                result_label(&state_repair),
            )));
        }

        if let Err(persistence) = persist_committed_with_receipt(
            &mut self.store,
            Some(&applied),
            Some(&applied_nft_fingerprint),
            restore_receipt.clone(),
        ) {
            let rollback = runner.restore(previous.as_ref());
            let state_repair = repair_committed_journal_after_rollback(
                &mut self.store,
                &mut *runner.executor,
                previous.as_ref(),
                previous_nft_fingerprint.as_deref(),
                rollback.is_ok(),
            );
            return Err(PrivilegedError::new(format!(
                "kernel apply completed but state commit failed: {persistence}; rollback: {}; state repair: {}",
                result_label(&rollback),
                result_label(&state_repair),
            )));
        }

        self.current = Some(applied);
        self.current_nft_fingerprint = Some(applied_nft_fingerprint);
        self.restore_receipt = restore_receipt;
        Ok(ApplyResponse::from_plan(plan, vec![]))
    }
    fn revalidate_current_ownership(&mut self) -> Result<(), PrivilegedError> {
        let Some(current) = self.current.as_ref() else {
            return Ok(());
        };
        let expected_nft_fingerprint =
            self.current_nft_fingerprint.as_deref().ok_or_else(|| {
                PrivilegedError::new(
                    "active RouteLane state is missing its committed nftables fingerprint",
                )
            })?;
        if !verify_persisted_ownership(
            &mut self.executor,
            current.plan(),
            expected_nft_fingerprint,
        )? {
            return Err(PrivilegedError::new(
                "committed RouteLane kernel resources are incomplete; refusing apply",
            ));
        }
        Ok(())
    }

    fn ensure_reconciled(&mut self) -> Result<(), PrivilegedError> {
        let record = match self.store.load()? {
            Some(record)
                if !self.initialized || record.pending.is_some() || record.cleanup_pending =>
            {
                record
            }
            Some(_) => return Ok(()),
            None if self.initialized => return Ok(()),
            None => {
                self.initialized = true;
                return Ok(());
            }
        };
        validate_stored_plan(record.active.as_ref(), "active")?;
        validate_stored_plan(record.pending.as_ref(), "pending")?;
        validate_stored_fingerprint_pair(
            record.active.as_ref(),
            record.active_nft_fingerprint.as_deref(),
        )?;
        validate_pending_expectations(
            record.active.as_ref(),
            record.pending.as_ref(),
            record.pending_expectations.as_ref(),
        )?;
        validate_restore_receipt(&record)?;

        let active = record.active.as_ref().map(AppliedKernelState::from_plan);
        if record.cleanup_pending {
            verify_cleanup_table_ownership(
                &mut self.executor,
                active.as_ref(),
                record.active_nft_fingerprint.as_deref(),
                true,
            )?;
            let mut runner = CommandKernelRunner::new(&mut self.executor, active.clone());
            runner
                .snapshot()
                .and_then(|_| runner.restore(None))
                .map_err(|error| {
                    PrivilegedError::new(format!("resume persisted cleanup: {error}"))
                })?;
            match record.restore_receipt.clone() {
                Some(receipt) => {
                    self.store
                        .save(&StoredAppliedState::committed(None, None, Some(receipt)))?
                }
                None => self.store.clear()?,
            }
            self.current = None;
            self.current_nft_fingerprint = None;
            self.restore_receipt = record.restore_receipt;
            self.initialized = true;
            return Ok(());
        }

        if let Some(pending) = &record.pending {
            let expectations = record
                .pending_expectations
                .as_ref()
                .expect("validated pending state has exact expectations");
            verify_pending_table_ownership(
                &mut self.executor,
                active.as_ref(),
                record.active_nft_fingerprint.as_deref(),
                pending,
                expectations,
            )?;
            let mut runner = CommandKernelRunner::new(&mut self.executor, active.clone());
            runner.next = Some(pending.clone());
            runner
                .snapshot()
                .and_then(|_| runner.restore(active.as_ref()))
                .map_err(|error| {
                    PrivilegedError::new(format!("recover pending kernel transaction: {error}"))
                })?;
            verify_exact_kernel_state(
                &mut self.executor,
                active.as_ref(),
                record.active_nft_fingerprint.as_deref(),
            )
            .map_err(|error| {
                PrivilegedError::new(format!(
                    "recovered kernel state differs from the exact rollback target; pending journal retained: {error}"
                ))
            })?;
            persist_committed(
                &mut self.store,
                active.as_ref(),
                record.active_nft_fingerprint.as_deref(),
            )?;
            self.current = active;
            self.current_nft_fingerprint = record.active_nft_fingerprint;
            self.restore_receipt = None;
            self.initialized = true;
            return Ok(());
        }

        if let Some(active) = active {
            let expected_nft_fingerprint = record
                .active_nft_fingerprint
                .as_deref()
                .expect("validated active state has an nft fingerprint");
            if !verify_persisted_ownership(
                &mut self.executor,
                active.plan(),
                expected_nft_fingerprint,
            )? {
                return Err(PrivilegedError::new(
                    "persisted RouteLane generation is incomplete; refusing implicit repair without a pending journal",
                ));
            }
            self.current = Some(active);
            self.current_nft_fingerprint = Some(expected_nft_fingerprint.to_owned());
            self.restore_receipt = record.restore_receipt;
        } else {
            if record.restore_receipt.is_none() {
                self.store.clear()?;
            }
            self.current = None;
            self.current_nft_fingerprint = None;
            self.restore_receipt = record.restore_receipt;
        }

        self.initialized = true;
        Ok(())
    }
}

fn validate_pending_expectations(
    active: Option<&KernelPlan>,
    pending: Option<&KernelPlan>,
    expectations: Option<&PendingExpectations>,
) -> Result<(), PrivilegedError> {
    match (pending, expectations) {
        (None, None) => Ok(()),
        (Some(pending), Some(expectations)) => {
            let expected = build_pending_expectations(active, pending)?;
            if &expected == expectations {
                Ok(())
            } else {
                Err(PrivilegedError::new(
                    "invalid persisted state: pending transition expectations do not match the journaled plans",
                ))
            }
        }
        _ => Err(PrivilegedError::new(
            "invalid persisted state: pending plan and exact transition expectations must be paired",
        )),
    }
}

fn validate_stored_fingerprint_pair(
    active: Option<&KernelPlan>,
    active_nft_fingerprint: Option<&str>,
) -> Result<(), PrivilegedError> {
    match (active, active_nft_fingerprint) {
        (None, None) => Ok(()),
        (Some(_), Some(fingerprint))
            if fingerprint.len() == 64
                && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Ok(())
        }
        _ => Err(PrivilegedError::new(
            "invalid persisted state: active plan and exact nftables fingerprint must be paired",
        )),
    }
}

fn validate_stored_plan(plan: Option<&KernelPlan>, label: &str) -> Result<(), PrivilegedError> {
    plan.map(validate_privileged_plan)
        .transpose()
        .map_err(|error| PrivilegedError::new(format!("invalid persisted {label} plan: {error}")))
        .map(|_| ())
}

fn validate_restore_receipt(record: &StoredAppliedState) -> Result<(), PrivilegedError> {
    let Some(receipt) = record.restore_receipt.as_ref() else {
        return Ok(());
    };
    if receipt.previous_generation.is_some() != receipt.previous_plan_hash.is_some() {
        return Err(PrivilegedError::new(
            "invalid persisted state: restore receipt previous generation and hash must be paired",
        ));
    }
    if record.cleanup_pending {
        if record.pending.is_some()
            || !receipt.failed_matches(record.active.as_ref())
            || !receipt.previous_matches(None)
        {
            return Err(PrivilegedError::new(
                "invalid persisted state: cleanup restore receipt does not bind the failed candidate to an empty target",
            ));
        }
        return Ok(());
    }
    if let Some(pending) = record.pending.as_ref() {
        if !receipt.failed_matches(record.active.as_ref())
            || !receipt.previous_matches(Some(pending))
        {
            return Err(PrivilegedError::new(
                "invalid persisted state: pending restore receipt does not bind the failed candidate and previous plan",
            ));
        }
        return Ok(());
    }
    if receipt.previous_matches(record.active.as_ref()) {
        Ok(())
    } else {
        Err(PrivilegedError::new(
            "invalid persisted state: committed restore receipt does not match the restored plan",
        ))
    }
}

fn persist_committed(
    store: &mut impl AppliedStateStore,
    state: Option<&AppliedKernelState>,
    nft_fingerprint: Option<&str>,
) -> Result<(), PrivilegedError> {
    persist_committed_with_receipt(store, state, nft_fingerprint, None)
}

fn persist_committed_with_receipt(
    store: &mut impl AppliedStateStore,
    state: Option<&AppliedKernelState>,
    nft_fingerprint: Option<&str>,
    restore_receipt: Option<RestoreReceipt>,
) -> Result<(), PrivilegedError> {
    match (state, nft_fingerprint) {
        (Some(state), Some(fingerprint)) => store.save(&StoredAppliedState::committed(
            Some(state.plan().clone()),
            Some(fingerprint.to_owned()),
            restore_receipt,
        )),
        (None, None) => match restore_receipt {
            Some(receipt) => store.save(&StoredAppliedState::committed(None, None, Some(receipt))),
            None => store.clear(),
        },
        _ => Err(PrivilegedError::new(
            "cannot persist an active plan without its exact nftables fingerprint",
        )),
    }
}

fn repair_committed_journal_after_rollback(
    store: &mut impl AppliedStateStore,
    executor: &mut impl CommandExecutor,
    state: Option<&AppliedKernelState>,
    nft_fingerprint: Option<&str>,
    rollback_succeeded: bool,
) -> Result<(), PrivilegedError> {
    if !rollback_succeeded {
        return Err(PrivilegedError::new(
            "rollback did not complete; exact pending state is still required for recovery",
        ));
    }
    verify_exact_kernel_state(executor, state, nft_fingerprint)?;
    persist_committed(store, state, nft_fingerprint)
}

fn verify_exact_kernel_state(
    executor: &mut impl CommandExecutor,
    state: Option<&AppliedKernelState>,
    nft_fingerprint: Option<&str>,
) -> Result<(), PrivilegedError> {
    match (state, nft_fingerprint) {
        (Some(state), Some(fingerprint)) => {
            if verify_persisted_ownership(executor, state.plan(), fingerprint)? {
                Ok(())
            } else {
                Err(PrivilegedError::new("exact rollback target is incomplete"))
            }
        }
        (None, None) => {
            let captured = capture_snapshot(executor)
                .map_err(|error| PrivilegedError::new(error.to_string()))?;
            if captured.snapshot.has_nft_table("inet", "routelane")
                || live_routing_ownership(&captured)? != RoutingOwnership::default()
            {
                return Err(PrivilegedError::new(
                    "exact empty rollback target still has live RouteLane resources",
                ));
            }
            Ok(())
        }
        _ => Err(PrivilegedError::new(
            "exact rollback target has an invalid plan/fingerprint pair",
        )),
    }
}

fn result_label<T, E: fmt::Display>(result: &Result<T, E>) -> String {
    match result {
        Ok(_) => "ok".to_owned(),
        Err(error) => error.to_string(),
    }
}

fn preflight_plan(
    executor: &mut impl CommandExecutor,
    current: Option<&AppliedKernelState>,
    next: &KernelPlan,
) -> Result<(), PrivilegedError> {
    let captured =
        capture_snapshot(executor).map_err(|error| PrivilegedError::new(error.to_string()))?;
    if current.is_none() && captured.snapshot.has_nft_table("inet", "routelane") {
        return Err(PrivilegedError::new(
            "routing resource conflict before journaling: unowned inet/routelane table already exists",
        ));
    }
    if current.is_none() && live_routing_ownership(&captured)? != RoutingOwnership::default() {
        return Err(PrivilegedError::new(
            "routing resource conflict before journaling: route or policy-rule state exists without persisted RouteLane ownership in the reserved ranges",
        ));
    }
    let conflicts = match current {
        Some(current) => {
            detect_resource_conflicts_for_update(next, current.plan(), &captured.snapshot)
        }
        None => detect_resource_conflicts(next, &captured.snapshot),
    };
    conflicts.map_err(|conflict| {
        PrivilegedError::new(format!(
            "routing resource conflict before journaling: {conflict:?}"
        ))
    })?;
    if unowned_requested_resource_present(&captured, current.map(AppliedKernelState::plan), next)
        .map_err(|error| PrivilegedError::new(error.to_string()))?
    {
        return Err(PrivilegedError::new(
            "routing resource conflict before journaling: requested route or policy-rule state exists without persisted RouteLane ownership",
        ));
    }
    Ok(())
}

fn unowned_requested_resource_present(
    captured: &CapturedSnapshot,
    current: Option<&KernelPlan>,
    next: &KernelPlan,
) -> Result<bool, RunnerError> {
    let ipv4_routes: Vec<serde_json::Value> = serde_json::from_slice(&captured.ipv4_routes)
        .map_err(|error| RunnerError::new(format!("invalid IPv4 route data: {error}")))?;
    let ipv6_routes: Vec<serde_json::Value> = serde_json::from_slice(&captured.ipv6_routes)
        .map_err(|error| RunnerError::new(format!("invalid IPv6 route data: {error}")))?;
    let ipv4_rules: Vec<serde_json::Value> = serde_json::from_slice(&captured.ipv4_rules)
        .map_err(|error| RunnerError::new(format!("invalid IPv4 rule data: {error}")))?;
    let ipv6_rules: Vec<serde_json::Value> = serde_json::from_slice(&captured.ipv6_rules)
        .map_err(|error| RunnerError::new(format!("invalid IPv6 rule data: {error}")))?;

    for lane in next
        .lanes
        .iter()
        .filter(|lane| lane.availability == super::LaneAvailability::Available)
    {
        if let Some(route) = lane.route_v4 {
            let gateway = route.gateway().map(ToString::to_string);
            if route_present(
                &ipv4_routes,
                lane.table,
                gateway.as_deref(),
                &lane.interface,
            ) && !current.is_some_and(|plan| plan_has_route(plan, IpFamily::V4, lane))
            {
                return Ok(true);
            }
            if rule_present(&ipv4_rules, lane.rule_priority, lane.table, lane.mark)
                && !current.is_some_and(|plan| plan_has_policy_rule(plan, IpFamily::V4, lane))
            {
                return Ok(true);
            }
        }
        if let Some(route) = lane.route_v6 {
            let gateway = route.gateway().map(ToString::to_string);
            if route_present(
                &ipv6_routes,
                lane.table,
                gateway.as_deref(),
                &lane.interface,
            ) && !current.is_some_and(|plan| plan_has_route(plan, IpFamily::V6, lane))
            {
                return Ok(true);
            }
            if rule_present(&ipv6_rules, lane.rule_priority, lane.table, lane.mark)
                && !current.is_some_and(|plan| plan_has_policy_rule(plan, IpFamily::V6, lane))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

struct CommandKernelRunner<'a, E> {
    executor: &'a mut E,
    previous: Option<AppliedKernelState>,
    next: Option<KernelPlan>,
    table_present: bool,
    guard_installed: bool,
}

impl<'a, E> CommandKernelRunner<'a, E> {
    fn new(executor: &'a mut E, previous: Option<AppliedKernelState>) -> Self {
        Self {
            executor,
            previous,
            next: None,
            table_present: false,
            guard_installed: false,
        }
    }
}

impl<E> KernelRunner for CommandKernelRunner<'_, E>
where
    E: CommandExecutor,
{
    fn snapshot(&mut self) -> Result<KernelSnapshot, RunnerError> {
        let captured = capture_snapshot(self.executor)?;
        self.table_present = captured.snapshot.has_nft_table("inet", "routelane");
        Ok(captured.snapshot)
    }

    fn install_guard(&mut self, next: &KernelPlan) -> Result<(), RunnerError> {
        self.next = Some(next.clone());
        let mode = if self.table_present {
            TableMode::Replace
        } else {
            TableMode::Create
        };
        let batch = render_transaction_ruleset(
            self.previous.as_ref().map(AppliedKernelState::plan),
            next,
            mode,
        )?;
        run_nft_batch(self.executor, batch, "install temporary RouteLane guard")?;
        self.table_present = true;
        self.guard_installed = true;
        Ok(())
    }

    fn prepare_routes(&mut self, next: &KernelPlan) -> Result<(), RunnerError> {
        let previous_commands = self
            .previous
            .as_ref()
            .map(|state| build_policy_commands(state.plan()))
            .unwrap_or_default();
        for command in build_policy_commands(next) {
            let is_policy_rule = command
                .args
                .starts_with(&["rule".to_owned(), "add".to_owned()]);
            if is_policy_rule && previous_commands.contains(&command) {
                continue;
            }
            run_required(
                self.executor,
                &command,
                "prepare RouteLane route or policy rule",
            )?;
        }
        Ok(())
    }

    fn activate_nft(&mut self, next: &KernelPlan) -> Result<(), RunnerError> {
        #[cfg(feature = "root-integration")]
        if std::env::var_os("ROUTELANE_TEST_FAIL_POINT").as_deref()
            == Some(std::ffi::OsStr::new("activate_nft"))
        {
            let captured = capture_snapshot(self.executor)?;
            if !captured_plan_present(&captured, next)? {
                return Err(RunnerError::new(
                    "root integration fail point reached before candidate routes and rules were prepared",
                ));
            }
            return Err(RunnerError::new(
                "root integration fail point before activate_nft",
            ));
        }
        let batch = render_guarded_ruleset(next)?;
        run_nft_batch(
            self.executor,
            batch,
            "activate generated RouteLane nftables ruleset",
        )?;
        self.table_present = true;
        self.guard_installed = true;
        Ok(())
    }

    fn verify(&mut self, next: &KernelPlan) -> Result<(), RunnerError> {
        verify_plan(self.executor, next)
    }

    fn remove_generation(&mut self, generation: u64) -> Result<(), RunnerError> {
        let previous = self
            .previous
            .as_ref()
            .filter(|state| state.generation == generation)
            .ok_or_else(|| RunnerError::new("previous applied generation is unavailable"))?
            .plan()
            .clone();
        let next = self
            .next
            .as_ref()
            .ok_or_else(|| RunnerError::new("next plan is unavailable during cleanup"))?
            .clone();

        remove_old_policy_rules(self.executor, &previous, &next)?;
        flush_unreused_route_tables(self.executor, &previous, &next)?;
        Ok(())
    }

    fn remove_guard(&mut self) -> Result<(), RunnerError> {
        if !self.guard_installed {
            return Ok(());
        }
        run_nft_batch(
            self.executor,
            b"flush chain inet routelane guard\ndelete chain inet routelane guard\n".to_vec(),
            "remove temporary RouteLane guard",
        )?;
        self.guard_installed = false;
        Ok(())
    }

    fn restore(&mut self, state: Option<&AppliedKernelState>) -> Result<(), RunnerError> {
        let mut plans = Vec::new();
        if let Some(previous) = &self.previous {
            plans.push(previous.plan().clone());
        }
        if let Some(next) = &self.next {
            plans.push(next.clone());
        }

        remove_plan_policy_rules(self.executor, &plans)?;
        flush_plan_route_tables(self.executor, &plans)?;

        if self.table_present {
            run_nft_batch(
                self.executor,
                b"delete table inet routelane\n".to_vec(),
                "clear RouteLane nftables state for rollback",
            )?;
            self.table_present = false;
        }
        self.guard_installed = false;

        if let Some(state) = state {
            for command in build_policy_commands(state.plan()) {
                run_required(
                    self.executor,
                    &command,
                    "restore previous RouteLane route or policy rule",
                )?;
            }
            let batch = render_owned_ruleset(state.plan(), TableMode::Create, false)?;
            run_nft_batch(
                self.executor,
                batch,
                "restore previous RouteLane nftables ruleset",
            )?;
            self.table_present = true;
            verify_plan(self.executor, state.plan())?;
        }

        Ok(())
    }
}

struct CapturedSnapshot {
    snapshot: KernelSnapshot,
    ipv4_rules: Vec<u8>,
    ipv6_rules: Vec<u8>,
    ipv4_routes: Vec<u8>,
    ipv6_routes: Vec<u8>,
}

fn capture_snapshot(executor: &mut impl CommandExecutor) -> Result<CapturedSnapshot, RunnerError> {
    let commands = build_snapshot_commands();
    let ipv4_routes = run_required(
        executor,
        &commands.ipv4_routes,
        "inspect IPv4 routing tables",
    )?;
    let ipv6_routes = run_required(
        executor,
        &commands.ipv6_routes,
        "inspect IPv6 routing tables",
    )?;
    let ipv4_rules = run_required(executor, &commands.ipv4_rules, "inspect IPv4 policy rules")?;
    let ipv6_rules = run_required(executor, &commands.ipv6_rules, "inspect IPv6 policy rules")?;
    let nft_command = CommandSpec {
        program: Program::Nft,
        family: None,
        args: ["-j", "list", "tables"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        stdin: None,
    };
    let nft = run_required(executor, &nft_command, "inspect nftables tables")?;

    let snapshot = KernelSnapshot::parse(SnapshotInput {
        ipv4_routes: &ipv4_routes,
        ipv6_routes: &ipv6_routes,
        ipv4_rules: &ipv4_rules,
        ipv6_rules: &ipv6_rules,
        nft: &nft,
    })
    .map_err(|error| RunnerError::new(error.to_string()))?;

    Ok(CapturedSnapshot {
        snapshot,
        ipv4_rules,
        ipv6_rules,
        ipv4_routes,
        ipv6_routes,
    })
}

fn render_guarded_ruleset(plan: &KernelPlan) -> Result<Vec<u8>, RunnerError> {
    render_owned_ruleset(plan, TableMode::Replace, true)
}

fn render_owned_ruleset(
    plan: &KernelPlan,
    mode: TableMode,
    include_guard: bool,
) -> Result<Vec<u8>, RunnerError> {
    render_nft_command_document(
        &expected_nft_document(Some(plan), None, include_guard),
        mode,
    )
}

fn render_transaction_ruleset(
    active: Option<&KernelPlan>,
    pending: &KernelPlan,
    mode: TableMode,
) -> Result<Vec<u8>, RunnerError> {
    render_nft_command_document(&expected_nft_document(active, Some(pending), true), mode)
}

fn render_nft_command_document(
    expected: &serde_json::Value,
    mode: TableMode,
) -> Result<Vec<u8>, RunnerError> {
    let objects = expected
        .get("nftables")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| RunnerError::new("expected nftables document is malformed"))?;
    let mut commands = Vec::with_capacity(objects.len() + usize::from(mode == TableMode::Replace));
    if mode == TableMode::Replace {
        commands.push(serde_json::json!({
            "delete": {"table": {"family": "inet", "name": "routelane"}}
        }));
    }
    commands.extend(
        objects
            .iter()
            .filter(|object| object.as_object().is_none_or(|object| !object.is_empty()))
            .map(|object| serde_json::json!({"add": object})),
    );
    serde_json::to_vec(&serde_json::json!({"nftables": commands}))
        .map_err(|error| RunnerError::new(format!("encode nftables JSON batch: {error}")))
}

fn expected_nft_document(
    plan: Option<&KernelPlan>,
    pending_transaction: Option<&KernelPlan>,
    include_guard: bool,
) -> serde_json::Value {
    let mut objects = vec![
        serde_json::json!({}),
        serde_json::json!({
            "table": {"family": "inet", "name": "routelane"}
        }),
    ];
    if let Some(plan) = plan {
        objects.push(chain_object(&state_chain_name(plan)));
    }
    if let Some(pending) = pending_transaction {
        objects.push(chain_object(&transaction_chain_name(pending)));
    }
    if include_guard {
        objects.push(serde_json::json!({
            "chain": {
                "family": "inet",
                "table": "routelane",
                "name": "guard",
                "type": "route",
                "hook": "output",
                "prio": -149,
                "policy": "accept",
            }
        }));
        objects.push(serde_json::json!({
            "rule": {
                "family": "inet",
                "table": "routelane",
                "chain": "guard",
                "expr": [
                    {
                        "match": {
                            "op": ">=",
                            "left": {"meta": {"key": "mark"}},
                            "right": MARK_BASE,
                        }
                    },
                    {
                        "match": {
                            "op": "<=",
                            "left": {"meta": {"key": "mark"}},
                            "right": MARK_MAX,
                        }
                    },
                    {"reject": {}},
                ],
            }
        }));
    }
    if let Some(plan) = plan {
        objects.push(serde_json::json!({
            "chain": {
                "family": "inet",
                "table": "routelane",
                "name": "output",
                "type": "route",
                "hook": "output",
                "prio": -150,
                "policy": "accept",
            }
        }));
        objects.extend(plan.rules.iter().map(nft_rule_object));
    }
    serde_json::json!({"nftables": objects})
}

fn chain_object(name: &str) -> serde_json::Value {
    serde_json::json!({
        "chain": {"family": "inet", "table": "routelane", "name": name}
    })
}

fn nft_rule_object(rule: &super::CompiledRule) -> serde_json::Value {
    let mut expressions = Vec::new();
    if let CompiledTarget::Network(network) = &rule.target {
        let protocol = if network.addr().is_ipv4() {
            "ip"
        } else {
            "ip6"
        };
        let destination = if network.prefix_len() == if network.addr().is_ipv4() { 32 } else { 128 }
        {
            serde_json::Value::String(network.addr().to_string())
        } else {
            serde_json::json!({
                "prefix": {
                    "addr": network.addr().to_string(),
                    "len": network.prefix_len(),
                }
            })
        };
        expressions.push(serde_json::json!({
            "match": {
                "op": "==",
                "left": {"payload": {"protocol": protocol, "field": "daddr"}},
                "right": destination,
            }
        }));
    }
    match rule.action {
        CompiledAction::Mark(mark) => {
            expressions.push(serde_json::json!({
                "mangle": {
                    "key": {"meta": {"key": "mark"}},
                    "value": mark,
                }
            }));
            expressions.push(serde_json::json!({"counter": null}));
            expressions.push(serde_json::json!({"return": null}));
        }
        CompiledAction::Direct => {
            expressions.push(serde_json::json!({
                "mangle": {
                    "key": {"meta": {"key": "mark"}},
                    "value": 0,
                }
            }));
            expressions.push(serde_json::json!({"counter": null}));
            expressions.push(serde_json::json!({"return": null}));
        }
        CompiledAction::Block => {
            expressions.push(serde_json::json!({"counter": null}));
            expressions.push(serde_json::json!({"reject": {}}));
        }
    }
    serde_json::json!({
        "rule": {
            "family": "inet",
            "table": "routelane",
            "chain": "output",
            "expr": expressions,
        }
    })
}

fn state_chain_name(plan: &KernelPlan) -> String {
    let serialized =
        serde_json::to_vec(&plan.content_hash()).expect("serializing a plan hash cannot fail");
    let fingerprint = blake3::hash(&serialized).to_hex();
    format!("state_g{}_{:.16}", plan.generation, fingerprint)
}

fn transaction_chain_name(plan: &KernelPlan) -> String {
    let serialized =
        serde_json::to_vec(&plan.content_hash()).expect("serializing a plan hash cannot fail");
    let fingerprint = blake3::hash(&serialized).to_hex();
    format!("transaction_g{}_{:.16}", plan.generation, fingerprint)
}

fn run_nft_batch(
    executor: &mut impl CommandExecutor,
    batch: Vec<u8>,
    context: &str,
) -> Result<(), RunnerError> {
    let is_json = batch.first() == Some(&b'{');
    let command = CommandSpec {
        program: Program::Nft,
        family: None,
        args: if is_json {
            vec!["-j".to_owned(), "-f".to_owned(), "-".to_owned()]
        } else {
            vec!["-f".to_owned(), "-".to_owned()]
        },
        stdin: Some(batch),
    };
    run_required(executor, &command, context).map(|_| ())
}

fn run_required(
    executor: &mut impl CommandExecutor,
    command: &CommandSpec,
    context: &str,
) -> Result<Vec<u8>, RunnerError> {
    let output = executor.execute(command)?;
    if !output.success {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RunnerError::new(format!("{context}: {}", stderr.trim())));
    }
    Ok(output.stdout)
}

fn ip_command<I, S>(family: IpFamily, args: I) -> CommandSpec
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    match family {
        IpFamily::V4 => CommandSpec::ip4(args),
        IpFamily::V6 => CommandSpec::ip6(args),
    }
}

fn verify_plan(executor: &mut impl CommandExecutor, plan: &KernelPlan) -> Result<(), RunnerError> {
    let captured = capture_snapshot(executor)?;
    if !captured_plan_present(&captured, plan)? {
        return Err(RunnerError::new(
            "generated RouteLane route or policy resources are incomplete",
        ));
    }
    if !nft_marker_present(executor, plan)? {
        return Err(RunnerError::new(
            "generated RouteLane nftables ownership marker is not active",
        ));
    }
    Ok(())
}

fn captured_plan_present(
    captured: &CapturedSnapshot,
    plan: &KernelPlan,
) -> Result<bool, RunnerError> {
    if !captured.snapshot.has_nft_table("inet", "routelane") {
        return Ok(false);
    }
    let ipv4_routes: Vec<serde_json::Value> = serde_json::from_slice(&captured.ipv4_routes)
        .map_err(|error| {
            RunnerError::new(format!("invalid IPv4 route verification data: {error}"))
        })?;
    let ipv6_routes: Vec<serde_json::Value> = serde_json::from_slice(&captured.ipv6_routes)
        .map_err(|error| {
            RunnerError::new(format!("invalid IPv6 route verification data: {error}"))
        })?;
    let ipv4_rules: Vec<serde_json::Value> =
        serde_json::from_slice(&captured.ipv4_rules).map_err(|error| {
            RunnerError::new(format!("invalid IPv4 rule verification data: {error}"))
        })?;
    let ipv6_rules: Vec<serde_json::Value> =
        serde_json::from_slice(&captured.ipv6_rules).map_err(|error| {
            RunnerError::new(format!("invalid IPv6 rule verification data: {error}"))
        })?;

    for lane in plan
        .lanes
        .iter()
        .filter(|lane| lane.availability == super::LaneAvailability::Available)
    {
        if let Some(route) = lane.route_v4 {
            let gateway = route.gateway().map(ToString::to_string);
            if !route_present(
                &ipv4_routes,
                lane.table,
                gateway.as_deref(),
                &lane.interface,
            ) || !rule_present(&ipv4_rules, lane.rule_priority, lane.table, lane.mark)
            {
                return Ok(false);
            }
        }
        if let Some(route) = lane.route_v6 {
            let gateway = route.gateway().map(ToString::to_string);
            if !route_present(
                &ipv6_routes,
                lane.table,
                gateway.as_deref(),
                &lane.interface,
            ) || !rule_present(&ipv6_rules, lane.rule_priority, lane.table, lane.mark)
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn verify_persisted_ownership(
    executor: &mut impl CommandExecutor,
    plan: &KernelPlan,
    expected_nft_fingerprint: &str,
) -> Result<bool, PrivilegedError> {
    let captured =
        capture_snapshot(executor).map_err(|error| PrivilegedError::new(error.to_string()))?;
    detect_resource_conflicts(plan, &captured.snapshot).map_err(|conflict| {
        PrivilegedError::new(format!(
            "persisted plan conflicts with live kernel resources: {conflict:?}"
        ))
    })?;
    let live_routing = live_routing_ownership(&captured)?;
    let expected_routing = plan_routing_ownership(plan);
    if live_routing != expected_routing {
        return Err(PrivilegedError::new(
            "persisted plan does not exactly own all live RouteLane route and policy-rule resources",
        ));
    }

    let table_present = captured.snapshot.has_nft_table("inet", "routelane");
    if table_present
        && !nft_marker_present(executor, plan)
            .map_err(|error| PrivilegedError::new(error.to_string()))?
    {
        return Err(PrivilegedError::new(
            "persisted plan does not own the live inet/routelane table",
        ));
    }
    if table_present {
        let live_fingerprint = live_nft_fingerprint(executor)
            .map_err(|error| PrivilegedError::new(error.to_string()))?;
        if live_fingerprint != expected_nft_fingerprint {
            return Err(PrivilegedError::new(
                "persisted plan ownership marker is present but the live inet/routelane table differs from the exact committed state",
            ));
        }
    }
    captured_plan_present(&captured, plan).map_err(|error| PrivilegedError::new(error.to_string()))
}

fn verify_cleanup_table_ownership(
    executor: &mut impl CommandExecutor,
    active: Option<&AppliedKernelState>,
    active_nft_fingerprint: Option<&str>,
    allow_command_prefix: bool,
) -> Result<(), PrivilegedError> {
    let captured =
        capture_snapshot(executor).map_err(|error| PrivilegedError::new(error.to_string()))?;
    let (active, fingerprint) = active.zip(active_nft_fingerprint).ok_or_else(|| {
        PrivilegedError::new("cleanup journal cannot own a live inet/routelane table")
    })?;
    detect_resource_conflicts(active.plan(), &captured.snapshot).map_err(|conflict| {
        PrivilegedError::new(format!(
            "cleanup journal conflicts with live kernel resources: {conflict:?}"
        ))
    })?;
    let live_routing = live_routing_ownership(&captured)?;
    let live_routing_fingerprint = routing_ownership_fingerprint(&live_routing)?;
    let valid_routing = if allow_command_prefix {
        cleanup_routing_fingerprints(active.plan())?.contains(&live_routing_fingerprint)
    } else {
        live_routing == plan_routing_ownership(active.plan())
    };
    if !valid_routing {
        return Err(PrivilegedError::new(
            "cleanup journal does not match an exact RouteLane cleanup command prefix",
        ));
    }
    if !captured.snapshot.has_nft_table("inet", "routelane") {
        return if allow_command_prefix && live_routing == RoutingOwnership::default() {
            Ok(())
        } else {
            Err(PrivilegedError::new(
                "cleanup journal is missing its committed nftables table before routing cleanup completed",
            ))
        };
    }
    if !nft_marker_present(executor, active.plan())
        .map_err(|error| PrivilegedError::new(error.to_string()))?
        || live_nft_fingerprint(executor)
            .map_err(|error| PrivilegedError::new(error.to_string()))?
            != fingerprint
    {
        return Err(PrivilegedError::new(
            "cleanup journal does not exactly own the live inet/routelane table",
        ));
    }
    Ok(())
}

fn cleanup_routing_fingerprints(plan: &KernelPlan) -> Result<Vec<String>, PrivilegedError> {
    let mut state = plan_routing_ownership(plan);
    let mut fingerprints = vec![routing_ownership_fingerprint(&state)?];
    let mut removed_rules = BTreeSet::new();
    for lane in plan
        .lanes
        .iter()
        .filter(|lane| lane.availability == super::LaneAvailability::Available)
    {
        for family in [IpFamily::V4, IpFamily::V6] {
            if lane_route_ownership(lane, family).is_none() {
                continue;
            }
            let rule = lane_rule_ownership(lane, family);
            if removed_rules.insert(rule.clone()) {
                state.rules.remove(&rule);
                push_routing_fingerprint(&mut fingerprints, &state)?;
            }
        }
    }
    for (family, table) in plan_route_keys(plan) {
        state
            .routes
            .retain(|route| route.family != family_number(family) || route.table != table);
        push_routing_fingerprint(&mut fingerprints, &state)?;
    }
    Ok(fingerprints)
}

fn verify_pending_table_ownership(
    executor: &mut impl CommandExecutor,
    active: Option<&AppliedKernelState>,
    active_nft_fingerprint: Option<&str>,
    pending: &KernelPlan,
    expectations: &PendingExpectations,
) -> Result<(), PrivilegedError> {
    let captured =
        capture_snapshot(executor).map_err(|error| PrivilegedError::new(error.to_string()))?;
    let table_present = captured.snapshot.has_nft_table("inet", "routelane");
    let live_fingerprint = table_present
        .then(|| live_nft_fingerprint(executor))
        .transpose()
        .map_err(|error| PrivilegedError::new(error.to_string()))?;
    let phase = match (active, active_nft_fingerprint, live_fingerprint.as_deref()) {
        (None, None, None) => PendingNftPhase::Untouched,
        (Some(_), Some(expected), Some(live)) if live == expected => PendingNftPhase::Untouched,
        (Some(_), Some(_), None) => PendingNftPhase::RollbackAbsent,
        (_, _, Some(live)) if live == expectations.guard_nft_fingerprint => PendingNftPhase::Guard,
        (_, _, Some(live)) if live == expectations.guardless_transaction_nft_fingerprint => {
            PendingNftPhase::GuardlessTransaction
        }
        (_, _, Some(live)) if live == expectations.activated_nft_fingerprint => {
            PendingNftPhase::Activated
        }
        (_, _, Some(live)) if live == expectations.committed_nft_fingerprint => {
            PendingNftPhase::Committed
        }
        _ => {
            return Err(PrivilegedError::new(
                "pending journal does not exactly match any persisted nftables crash state",
            ));
        }
    };
    validate_pending_routing_resources(
        &captured,
        active.map(AppliedKernelState::plan),
        pending,
        phase,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingNftPhase {
    Untouched,
    Guard,
    GuardlessTransaction,
    Activated,
    Committed,
    RollbackAbsent,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct RouteOwnership {
    family: u8,
    table: u32,
    gateway: Option<String>,
    interface: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct RuleOwnership {
    family: u8,
    priority: u32,
    table: u32,
    mark: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
struct RoutingOwnership {
    routes: BTreeSet<RouteOwnership>,
    rules: BTreeSet<RuleOwnership>,
}

fn validate_pending_routing_resources(
    captured: &CapturedSnapshot,
    active: Option<&KernelPlan>,
    pending: &KernelPlan,
    phase: PendingNftPhase,
) -> Result<(), PrivilegedError> {
    let live = live_routing_ownership(captured)?;
    let valid = routing_state_reachable_for_phase(&live, active, pending, phase)?;
    if valid {
        Ok(())
    } else {
        Err(PrivilegedError::new(
            "pending journal live route or policy-rule state is outside the persisted crash expectations",
        ))
    }
}

fn routing_state_reachable_for_phase(
    live: &RoutingOwnership,
    active: Option<&KernelPlan>,
    pending: &KernelPlan,
    phase: PendingNftPhase,
) -> Result<bool, PrivilegedError> {
    if phase == PendingNftPhase::RollbackAbsent {
        return Ok(build_absent_rollback_states(active)
            .iter()
            .any(|state| state == live));
    }
    if phase == PendingNftPhase::Committed {
        let committed = build_forward_routing_states(active, pending, PendingNftPhase::Committed)?;
        if committed
            .iter()
            .any(|state| state == live || rollback_prefix_reaches(state, active, pending, live))
        {
            return Ok(true);
        }
        let activated = build_forward_routing_states(active, pending, PendingNftPhase::Activated)?;
        return Ok(activated
            .iter()
            .any(|state| state == live || rollback_prefix_reaches(state, active, pending, live)));
    }
    let phase = if phase == PendingNftPhase::GuardlessTransaction {
        PendingNftPhase::Guard
    } else {
        phase
    };
    let starting_states = build_forward_routing_states(active, pending, phase)?;
    Ok(starting_states
        .iter()
        .any(|state| state == live || rollback_prefix_reaches(state, active, pending, live)))
}

fn plan_routing_ownership(plan: &KernelPlan) -> RoutingOwnership {
    let mut ownership = RoutingOwnership::default();
    for lane in plan
        .lanes
        .iter()
        .filter(|lane| lane.availability == super::LaneAvailability::Available)
    {
        if let Some(route) = lane_route_ownership(lane, IpFamily::V4) {
            ownership.routes.insert(route);
            ownership
                .rules
                .insert(lane_rule_ownership(lane, IpFamily::V4));
        }
        if let Some(route) = lane_route_ownership(lane, IpFamily::V6) {
            ownership.routes.insert(route);
            ownership
                .rules
                .insert(lane_rule_ownership(lane, IpFamily::V6));
        }
    }
    ownership
}

fn build_forward_routing_states(
    active: Option<&KernelPlan>,
    pending: &KernelPlan,
    phase: PendingNftPhase,
) -> Result<Vec<RoutingOwnership>, PrivilegedError> {
    let mut state = active.map(plan_routing_ownership).unwrap_or_default();
    if phase == PendingNftPhase::Untouched {
        return Ok(vec![state]);
    }
    let mut guard_states = vec![state.clone()];

    for lane in pending
        .lanes
        .iter()
        .filter(|lane| lane.availability == super::LaneAvailability::Available)
    {
        for family in [IpFamily::V4, IpFamily::V6] {
            if let Some(route) = lane_route_ownership(lane, family) {
                state.routes.retain(|existing| {
                    existing.family != route.family || existing.table != route.table
                });
                state.routes.insert(route);
                guard_states.push(state.clone());
            }
        }
    }
    for lane in pending
        .lanes
        .iter()
        .filter(|lane| lane.availability == super::LaneAvailability::Available)
    {
        for family in [IpFamily::V4, IpFamily::V6] {
            if lane_route_ownership(lane, family).is_some() {
                state.rules.insert(lane_rule_ownership(lane, family));
                guard_states.push(state.clone());
            }
        }
    }
    if phase == PendingNftPhase::Guard {
        return Ok(guard_states);
    }

    let mut activated_states = vec![state.clone()];
    if let Some(active) = active {
        for lane in active
            .lanes
            .iter()
            .filter(|lane| lane.availability == super::LaneAvailability::Available)
        {
            for family in [IpFamily::V4, IpFamily::V6] {
                if lane_route_ownership(lane, family).is_some()
                    && !plan_has_policy_rule(pending, family, lane)
                {
                    state.rules.remove(&lane_rule_ownership(lane, family));
                    activated_states.push(state.clone());
                }
            }
        }
        let pending_route_keys = plan_route_keys(pending);
        for (family, table) in plan_route_keys(active) {
            if pending_route_keys.contains(&(family, table)) {
                continue;
            }
            state
                .routes
                .retain(|route| route.family != family_number(family) || route.table != table);
            activated_states.push(state.clone());
        }
    }
    let committed_state = plan_routing_ownership(pending);
    if state != committed_state {
        return Err(PrivilegedError::new(
            "cannot derive an exact reachable pending routing state",
        ));
    }
    match phase {
        PendingNftPhase::Activated => Ok(activated_states),
        PendingNftPhase::Committed => Ok(vec![committed_state]),
        PendingNftPhase::Untouched
        | PendingNftPhase::Guard
        | PendingNftPhase::GuardlessTransaction => unreachable!(),
        PendingNftPhase::RollbackAbsent => Err(PrivilegedError::new(
            "rollback-without-nft routing states use the restore-prefix model",
        )),
    }
}

fn rollback_prefix_reaches(
    starting_state: &RoutingOwnership,
    active: Option<&KernelPlan>,
    pending: &KernelPlan,
    expected: &RoutingOwnership,
) -> bool {
    let mut state = starting_state.clone();
    let mut rules = BTreeSet::new();
    for plan in active.into_iter().chain(std::iter::once(pending)) {
        for lane in plan
            .lanes
            .iter()
            .filter(|lane| lane.availability == super::LaneAvailability::Available)
        {
            for family in [IpFamily::V4, IpFamily::V6] {
                if lane_route_ownership(lane, family).is_none() {
                    continue;
                }
                let rule = lane_rule_ownership(lane, family);
                if rules.insert(rule.clone()) && state.rules.remove(&rule) && state == *expected {
                    return true;
                }
            }
        }
    }

    let mut route_keys = Vec::new();
    for key in active
        .into_iter()
        .chain(std::iter::once(pending))
        .flat_map(plan_route_keys)
    {
        if !route_keys.contains(&key) {
            route_keys.push(key);
        }
    }
    for (family, table) in route_keys {
        let old_len = state.routes.len();
        state
            .routes
            .retain(|route| route.family != family_number(family) || route.table != table);
        if state.routes.len() != old_len && state == *expected {
            return true;
        }
    }
    false
}

fn build_absent_rollback_states(active: Option<&KernelPlan>) -> Vec<RoutingOwnership> {
    let mut state = RoutingOwnership::default();
    let mut states = vec![state.clone()];
    let Some(active) = active else {
        return states;
    };
    for lane in active
        .lanes
        .iter()
        .filter(|lane| lane.availability == super::LaneAvailability::Available)
    {
        for family in [IpFamily::V4, IpFamily::V6] {
            if let Some(route) = lane_route_ownership(lane, family) {
                state.routes.retain(|existing| {
                    existing.family != route.family || existing.table != route.table
                });
                state.routes.insert(route);
                states.push(state.clone());
            }
        }
    }
    for lane in active
        .lanes
        .iter()
        .filter(|lane| lane.availability == super::LaneAvailability::Available)
    {
        for family in [IpFamily::V4, IpFamily::V6] {
            if lane_route_ownership(lane, family).is_some() {
                state.rules.insert(lane_rule_ownership(lane, family));
                states.push(state.clone());
            }
        }
    }
    states
}

fn routing_ownership_fingerprint(ownership: &RoutingOwnership) -> Result<String, PrivilegedError> {
    let encoded = serde_json::to_vec(ownership)
        .map_err(|error| PrivilegedError::new(format!("encode routing ownership: {error}")))?;
    Ok(blake3::hash(&encoded).to_hex().to_string())
}

fn push_routing_fingerprint(
    fingerprints: &mut Vec<String>,
    ownership: &RoutingOwnership,
) -> Result<(), PrivilegedError> {
    let fingerprint = routing_ownership_fingerprint(ownership)?;
    if !fingerprints.contains(&fingerprint) {
        fingerprints.push(fingerprint);
    }
    Ok(())
}

fn lane_route_ownership(lane: &super::LanePlan, family: IpFamily) -> Option<RouteOwnership> {
    let gateway = match family {
        IpFamily::V4 => lane.route_v4?.gateway().map(ToString::to_string),
        IpFamily::V6 => lane.route_v6?.gateway().map(ToString::to_string),
    };
    Some(RouteOwnership {
        family: family_number(family),
        table: lane.table,
        gateway,
        interface: lane.interface.clone(),
    })
}

fn lane_rule_ownership(lane: &super::LanePlan, family: IpFamily) -> RuleOwnership {
    RuleOwnership {
        family: family_number(family),
        priority: lane.rule_priority,
        table: lane.table,
        mark: lane.mark,
    }
}

fn family_number(family: IpFamily) -> u8 {
    match family {
        IpFamily::V4 => 4,
        IpFamily::V6 => 6,
    }
}

fn live_routing_ownership(
    captured: &CapturedSnapshot,
) -> Result<RoutingOwnership, PrivilegedError> {
    let mut ownership = RoutingOwnership::default();
    collect_live_routes(&captured.ipv4_routes, 4, &mut ownership)?;
    collect_live_routes(&captured.ipv6_routes, 6, &mut ownership)?;
    collect_live_rules(&captured.ipv4_rules, 4, &mut ownership)?;
    collect_live_rules(&captured.ipv6_rules, 6, &mut ownership)?;
    Ok(ownership)
}

fn collect_live_routes(
    json: &[u8],
    family: u8,
    ownership: &mut RoutingOwnership,
) -> Result<(), PrivilegedError> {
    let routes: Vec<serde_json::Value> = serde_json::from_slice(json)
        .map_err(|error| PrivilegedError::new(format!("invalid live route JSON: {error}")))?;
    for route in routes {
        let Some(table) = json_u32(route.get("table")) else {
            continue;
        };
        if !(TABLE_BASE..=TABLE_MAX).contains(&table) {
            continue;
        }
        let object = route.as_object().ok_or_else(|| {
            PrivilegedError::new("live RouteLane routing-table entry is not an object")
        })?;
        let allowed = [
            "type", "dst", "gateway", "dev", "table", "metric", "protocol", "scope", "prefsrc",
            "flags", "pref",
        ];
        if object.keys().any(|key| !allowed.contains(&key.as_str()))
            || route.get("dst").and_then(serde_json::Value::as_str) != Some("default")
            || !absent_or_exact_string(route.get("type"), "unicast")
            || !absent_or_exact_u32(route.get("metric"), if family == 4 { 0 } else { 1024 })
            || !absent_or_exact_string(route.get("protocol"), "boot")
            || !route_scope_is_exact(
                route.get("scope"),
                route
                    .get("gateway")
                    .and_then(serde_json::Value::as_str)
                    .is_none(),
            )
            || route.get("prefsrc").is_some()
            || !absent_or_empty_array(route.get("flags"))
            || (family == 4 && route.get("pref").is_some())
            || (family == 6 && !absent_or_exact_string(route.get("pref"), "medium"))
        {
            return Err(PrivilegedError::new(
                "live RouteLane routing-table entry has foreign semantics",
            ));
        }
        let resource = RouteOwnership {
            family,
            table,
            gateway: route
                .get("gateway")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            interface: route
                .get("dev")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    PrivilegedError::new("live RouteLane route is missing its interface")
                })?
                .to_owned(),
        };
        if !ownership.routes.insert(resource) {
            return Err(PrivilegedError::new(
                "live RouteLane routing-table entry is duplicated",
            ));
        }
    }
    Ok(())
}

fn collect_live_rules(
    json: &[u8],
    family: u8,
    ownership: &mut RoutingOwnership,
) -> Result<(), PrivilegedError> {
    let rules: Vec<serde_json::Value> = serde_json::from_slice(json)
        .map_err(|error| PrivilegedError::new(format!("invalid live policy-rule JSON: {error}")))?;
    for rule in rules {
        let Some(priority) = json_u32(rule.get("priority")) else {
            continue;
        };
        if !(RULE_PRIORITY_BASE..=RULE_PRIORITY_MAX).contains(&priority) {
            continue;
        }
        let object = rule.as_object().ok_or_else(|| {
            PrivilegedError::new("live RouteLane policy-rule entry is not an object")
        })?;
        let allowed = ["priority", "table", "fwmark", "mark", "src", "dst"];
        if object.keys().any(|key| !allowed.contains(&key.as_str()))
            || !absent_or_exact_string(rule.get("src"), "all")
            || !absent_or_exact_string(rule.get("dst"), "all")
        {
            return Err(PrivilegedError::new(
                "live RouteLane policy-rule entry has foreign semantics",
            ));
        }
        let resource = RuleOwnership {
            family,
            priority,
            table: json_u32(rule.get("table")).ok_or_else(|| {
                PrivilegedError::new("live RouteLane policy rule is missing its table")
            })?,
            mark: rule
                .get("fwmark")
                .or_else(|| rule.get("mark"))
                .and_then(parse_exact_mark)
                .ok_or_else(|| {
                    PrivilegedError::new("live RouteLane policy rule is missing its mark")
                })?,
        };
        if !ownership.rules.insert(resource) {
            return Err(PrivilegedError::new(
                "live RouteLane policy-rule entry is duplicated",
            ));
        }
    }
    Ok(())
}

fn absent_or_exact_string(value: Option<&serde_json::Value>, expected: &str) -> bool {
    value.is_none_or(|value| value.as_str() == Some(expected))
}

fn route_scope_is_exact(value: Option<&serde_json::Value>, gatewayless: bool) -> bool {
    value.is_none_or(|value| {
        value.as_str() == Some("global") || (gatewayless && value.as_str() == Some("link"))
    })
}

fn absent_or_exact_u32(value: Option<&serde_json::Value>, expected: u32) -> bool {
    value.is_none_or(|value| json_u32(Some(value)) == Some(expected))
}

fn absent_or_empty_array(value: Option<&serde_json::Value>) -> bool {
    value.is_none_or(|value| value.as_array().is_some_and(Vec::is_empty))
}

fn parse_exact_mark(value: &serde_json::Value) -> Option<u32> {
    match value {
        serde_json::Value::Number(number) => number.as_u64()?.try_into().ok(),
        serde_json::Value::String(text) => {
            let (mark, mask) = text
                .split_once('/')
                .map_or((text.as_str(), None), |(mark, mask)| (mark, Some(mask)));
            if mask.is_some_and(|mask| parse_u32_text(mask) != Some(u32::MAX)) {
                return None;
            }
            parse_u32_text(mark)
        }
        _ => None,
    }
}

fn parse_u32_text(value: &str) -> Option<u32> {
    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map_or_else(
            || value.parse().ok(),
            |hex| u32::from_str_radix(hex, 16).ok(),
        )
}

fn nft_marker_present(
    executor: &mut impl CommandExecutor,
    plan: &KernelPlan,
) -> Result<bool, RunnerError> {
    nft_chain_present(executor, &state_chain_name(plan))
}

fn nft_chain_present(
    executor: &mut impl CommandExecutor,
    expected: &str,
) -> Result<bool, RunnerError> {
    let document = read_nft_table(executor)?;
    Ok(document
        .get("nftables")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|objects| {
            objects.iter().any(|object| {
                let chain = object.get("chain");
                chain
                    .and_then(|chain| chain.get("family"))
                    .and_then(serde_json::Value::as_str)
                    == Some("inet")
                    && chain
                        .and_then(|chain| chain.get("table"))
                        .and_then(serde_json::Value::as_str)
                        == Some("routelane")
                    && chain
                        .and_then(|chain| chain.get("name"))
                        .and_then(serde_json::Value::as_str)
                        == Some(expected)
            })
        }))
}

fn live_nft_fingerprint(executor: &mut impl CommandExecutor) -> Result<String, RunnerError> {
    let document = read_nft_table(executor)?;
    nft_document_fingerprint(&document)
}

fn nft_document_fingerprint(document: &serde_json::Value) -> Result<String, RunnerError> {
    let mut document = document.clone();
    normalize_nft_document(&mut document);
    let canonical = serde_json::to_vec(&document)
        .map_err(|error| RunnerError::new(format!("encode canonical nftables table: {error}")))?;
    Ok(blake3::hash(&canonical).to_hex().to_string())
}

fn build_pending_expectations(
    active: Option<&KernelPlan>,
    pending: &KernelPlan,
) -> Result<PendingExpectations, PrivilegedError> {
    let guard_nft_fingerprint =
        nft_document_fingerprint(&expected_nft_document(active, Some(pending), true))
            .map_err(|error| PrivilegedError::new(error.to_string()))?;
    let guardless_transaction_nft_fingerprint =
        nft_document_fingerprint(&expected_nft_document(active, Some(pending), false))
            .map_err(|error| PrivilegedError::new(error.to_string()))?;
    let activated_nft_fingerprint =
        nft_document_fingerprint(&expected_nft_document(Some(pending), None, true))
            .map_err(|error| PrivilegedError::new(error.to_string()))?;
    let committed_nft_fingerprint =
        nft_document_fingerprint(&expected_nft_document(Some(pending), None, false))
            .map_err(|error| PrivilegedError::new(error.to_string()))?;
    let committed_routing_fingerprint =
        routing_ownership_fingerprint(&plan_routing_ownership(pending))?;
    let routing_model_fingerprint = routing_model_fingerprint(active, pending)?;
    Ok(PendingExpectations {
        guard_nft_fingerprint,
        guardless_transaction_nft_fingerprint,
        activated_nft_fingerprint,
        committed_nft_fingerprint,
        committed_routing_fingerprint,
        routing_model_fingerprint,
    })
}

fn routing_model_fingerprint(
    active: Option<&KernelPlan>,
    pending: &KernelPlan,
) -> Result<String, PrivilegedError> {
    let model = (
        ROUTING_CRASH_MODEL_VERSION,
        active.map(KernelPlan::content_hash),
        pending.content_hash(),
    );
    let encoded = serde_json::to_vec(&model)
        .map_err(|error| PrivilegedError::new(format!("encode routing crash model: {error}")))?;
    Ok(blake3::hash(&encoded).to_hex().to_string())
}

fn read_nft_table(executor: &mut impl CommandExecutor) -> Result<serde_json::Value, RunnerError> {
    let command = CommandSpec {
        program: Program::Nft,
        family: None,
        args: ["-j", "list", "table", "inet", "routelane"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        stdin: None,
    };
    let output = run_required(executor, &command, "inspect exact RouteLane nftables table")?;
    serde_json::from_slice(&output)
        .map_err(|error| RunnerError::new(format!("invalid nftables table JSON: {error}")))
}

fn normalize_nft_document(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            object.remove("metainfo");
            object.remove("handle");
            if object.get("counter").is_some_and(|counter| {
                counter.as_object().is_some_and(|counter| {
                    counter.contains_key("packets") || counter.contains_key("bytes")
                })
            }) {
                object.insert("counter".to_owned(), serde_json::Value::Null);
            }
            for nested in object.values_mut() {
                normalize_nft_document(nested);
            }
        }
        serde_json::Value::Array(values) => {
            for nested in values {
                normalize_nft_document(nested);
            }
        }
        _ => {}
    }
}

fn route_present(
    routes: &[serde_json::Value],
    table: u32,
    gateway: Option<&str>,
    interface: &str,
) -> bool {
    routes.iter().any(|route| {
        route.get("dst").and_then(serde_json::Value::as_str) == Some("default")
            && json_u32(route.get("table")) == Some(table)
            && route.get("gateway").and_then(serde_json::Value::as_str) == gateway
            && route.get("dev").and_then(serde_json::Value::as_str) == Some(interface)
    })
}

fn rule_present(rules: &[serde_json::Value], priority: u32, table: u32, mark: u32) -> bool {
    rules.iter().any(|rule| {
        json_u32(rule.get("priority")) == Some(priority)
            && json_u32(rule.get("table")) == Some(table)
            && rule.get("fwmark").and_then(parse_mark) == Some(mark)
    })
}

fn json_u32(value: Option<&serde_json::Value>) -> Option<u32> {
    match value? {
        serde_json::Value::Number(number) => number.as_u64()?.try_into().ok(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

fn parse_mark(value: &serde_json::Value) -> Option<u32> {
    match value {
        serde_json::Value::Number(number) => number.as_u64()?.try_into().ok(),
        serde_json::Value::String(text) => {
            let value = text
                .split_once('/')
                .map_or(text.as_str(), |(value, _)| value);
            value.strip_prefix("0x").map_or_else(
                || value.parse().ok(),
                |hex| u32::from_str_radix(hex, 16).ok(),
            )
        }
        _ => None,
    }
}

fn remove_old_policy_rules(
    executor: &mut impl CommandExecutor,
    previous: &KernelPlan,
    next: &KernelPlan,
) -> Result<(), RunnerError> {
    for lane in previous
        .lanes
        .iter()
        .filter(|lane| lane.availability == super::LaneAvailability::Available)
    {
        if lane.route_v4.is_some() && !plan_has_policy_rule(next, IpFamily::V4, lane) {
            delete_one_rule_if_present(executor, IpFamily::V4, lane)?;
        }
        if lane.route_v6.is_some() && !plan_has_policy_rule(next, IpFamily::V6, lane) {
            delete_one_rule_if_present(executor, IpFamily::V6, lane)?;
        }
    }
    Ok(())
}

fn plan_has_policy_rule(plan: &KernelPlan, family: IpFamily, expected: &super::LanePlan) -> bool {
    plan.lanes.iter().any(|lane| {
        lane.availability == super::LaneAvailability::Available
            && lane.table == expected.table
            && lane.mark == expected.mark
            && lane.rule_priority == expected.rule_priority
            && match family {
                IpFamily::V4 => lane.route_v4.is_some(),
                IpFamily::V6 => lane.route_v6.is_some(),
            }
    })
}

fn plan_has_route(plan: &KernelPlan, family: IpFamily, expected: &super::LanePlan) -> bool {
    plan.lanes.iter().any(|lane| {
        lane.availability == super::LaneAvailability::Available
            && lane.table == expected.table
            && lane.interface == expected.interface
            && match family {
                IpFamily::V4 => lane.route_v4 == expected.route_v4,
                IpFamily::V6 => lane.route_v6 == expected.route_v6,
            }
    })
}

fn delete_one_rule_if_present(
    executor: &mut impl CommandExecutor,
    family: IpFamily,
    lane: &super::LanePlan,
) -> Result<(), RunnerError> {
    if !live_rule_present(executor, family, lane)? {
        return Ok(());
    }
    let command = ip_command(
        family,
        [
            "rule".to_owned(),
            "del".to_owned(),
            "fwmark".to_owned(),
            format!("{:#010x}", lane.mark),
            "lookup".to_owned(),
            lane.table.to_string(),
            "priority".to_owned(),
            lane.rule_priority.to_string(),
        ],
    );
    run_required(executor, &command, "remove previous RouteLane policy rule").map(|_| ())
}

fn live_rule_present(
    executor: &mut impl CommandExecutor,
    family: IpFamily,
    lane: &super::LanePlan,
) -> Result<bool, RunnerError> {
    let command = ip_command(family, ["-j", "-N", "rule", "show"]);
    let output = run_required(executor, &command, "inspect RouteLane policy rule")?;
    let rules: Vec<serde_json::Value> = serde_json::from_slice(&output)
        .map_err(|error| RunnerError::new(format!("invalid policy rule JSON: {error}")))?;
    Ok(rule_present(
        &rules,
        lane.rule_priority,
        lane.table,
        lane.mark,
    ))
}

fn flush_unreused_route_tables(
    executor: &mut impl CommandExecutor,
    previous: &KernelPlan,
    next: &KernelPlan,
) -> Result<(), RunnerError> {
    let next_routes = plan_route_keys(next);
    let captured = capture_snapshot(executor)?;
    for (family, table) in plan_route_keys(previous) {
        if next_routes.contains(&(family, table)) {
            continue;
        }
        let routes = match family {
            IpFamily::V4 => &captured.ipv4_routes,
            IpFamily::V6 => &captured.ipv6_routes,
        };
        if !route_table_present(routes, table)? {
            continue;
        }
        let command = ip_command(
            family,
            [
                "route".to_owned(),
                "flush".to_owned(),
                "table".to_owned(),
                table.to_string(),
            ],
        );
        run_required(executor, &command, "flush previous RouteLane routing table")?;
    }
    Ok(())
}

fn plan_route_keys(plan: &KernelPlan) -> Vec<(IpFamily, u32)> {
    let mut keys = Vec::new();
    for lane in plan
        .lanes
        .iter()
        .filter(|lane| lane.availability == super::LaneAvailability::Available)
    {
        let ipv4 = (IpFamily::V4, lane.table);
        if lane.route_v4.is_some() && !keys.contains(&ipv4) {
            keys.push(ipv4);
        }
        let ipv6 = (IpFamily::V6, lane.table);
        if lane.route_v6.is_some() && !keys.contains(&ipv6) {
            keys.push(ipv6);
        }
    }
    keys
}

fn remove_plan_policy_rules(
    executor: &mut impl CommandExecutor,
    plans: &[KernelPlan],
) -> Result<(), RunnerError> {
    let mut rules = Vec::new();
    for plan in plans {
        for lane in plan
            .lanes
            .iter()
            .filter(|lane| lane.availability == super::LaneAvailability::Available)
        {
            if lane.route_v4.is_some() {
                let rule = (IpFamily::V4, lane.rule_priority, lane.table, lane.mark);
                if !rules.contains(&rule) {
                    rules.push(rule);
                }
            }
            if lane.route_v6.is_some() {
                let rule = (IpFamily::V6, lane.rule_priority, lane.table, lane.mark);
                if !rules.contains(&rule) {
                    rules.push(rule);
                }
            }
        }
    }

    for (family, priority, table, mark) in rules {
        for _ in 0..3 {
            let lane = super::LanePlan {
                id: LaneId::from("rollback"),
                interface: "lo".to_owned(),
                route_v4: None,
                route_v6: None,
                availability: super::LaneAvailability::Available,
                table,
                mark,
                rule_priority: priority,
                failure_policy: crate::domain::LaneFailurePolicy::Inactive,
            };
            if !live_rule_present(executor, family, &lane)? {
                break;
            }
            delete_one_rule_if_present(executor, family, &lane)?;
        }
        let lane = super::LanePlan {
            id: LaneId::from("rollback"),
            interface: "lo".to_owned(),
            route_v4: None,
            route_v6: None,
            availability: super::LaneAvailability::Available,
            table,
            mark,
            rule_priority: priority,
            failure_policy: crate::domain::LaneFailurePolicy::Inactive,
        };
        if live_rule_present(executor, family, &lane)? {
            return Err(RunnerError::new(
                "too many duplicate RouteLane policy rules during rollback",
            ));
        }
    }
    Ok(())
}

fn flush_plan_route_tables(
    executor: &mut impl CommandExecutor,
    plans: &[KernelPlan],
) -> Result<(), RunnerError> {
    let mut keys = Vec::new();
    for key in plans.iter().flat_map(plan_route_keys) {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    let captured = capture_snapshot(executor)?;
    for (family, table) in keys {
        let routes = match family {
            IpFamily::V4 => &captured.ipv4_routes,
            IpFamily::V6 => &captured.ipv6_routes,
        };
        if !route_table_present(routes, table)? {
            continue;
        }
        let command = ip_command(
            family,
            [
                "route".to_owned(),
                "flush".to_owned(),
                "table".to_owned(),
                table.to_string(),
            ],
        );
        run_required(executor, &command, "flush RouteLane table during rollback")?;
    }
    Ok(())
}

fn route_table_present(input: &[u8], table: u32) -> Result<bool, RunnerError> {
    let routes: Vec<serde_json::Value> = serde_json::from_slice(input)
        .map_err(|error| RunnerError::new(format!("invalid route JSON: {error}")))?;
    Ok(routes
        .iter()
        .any(|route| json_u32(route.get("table")) == Some(table)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    ZeroGeneration,
    TooManyLanes { actual: usize, maximum: usize },
    TooManyRules { actual: usize, maximum: usize },
    TableOutOfRange(u32),
    MarkOutOfRange(u32),
    PriorityOutOfRange(u32),
    DuplicateLaneId(LaneId),
    DuplicateTable(u32),
    DuplicateMark(u32),
    DuplicatePriority(u32),
    InvalidInterface(String),
    UnknownMark(u32),
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroGeneration => formatter.write_str("generation must be nonzero"),
            Self::TooManyLanes { actual, maximum } => {
                write!(formatter, "plan has {actual} lanes; maximum is {maximum}")
            }
            Self::TooManyRules { actual, maximum } => {
                write!(formatter, "plan has {actual} rules; maximum is {maximum}")
            }
            Self::TableOutOfRange(table) => {
                write!(
                    formatter,
                    "routing table {table} is outside the reserved range"
                )
            }
            Self::MarkOutOfRange(mark) => {
                write!(formatter, "mark {mark:#010x} is outside the reserved range")
            }
            Self::PriorityOutOfRange(priority) => write!(
                formatter,
                "rule priority {priority} is outside the reserved range"
            ),
            Self::DuplicateLaneId(id) => write!(formatter, "duplicate lane ID {id:?}"),
            Self::DuplicateTable(table) => write!(formatter, "duplicate routing table {table}"),
            Self::DuplicateMark(mark) => write!(formatter, "duplicate mark {mark:#010x}"),
            Self::DuplicatePriority(priority) => {
                write!(formatter, "duplicate rule priority {priority}")
            }
            Self::InvalidInterface(interface) => {
                write!(formatter, "invalid kernel interface name {interface:?}")
            }
            Self::UnknownMark(mark) => {
                write!(
                    formatter,
                    "compiled rule refers to unknown mark {mark:#010x}"
                )
            }
        }
    }
}

impl std::error::Error for ValidationError {}

pub fn validate_privileged_plan(plan: &KernelPlan) -> Result<(), ValidationError> {
    if plan.generation == 0 {
        return Err(ValidationError::ZeroGeneration);
    }
    if plan.lanes.len() > MAX_LANES {
        return Err(ValidationError::TooManyLanes {
            actual: plan.lanes.len(),
            maximum: MAX_LANES,
        });
    }
    if plan.rules.len() > MAX_RULES {
        return Err(ValidationError::TooManyRules {
            actual: plan.rules.len(),
            maximum: MAX_RULES,
        });
    }

    let mut lane_ids = BTreeSet::new();
    let mut tables = BTreeSet::new();
    let mut marks = BTreeSet::new();
    let mut priorities = BTreeSet::new();

    for lane in &plan.lanes {
        if !(TABLE_BASE..=TABLE_MAX).contains(&lane.table) {
            return Err(ValidationError::TableOutOfRange(lane.table));
        }
        if !(MARK_BASE..=MARK_MAX).contains(&lane.mark) {
            return Err(ValidationError::MarkOutOfRange(lane.mark));
        }
        if !(RULE_PRIORITY_BASE..=RULE_PRIORITY_MAX).contains(&lane.rule_priority) {
            return Err(ValidationError::PriorityOutOfRange(lane.rule_priority));
        }
        if !is_kernel_interface_name(&lane.interface) {
            return Err(ValidationError::InvalidInterface(lane.interface.clone()));
        }
        if !lane_ids.insert(lane.id.clone()) {
            return Err(ValidationError::DuplicateLaneId(lane.id.clone()));
        }
        if !tables.insert(lane.table) {
            return Err(ValidationError::DuplicateTable(lane.table));
        }
        if !marks.insert(lane.mark) {
            return Err(ValidationError::DuplicateMark(lane.mark));
        }
        if !priorities.insert(lane.rule_priority) {
            return Err(ValidationError::DuplicatePriority(lane.rule_priority));
        }
    }

    for rule in &plan.rules {
        if let CompiledAction::Mark(mark) = rule.action {
            if !marks.contains(&mark) {
                return Err(ValidationError::UnknownMark(mark));
            }
        }
    }

    Ok(())
}

fn is_kernel_interface_name(interface: &str) -> bool {
    !interface.is_empty()
        && interface.len() <= 15
        && interface
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizationError {
    UidLookup(String),
    PolicyCheck(String),
    Denied,
    Challenge,
}

impl fmt::Display for AuthorizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UidLookup(message) => write!(formatter, "caller UID lookup failed: {message}"),
            Self::PolicyCheck(message) => {
                write!(formatter, "authorization check failed: {message}")
            }
            Self::Denied => formatter.write_str("caller is not authorized"),
            Self::Challenge => formatter.write_str("authorization requires an unmet challenge"),
        }
    }
}

impl std::error::Error for AuthorizationError {}

pub trait Authorizer: Send + Sync {
    type Future<'a>: Future<Output = Result<(), AuthorizationError>> + Send + 'a
    where
        Self: 'a;

    fn authorize<'a>(&'a self, sender: &'a str) -> Self::Future<'a>;
}

#[derive(Clone)]
pub struct SystemPolkitAuthorizer {
    connection: zbus::Connection,
}

impl SystemPolkitAuthorizer {
    pub fn new(connection: zbus::Connection) -> Self {
        Self { connection }
    }
}

impl Authorizer for SystemPolkitAuthorizer {
    type Future<'a> = Pin<Box<dyn Future<Output = Result<(), AuthorizationError>> + Send + 'a>>;

    fn authorize<'a>(&'a self, sender: &'a str) -> Self::Future<'a> {
        Box::pin(async move {
            let sender_name = zbus::names::BusName::try_from(sender)
                .map_err(|error| AuthorizationError::UidLookup(error.to_string()))?;
            let dbus = zbus::fdo::DBusProxy::new(&self.connection)
                .await
                .map_err(|error| AuthorizationError::UidLookup(error.to_string()))?;
            let _caller_uid = dbus
                .get_connection_unix_user(sender_name)
                .await
                .map_err(|error| AuthorizationError::UidLookup(error.to_string()))?;

            let policykit = zbus::Proxy::new(
                &self.connection,
                "org.freedesktop.PolicyKit1",
                "/org/freedesktop/PolicyKit1/Authority",
                "org.freedesktop.PolicyKit1.Authority",
            )
            .await
            .map_err(|error| AuthorizationError::PolicyCheck(error.to_string()))?;
            let subject_details = HashMap::from([("name", zbus::zvariant::Value::from(sender))]);
            let subject = ("system-bus-name", subject_details);
            let details = HashMap::<&str, &str>::new();
            let (authorized, challenge, _details): (bool, bool, HashMap<String, String>) =
                policykit
                    .call(
                        "CheckAuthorization",
                        &(subject, "io.github.routelane.apply", details, 1u32, ""),
                    )
                    .await
                    .map_err(|error| AuthorizationError::PolicyCheck(error.to_string()))?;

            authorization_result(authorized, challenge)
        })
    }
}

fn authorization_result(authorized: bool, challenge: bool) -> Result<(), AuthorizationError> {
    if challenge {
        Err(AuthorizationError::Challenge)
    } else if authorized {
        Ok(())
    } else {
        Err(AuthorizationError::Denied)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyResponse {
    pub generation: u64,
    pub plan_hash: PlanHash,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreResponse {
    pub generation: Option<u64>,
    pub plan_hash: Option<PlanHash>,
}

impl RestoreResponse {
    pub fn from_plan(plan: Option<&KernelPlan>) -> Self {
        Self {
            generation: plan.map(|plan| plan.generation),
            plan_hash: plan.map(KernelPlan::content_hash),
        }
    }
}

impl ApplyResponse {
    pub fn from_plan(plan: &KernelPlan, diagnostics: Vec<String>) -> Self {
        Self {
            generation: plan.generation,
            plan_hash: plan.content_hash(),
            diagnostics,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectResponse {
    pub generation: Option<u64>,
    pub plan_hash: Option<PlanHash>,
    pub applied_plan: Option<KernelPlan>,
    pub routelane_table_present: bool,
    pub snapshot: KernelSnapshot,
    pub diagnostics: Vec<String>,
}

impl InspectResponse {
    pub fn empty() -> Self {
        Self {
            generation: None,
            plan_hash: None,
            applied_plan: None,
            routelane_table_present: false,
            snapshot: KernelSnapshot::default(),
            diagnostics: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupResponse {
    pub cleaned: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivilegedError {
    message: String,
}

impl PrivilegedError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for PrivilegedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PrivilegedError {}

pub trait PrivilegedBackend: Send {
    fn apply(&mut self, plan: &KernelPlan) -> Result<ApplyResponse, PrivilegedError>;
    fn restore(
        &mut self,
        failed_candidate: &KernelPlan,
        previous: Option<&KernelPlan>,
    ) -> Result<RestoreResponse, PrivilegedError>;
    fn inspect(&mut self) -> Result<InspectResponse, PrivilegedError>;
    fn cleanup(&mut self) -> Result<CleanupResponse, PrivilegedError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    MissingSender,
    Authorization(AuthorizationError),
    PlanTooLarge { actual: usize, maximum: usize },
    InvalidPlanJson(String),
    Validation(ValidationError),
    Backend(String),
    StatePoisoned,
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSender => formatter.write_str("D-Bus method call has no unique sender"),
            Self::Authorization(error) => error.fmt(formatter),
            Self::PlanTooLarge { actual, maximum } => write!(
                formatter,
                "plan JSON is {actual} bytes; maximum is {maximum} bytes"
            ),
            Self::InvalidPlanJson(message) => write!(formatter, "invalid plan JSON: {message}"),
            Self::Validation(error) => error.fmt(formatter),
            Self::Backend(message) => write!(formatter, "privileged backend failed: {message}"),
            Self::StatePoisoned => formatter.write_str("privileged helper state is unavailable"),
        }
    }
}

impl std::error::Error for ServiceError {}

pub struct PrivilegedService<A, B> {
    authorizer: A,
    backend: Arc<Mutex<B>>,
}

impl<A, B> PrivilegedService<A, B>
where
    A: Authorizer,
    B: PrivilegedBackend + 'static,
{
    pub fn new(authorizer: A, backend: B) -> Self {
        Self {
            authorizer,
            backend: Arc::new(Mutex::new(backend)),
        }
    }

    pub async fn apply_plan(
        &self,
        sender: Option<&str>,
        plan_json: &str,
    ) -> Result<ApplyResponse, ServiceError> {
        let sender = self.authorize(sender).await?;
        let _ = sender;

        if plan_json.len() > MAX_PLAN_JSON_BYTES {
            return Err(ServiceError::PlanTooLarge {
                actual: plan_json.len(),
                maximum: MAX_PLAN_JSON_BYTES,
            });
        }

        let plan: KernelPlan = serde_json::from_str(plan_json)
            .map_err(|error| ServiceError::InvalidPlanJson(error.to_string()))?;
        validate_privileged_plan(&plan).map_err(ServiceError::Validation)?;

        self.run_backend(move |backend| backend.apply(&plan)).await
    }

    pub async fn inspect(&self, _sender: Option<&str>) -> Result<InspectResponse, ServiceError> {
        self.run_backend(PrivilegedBackend::inspect).await
    }

    pub async fn restore_plan(
        &self,
        sender: Option<&str>,
        failed_candidate_json: &str,
        previous_json: Option<&str>,
    ) -> Result<RestoreResponse, ServiceError> {
        self.authorize(sender).await?;
        let failed_candidate = decode_plan(failed_candidate_json)?;
        let previous = previous_json.map(decode_plan).transpose()?;
        self.run_backend(move |backend| backend.restore(&failed_candidate, previous.as_ref()))
            .await
    }

    pub async fn cleanup(&self, sender: Option<&str>) -> Result<CleanupResponse, ServiceError> {
        self.authorize(sender).await?;
        self.run_backend(PrivilegedBackend::cleanup).await
    }

    async fn authorize<'a>(&self, sender: Option<&'a str>) -> Result<&'a str, ServiceError> {
        let sender = sender.ok_or(ServiceError::MissingSender)?;
        self.authorizer
            .authorize(sender)
            .await
            .map_err(ServiceError::Authorization)?;
        Ok(sender)
    }

    async fn run_backend<R, F>(&self, operation: F) -> Result<R, ServiceError>
    where
        R: Send + 'static,
        F: FnOnce(&mut B) -> Result<R, PrivilegedError> + Send + 'static,
    {
        let backend = Arc::clone(&self.backend);
        tokio::task::spawn_blocking(move || {
            let mut backend = backend.lock().map_err(|_| ServiceError::StatePoisoned)?;
            operation(&mut backend).map_err(|error| ServiceError::Backend(error.to_string()))
        })
        .await
        .map_err(|_| ServiceError::StatePoisoned)?
    }
}

fn decode_plan(plan_json: &str) -> Result<KernelPlan, ServiceError> {
    if plan_json.len() > MAX_PLAN_JSON_BYTES {
        return Err(ServiceError::PlanTooLarge {
            actual: plan_json.len(),
            maximum: MAX_PLAN_JSON_BYTES,
        });
    }
    let plan: KernelPlan = serde_json::from_str(plan_json)
        .map_err(|error| ServiceError::InvalidPlanJson(error.to_string()))?;
    validate_privileged_plan(&plan).map_err(ServiceError::Validation)?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use std::future::{ready, Ready};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use crate::domain::{LaneFailurePolicy, LaneId, ProfileId, RuleId};

    use super::{
        authorization_result, build_pending_expectations, command_invocation,
        execute_invocation_with_timeout, nft_document_fingerprint, nft_rule_object,
        render_guarded_ruleset, render_owned_ruleset, render_transaction_ruleset, run_nft_batch,
        validate_privileged_plan, validate_root_owned_file_attributes, AppliedStateStore,
        ApplyResponse, AuthorizationError, Authorizer, CleanupResponse, CommandExecutor,
        CommandInvocation, CommandOutput, CommandPrivilegedBackend, InspectResponse,
        PrivilegedBackend, PrivilegedError, PrivilegedService, RestoreReceipt, RestoreResponse,
        ServiceError, StoredAppliedState, ValidationError, IP_BINARY, MAX_PLAN_JSON_BYTES,
        NFT_BINARY,
    };
    use crate::backend::{
        CommandSpec, CompiledAction, CompiledRule, CompiledTarget, IpFamily, KernelPlan,
        LaneAvailability, LanePlan, Program, ResourceState, RouteMode, TableMode, MARK_BASE,
        RULE_PRIORITY_BASE, TABLE_BASE,
    };

    fn fixture_lane(id: &str, offset: u32) -> LanePlan {
        LanePlan {
            id: LaneId::from(id),
            interface: format!("wg{offset}"),
            route_v4: Some(RouteMode::Via(format!("10.0.{offset}.1").parse().unwrap())),
            route_v6: None,
            availability: LaneAvailability::Available,
            table: TABLE_BASE + offset,
            mark: MARK_BASE + offset,
            rule_priority: RULE_PRIORITY_BASE + offset,
            failure_policy: LaneFailurePolicy::Inactive,
        }
    }

    fn fixture_valid_plan() -> KernelPlan {
        KernelPlan {
            generation: 1,
            profile_id: ProfileId::from("default"),
            lanes: vec![fixture_lane("privacy", 0)],
            rules: vec![CompiledRule {
                id: RuleId::from("route-example"),
                priority: 10,
                target: CompiledTarget::Network("203.0.113.0/24".parse().unwrap()),
                action: CompiledAction::Mark(MARK_BASE),
            }],
        }
    }

    fn fixture_two_lane_plan() -> KernelPlan {
        let mut plan = fixture_valid_plan();
        plan.lanes.push(fixture_lane("work", 1));
        plan
    }

    #[test]
    fn privileged_nft_renderer_makes_mark_action_terminal_after_counter() {
        let mut plan = fixture_valid_plan();
        let rule = plan.rules.remove(0);

        assert_eq!(
            nft_rule_object(&rule)["rule"]["expr"],
            serde_json::json!([
                {
                    "match": {
                        "op": "==",
                        "left": {"payload": {"protocol": "ip", "field": "daddr"}},
                        "right": {"prefix": {"addr": "203.0.113.0", "len": 24}}
                    }
                },
                {
                    "mangle": {
                        "key": {"meta": {"key": "mark"}},
                        "value": MARK_BASE
                    }
                },
                {"counter": null},
                {"return": null}
            ])
        );
    }

    #[test]
    fn fingerprint_ignores_live_counter_values_for_a_counter_expression() {
        let expected = serde_json::json!({
            "nftables": [{"rule": {"expr": [{"counter": null}]}}]
        });
        let live = serde_json::json!({
            "nftables": [{"rule": {"expr": [{
                "counter": {"packets": 12, "bytes": 34}
            }]}}]
        });

        assert_eq!(
            nft_document_fingerprint(&expected).unwrap(),
            nft_document_fingerprint(&live).unwrap()
        );
    }

    #[test]
    fn rejects_mark_outside_routelane_range() {
        let mut plan = fixture_valid_plan();
        plan.lanes[0].mark = 0x1;

        assert_eq!(
            validate_privileged_plan(&plan),
            Err(ValidationError::MarkOutOfRange(0x1))
        );
    }

    #[test]
    fn rejects_table_outside_routelane_range() {
        let mut plan = fixture_valid_plan();
        plan.lanes[0].table = 254;

        assert_eq!(
            validate_privileged_plan(&plan),
            Err(ValidationError::TableOutOfRange(254))
        );
    }

    #[test]
    fn rejects_duplicate_marks_even_when_each_mark_is_in_range() {
        let mut plan = fixture_two_lane_plan();
        plan.lanes[1].mark = plan.lanes[0].mark;

        assert_eq!(
            validate_privileged_plan(&plan),
            Err(ValidationError::DuplicateMark(MARK_BASE))
        );
    }

    #[test]
    fn validates_every_privileged_plan_boundary() {
        let cases = [
            (
                {
                    let mut plan = fixture_valid_plan();
                    plan.generation = 0;
                    plan
                },
                ValidationError::ZeroGeneration,
            ),
            (
                {
                    let mut plan = fixture_valid_plan();
                    plan.lanes[0].rule_priority = 42;
                    plan
                },
                ValidationError::PriorityOutOfRange(42),
            ),
            (
                {
                    let mut plan = fixture_two_lane_plan();
                    plan.lanes[1].id = plan.lanes[0].id.clone();
                    plan
                },
                ValidationError::DuplicateLaneId(LaneId::from("privacy")),
            ),
            (
                {
                    let mut plan = fixture_two_lane_plan();
                    plan.lanes[1].table = plan.lanes[0].table;
                    plan
                },
                ValidationError::DuplicateTable(TABLE_BASE),
            ),
            (
                {
                    let mut plan = fixture_two_lane_plan();
                    plan.lanes[1].rule_priority = plan.lanes[0].rule_priority;
                    plan
                },
                ValidationError::DuplicatePriority(RULE_PRIORITY_BASE),
            ),
            (
                {
                    let mut plan = fixture_valid_plan();
                    plan.lanes[0].interface.clear();
                    plan
                },
                ValidationError::InvalidInterface(String::new()),
            ),
            (
                {
                    let mut plan = fixture_valid_plan();
                    plan.lanes[0].interface = "interface-name16".to_owned();
                    plan
                },
                ValidationError::InvalidInterface("interface-name16".to_owned()),
            ),
            (
                {
                    let mut plan = fixture_valid_plan();
                    plan.lanes[0].interface = "wg/escape".to_owned();
                    plan
                },
                ValidationError::InvalidInterface("wg/escape".to_owned()),
            ),
            (
                {
                    let mut plan = fixture_valid_plan();
                    plan.rules[0].action = CompiledAction::Mark(MARK_BASE + 50);
                    plan
                },
                ValidationError::UnknownMark(MARK_BASE + 50),
            ),
        ];

        for (plan, expected) in cases {
            assert_eq!(validate_privileged_plan(&plan), Err(expected));
        }
    }

    #[test]
    fn rejects_lane_and_rule_counts_above_hard_limits() {
        let mut too_many_lanes = fixture_valid_plan();
        too_many_lanes.lanes = (0..101)
            .map(|offset| fixture_lane(&format!("lane-{offset}"), offset))
            .collect();
        assert_eq!(
            validate_privileged_plan(&too_many_lanes),
            Err(ValidationError::TooManyLanes {
                actual: 101,
                maximum: 100,
            })
        );

        let mut too_many_rules = fixture_valid_plan();
        too_many_rules.rules = (0..10_001)
            .map(|offset| CompiledRule {
                id: RuleId::from(format!("rule-{offset}").as_str()),
                priority: offset,
                target: CompiledTarget::Any,
                action: CompiledAction::Direct,
            })
            .collect();
        assert_eq!(
            validate_privileged_plan(&too_many_rules),
            Err(ValidationError::TooManyRules {
                actual: 10_001,
                maximum: 10_000,
            })
        );
    }

    #[test]
    fn command_mapping_is_closed_to_ip_and_nft_without_a_shell() {
        let ip = CommandSpec::ip4(["rule", "show"]);
        let invocation = command_invocation(&ip).unwrap();
        assert_eq!(invocation.program, IP_BINARY);
        assert_eq!(invocation.args, vec!["-4", "rule", "show"]);
        assert_eq!(invocation.stdin, None);

        let nft = CommandSpec {
            program: Program::Nft,
            family: None,
            args: vec!["-f".to_owned(), "-".to_owned()],
            stdin: Some(b"add table inet routelane\n".to_vec()),
        };
        let invocation = command_invocation(&nft).unwrap();
        assert_eq!(invocation.program, NFT_BINARY);
        assert_eq!(invocation.args, vec!["-f", "-"]);
        assert_eq!(
            invocation.stdin,
            Some(b"add table inet routelane\n".as_slice())
        );
    }

    #[test]
    fn command_mapping_rejects_invalid_program_shapes() {
        let ip_with_stdin = CommandSpec {
            program: Program::Ip,
            family: Some(IpFamily::V4),
            args: vec!["rule".to_owned(), "show".to_owned()],
            stdin: Some(vec![]),
        };
        assert!(command_invocation(&ip_with_stdin).is_err());

        let nft_with_family = CommandSpec {
            program: Program::Nft,
            family: Some(IpFamily::V6),
            args: vec!["list".to_owned(), "tables".to_owned()],
            stdin: None,
        };
        assert!(command_invocation(&nft_with_family).is_err());
    }

    #[test]
    fn persisted_state_load_requires_exact_0600_mode() {
        assert!(validate_root_owned_file_attributes(true, 0, 0o600).is_ok());

        for mode in [0o400, 0o200, 0o640, 0o660, 0o700] {
            assert!(
                validate_root_owned_file_attributes(true, 0, mode).is_err(),
                "mode {mode:o} must not be accepted for persisted state"
            );
        }
    }

    #[test]
    fn process_commands_are_killed_after_a_bounded_timeout() {
        let invocation = CommandInvocation {
            program: "/bin/sleep",
            args: vec!["5"],
            stdin: None,
        };
        let started = Instant::now();

        let error =
            execute_invocation_with_timeout(&invocation, Duration::from_millis(50)).unwrap_err();

        assert!(error.to_string().contains("timed out"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timed out command was not terminated promptly"
        );
    }

    #[test]
    fn command_timeout_terminates_descendants_holding_output_pipes() {
        let invocation = CommandInvocation {
            program: "/bin/sh",
            args: vec!["-c", "sleep 5"],
            stdin: None,
        };
        let started = Instant::now();

        let error =
            execute_invocation_with_timeout(&invocation, Duration::from_millis(50)).unwrap_err();

        assert!(error.to_string().contains("timed out"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a descendant kept command output pipes open after timeout"
        );
    }

    #[test]
    fn command_stdout_above_the_limit_terminates_the_process_group() {
        let invocation = CommandInvocation {
            program: "/bin/sh",
            args: vec!["-c", "head -c 2097152 /dev/zero"],
            stdin: None,
        };
        let started = Instant::now();

        let error =
            execute_invocation_with_timeout(&invocation, Duration::from_secs(5)).unwrap_err();

        assert!(error.to_string().contains("stdout exceeded"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "an overflowing stdout pipe did not terminate the command promptly"
        );
    }

    #[test]
    fn command_stderr_above_the_limit_terminates_the_process_group() {
        let invocation = CommandInvocation {
            program: "/bin/sh",
            args: vec!["-c", "head -c 2097152 /dev/zero >&2"],
            stdin: None,
        };
        let started = Instant::now();

        let error =
            execute_invocation_with_timeout(&invocation, Duration::from_secs(5)).unwrap_err();

        assert!(error.to_string().contains("stderr exceeded"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "an overflowing stderr pipe did not terminate the command promptly"
        );
    }

    #[test]
    fn output_overflow_terminates_descendants_holding_the_pipe_without_hanging() {
        let invocation = CommandInvocation {
            program: "/bin/sh",
            args: vec!["-c", "(head -c 2097152 /dev/zero; sleep 5) & wait"],
            stdin: None,
        };
        let started = Instant::now();

        let error =
            execute_invocation_with_timeout(&invocation, Duration::from_secs(5)).unwrap_err();

        assert!(error.to_string().contains("stdout exceeded"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a descendant kept the overflowing output pipe open"
        );
    }

    #[test]
    fn polkit_challenge_and_denial_are_never_treated_as_authorized() {
        assert_eq!(authorization_result(true, false), Ok(()));
        assert_eq!(
            authorization_result(false, false),
            Err(AuthorizationError::Denied)
        );
        assert_eq!(
            authorization_result(false, true),
            Err(AuthorizationError::Challenge)
        );
        assert_eq!(
            authorization_result(true, true),
            Err(AuthorizationError::Challenge)
        );
    }

    #[derive(Default)]
    struct FakeKernelState {
        commands: Vec<CommandSpec>,
        has_route: bool,
        gatewayless_route: bool,
        has_rule: bool,
        has_table: bool,
        nft_objects: Option<Vec<serde_json::Value>>,
        marker: Option<String>,
        transaction_marker: Option<String>,
        has_guard: bool,
        output_tampered: bool,
        extra_nft_rule: bool,
        fail_activation: bool,
        fail_rollback: bool,
        fail_route_flush: bool,
        tamper_json_result: bool,
        foreign_reserved_route: bool,
    }

    #[derive(Clone)]
    struct FakeCommandExecutor {
        state: Arc<Mutex<FakeKernelState>>,
    }

    impl CommandExecutor for FakeCommandExecutor {
        fn execute(
            &mut self,
            spec: &CommandSpec,
        ) -> Result<CommandOutput, crate::backend::RunnerError> {
            let mut state = self.state.lock().unwrap();
            state.commands.push(spec.clone());

            match spec.program {
                Program::Ip => match spec
                    .args
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice()
                {
                    [json, numeric, "route", "show", "table", "all"]
                        if *json == "-j" && *numeric == "-N" =>
                    {
                        let mut body = Vec::new();
                        if state.has_route && spec.family == Some(IpFamily::V4) {
                            let mut route = serde_json::json!({
                                "dst": "default",
                                "dev": "wg0",
                                "table": 20_000,
                                "flags": [],
                            });
                            if state.gatewayless_route {
                                route["scope"] = serde_json::json!("link");
                            } else {
                                route["gateway"] = serde_json::json!("10.0.0.1");
                            }
                            body.push(route);
                        }
                        if state.foreign_reserved_route && spec.family == Some(IpFamily::V4) {
                            body.push(serde_json::json!({
                                "dst": "default",
                                "gateway": "10.0.1.1",
                                "dev": "foreign0",
                                "table": 20_001,
                                "flags": [],
                            }));
                        }
                        Ok(CommandOutput::success(serde_json::to_vec(&body).unwrap()))
                    }
                    [json, numeric, "rule", "show"] if *json == "-j" && *numeric == "-N" => {
                        let body = if state.has_rule && spec.family == Some(IpFamily::V4) {
                            serde_json::json!([{
                                "priority": 10_000,
                                "src": "all",
                                "fwmark": "0x524c0001",
                                "table": 20_000,
                            }])
                        } else {
                            serde_json::json!([])
                        };
                        Ok(CommandOutput::success(serde_json::to_vec(&body).unwrap()))
                    }
                    ["route", "replace", ..] => {
                        state.has_route = true;
                        state.gatewayless_route =
                            !spec.args.iter().any(|argument| argument == "via");
                        Ok(CommandOutput::success(vec![]))
                    }
                    ["rule", "add", ..] => {
                        state.has_rule = true;
                        Ok(CommandOutput::success(vec![]))
                    }
                    ["rule", "del", ..] => {
                        state.has_rule = false;
                        Ok(CommandOutput::success(vec![]))
                    }
                    ["route", "flush", ..] => {
                        if state.fail_route_flush {
                            return Ok(CommandOutput::failure(
                                "injected route-table flush failure",
                            ));
                        }
                        state.has_route = false;
                        state.gatewayless_route = false;
                        Ok(CommandOutput::success(vec![]))
                    }
                    _ => Err(crate::backend::RunnerError::new(format!(
                        "unexpected ip command: {:?}",
                        spec.args
                    ))),
                },
                Program::Nft if spec.args == ["-j", "list", "tables"] => {
                    let body = if state.has_table {
                        serde_json::json!({
                            "nftables": [{
                                "table": {"family": "inet", "name": "routelane"}
                            }]
                        })
                    } else {
                        serde_json::json!({"nftables": []})
                    };
                    Ok(CommandOutput::success(serde_json::to_vec(&body).unwrap()))
                }
                Program::Nft if spec.args == ["-j", "list", "table", "inet", "routelane"] => {
                    if let Some(objects) = &state.nft_objects {
                        let mut nftables = vec![serde_json::json!({
                            "metainfo": {
                                "version": "fake",
                                "release_name": "fake",
                                "json_schema_version": 1,
                            }
                        })];
                        nftables.extend(objects.clone());
                        if state.output_tampered {
                            if let Some(rule) = nftables.iter_mut().find_map(|object| {
                                object.get_mut("rule").filter(|rule| {
                                    rule.get("chain").and_then(serde_json::Value::as_str)
                                        == Some("output")
                                })
                            }) {
                                rule["expr"] = serde_json::json!([{"comment": "tampered-output"}]);
                            }
                        }
                        if state.extra_nft_rule {
                            nftables.push(serde_json::json!({
                                "rule": {
                                    "family": "inet",
                                    "table": "routelane",
                                    "chain": "output",
                                    "expr": [{"comment": "unexpected-extra-rule"}],
                                }
                            }));
                        }
                        return Ok(CommandOutput::success(
                            serde_json::to_vec(&serde_json::json!({
                                "nftables": nftables
                            }))
                            .unwrap(),
                        ));
                    }
                    let mut nftables = Vec::new();
                    if state.has_table {
                        nftables.push(serde_json::json!({
                            "table": {"family": "inet", "name": "routelane", "handle": 9}
                        }));
                    }
                    if let Some(marker) = &state.marker {
                        nftables.push(serde_json::json!({
                            "chain": {
                                "family": "inet",
                                "table": "routelane",
                                "name": marker,
                                "handle": 10,
                            }
                        }));
                        nftables.push(serde_json::json!({
                            "chain": {
                                "family": "inet",
                                "table": "routelane",
                                "name": "output",
                                "type": "route",
                                "hook": "output",
                                "prio": -150,
                                "policy": "accept",
                                "handle": 11,
                            }
                        }));
                        nftables.push(serde_json::json!({
                            "rule": {
                                "family": "inet",
                                "table": "routelane",
                                "chain": "output",
                                "expr": [{
                                    "comment": if state.output_tampered {
                                        "tampered-output"
                                    } else {
                                        "generated-output"
                                    },
                                    "counter": {"packets": 12, "bytes": 34}
                                }],
                                "handle": 12,
                            }
                        }));
                    }
                    if let Some(marker) = &state.transaction_marker {
                        nftables.push(serde_json::json!({
                            "chain": {
                                "family": "inet",
                                "table": "routelane",
                                "name": marker,
                                "handle": 14,
                            }
                        }));
                    }
                    if state.extra_nft_rule {
                        nftables.push(serde_json::json!({
                            "rule": {
                                "family": "inet",
                                "table": "routelane",
                                "chain": "output",
                                "expr": [{"comment": "unexpected-extra-rule"}],
                                "handle": 15,
                            }
                        }));
                    }
                    if state.has_guard {
                        nftables.push(serde_json::json!({
                            "chain": {
                                "family": "inet",
                                "table": "routelane",
                                "name": "guard",
                                "handle": 13,
                            }
                        }));
                    }
                    Ok(CommandOutput::success(
                        serde_json::to_vec(&serde_json::json!({
                            "nftables": nftables
                        }))
                        .unwrap(),
                    ))
                }
                Program::Nft if spec.args == ["-j", "-f", "-"] => {
                    let document: serde_json::Value =
                        serde_json::from_slice(spec.stdin.as_deref().unwrap()).unwrap();
                    let commands = document
                        .get("nftables")
                        .and_then(serde_json::Value::as_array)
                        .unwrap();
                    let activates_output = commands.iter().any(|command| {
                        command
                            .get("add")
                            .and_then(|object| object.get("chain"))
                            .and_then(|chain| chain.get("name"))
                            .and_then(serde_json::Value::as_str)
                            == Some("output")
                    });
                    if state.fail_activation && activates_output {
                        return Ok(CommandOutput::failure("injected nft activation failure"));
                    }
                    let mut objects = Vec::new();
                    for command in commands {
                        if command.get("delete").is_some() {
                            objects.clear();
                        }
                        if let Some(object) = command.get("add") {
                            objects.push(object.clone());
                        }
                    }
                    state.has_table = objects.iter().any(|object| object.get("table").is_some());
                    state.marker = objects.iter().find_map(|object| {
                        object
                            .get("chain")
                            .and_then(|chain| chain.get("name"))
                            .and_then(serde_json::Value::as_str)
                            .filter(|name| name.starts_with("state_"))
                            .map(str::to_owned)
                    });
                    state.transaction_marker = objects.iter().find_map(|object| {
                        object
                            .get("chain")
                            .and_then(|chain| chain.get("name"))
                            .and_then(serde_json::Value::as_str)
                            .filter(|name| name.starts_with("transaction_"))
                            .map(str::to_owned)
                    });
                    state.has_guard = objects.iter().any(|object| {
                        object
                            .get("chain")
                            .and_then(|chain| chain.get("name"))
                            .and_then(serde_json::Value::as_str)
                            == Some("guard")
                    });
                    state.output_tampered = false;
                    state.extra_nft_rule = state.tamper_json_result;
                    state.nft_objects = Some(objects);
                    Ok(CommandOutput::success(vec![]))
                }
                Program::Nft if spec.args == ["-f", "-"] => {
                    let batch = std::str::from_utf8(spec.stdin.as_deref().unwrap()).unwrap();
                    if state.fail_activation && batch.contains("chain inet routelane output") {
                        return Ok(CommandOutput::failure("injected nft activation failure"));
                    }
                    if state.fail_rollback && batch.contains("delete table inet routelane") {
                        return Ok(CommandOutput::failure("injected nft rollback failure"));
                    }
                    if batch.contains("delete chain inet routelane guard")
                        && !batch.contains("flush chain inet routelane guard")
                    {
                        return Ok(CommandOutput::failure(
                            "cannot delete a non-empty guard chain",
                        ));
                    }
                    if batch.contains("delete table inet routelane")
                        && !batch.contains("add table inet routelane")
                    {
                        state.has_table = false;
                        state.nft_objects = None;
                        state.marker = None;
                        state.transaction_marker = None;
                        state.has_guard = false;
                    }
                    if batch.contains("add table inet routelane") {
                        state.has_table = true;
                        state.has_guard = batch.contains("add chain inet routelane guard");
                        state.output_tampered = false;
                        state.extra_nft_rule = false;
                        state.marker = batch.lines().find_map(|line| {
                            line.strip_prefix("add chain inet routelane state_")
                                .map(|suffix| format!("state_{}", suffix.trim()))
                        });
                        state.transaction_marker = None;
                    }
                    if let Some(marker) = batch.lines().find_map(|line| {
                        line.strip_prefix("add chain inet routelane transaction_")
                            .map(|suffix| format!("transaction_{}", suffix.trim()))
                    }) {
                        state.transaction_marker = Some(marker);
                    }
                    if batch.contains("add chain inet routelane guard") {
                        state.has_guard = true;
                    }
                    if batch.contains("delete chain inet routelane guard") {
                        state.has_guard = false;
                        if let Some(objects) = &mut state.nft_objects {
                            objects.retain(|object| {
                                object
                                    .get("chain")
                                    .and_then(|chain| chain.get("name"))
                                    .and_then(serde_json::Value::as_str)
                                    != Some("guard")
                                    && object
                                        .get("rule")
                                        .and_then(|rule| rule.get("chain"))
                                        .and_then(serde_json::Value::as_str)
                                        != Some("guard")
                            });
                        }
                    }
                    Ok(CommandOutput::success(vec![]))
                }
                _ => Err(crate::backend::RunnerError::new(format!(
                    "unexpected command: {spec:?}"
                ))),
            }
        }
    }

    #[test]
    fn command_backend_applies_the_full_transaction_before_success() {
        let state = Arc::new(Mutex::new(FakeKernelState::default()));
        let executor = FakeCommandExecutor {
            state: Arc::clone(&state),
        };
        let mut backend = CommandPrivilegedBackend::new(executor);
        let plan = fixture_valid_plan();

        let response = backend.apply(&plan).unwrap();
        let inspection = backend.inspect().unwrap();
        let state = state.lock().unwrap();

        assert_eq!(response, ApplyResponse::from_plan(&plan, vec![]));
        assert_eq!(inspection.generation, Some(1));
        assert!(inspection.routelane_table_present);
        assert!(state.has_route);
        assert!(state.has_rule);
        assert!(state.has_table);
        assert!(state.commands.iter().any(|command| {
            command.program == Program::Nft
                && command
                    .stdin
                    .as_deref()
                    .and_then(|batch| serde_json::from_slice::<serde_json::Value>(batch).ok())
                    .and_then(|document| document.get("nftables").cloned())
                    .and_then(|commands| commands.as_array().cloned())
                    .is_some_and(|commands| {
                        commands.iter().any(|command| {
                            command
                                .get("add")
                                .and_then(|object| object.get("chain"))
                                .and_then(|chain| chain.get("name"))
                                .and_then(serde_json::Value::as_str)
                                == Some("output")
                        })
                    })
        }));
    }

    #[test]
    fn command_backend_applies_and_inspects_exact_gatewayless_route() {
        let state = Arc::new(Mutex::new(FakeKernelState::default()));
        let executor = FakeCommandExecutor {
            state: Arc::clone(&state),
        };
        let mut backend = CommandPrivilegedBackend::new(executor);
        let mut plan = fixture_valid_plan();
        plan.lanes[0].route_v4 = Some(RouteMode::Device);

        let response = backend.apply(&plan).unwrap();
        let inspection = backend.inspect().unwrap();
        let state = state.lock().unwrap();

        assert_eq!(response, ApplyResponse::from_plan(&plan, vec![]));
        assert_eq!(inspection.applied_plan, Some(plan));
        assert!(state.gatewayless_route);
        assert!(state.commands.iter().any(|command| {
            command.program == Program::Ip
                && command.family == Some(IpFamily::V4)
                && command.args
                    == [
                        "route", "replace", "default", "dev", "wg0", "table", "20000",
                    ]
        }));
    }

    #[test]
    fn restarted_backend_recovers_exact_gatewayless_journal_ownership() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let mut plan = fixture_valid_plan();
        plan.lanes[0].route_v4 = Some(RouteMode::Device);
        {
            let mut backend = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            backend.apply(&plan).unwrap();
        }

        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store,
        );
        let inspected = restarted.inspect().unwrap();

        assert_eq!(inspected.generation, Some(1));
        assert_eq!(inspected.applied_plan, Some(plan));
        assert!(kernel.lock().unwrap().gatewayless_route);
    }

    #[test]
    fn restore_reinstates_exact_previous_journal_and_accepts_a_different_next_generation() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let first = fixture_valid_plan();
        let mut failed_candidate = first.clone();
        failed_candidate.generation = 2;
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        backend.apply(&first).unwrap();
        backend.apply(&failed_candidate).unwrap();

        let restored = backend.restore(&failed_candidate, Some(&first)).unwrap();

        assert_eq!(restored, RestoreResponse::from_plan(Some(&first)));
        assert_eq!(backend.inspect().unwrap().generation, Some(1));
        drop(backend);
        let journal_after_restore = store.state.lock().unwrap().clone();
        let mutations_after_restore = mutation_count(&kernel.lock().unwrap());
        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );

        let retried = restarted.restore(&failed_candidate, Some(&first)).unwrap();

        assert_eq!(retried, RestoreResponse::from_plan(Some(&first)));
        assert_eq!(*store.state.lock().unwrap(), journal_after_restore);
        assert_eq!(
            mutation_count(&kernel.lock().unwrap()),
            mutations_after_restore
        );
        {
            let persisted = store.state.lock().unwrap();
            let persisted = persisted.as_ref().unwrap();
            assert_eq!(persisted.active.as_ref(), Some(&first));
            assert!(persisted.pending.is_none());
            assert!(persisted.pending_expectations.is_none());
        }

        let mut different_next = first.clone();
        different_next.generation = 2;
        different_next.rules[0].action = CompiledAction::Block;
        assert_eq!(
            restarted.apply(&different_next).unwrap().generation,
            different_next.generation
        );
        assert!(
            store
                .state
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .restore_receipt
                .is_none(),
            "a new apply must invalidate the old restore receipt"
        );
        let journal_after_new_apply = store.state.lock().unwrap().clone();
        let mutations_after_new_apply = mutation_count(&kernel.lock().unwrap());
        let error = restarted
            .restore(&failed_candidate, Some(&first))
            .unwrap_err();
        assert!(error.to_string().contains("not the failed candidate"));
        assert_eq!(*store.state.lock().unwrap(), journal_after_new_apply);
        assert_eq!(
            mutation_count(&kernel.lock().unwrap()),
            mutations_after_new_apply
        );
    }

    #[test]
    fn restore_to_empty_retry_uses_exact_receipt_without_kernel_or_journal_mutation() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let failed_candidate = fixture_valid_plan();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        backend.apply(&failed_candidate).unwrap();
        assert_eq!(
            backend.restore(&failed_candidate, None).unwrap(),
            RestoreResponse::from_plan(None)
        );
        drop(backend);
        let journal_after_restore = store.state.lock().unwrap().clone();
        let mutations_after_restore = mutation_count(&kernel.lock().unwrap());
        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );

        let retried = restarted.restore(&failed_candidate, None).unwrap();

        assert_eq!(retried, RestoreResponse::from_plan(None));
        assert_eq!(*store.state.lock().unwrap(), journal_after_restore);
        assert_eq!(
            mutation_count(&kernel.lock().unwrap()),
            mutations_after_restore
        );
    }

    #[test]
    fn cleanup_after_restore_invalidates_the_receipt() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let previous = fixture_valid_plan();
        let mut failed_candidate = previous.clone();
        failed_candidate.generation = 2;
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        backend.apply(&previous).unwrap();
        backend.apply(&failed_candidate).unwrap();
        backend.restore(&failed_candidate, Some(&previous)).unwrap();

        assert!(backend.cleanup().unwrap().cleaned);
        assert!(store.state.lock().unwrap().is_none());
        let mutations_after_cleanup = mutation_count(&kernel.lock().unwrap());

        let error = backend
            .restore(&failed_candidate, Some(&previous))
            .unwrap_err();

        assert!(error.to_string().contains("no candidate"));
        assert!(store.state.lock().unwrap().is_none());
        assert_eq!(
            mutation_count(&kernel.lock().unwrap()),
            mutations_after_cleanup
        );
    }

    #[test]
    fn idempotent_apply_after_restore_invalidates_the_receipt() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let previous = fixture_valid_plan();
        let mut failed_candidate = previous.clone();
        failed_candidate.generation = 2;
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        backend.apply(&previous).unwrap();
        backend.apply(&failed_candidate).unwrap();
        backend.restore(&failed_candidate, Some(&previous)).unwrap();
        let mutations_after_restore = mutation_count(&kernel.lock().unwrap());

        backend.apply(&previous).unwrap();

        assert!(store
            .state
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .restore_receipt
            .is_none());
        assert_eq!(
            mutation_count(&kernel.lock().unwrap()),
            mutations_after_restore
        );
        let journal_after_apply = store.state.lock().unwrap().clone();
        let error = backend
            .restore(&failed_candidate, Some(&previous))
            .unwrap_err();
        assert!(error.to_string().contains("not the failed candidate"));
        assert_eq!(*store.state.lock().unwrap(), journal_after_apply);
        assert_eq!(
            mutation_count(&kernel.lock().unwrap()),
            mutations_after_restore
        );
    }

    #[test]
    fn crashed_pending_restore_recovers_candidate_and_clears_uncommitted_receipt() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let previous = fixture_valid_plan();
        let mut failed_candidate = previous.clone();
        failed_candidate.generation = 2;
        {
            let mut backend = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            backend.apply(&previous).unwrap();
            backend.apply(&failed_candidate).unwrap();
        }
        {
            let mut persisted = store.state.lock().unwrap();
            let active_nft_fingerprint = persisted.as_ref().unwrap().active_nft_fingerprint.clone();
            *persisted = Some(StoredAppliedState {
                active: Some(failed_candidate.clone()),
                active_nft_fingerprint,
                pending: Some(previous.clone()),
                pending_expectations: Some(
                    build_pending_expectations(Some(&failed_candidate), &previous).unwrap(),
                ),
                cleanup_pending: false,
                restore_receipt: Some(RestoreReceipt::new(&failed_candidate, Some(&previous))),
            });
        }
        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );

        let inspection = restarted.inspect().unwrap();

        assert_eq!(inspection.generation, Some(failed_candidate.generation));
        {
            let persisted = store.state.lock().unwrap();
            let record = persisted.as_ref().unwrap();
            assert_eq!(record.active.as_ref(), Some(&failed_candidate));
            assert!(record.pending.is_none());
            assert!(record.restore_receipt.is_none());
        }
        assert_eq!(
            restarted
                .restore(&failed_candidate, Some(&previous))
                .unwrap(),
            RestoreResponse::from_plan(Some(&previous))
        );
    }

    #[test]
    fn crashed_cleanup_restore_commits_empty_receipt_for_a_safe_retry() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let failed_candidate = fixture_valid_plan();
        {
            let mut backend = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            backend.apply(&failed_candidate).unwrap();
        }
        {
            let mut persisted = store.state.lock().unwrap();
            let active_nft_fingerprint = persisted.as_ref().unwrap().active_nft_fingerprint.clone();
            *persisted = Some(StoredAppliedState {
                active: Some(failed_candidate.clone()),
                active_nft_fingerprint,
                pending: None,
                pending_expectations: None,
                cleanup_pending: true,
                restore_receipt: Some(RestoreReceipt::new(&failed_candidate, None)),
            });
        }
        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );

        assert_eq!(restarted.inspect().unwrap().generation, None);
        let journal_after_recovery = store.state.lock().unwrap().clone();
        let mutations_after_recovery = mutation_count(&kernel.lock().unwrap());
        assert_eq!(
            restarted.restore(&failed_candidate, None).unwrap(),
            RestoreResponse::from_plan(None)
        );
        assert_eq!(*store.state.lock().unwrap(), journal_after_recovery);
        assert_eq!(
            mutation_count(&kernel.lock().unwrap()),
            mutations_after_recovery
        );
    }

    #[test]
    fn mismatched_pending_restore_receipt_is_rejected_without_mutation() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let previous = fixture_valid_plan();
        let mut failed_candidate = previous.clone();
        failed_candidate.generation = 2;
        {
            let mut backend = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            backend.apply(&previous).unwrap();
            backend.apply(&failed_candidate).unwrap();
        }
        {
            let mut persisted = store.state.lock().unwrap();
            let active_nft_fingerprint = persisted.as_ref().unwrap().active_nft_fingerprint.clone();
            let mut different_candidate = failed_candidate.clone();
            different_candidate.rules[0].action = CompiledAction::Block;
            *persisted = Some(StoredAppliedState {
                active: Some(failed_candidate.clone()),
                active_nft_fingerprint,
                pending: Some(previous.clone()),
                pending_expectations: Some(
                    build_pending_expectations(Some(&failed_candidate), &previous).unwrap(),
                ),
                cleanup_pending: false,
                restore_receipt: Some(RestoreReceipt::new(&different_candidate, Some(&previous))),
            });
        }
        let journal_before = store.state.lock().unwrap().clone();
        let mutations_before = mutation_count(&kernel.lock().unwrap());
        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );

        let error = restarted.inspect().unwrap_err();

        assert!(error.to_string().contains("restore receipt"));
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        assert_eq!(mutation_count(&kernel.lock().unwrap()), mutations_before);
    }

    #[test]
    fn restore_rejects_a_noncurrent_candidate_before_journal_or_kernel_mutation() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let current = fixture_valid_plan();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        backend.apply(&current).unwrap();
        let journal_before = store.state.lock().unwrap().clone();
        let mutations_before = mutation_count(&kernel.lock().unwrap());
        let mut other = current.clone();
        other.generation = 2;

        let error = backend.restore(&other, Some(&current)).unwrap_err();

        assert!(error.to_string().contains("not the failed candidate"));
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        assert_eq!(mutation_count(&kernel.lock().unwrap()), mutations_before);
    }

    #[test]
    fn command_backend_reports_activation_failure_after_real_cleanup() {
        let state = Arc::new(Mutex::new(FakeKernelState {
            fail_activation: true,
            ..FakeKernelState::default()
        }));
        let executor = FakeCommandExecutor {
            state: Arc::clone(&state),
        };
        let mut backend = CommandPrivilegedBackend::new(executor);

        let error = backend.apply(&fixture_valid_plan()).unwrap_err();
        let state = state.lock().unwrap();

        assert!(error.to_string().contains("activate nftables"));
        assert!(!state.has_route);
        assert!(!state.has_rule);
        assert!(!state.has_table);
    }

    #[test]
    fn failed_rollback_retains_the_exact_pending_journal() {
        let kernel = Arc::new(Mutex::new(FakeKernelState {
            fail_activation: true,
            fail_rollback: true,
            ..FakeKernelState::default()
        }));
        let store = FakeAppliedStateStore::default();
        let plan = fixture_valid_plan();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );

        let error = backend.apply(&plan).unwrap_err();

        assert!(error.to_string().contains("rollback"));
        let persisted = store.state.lock().unwrap();
        let persisted = persisted.as_ref().expect("pending journal must remain");
        assert!(persisted.active.is_none());
        assert_eq!(persisted.pending.as_ref(), Some(&plan));
        assert!(persisted.pending_expectations.is_some());
    }

    #[test]
    fn restarted_backend_recovers_exact_guardless_transaction_after_failed_rollback() {
        let kernel = Arc::new(Mutex::new(FakeKernelState {
            fail_activation: true,
            fail_rollback: true,
            ..FakeKernelState::default()
        }));
        let store = FakeAppliedStateStore::default();
        let plan = fixture_valid_plan();
        {
            let mut backend = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            backend.apply(&plan).unwrap_err();
        }
        {
            let mut kernel = kernel.lock().unwrap();
            assert!(kernel.has_table);
            assert!(!kernel.has_guard);
            assert!(kernel.transaction_marker.is_some());
            kernel.fail_activation = false;
            kernel.fail_rollback = false;
        }

        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        let inspection = restarted.inspect().unwrap();

        assert_eq!(inspection.generation, None);
        assert!(!inspection.routelane_table_present);
        assert!(store.state.lock().unwrap().is_none());
        let kernel = kernel.lock().unwrap();
        assert!(!kernel.has_table);
        assert!(kernel.transaction_marker.is_none());
    }

    #[test]
    fn same_process_cleanup_reconciles_exact_guardless_transaction_before_clearing_journal() {
        let kernel = Arc::new(Mutex::new(FakeKernelState {
            fail_activation: true,
            fail_rollback: true,
            ..FakeKernelState::default()
        }));
        let store = FakeAppliedStateStore::default();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        backend.apply(&fixture_valid_plan()).unwrap_err();
        {
            let mut kernel = kernel.lock().unwrap();
            kernel.fail_activation = false;
            kernel.fail_rollback = false;
        }

        let response = backend.cleanup().unwrap();

        assert!(!response.cleaned);
        assert!(store.state.lock().unwrap().is_none());
        let kernel = kernel.lock().unwrap();
        assert!(!kernel.has_table);
        assert!(kernel.transaction_marker.is_none());
    }

    #[test]
    fn same_process_cleanup_rejects_inexact_failed_rollback_without_clearing_journal() {
        let kernel = Arc::new(Mutex::new(FakeKernelState {
            fail_activation: true,
            fail_rollback: true,
            ..FakeKernelState::default()
        }));
        let store = FakeAppliedStateStore::default();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        backend.apply(&fixture_valid_plan()).unwrap_err();
        {
            let mut kernel = kernel.lock().unwrap();
            kernel.fail_activation = false;
            kernel.fail_rollback = false;
            kernel.extra_nft_rule = true;
        }
        let journal_before = store.state.lock().unwrap().clone();
        let mutations_before = mutation_count(&kernel.lock().unwrap());

        let error = backend.cleanup().unwrap_err();

        assert!(error.to_string().contains("pending"));
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        let kernel = kernel.lock().unwrap();
        assert_eq!(mutation_count(&kernel), mutations_before);
        assert!(kernel.has_table);
        assert!(kernel.extra_nft_rule);
    }

    #[test]
    fn same_process_apply_checks_inexact_pending_journal_before_preflight() {
        let kernel = Arc::new(Mutex::new(FakeKernelState {
            fail_activation: true,
            fail_rollback: true,
            ..FakeKernelState::default()
        }));
        let store = FakeAppliedStateStore::default();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        let mut next = fixture_valid_plan();
        backend.apply(&next).unwrap_err();
        {
            let mut kernel = kernel.lock().unwrap();
            kernel.fail_activation = false;
            kernel.fail_rollback = false;
            kernel.extra_nft_rule = true;
        }
        let journal_before = store.state.lock().unwrap().clone();
        let mutations_before = mutation_count(&kernel.lock().unwrap());
        next.generation = 2;

        let error = backend.apply(&next).unwrap_err();

        assert!(error.to_string().contains("pending"));
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        let kernel = kernel.lock().unwrap();
        assert_eq!(mutation_count(&kernel), mutations_before);
        assert!(kernel.has_table);
        assert!(kernel.extra_nft_rule);
    }

    #[test]
    fn same_process_inspect_rejects_inexact_pending_journal_without_mutation() {
        let kernel = Arc::new(Mutex::new(FakeKernelState {
            fail_activation: true,
            fail_rollback: true,
            ..FakeKernelState::default()
        }));
        let store = FakeAppliedStateStore::default();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        backend.apply(&fixture_valid_plan()).unwrap_err();
        {
            let mut kernel = kernel.lock().unwrap();
            kernel.fail_activation = false;
            kernel.fail_rollback = false;
            kernel.extra_nft_rule = true;
        }
        let journal_before = store.state.lock().unwrap().clone();
        let mutations_before = mutation_count(&kernel.lock().unwrap());

        let error = backend.inspect().unwrap_err();

        assert!(error.to_string().contains("pending"));
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        assert_eq!(mutation_count(&kernel.lock().unwrap()), mutations_before);
    }

    #[test]
    fn higher_generation_reuses_identical_policy_rule_without_deleting_it() {
        let state = Arc::new(Mutex::new(FakeKernelState::default()));
        let executor = FakeCommandExecutor {
            state: Arc::clone(&state),
        };
        let mut backend = CommandPrivilegedBackend::new(executor);
        let first = fixture_valid_plan();
        backend.apply(&first).unwrap();
        let mut second = first.clone();
        second.generation = 2;

        backend.apply(&second).unwrap();
        let state = state.lock().unwrap();

        assert!(state.has_rule);
        assert_eq!(
            state
                .commands
                .iter()
                .filter(|command| command
                    .args
                    .starts_with(&["rule".to_owned(), "add".to_owned()]))
                .count(),
            1
        );
    }

    #[test]
    fn first_apply_rejects_exact_unowned_route_and_rule_without_mutation() {
        let kernel = Arc::new(Mutex::new(FakeKernelState {
            has_route: true,
            has_rule: true,
            fail_activation: true,
            ..FakeKernelState::default()
        }));
        let store = FakeAppliedStateStore::default();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );

        let error = backend.apply(&fixture_valid_plan()).unwrap_err();

        assert!(error
            .to_string()
            .contains("without persisted RouteLane ownership"));
        assert!(store.state.lock().unwrap().is_none());
        let kernel = kernel.lock().unwrap();
        assert!(kernel.has_route);
        assert!(kernel.has_rule);
        assert!(!kernel.has_table);
        assert!(kernel.commands.iter().all(|command| command.stdin.is_none()
            && command.args.first().is_some_and(|arg| arg == "-j")));
    }

    #[test]
    fn first_apply_rejects_any_foreign_reserved_routing_resource_before_journaling() {
        let kernel = Arc::new(Mutex::new(FakeKernelState {
            foreign_reserved_route: true,
            ..FakeKernelState::default()
        }));
        let store = FakeAppliedStateStore::default();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );

        let error = backend.apply(&fixture_valid_plan()).unwrap_err();

        assert!(error.to_string().contains("routing resource conflict"));
        assert!(store.state.lock().unwrap().is_none());
        let kernel = kernel.lock().unwrap();
        assert_eq!(mutation_count(&kernel), 0);
        assert!(kernel.foreign_reserved_route);
    }

    #[test]
    fn pending_expectations_include_exact_rollback_command_prefixes() {
        let pending = fixture_two_lane_plan();
        let mut rollback_prefix = super::plan_routing_ownership(&pending);
        rollback_prefix
            .rules
            .remove(&super::lane_rule_ownership(&pending.lanes[0], IpFamily::V4));

        assert!(super::routing_state_reachable_for_phase(
            &rollback_prefix,
            None,
            &pending,
            super::PendingNftPhase::Guard,
        )
        .unwrap());
    }

    #[test]
    fn committed_nft_phase_accepts_exact_activated_guard_cleanup_prefix() {
        let active = fixture_valid_plan();
        let mut pending = fixture_valid_plan();
        pending.generation = 2;
        pending.lanes = vec![fixture_lane("work", 1)];
        let live = super::RoutingOwnership {
            routes: [
                super::RouteOwnership {
                    family: 4,
                    table: 20_000,
                    gateway: Some("10.0.0.1".to_owned()),
                    interface: "wg0".to_owned(),
                },
                super::RouteOwnership {
                    family: 4,
                    table: 20_001,
                    gateway: Some("10.0.1.1".to_owned()),
                    interface: "wg1".to_owned(),
                },
            ]
            .into_iter()
            .collect(),
            rules: [
                super::RuleOwnership {
                    family: 4,
                    priority: 10_000,
                    table: 20_000,
                    mark: 0x524c0001,
                },
                super::RuleOwnership {
                    family: 4,
                    priority: 10_001,
                    table: 20_001,
                    mark: 0x524c0002,
                },
            ]
            .into_iter()
            .collect(),
        };

        assert!(super::routing_state_reachable_for_phase(
            &live,
            Some(&active),
            &pending,
            super::PendingNftPhase::Committed,
        )
        .unwrap());
    }

    #[test]
    fn max_dual_stack_plan_uses_a_compact_pending_journal_model() {
        let mut pending = fixture_valid_plan();
        pending.lanes = (0..100)
            .map(|offset| {
                let mut lane = fixture_lane(&format!("lane-{offset}"), offset);
                lane.route_v6 = Some(RouteMode::Via(
                    format!("2001:db8::{:x}", offset + 1).parse().unwrap(),
                ));
                lane
            })
            .collect();
        validate_privileged_plan(&pending).unwrap();
        let expectations = build_pending_expectations(None, &pending).unwrap();
        assert!(serde_json::to_vec(&expectations).unwrap().len() < 1024);

        let journal = StoredAppliedState {
            active: None,
            active_nft_fingerprint: None,
            pending: Some(pending.clone()),
            pending_expectations: Some(expectations),
            cleanup_pending: false,
            restore_receipt: None,
        };
        assert!(
            serde_json::to_vec(&journal).unwrap().len() < super::MAX_STATE_BYTES as usize,
            "a valid maximum-lane pending journal must fit the bounded state file"
        );

        let mut rollback_prefix = super::plan_routing_ownership(&pending);
        rollback_prefix
            .rules
            .remove(&super::lane_rule_ownership(&pending.lanes[0], IpFamily::V4));
        assert!(super::routing_state_reachable_for_phase(
            &rollback_prefix,
            None,
            &pending,
            super::PendingNftPhase::Guard,
        )
        .unwrap());
    }

    #[derive(Clone, Default)]
    struct FakeAppliedStateStore {
        state: Arc<Mutex<Option<StoredAppliedState>>>,
    }

    impl AppliedStateStore for FakeAppliedStateStore {
        fn load(&mut self) -> Result<Option<StoredAppliedState>, PrivilegedError> {
            Ok(self.state.lock().unwrap().clone())
        }

        fn save(&mut self, state: &StoredAppliedState) -> Result<(), PrivilegedError> {
            *self.state.lock().unwrap() = Some(state.clone());
            Ok(())
        }

        fn clear(&mut self) -> Result<(), PrivilegedError> {
            *self.state.lock().unwrap() = None;
            Ok(())
        }
    }

    fn mutation_count(state: &FakeKernelState) -> usize {
        state
            .commands
            .iter()
            .filter(|command| {
                command.stdin.is_some() || command.args.first().is_some_and(|arg| arg != "-j")
            })
            .count()
    }

    #[test]
    fn same_generation_apply_revalidates_live_state_before_journal_or_mutation() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let plan = fixture_valid_plan();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        backend.apply(&plan).unwrap();
        let journal_before = store.state.lock().unwrap().clone();
        {
            let mut kernel = kernel.lock().unwrap();
            kernel.output_tampered = true;
        }
        let mutations_before = mutation_count(&kernel.lock().unwrap());

        let error = backend.apply(&plan).unwrap_err();

        assert!(error
            .to_string()
            .contains("differs from the exact committed state"));
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        let kernel = kernel.lock().unwrap();
        assert_eq!(mutation_count(&kernel), mutations_before);
        assert!(kernel.output_tampered);
    }

    #[test]
    fn higher_generation_apply_revalidates_live_state_before_journal_or_mutation() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let first = fixture_valid_plan();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        backend.apply(&first).unwrap();
        let journal_before = store.state.lock().unwrap().clone();
        {
            let mut kernel = kernel.lock().unwrap();
            kernel.output_tampered = true;
        }
        let mutations_before = mutation_count(&kernel.lock().unwrap());
        let mut second = first;
        second.generation = 2;

        let error = backend.apply(&second).unwrap_err();

        assert!(error
            .to_string()
            .contains("differs from the exact committed state"));
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        let kernel = kernel.lock().unwrap();
        assert_eq!(mutation_count(&kernel), mutations_before);
        assert!(kernel.output_tampered);
    }

    #[test]
    fn restarted_backend_reconciles_persisted_owned_generation() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let first = fixture_valid_plan();
        {
            let mut backend = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            backend.apply(&first).unwrap();
        }

        let mut second = first.clone();
        second.generation = 2;
        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        let response = restarted.apply(&second).unwrap();

        assert_eq!(response.generation, 2);
        let persisted = store.state.lock().unwrap();
        let persisted = persisted.as_ref().unwrap();
        assert_eq!(persisted.active.as_ref().unwrap().generation, 2);
        assert!(persisted.pending.is_none());
    }

    #[test]
    fn restarted_backend_recovers_active_to_pending_crash_after_guard() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let active = fixture_valid_plan();
        {
            let mut backend = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            backend.apply(&active).unwrap();
        }
        let mut pending = active.clone();
        pending.generation = 2;
        {
            let mut persisted = store.state.lock().unwrap();
            let active_fingerprint = persisted.as_ref().unwrap().active_nft_fingerprint.clone();
            *persisted = Some(StoredAppliedState {
                active: Some(active.clone()),
                active_nft_fingerprint: active_fingerprint,
                pending: Some(pending.clone()),
                pending_expectations: Some(
                    build_pending_expectations(Some(&active), &pending).unwrap(),
                ),
                cleanup_pending: false,
                restore_receipt: None,
            });
        }
        let mut crash_executor = FakeCommandExecutor {
            state: Arc::clone(&kernel),
        };
        run_nft_batch(
            &mut crash_executor,
            render_transaction_ruleset(Some(&active), &pending, TableMode::Replace).unwrap(),
            "simulate crash after transaction guard",
        )
        .unwrap();
        assert!(kernel.lock().unwrap().transaction_marker.is_some());

        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        let inspection = restarted.inspect().unwrap();

        assert_eq!(inspection.generation, Some(active.generation));
        let kernel = kernel.lock().unwrap();
        assert!(!kernel.has_guard);
        assert!(kernel.transaction_marker.is_none());
        drop(kernel);
        let persisted = store.state.lock().unwrap();
        let persisted = persisted.as_ref().unwrap();
        assert_eq!(persisted.active.as_ref(), Some(&active));
        assert!(persisted.pending.is_none());
        assert!(persisted.active_nft_fingerprint.is_some());
    }

    #[test]
    fn pending_recovery_rejects_an_inexact_rollback_and_retains_the_journal() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let active = fixture_valid_plan();
        {
            let mut backend = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            backend.apply(&active).unwrap();
        }
        let mut pending = active.clone();
        pending.generation = 2;
        {
            let mut persisted = store.state.lock().unwrap();
            let active_fingerprint = persisted.as_ref().unwrap().active_nft_fingerprint.clone();
            *persisted = Some(StoredAppliedState {
                active: Some(active.clone()),
                active_nft_fingerprint: active_fingerprint,
                pending: Some(pending.clone()),
                pending_expectations: Some(
                    build_pending_expectations(Some(&active), &pending).unwrap(),
                ),
                cleanup_pending: false,
                restore_receipt: None,
            });
        }
        let mut crash_executor = FakeCommandExecutor {
            state: Arc::clone(&kernel),
        };
        run_nft_batch(
            &mut crash_executor,
            render_transaction_ruleset(Some(&active), &pending, TableMode::Replace).unwrap(),
            "simulate crash after transaction guard",
        )
        .unwrap();
        kernel.lock().unwrap().tamper_json_result = true;
        let journal_before = store.state.lock().unwrap().clone();

        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        let error = restarted.inspect().unwrap_err();

        assert!(error.to_string().contains("exact rollback target"));
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        assert!(kernel.lock().unwrap().extra_nft_rule);
    }

    #[test]
    fn restarted_backend_recovers_each_exact_pending_nft_crash_window() {
        #[derive(Clone, Copy)]
        enum CrashWindow {
            Untouched,
            Activated,
            Committed,
        }

        for window in [
            CrashWindow::Untouched,
            CrashWindow::Activated,
            CrashWindow::Committed,
        ] {
            let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
            let store = FakeAppliedStateStore::default();
            let active = fixture_valid_plan();
            {
                let mut backend = CommandPrivilegedBackend::with_store(
                    FakeCommandExecutor {
                        state: Arc::clone(&kernel),
                    },
                    store.clone(),
                );
                backend.apply(&active).unwrap();
            }
            let mut pending = active.clone();
            pending.generation = 2;
            {
                let mut persisted = store.state.lock().unwrap();
                let active_fingerprint = persisted.as_ref().unwrap().active_nft_fingerprint.clone();
                *persisted = Some(StoredAppliedState {
                    active: Some(active.clone()),
                    active_nft_fingerprint: active_fingerprint,
                    pending: Some(pending.clone()),
                    pending_expectations: Some(
                        build_pending_expectations(Some(&active), &pending).unwrap(),
                    ),
                    cleanup_pending: false,
                    restore_receipt: None,
                });
            }
            let mut crash_executor = FakeCommandExecutor {
                state: Arc::clone(&kernel),
            };
            let batch = match window {
                CrashWindow::Untouched => None,
                CrashWindow::Activated => Some(render_guarded_ruleset(&pending).unwrap()),
                CrashWindow::Committed => {
                    Some(render_owned_ruleset(&pending, TableMode::Replace, false).unwrap())
                }
            };
            if let Some(batch) = batch {
                run_nft_batch(
                    &mut crash_executor,
                    batch,
                    "simulate exact pending crash window",
                )
                .unwrap();
            }

            let mut restarted = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            assert_eq!(restarted.inspect().unwrap().generation, Some(1));
            let persisted = store.state.lock().unwrap();
            let persisted = persisted.as_ref().unwrap();
            assert_eq!(persisted.active.as_ref(), Some(&active));
            assert!(persisted.pending.is_none());
            assert!(persisted.pending_expectations.is_none());
        }
    }

    #[test]
    fn pending_activation_with_impossible_routing_state_is_rejected_without_cleanup_mutation() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let active = fixture_valid_plan();
        {
            let mut backend = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            backend.apply(&active).unwrap();
        }
        let mut pending = active.clone();
        pending.generation = 2;
        {
            let mut persisted = store.state.lock().unwrap();
            let active_fingerprint = persisted.as_ref().unwrap().active_nft_fingerprint.clone();
            *persisted = Some(StoredAppliedState {
                active: Some(active.clone()),
                active_nft_fingerprint: active_fingerprint,
                pending: Some(pending.clone()),
                pending_expectations: Some(
                    build_pending_expectations(Some(&active), &pending).unwrap(),
                ),
                cleanup_pending: false,
                restore_receipt: None,
            });
        }
        let mut crash_executor = FakeCommandExecutor {
            state: Arc::clone(&kernel),
        };
        run_nft_batch(
            &mut crash_executor,
            render_guarded_ruleset(&pending).unwrap(),
            "simulate crash after pending nft activation",
        )
        .unwrap();
        kernel.lock().unwrap().has_route = false;
        let journal_before = store.state.lock().unwrap().clone();
        let mutations_before = mutation_count(&kernel.lock().unwrap());

        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        let error = restarted.inspect().unwrap_err();

        assert!(error.to_string().contains("route or policy-rule"));
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        assert_eq!(mutation_count(&kernel.lock().unwrap()), mutations_before);
    }

    #[test]
    fn pending_guard_marker_with_extra_rule_is_rejected_without_cleanup_mutation() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let active = fixture_valid_plan();
        {
            let mut backend = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            backend.apply(&active).unwrap();
        }
        let mut pending = active.clone();
        pending.generation = 2;
        {
            let mut persisted = store.state.lock().unwrap();
            let active_fingerprint = persisted.as_ref().unwrap().active_nft_fingerprint.clone();
            *persisted = Some(StoredAppliedState {
                active: Some(active.clone()),
                active_nft_fingerprint: active_fingerprint,
                pending: Some(pending.clone()),
                pending_expectations: Some(
                    build_pending_expectations(Some(&active), &pending).unwrap(),
                ),
                cleanup_pending: false,
                restore_receipt: None,
            });
        }
        let mut crash_executor = FakeCommandExecutor {
            state: Arc::clone(&kernel),
        };
        run_nft_batch(
            &mut crash_executor,
            render_transaction_ruleset(Some(&active), &pending, TableMode::Replace).unwrap(),
            "simulate crash after transaction guard",
        )
        .unwrap();
        kernel.lock().unwrap().extra_nft_rule = true;
        let journal_before = store.state.lock().unwrap().clone();
        let mutations_before = mutation_count(&kernel.lock().unwrap());

        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        let error = restarted.inspect().unwrap_err();

        assert!(error.to_string().contains("pending"));
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        let kernel = kernel.lock().unwrap();
        assert_eq!(mutation_count(&kernel), mutations_before);
        assert!(kernel.extra_nft_rule);
        assert!(kernel.transaction_marker.is_some());
    }

    #[test]
    fn pending_activation_marker_with_changed_rule_is_rejected_without_cleanup_mutation() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let active = fixture_valid_plan();
        {
            let mut backend = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            backend.apply(&active).unwrap();
        }
        let mut pending = active.clone();
        pending.generation = 2;
        {
            let mut persisted = store.state.lock().unwrap();
            let active_fingerprint = persisted.as_ref().unwrap().active_nft_fingerprint.clone();
            *persisted = Some(StoredAppliedState {
                active: Some(active.clone()),
                active_nft_fingerprint: active_fingerprint,
                pending: Some(pending.clone()),
                pending_expectations: Some(
                    build_pending_expectations(Some(&active), &pending).unwrap(),
                ),
                cleanup_pending: false,
                restore_receipt: None,
            });
        }
        let mut crash_executor = FakeCommandExecutor {
            state: Arc::clone(&kernel),
        };
        run_nft_batch(
            &mut crash_executor,
            render_guarded_ruleset(&pending).unwrap(),
            "simulate crash after pending nft activation",
        )
        .unwrap();
        kernel.lock().unwrap().output_tampered = true;
        let journal_before = store.state.lock().unwrap().clone();
        let mutations_before = mutation_count(&kernel.lock().unwrap());

        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        let error = restarted.inspect().unwrap_err();

        assert!(error.to_string().contains("pending"));
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        let kernel = kernel.lock().unwrap();
        assert_eq!(mutation_count(&kernel), mutations_before);
        assert!(kernel.output_tampered);
        assert!(kernel.marker.is_some());
    }

    #[test]
    fn read_only_inspect_returns_tampered_nft_evidence_but_next_apply_stays_fail_closed() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        {
            let mut backend = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            backend.apply(&fixture_valid_plan()).unwrap();
        }
        {
            let mut state = kernel.lock().unwrap();
            assert!(state.marker.is_some());
            state.output_tampered = true;
        }
        let mutation_count_before = kernel
            .lock()
            .unwrap()
            .commands
            .iter()
            .filter(|command| {
                command.stdin.is_some() || command.args.first().is_some_and(|arg| arg != "-j")
            })
            .count();

        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store,
        );
        let inspection = restarted.inspect().unwrap();

        assert_eq!(inspection.generation, Some(1));
        assert!(!inspection.snapshot.nft_is_owned_by(&fixture_valid_plan()));
        let mut next = fixture_valid_plan();
        next.generation = 2;
        let error = restarted.apply(&next).unwrap_err();
        assert!(error
            .to_string()
            .contains("differs from the exact committed state"));
        let state = kernel.lock().unwrap();
        let mutation_count_after = state
            .commands
            .iter()
            .filter(|command| {
                command.stdin.is_some() || command.args.first().is_some_and(|arg| arg != "-j")
            })
            .count();
        assert_eq!(mutation_count_after, mutation_count_before);
        assert!(state.output_tampered);
    }

    #[test]
    fn read_only_inspect_returns_missing_table_evidence_without_repair() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        {
            let mut backend = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            backend.apply(&fixture_valid_plan()).unwrap();
        }
        {
            let mut state = kernel.lock().unwrap();
            state.has_table = false;
            state.nft_objects = None;
            state.marker = None;
        }
        let journal_before = store.state.lock().unwrap().clone();
        let mutations_before = mutation_count(&kernel.lock().unwrap());

        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        let inspection = restarted.inspect().unwrap();

        assert_eq!(inspection.generation, Some(1));
        assert!(!inspection.routelane_table_present);
        assert!(!inspection.snapshot.nft_is_owned_by(&fixture_valid_plan()));
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        assert_eq!(mutation_count(&kernel.lock().unwrap()), mutations_before);
    }

    #[test]
    fn read_only_inspect_returns_missing_rule_evidence_but_mutation_revalidates_exactly() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let plan = fixture_valid_plan();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        backend.apply(&plan).unwrap();
        kernel.lock().unwrap().has_rule = false;
        let journal_before = store.state.lock().unwrap().clone();
        let mutations_before = mutation_count(&kernel.lock().unwrap());

        let inspection = backend.inspect().unwrap();

        assert_eq!(
            inspection
                .snapshot
                .rule_state(IpFamily::V4, RULE_PRIORITY_BASE, TABLE_BASE, MARK_BASE,),
            ResourceState::Missing
        );
        let mut next = plan;
        next.generation = 2;
        assert!(backend.apply(&next).is_err());
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        assert_eq!(mutation_count(&kernel.lock().unwrap()), mutations_before);
    }

    #[test]
    fn first_apply_rejects_foreign_routelane_table_without_mutation_or_journal() {
        let kernel = Arc::new(Mutex::new(FakeKernelState {
            has_table: true,
            ..FakeKernelState::default()
        }));
        let store = FakeAppliedStateStore::default();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );

        let error = backend.apply(&fixture_valid_plan()).unwrap_err();

        assert!(error.to_string().contains("unowned inet/routelane table"));
        assert!(store.state.lock().unwrap().is_none());
        let state = kernel.lock().unwrap();
        assert!(state.has_table);
        assert!(state.commands.iter().all(|command| command.stdin.is_none()
            && command.args.first().is_some_and(|arg| arg == "-j")));
    }

    #[test]
    fn invalid_persisted_plan_is_rejected_before_kernel_access() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let mut invalid = fixture_valid_plan();
        invalid.lanes[0].mark = 1;
        let store = FakeAppliedStateStore::default();
        *store.state.lock().unwrap() = Some(StoredAppliedState {
            active: Some(invalid),
            active_nft_fingerprint: None,
            pending: None,
            pending_expectations: None,
            cleanup_pending: false,
            restore_receipt: None,
        });
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store,
        );

        let error = backend.apply(&fixture_valid_plan()).unwrap_err();

        assert!(error.to_string().contains("persisted"));
        assert!(kernel.lock().unwrap().commands.is_empty());
    }

    #[test]
    fn cleanup_uses_persisted_ownership_and_clears_the_store() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        backend.apply(&fixture_valid_plan()).unwrap();

        let response = backend.cleanup().unwrap();

        assert!(response.cleaned);
        assert!(store.state.lock().unwrap().is_none());
        let kernel = kernel.lock().unwrap();
        assert!(!kernel.has_route);
        assert!(!kernel.has_rule);
        assert!(!kernel.has_table);
    }

    #[test]
    fn restarted_backend_resumes_an_exact_cleanup_command_prefix() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        {
            let mut backend = CommandPrivilegedBackend::with_store(
                FakeCommandExecutor {
                    state: Arc::clone(&kernel),
                },
                store.clone(),
            );
            backend.apply(&fixture_valid_plan()).unwrap();
            kernel.lock().unwrap().fail_route_flush = true;
            let error = backend.cleanup().unwrap_err();
            assert!(error.to_string().contains("flush"));
        }
        {
            let kernel = kernel.lock().unwrap();
            assert!(!kernel.has_rule);
            assert!(kernel.has_route);
            assert!(kernel.has_table);
        }
        assert!(store
            .state
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|state| state.cleanup_pending));
        kernel.lock().unwrap().fail_route_flush = false;

        let mut restarted = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        let response = restarted.inspect().unwrap();

        assert_eq!(response.generation, None);
        assert!(store.state.lock().unwrap().is_none());
        let kernel = kernel.lock().unwrap();
        assert!(!kernel.has_route);
        assert!(!kernel.has_rule);
        assert!(!kernel.has_table);
    }

    #[derive(Clone)]
    struct FakeAuthorizer {
        result: Result<(), AuthorizationError>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Authorizer for FakeAuthorizer {
        type Future<'a> = Ready<Result<(), AuthorizationError>>;

        fn authorize<'a>(&'a self, _sender: &'a str) -> Self::Future<'a> {
            self.events.lock().unwrap().push("authorize");
            ready(self.result.clone())
        }
    }

    #[derive(Clone)]
    struct FakeBackend {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl PrivilegedBackend for FakeBackend {
        fn apply(&mut self, plan: &KernelPlan) -> Result<ApplyResponse, PrivilegedError> {
            self.events.lock().unwrap().push("apply");
            Ok(ApplyResponse::from_plan(plan, vec![]))
        }

        fn restore(
            &mut self,
            _failed_candidate: &KernelPlan,
            previous: Option<&KernelPlan>,
        ) -> Result<RestoreResponse, PrivilegedError> {
            self.events.lock().unwrap().push("restore");
            Ok(RestoreResponse::from_plan(previous))
        }

        fn inspect(&mut self) -> Result<InspectResponse, PrivilegedError> {
            self.events.lock().unwrap().push("inspect");
            Ok(InspectResponse::empty())
        }

        fn cleanup(&mut self) -> Result<CleanupResponse, PrivilegedError> {
            self.events.lock().unwrap().push("cleanup");
            Ok(CleanupResponse {
                cleaned: true,
                diagnostics: vec![],
            })
        }
    }

    struct BlockingBackend;

    impl PrivilegedBackend for BlockingBackend {
        fn apply(&mut self, plan: &KernelPlan) -> Result<ApplyResponse, PrivilegedError> {
            std::thread::sleep(Duration::from_millis(250));
            Ok(ApplyResponse::from_plan(plan, vec![]))
        }

        fn restore(
            &mut self,
            _failed_candidate: &KernelPlan,
            previous: Option<&KernelPlan>,
        ) -> Result<RestoreResponse, PrivilegedError> {
            Ok(RestoreResponse::from_plan(previous))
        }

        fn inspect(&mut self) -> Result<InspectResponse, PrivilegedError> {
            Ok(InspectResponse::empty())
        }

        fn cleanup(&mut self) -> Result<CleanupResponse, PrivilegedError> {
            Ok(CleanupResponse {
                cleaned: true,
                diagnostics: vec![],
            })
        }
    }

    fn fixture_service(
        authorization: Result<(), AuthorizationError>,
    ) -> (
        PrivilegedService<FakeAuthorizer, FakeBackend>,
        Arc<Mutex<Vec<&'static str>>>,
    ) {
        let events = Arc::new(Mutex::new(Vec::new()));
        (
            PrivilegedService::new(
                FakeAuthorizer {
                    result: authorization,
                    events: Arc::clone(&events),
                },
                FakeBackend {
                    events: Arc::clone(&events),
                },
            ),
            events,
        )
    }

    #[tokio::test]
    async fn denial_precedes_deserialization_validation_and_mutation() {
        let (service, events) = fixture_service(Err(AuthorizationError::Denied));

        let error = service
            .apply_plan(Some(":1.42"), "{not valid json")
            .await
            .unwrap_err();

        assert_eq!(
            error,
            ServiceError::Authorization(AuthorizationError::Denied)
        );
        assert_eq!(*events.lock().unwrap(), vec!["authorize"]);
    }

    #[tokio::test]
    async fn authorized_invalid_or_oversized_plan_never_reaches_backend() {
        let (service, events) = fixture_service(Ok(()));
        let mut invalid = fixture_valid_plan();
        invalid.lanes[0].mark = 1;

        let error = service
            .apply_plan(Some(":1.42"), &serde_json::to_string(&invalid).unwrap())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            ServiceError::Validation(ValidationError::MarkOutOfRange(1))
        );
        assert_eq!(*events.lock().unwrap(), vec!["authorize"]);

        events.lock().unwrap().clear();
        let oversized = "x".repeat(MAX_PLAN_JSON_BYTES + 1);
        let error = service
            .apply_plan(Some(":1.42"), &oversized)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            ServiceError::PlanTooLarge {
                actual: MAX_PLAN_JSON_BYTES + 1,
                maximum: MAX_PLAN_JSON_BYTES,
            }
        );
        assert_eq!(*events.lock().unwrap(), vec!["authorize"]);
    }

    #[tokio::test]
    async fn missing_sender_is_rejected_before_authorization_or_mutation() {
        let (service, events) = fixture_service(Ok(()));

        assert_eq!(
            service
                .apply_plan(None, &serde_json::to_string(&fixture_valid_plan()).unwrap())
                .await,
            Err(ServiceError::MissingSender)
        );
        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn authorized_apply_returns_structured_response_after_backend_success() {
        let (service, events) = fixture_service(Ok(()));
        let plan = fixture_valid_plan();

        let response = service
            .apply_plan(Some(":1.42"), &serde_json::to_string(&plan).unwrap())
            .await
            .unwrap();
        let json = serde_json::to_value(response).unwrap();

        assert_eq!(json["generation"], 1);
        assert_eq!(
            json["plan_hash"],
            serde_json::to_value(plan.content_hash()).unwrap()
        );
        assert_eq!(json["diagnostics"], serde_json::json!([]));
        assert_eq!(*events.lock().unwrap(), vec!["authorize", "apply"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn synchronous_backend_work_does_not_block_the_async_runtime() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let service = PrivilegedService::new(
            FakeAuthorizer {
                result: Ok(()),
                events,
            },
            BlockingBackend,
        );
        let plan = serde_json::to_string(&fixture_valid_plan()).unwrap();
        let started = Instant::now();

        let (response, timer_elapsed) =
            tokio::join!(service.apply_plan(Some(":1.42"), &plan), async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                started.elapsed()
            });

        response.unwrap();
        assert!(
            timer_elapsed < Duration::from_millis(100),
            "async runtime was blocked for {timer_elapsed:?}"
        );
    }

    #[tokio::test]
    async fn inspect_is_read_only_while_cleanup_and_restore_require_authorization() {
        let (denied, denied_events) = fixture_service(Err(AuthorizationError::Challenge));
        let failed_candidate = {
            let mut plan = fixture_valid_plan();
            plan.generation = 2;
            serde_json::to_string(&plan).unwrap()
        };
        let previous = serde_json::to_string(&fixture_valid_plan()).unwrap();
        denied.inspect(None).await.unwrap();
        assert_eq!(
            denied.cleanup(Some(":1.42")).await,
            Err(ServiceError::Authorization(AuthorizationError::Challenge))
        );
        assert_eq!(
            denied
                .restore_plan(Some(":1.42"), &failed_candidate, Some(&previous))
                .await,
            Err(ServiceError::Authorization(AuthorizationError::Challenge))
        );
        assert_eq!(
            *denied_events.lock().unwrap(),
            vec!["inspect", "authorize", "authorize"]
        );

        let (allowed, allowed_events) = fixture_service(Ok(()));
        allowed.inspect(None).await.unwrap();
        allowed.cleanup(Some(":1.42")).await.unwrap();
        assert_eq!(
            allowed
                .restore_plan(Some(":1.42"), &failed_candidate, Some(&previous))
                .await
                .unwrap(),
            RestoreResponse::from_plan(Some(&fixture_valid_plan()))
        );
        assert_eq!(
            *allowed_events.lock().unwrap(),
            vec!["inspect", "authorize", "cleanup", "authorize", "restore"]
        );
    }
}
