use std::collections::{BTreeMap, BTreeSet, HashMap};
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

use super::nft_document::{
    has_valid_policy_seal, normalize_nft_document, state_chain_name, POLICY_SEAL_PREFIX,
};
use crate::domain::LaneId;

use super::interface_settings::{InterfaceSettingReceipt, MarkAwareRpf, MarkAwareRpfBackend};
use super::legacy_journal::{legacy_nft_fingerprint, LegacyJournalV1};
use super::transaction::restore_plan as restore_kernel_plan;
use super::{
    apply_plan as apply_kernel_plan, build_snapshot_commands, detect_resource_conflicts,
    detect_resource_conflicts_for_update, install_steps, retire_steps, transition_steps,
    AppliedKernelState, CommandSpec, CompiledAction, CompiledTarget, IpFamily, KernelPlan,
    KernelRunner, KernelSnapshot, OwnedRoute, PlanHash, Program, RoutingStep, RunnerError,
    SnapshotInput, TableMode, MARK_BASE, MARK_MAX, RULE_PRIORITY_BASE, RULE_PRIORITY_MAX,
    TABLE_BASE, TABLE_MAX, TERMINAL_ROUTE_METRIC,
};

pub const MAX_PLAN_JSON_BYTES: usize = 1024 * 1024;
/// Two bounded plans plus fingerprints, at most one setting receipt per lane
/// (committed and pending) and the envelope.
const MAX_STATE_BYTES: u64 = (MAX_PLAN_JSON_BYTES as u64 * 2) + 128 * 1024;
/// Explicit journal schema. Unversioned journals are schema 1 and cleanup-only.
const JOURNAL_VERSION: u32 = 2;
const IP_BINARY: &str = "/usr/sbin/ip";
const NFT_BINARY: &str = "/usr/sbin/nft";
const STATE_DIRECTORY: &str = "/run/routelane";
const STATE_FILE: &str = "/run/routelane/applied-plan.json";
const O_NOFOLLOW: i32 = 0o400000;
const MAX_LANES: usize = 100;
const MAX_RULES: usize = 10_000;
const ROUTING_CRASH_MODEL_VERSION: &str = "exact-command-prefix-v4-terminal";
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

pub(crate) fn run_topology_query(
    query: super::topology::TopologyQuery,
) -> Result<Vec<u8>, RunnerError> {
    let output = execute_invocation_with_timeout(
        &CommandInvocation {
            program: IP_BINARY,
            args: query.arguments().to_vec(),
            stdin: None,
        },
        COMMAND_TIMEOUT,
    )?;
    if !output.success {
        return Err(RunnerError::new(format!(
            "read kernel topology: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output.stdout)
}

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
    journal_version: u32,
    pub active: Option<KernelPlan>,
    pub active_nft_fingerprint: Option<String>,
    pub pending: Option<KernelPlan>,
    #[serde(default)]
    pub pending_expectations: Option<PendingExpectations>,
    #[serde(default)]
    pub cleanup_pending: bool,
    #[serde(default)]
    restore_receipt: Option<RestoreReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    orphan_cleanup: Option<OrphanedOwnership>,
    /// Interface settings the committed generation changed, restored at cleanup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    interface_settings: Vec<InterfaceSettingReceipt>,
    /// Setting intents journaled by an in-flight transition before each write.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_interface_settings: Vec<InterfaceSettingReceipt>,
    /// An unversioned schema-1 journal awaiting cleanup; never armed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_v1: Option<LegacyJournalV1>,
}

impl Default for StoredAppliedState {
    fn default() -> Self {
        Self {
            journal_version: JOURNAL_VERSION,
            active: None,
            active_nft_fingerprint: None,
            pending: None,
            pending_expectations: None,
            cleanup_pending: false,
            restore_receipt: None,
            orphan_cleanup: None,
            interface_settings: vec![],
            pending_interface_settings: vec![],
            legacy_v1: None,
        }
    }
}

/// Decodes a persisted journal. A versioned journal must be exactly this
/// schema; an unversioned one is accepted only as a strict schema-1 record,
/// wrapped for cleanup and never converted into a current plan.
fn decode_journal(json: &[u8]) -> Result<StoredAppliedState, PrivilegedError> {
    let value: serde_json::Value = serde_json::from_slice(json)
        .map_err(|error| state_error(format!("decode persisted applied state: {error}")))?;
    let Some(version) = value.get("journal_version").cloned() else {
        let legacy: LegacyJournalV1 = serde_json::from_value(value).map_err(|error| {
            state_error(format!(
                "decode unversioned schema-1 applied state: {error}"
            ))
        })?;
        return Ok(StoredAppliedState {
            legacy_v1: Some(legacy),
            ..StoredAppliedState::default()
        });
    };
    if version.as_u64() != Some(u64::from(JOURNAL_VERSION)) {
        return Err(state_error(format!(
            "unsupported applied-state journal version {version}"
        )));
    }
    serde_json::from_value(value)
        .map_err(|error| state_error(format!("decode persisted applied state: {error}")))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrphanedOwnership {
    nft_fingerprint: Option<String>,
    routing: RoutingOwnership,
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
            restore_receipt,
            ..Self::default()
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
        decode_journal(&json).map(Some)
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
    settings: Box<dyn MarkAwareRpfBackend>,
    current: Option<AppliedKernelState>,
    current_nft_fingerprint: Option<String>,
    interface_settings: Vec<InterfaceSettingReceipt>,
    restore_receipt: Option<RestoreReceipt>,
    legacy: Option<LegacyJournalV1>,
    initialized: bool,
}

/// Used until a backend is given the fixed-purpose system adapter; any lane
/// that would need a mark-aware setting fails closed instead of skipping it.
struct UnconfiguredMarkAwareRpf;

impl MarkAwareRpfBackend for UnconfiguredMarkAwareRpf {
    fn identity(&mut self, _: &str) -> Result<super::InterfaceIdentity, PrivilegedError> {
        Err(PrivilegedError::new(
            "mark-aware interface settings are not configured for this backend",
        ))
    }
    fn rp_filter(&mut self, _: &str) -> Result<u8, PrivilegedError> {
        Err(PrivilegedError::new(
            "mark-aware interface settings are not configured for this backend",
        ))
    }
    fn src_valid_mark(&mut self, _: &str) -> Result<u8, PrivilegedError> {
        Err(PrivilegedError::new(
            "mark-aware interface settings are not configured for this backend",
        ))
    }
    fn set_src_valid_mark(
        &mut self,
        _: &super::InterfaceIdentity,
        _: u8,
    ) -> Result<(), PrivilegedError> {
        Err(PrivilegedError::new(
            "mark-aware interface settings are not configured for this backend",
        ))
    }
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
            settings: Box::new(UnconfiguredMarkAwareRpf),
            current: None,
            current_nft_fingerprint: None,
            interface_settings: vec![],
            restore_receipt: None,
            legacy: None,
            initialized: false,
        }
    }

    /// Supplies the fixed-purpose adapter for per-interface `src_valid_mark`.
    pub fn with_interface_settings(mut self, settings: impl MarkAwareRpfBackend + 'static) -> Self {
        self.settings = Box::new(settings);
        self
    }
}

pub type SystemPrivilegedBackend =
    CommandPrivilegedBackend<ProcessCommandExecutor, FileAppliedStateStore>;

impl Default for SystemPrivilegedBackend {
    fn default() -> Self {
        Self::with_store(ProcessCommandExecutor, FileAppliedStateStore::default())
            .with_interface_settings(super::SystemMarkAwareRpfBackend)
    }
}

impl<E, S> PrivilegedBackend for CommandPrivilegedBackend<E, S>
where
    E: CommandExecutor,
    S: AppliedStateStore,
{
    fn apply(&mut self, plan: &KernelPlan) -> Result<ApplyResponse, PrivilegedError> {
        self.ensure_reconciled()?;
        self.refuse_legacy_activation()?;
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
                        &self.interface_settings,
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
        self.refuse_legacy_activation()?;
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
        let diagnostics = if self.legacy.is_some() {
            vec![
                "a schema-1 RouteLane journal is cleanup-only; run cleanup before activating"
                    .to_owned(),
            ]
        } else {
            vec![]
        };
        Ok(InspectResponse {
            generation: self.current.as_ref().map(|state| state.generation),
            plan_hash: self.current.as_ref().map(|state| state.plan_hash),
            applied_plan: self.current.as_ref().map(|state| state.plan().clone()),
            routelane_table_present: snapshot.has_nft_table("inet", "routelane"),
            snapshot,
            diagnostics,
        })
    }

    fn cleanup(&mut self) -> Result<CleanupResponse, PrivilegedError> {
        self.cleanup_with_receipt(None)
    }

    fn recover_orphaned(&mut self) -> Result<OrphanRecoveryResponse, PrivilegedError> {
        self.ensure_reconciled()?;
        if self.current.is_some() || self.legacy.is_some() {
            return Err(PrivilegedError::new(
                "validated RouteLane state must use normal cleanup",
            ));
        }
        let ownership = capture_verified_orphaned_ownership(&mut self.executor)?;
        if ownership.nft_fingerprint.is_none() && ownership.routing == RoutingOwnership::default() {
            return Ok(OrphanRecoveryResponse {
                cleaned: false,
                diagnostics: vec![],
            });
        }
        self.store.save(&StoredAppliedState {
            orphan_cleanup: Some(ownership),
            ..StoredAppliedState::default()
        })?;
        resume_orphan_cleanup(&mut self.executor, &mut self.store)?;
        Ok(OrphanRecoveryResponse {
            cleaned: true,
            diagnostics: vec![],
        })
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
        if self.legacy.is_some() {
            return self.cleanup_legacy();
        }
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
        )?;

        self.store.save(&StoredAppliedState {
            active: Some(current.plan().clone()),
            active_nft_fingerprint: self.current_nft_fingerprint.clone(),
            cleanup_pending: true,
            restore_receipt: restore_receipt.clone(),
            interface_settings: self.interface_settings.clone(),
            ..StoredAppliedState::default()
        })?;
        let mut runner = CommandKernelRunner::new(&mut self.executor, Some(current));
        runner
            .snapshot()
            .and_then(|_| runner.restore(None))
            .map_err(|error| PrivilegedError::new(error.to_string()))?;
        let diagnostics = restore_setting_receipts(&mut *self.settings, &self.interface_settings);
        self.interface_settings.clear();
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
            diagnostics,
        })
    }

    fn refuse_legacy_activation(&self) -> Result<(), PrivilegedError> {
        if self.legacy.is_some() {
            return Err(PrivilegedError::new(
                "a schema-1 RouteLane journal is cleanup-only; clean it up before activation",
            ));
        }
        Ok(())
    }

    /// Removes exactly what a schema-1 helper proved it owned. The record is
    /// never converted into a current plan, so it cannot arm routing.
    fn cleanup_legacy(&mut self) -> Result<CleanupResponse, PrivilegedError> {
        let mut legacy = self
            .legacy
            .clone()
            .expect("legacy cleanup requires a schema-1 record");
        if legacy.pending.is_some()
            || legacy.pending_expectations.is_some()
            || legacy.orphan_cleanup.is_some()
        {
            return Err(PrivilegedError::new(
                "an interrupted schema-1 transaction cannot be proven by this helper; leaving its resources untouched",
            ));
        }
        let Some(active) = legacy.active.clone() else {
            self.store.clear()?;
            self.legacy = None;
            return Ok(CleanupResponse {
                cleaned: false,
                diagnostics: vec!["no schema-1 RouteLane ownership to clean".to_owned()],
            });
        };
        let fingerprint = legacy.active_nft_fingerprint.clone().ok_or_else(|| {
            PrivilegedError::new("schema-1 journal is missing its nftables fingerprint")
        })?;
        let (routes, rules) = active.ownership();
        let expected = RoutingOwnership {
            routes: routes
                .into_iter()
                .map(|route| RouteOwnership {
                    family: route.family,
                    table: route.table,
                    gateway: route.gateway,
                    interface: route.interface,
                    terminal: false,
                })
                .collect(),
            rules: rules
                .into_iter()
                .map(|rule| RuleOwnership {
                    family: rule.family,
                    priority: rule.priority,
                    table: rule.table,
                    mark: rule.mark,
                })
                .collect(),
        };
        let verify = |executor: &mut E, exact: bool| -> Result<bool, PrivilegedError> {
            let captured = capture_snapshot(executor)
                .map_err(|error| PrivilegedError::new(error.to_string()))?;
            let live = live_routing_ownership(&captured)?;
            let owned = if exact {
                live == expected
            } else {
                routing_ownership_is_subset(&live, &expected)
            };
            if !owned {
                return Err(PrivilegedError::new(
                    "schema-1 journal does not own the exact live RouteLane routing state",
                ));
            }
            if !captured.snapshot.has_nft_table("inet", "routelane") {
                return if live == RoutingOwnership::default() && !exact {
                    Ok(false)
                } else {
                    Err(PrivilegedError::new(
                        "schema-1 journal is missing its committed nftables table",
                    ))
                };
            }
            let document = read_nft_table_once(executor)
                .map_err(|error| PrivilegedError::new(error.to_string()))?;
            let marker = active.state_chain_name();
            let marked = document
                .get("nftables")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|objects| {
                    objects.iter().any(|object| {
                        object
                            .get("chain")
                            .and_then(|chain| chain.get("name"))
                            .and_then(serde_json::Value::as_str)
                            == Some(marker.as_str())
                    })
                });
            if !marked || legacy_nft_fingerprint(&document) != fingerprint {
                return Err(PrivilegedError::new(
                    "schema-1 journal does not exactly own the live inet/routelane table",
                ));
            }
            Ok(true)
        };
        let table_present = verify(&mut self.executor, !legacy.cleanup_pending)?;
        legacy.cleanup_pending = true;
        self.store.save(&StoredAppliedState {
            legacy_v1: Some(legacy),
            ..StoredAppliedState::default()
        })?;
        let to_runner = |error: RunnerError| PrivilegedError::new(error.to_string());
        for rule in &expected.rules {
            let family = if rule.family == 4 {
                IpFamily::V4
            } else {
                IpFamily::V6
            };
            run_routing_step(
                &mut self.executor,
                &RoutingStep::DeleteRule {
                    family,
                    priority: rule.priority,
                    table: rule.table,
                    mark: rule.mark,
                },
                "remove schema-1 RouteLane policy rule",
            )
            .map_err(to_runner)?;
        }
        for route in &expected.routes {
            let family = if route.family == 4 {
                IpFamily::V4
            } else {
                IpFamily::V6
            };
            run_routing_step(
                &mut self.executor,
                &RoutingStep::FlushTable {
                    family,
                    table: route.table,
                },
                "flush schema-1 RouteLane routing table",
            )
            .map_err(to_runner)?;
        }
        if table_present && verify(&mut self.executor, false)? {
            run_nft_batch(
                &mut self.executor,
                b"delete table inet routelane\n".to_vec(),
                "delete schema-1 RouteLane nftables table",
            )
            .map_err(to_runner)?;
        }
        verify_exact_kernel_state(&mut self.executor, None, None)?;
        self.store.clear()?;
        self.legacy = None;
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
        if record.legacy_v1.is_some() {
            validate_legacy_envelope(&record)?;
            self.current = None;
            self.current_nft_fingerprint = None;
            self.interface_settings.clear();
            self.restore_receipt = None;
            self.legacy = record.legacy_v1;
            self.initialized = true;
            return Ok(());
        }
        if record.pending.is_some() || record.cleanup_pending {
            return self.ensure_reconciled();
        }
        validate_stored_plan(record.active.as_ref(), "active")?;
        validate_stored_fingerprint_pair(
            record.active.as_ref(),
            record.active_nft_fingerprint.as_deref(),
        )?;
        validate_restore_receipt(&record)?;
        validate_setting_receipts(&record)?;
        self.current = record.active.as_ref().map(AppliedKernelState::from_plan);
        self.current_nft_fingerprint = record.active_nft_fingerprint;
        self.interface_settings = record.interface_settings;
        self.restore_receipt = record.restore_receipt;
        self.legacy = None;
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
        let previous_settings = self.interface_settings.clone();
        preflight_plan(&mut self.executor, previous.as_ref(), plan)?;
        let pending_expectations =
            build_pending_expectations(previous.as_ref().map(AppliedKernelState::plan), plan)?;
        let mut record = StoredAppliedState {
            active: previous.as_ref().map(|state| state.plan().clone()),
            active_nft_fingerprint: previous_nft_fingerprint.clone(),
            pending: Some(plan.clone()),
            pending_expectations: Some(pending_expectations.clone()),
            restore_receipt: restore_receipt.clone(),
            interface_settings: previous_settings.clone(),
            ..StoredAppliedState::default()
        };
        self.store.save(&record)?;
        self.restore_receipt = None;

        // Kernel state is still untouched: undo any journaled setting write and
        // return the journal to the previous committed generation.
        let new_settings = match ensure_interface_settings(
            &mut *self.settings,
            &mut self.store,
            &mut record,
            plan,
        ) {
            Ok(receipts) => receipts,
            Err(error) => {
                let restored = restore_setting_receipts(
                    &mut *self.settings,
                    &record.pending_interface_settings,
                );
                let state_repair = persist_committed(
                    &mut self.store,
                    previous.as_ref(),
                    previous_nft_fingerprint.as_deref(),
                    &previous_settings,
                );
                return Err(PrivilegedError::new(format!(
                    "{error}; setting restore: {}; state repair: {}",
                    if restored.is_empty() {
                        "ok".to_owned()
                    } else {
                        restored.join("; ")
                    },
                    result_label(&state_repair),
                )));
            }
        };
        let committed_settings = previous_settings
            .iter()
            .chain(&new_settings)
            .cloned()
            .collect::<Vec<_>>();

        let settings = &mut *self.settings;
        let mut runner = CommandKernelRunner::new(&mut self.executor, previous.clone());
        let mut undo = |runner: &mut CommandKernelRunner<'_, E>,
                        store: &mut S,
                        rollback_succeeded: bool|
         -> Result<(), PrivilegedError> {
            repair_committed_journal_after_rollback(
                store,
                &mut *runner.executor,
                previous.as_ref(),
                previous_nft_fingerprint.as_deref(),
                &previous_settings,
                rollback_succeeded,
            )?;
            // Only after the journal is back on the previous generation; a
            // retained pending journal restores these during recovery instead.
            let diverged = restore_setting_receipts(settings, &new_settings);
            if diverged.is_empty() {
                Ok(())
            } else {
                Err(PrivilegedError::new(diverged.join("; ")))
            }
        };
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
                let state_repair = undo(&mut runner, &mut self.store, rollback_succeeded);
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
                let state_repair = undo(&mut runner, &mut self.store, rollback.is_ok());
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
            let state_repair = undo(&mut runner, &mut self.store, rollback.is_ok());
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
            &committed_settings,
        ) {
            let rollback = runner.restore(previous.as_ref());
            let state_repair = undo(&mut runner, &mut self.store, rollback.is_ok());
            return Err(PrivilegedError::new(format!(
                "kernel apply completed but state commit failed: {persistence}; rollback: {}; state repair: {}",
                result_label(&rollback),
                result_label(&state_repair),
            )));
        }

        self.current = Some(applied);
        self.current_nft_fingerprint = Some(applied_nft_fingerprint);
        self.interface_settings = committed_settings;
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
                if !self.initialized
                    || record.pending.is_some()
                    || record.cleanup_pending
                    || record.orphan_cleanup.is_some() =>
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
        if record.legacy_v1.is_some() {
            // Schema-1 state is only ever removed by an explicit cleanup.
            validate_legacy_envelope(&record)?;
            self.current = None;
            self.current_nft_fingerprint = None;
            self.interface_settings.clear();
            self.restore_receipt = None;
            self.legacy = record.legacy_v1;
            self.initialized = true;
            return Ok(());
        }
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
        validate_setting_receipts(&record)?;

        if record.orphan_cleanup.is_some() {
            validate_orphan_journal(&record)?;
            resume_orphan_cleanup(&mut self.executor, &mut self.store)?;
            self.current = None;
            self.current_nft_fingerprint = None;
            self.restore_receipt = None;
            self.initialized = true;
            return Ok(());
        }

        let active = record.active.as_ref().map(AppliedKernelState::from_plan);
        if record.cleanup_pending {
            verify_cleanup_table_ownership(
                &mut self.executor,
                active.as_ref(),
                record.active_nft_fingerprint.as_deref(),
            )?;
            let mut runner = CommandKernelRunner::new(&mut self.executor, active.clone());
            runner
                .snapshot()
                .and_then(|_| runner.restore(None))
                .map_err(|error| {
                    PrivilegedError::new(format!("resume persisted cleanup: {error}"))
                })?;
            restore_setting_receipts(&mut *self.settings, &record.interface_settings);
            match record.restore_receipt.clone() {
                Some(receipt) => {
                    self.store
                        .save(&StoredAppliedState::committed(None, None, Some(receipt)))?
                }
                None => self.store.clear()?,
            }
            self.current = None;
            self.current_nft_fingerprint = None;
            self.interface_settings.clear();
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
            let interrupted = record
                .pending_interface_settings
                .iter()
                .filter(|receipt| !record.interface_settings.contains(receipt))
                .cloned()
                .collect::<Vec<_>>();
            restore_setting_receipts(&mut *self.settings, &interrupted);
            persist_committed_with_receipt(
                &mut self.store,
                active.as_ref(),
                record.active_nft_fingerprint.as_deref(),
                None,
                &record.interface_settings,
            )?;
            self.current = active;
            self.current_nft_fingerprint = record.active_nft_fingerprint;
            self.interface_settings = record.interface_settings;
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
            self.interface_settings = record.interface_settings;
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
    settings: &[InterfaceSettingReceipt],
) -> Result<(), PrivilegedError> {
    persist_committed_with_receipt(store, state, nft_fingerprint, None, settings)
}

fn persist_committed_with_receipt(
    store: &mut impl AppliedStateStore,
    state: Option<&AppliedKernelState>,
    nft_fingerprint: Option<&str>,
    restore_receipt: Option<RestoreReceipt>,
    settings: &[InterfaceSettingReceipt],
) -> Result<(), PrivilegedError> {
    if state.is_none() && !settings.is_empty() {
        return Err(PrivilegedError::new(
            "interface setting receipts require an active committed plan",
        ));
    }
    match (state, nft_fingerprint) {
        (Some(state), Some(fingerprint)) => store.save(&StoredAppliedState {
            interface_settings: settings.to_vec(),
            ..StoredAppliedState::committed(
                Some(state.plan().clone()),
                Some(fingerprint.to_owned()),
                restore_receipt,
            )
        }),
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
    settings: &[InterfaceSettingReceipt],
    rollback_succeeded: bool,
) -> Result<(), PrivilegedError> {
    if !rollback_succeeded {
        return Err(PrivilegedError::new(
            "rollback did not complete; exact pending state is still required for recovery",
        ));
    }
    verify_exact_kernel_state(executor, state, nft_fingerprint)?;
    persist_committed(store, state, nft_fingerprint, settings)
}

/// Lanes whose IPv4 replies need mark-aware reverse-path filtering.
fn mark_aware_identities(plan: &KernelPlan) -> Vec<super::InterfaceIdentity> {
    let mut identities = Vec::new();
    for lane in &plan.lanes {
        if lane.availability != super::LaneAvailability::Available || lane.route_v4.is_none() {
            continue;
        }
        if let Some(identity) = &lane.identity {
            if !identities.contains(identity) {
                identities.push(identity.clone());
            }
        }
    }
    identities
}

/// Journals each setting intent (durably) before its write. Returns receipts
/// for writes this transition made; already-owned interfaces are skipped.
fn ensure_interface_settings(
    settings: &mut dyn MarkAwareRpfBackend,
    store: &mut impl AppliedStateStore,
    record: &mut StoredAppliedState,
    plan: &KernelPlan,
) -> Result<Vec<InterfaceSettingReceipt>, PrivilegedError> {
    let mut applied = Vec::new();
    for identity in mark_aware_identities(plan) {
        if record
            .interface_settings
            .iter()
            .any(|receipt| receipt.identity == identity)
        {
            continue;
        }
        let mut coordinator =
            MarkAwareRpf::new(&mut *settings, |receipt: &InterfaceSettingReceipt| {
                record.pending_interface_settings.push(receipt.clone());
                store.save(record).map_err(|error| {
                    PrivilegedError::new(format!("interface setting journal: {error}"))
                })
            });
        if let Some(receipt) = coordinator.ensure_mark_aware_rpf(&identity)? {
            applied.push(receipt);
        }
    }
    Ok(applied)
}

/// Compare-and-restore. A changed value or replaced interface is reported and
/// left untouched; the receipt is then spent.
fn restore_setting_receipts(
    settings: &mut dyn MarkAwareRpfBackend,
    receipts: &[InterfaceSettingReceipt],
) -> Vec<String> {
    let mut coordinator = MarkAwareRpf::new(settings, |_: &InterfaceSettingReceipt| Ok(()));
    receipts
        .iter()
        .filter_map(|receipt| {
            coordinator
                .restore_mark_aware_rpf(receipt)
                .err()
                .map(|error| {
                    format!(
                        "interface {} src_valid_mark left unchanged: {error}",
                        receipt.identity.name
                    )
                })
        })
        .collect()
}

fn validate_setting_receipts(record: &StoredAppliedState) -> Result<(), PrivilegedError> {
    if !record.interface_settings.is_empty() && record.active.is_none() {
        return Err(PrivilegedError::new(
            "invalid persisted state: interface setting receipts without an active plan",
        ));
    }
    if !record.pending_interface_settings.is_empty() && record.pending.is_none() {
        return Err(PrivilegedError::new(
            "invalid persisted state: pending interface settings without a pending plan",
        ));
    }
    let receipts = record
        .interface_settings
        .iter()
        .chain(&record.pending_interface_settings);
    if receipts
        .clone()
        .any(|receipt| receipt.previous != 0 || receipt.applied != 1)
        || receipts.count() > 2 * MAX_LANES
    {
        return Err(PrivilegedError::new(
            "invalid persisted state: interface setting receipts are out of bounds",
        ));
    }
    Ok(())
}

fn validate_legacy_envelope(record: &StoredAppliedState) -> Result<(), PrivilegedError> {
    let empty = StoredAppliedState {
        legacy_v1: record.legacy_v1.clone(),
        ..StoredAppliedState::default()
    };
    if *record != empty {
        return Err(PrivilegedError::new(
            "invalid persisted state: a schema-1 journal cannot carry current-schema state",
        ));
    }
    Ok(())
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
    if unowned_requested_resource_present(&captured, current.map(AppliedKernelState::plan), next)? {
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
) -> Result<bool, PrivilegedError> {
    let live = live_routing_ownership(captured)?;
    let requested = plan_routing_ownership(next);
    let owned = current.map(plan_routing_ownership).unwrap_or_default();
    Ok(live
        .routes
        .iter()
        .any(|route| requested.routes.contains(route) && !owned.routes.contains(route))
        || live
            .rules
            .iter()
            .any(|rule| requested.rules.contains(rule) && !owned.rules.contains(rule)))
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
        let previous = self.previous.as_ref().map(AppliedKernelState::plan);
        for step in transition_steps(previous, next) {
            run_routing_step(
                self.executor,
                &step,
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

        for step in retire_steps(&previous, &next) {
            run_routing_step(
                self.executor,
                &step,
                "remove previous RouteLane route, policy rule or table",
            )?;
        }
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
        for step in rollback_table_order(&plans) {
            run_routing_step(
                self.executor,
                &step,
                "flush RouteLane table during rollback",
            )?;
        }

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
            for step in install_steps(state.plan()) {
                run_routing_step(
                    self.executor,
                    &step,
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
    super::render_nft_document(plan, pending_transaction, include_guard)
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
    let live =
        live_routing_ownership(captured).map_err(|error| RunnerError::new(error.to_string()))?;
    let expected = plan_routing_ownership(plan);
    Ok(expected.routes.is_subset(&live.routes) && expected.rules.is_subset(&live.rules))
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
    if !routing_ownership_is_subset(&live_routing, &plan_routing_ownership(active.plan())) {
        return Err(PrivilegedError::new(
            "cleanup journal does not own the exact live RouteLane routing subset",
        ));
    }
    if !captured.snapshot.has_nft_table("inet", "routelane") {
        return if live_routing == RoutingOwnership::default() {
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct RouteOwnership {
    family: u8,
    table: u32,
    gateway: Option<String>,
    interface: String,
    /// Lowest-preference unreachable default; omitted when false so unicast-only
    /// ownership keeps its previous serialized fingerprint.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct RuleOwnership {
    family: u8,
    priority: u32,
    table: u32,
    mark: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct RoutingOwnership {
    routes: BTreeSet<RouteOwnership>,
    rules: BTreeSet<RuleOwnership>,
}

fn routing_ownership_is_subset(live: &RoutingOwnership, expected: &RoutingOwnership) -> bool {
    live.routes.is_subset(&expected.routes) && live.rules.is_subset(&expected.rules)
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
    for step in install_steps(plan) {
        apply_routing_step(&mut ownership, &step);
    }
    ownership
}

fn route_ownership(family: IpFamily, table: u32, route: &OwnedRoute) -> RouteOwnership {
    match route {
        OwnedRoute::Unicast { gateway, interface } => RouteOwnership {
            family: family_number(family),
            table,
            gateway: gateway.map(|gateway| gateway.to_string()),
            interface: interface.clone(),
            terminal: false,
        },
        OwnedRoute::Terminal => RouteOwnership {
            family: family_number(family),
            table,
            gateway: None,
            interface: String::new(),
            terminal: true,
        },
    }
}

fn rule_ownership(family: IpFamily, priority: u32, table: u32, mark: u32) -> RuleOwnership {
    RuleOwnership {
        family: family_number(family),
        priority,
        table,
        mark,
    }
}

/// Models one typed step exactly as the kernel applies it: a replace swaps
/// the route of the same kind in the same table, removals are idempotent.
fn apply_routing_step(state: &mut RoutingOwnership, step: &RoutingStep) {
    match step {
        RoutingStep::ReplaceRoute {
            family,
            table,
            route,
        } => {
            let route = route_ownership(*family, *table, route);
            state.routes.retain(|existing| {
                existing.family != route.family
                    || existing.table != route.table
                    || existing.terminal != route.terminal
            });
            state.routes.insert(route);
        }
        RoutingStep::DeleteRoute {
            family,
            table,
            route,
        } => {
            state
                .routes
                .remove(&route_ownership(*family, *table, route));
        }
        RoutingStep::AddRule {
            family,
            priority,
            table,
            mark,
        } => {
            state
                .rules
                .insert(rule_ownership(*family, *priority, *table, *mark));
        }
        RoutingStep::DeleteRule {
            family,
            priority,
            table,
            mark,
        } => {
            state
                .rules
                .remove(&rule_ownership(*family, *priority, *table, *mark));
        }
        RoutingStep::FlushTable { family, table } => {
            let family = family_number(*family);
            state
                .routes
                .retain(|route| route.family != family || route.table != *table);
        }
    }
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
    for step in transition_steps(active, pending) {
        apply_routing_step(&mut state, &step);
        guard_states.push(state.clone());
    }
    if phase == PendingNftPhase::Guard {
        return Ok(guard_states);
    }

    let mut activated_states = vec![state.clone()];
    if let Some(active) = active {
        for step in retire_steps(active, pending) {
            apply_routing_step(&mut state, &step);
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

/// Rules a rollback removes, in the exact order `restore` removes them.
fn rollback_rule_order<'a>(plans: impl IntoIterator<Item = &'a KernelPlan>) -> Vec<RoutingStep> {
    let mut rules = Vec::new();
    for plan in plans {
        for step in install_steps(plan) {
            if let RoutingStep::AddRule {
                family,
                priority,
                table,
                mark,
            } = step
            {
                let delete = RoutingStep::DeleteRule {
                    family,
                    priority,
                    table,
                    mark,
                };
                if !rules.contains(&delete) {
                    rules.push(delete);
                }
            }
        }
    }
    rules
}

/// Tables a rollback flushes, in the exact order `restore` flushes them.
fn rollback_table_order<'a>(plans: impl IntoIterator<Item = &'a KernelPlan>) -> Vec<RoutingStep> {
    let mut tables = Vec::new();
    for plan in plans {
        for lane in &plan.lanes {
            for family in [IpFamily::V4, IpFamily::V6] {
                let flush = RoutingStep::FlushTable {
                    family,
                    table: lane.table,
                };
                if !tables.contains(&flush) {
                    tables.push(flush);
                }
            }
        }
    }
    tables
}

fn rollback_prefix_reaches(
    starting_state: &RoutingOwnership,
    active: Option<&KernelPlan>,
    pending: &KernelPlan,
    expected: &RoutingOwnership,
) -> bool {
    let plans = || active.into_iter().chain(std::iter::once(pending));
    let mut state = starting_state.clone();
    for step in rollback_rule_order(plans())
        .into_iter()
        .chain(rollback_table_order(plans()))
    {
        let before = state.clone();
        apply_routing_step(&mut state, &step);
        if state != before && state == *expected {
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
    for step in install_steps(active) {
        apply_routing_step(&mut state, &step);
        states.push(state.clone());
    }
    states
}

fn routing_ownership_fingerprint(ownership: &RoutingOwnership) -> Result<String, PrivilegedError> {
    let encoded = serde_json::to_vec(ownership)
        .map_err(|error| PrivilegedError::new(format!("encode routing ownership: {error}")))?;
    Ok(blake3::hash(&encoded).to_hex().to_string())
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

fn validate_orphan_journal(record: &StoredAppliedState) -> Result<(), PrivilegedError> {
    if record.orphan_cleanup.is_none()
        || record.active.is_some()
        || record.active_nft_fingerprint.is_some()
        || record.pending.is_some()
        || record.pending_expectations.is_some()
        || record.cleanup_pending
        || record.restore_receipt.is_some()
    {
        return Err(PrivilegedError::new(
            "invalid persisted state: orphan cleanup must be an isolated journal",
        ));
    }
    Ok(())
}

fn capture_verified_orphaned_ownership(
    executor: &mut impl CommandExecutor,
) -> Result<OrphanedOwnership, PrivilegedError> {
    let captured =
        capture_snapshot(executor).map_err(|error| PrivilegedError::new(error.to_string()))?;
    let routing = live_routing_ownership(&captured)?;
    validate_orphan_routing(&routing)?;
    let nft_fingerprint = if captured.snapshot.has_nft_table("inet", "routelane") {
        let document =
            read_nft_table(executor).map_err(|error| PrivilegedError::new(error.to_string()))?;
        validate_orphan_nft_document(&document)?;
        Some(
            nft_document_fingerprint(&document)
                .map_err(|error| PrivilegedError::new(error.to_string()))?,
        )
    } else {
        None
    };
    if nft_fingerprint.is_none() && routing != RoutingOwnership::default() {
        return Err(PrivilegedError::new(
            "ambiguous RouteLane ownership: reserved routing resources have no ownership-marked nftables table",
        ));
    }
    Ok(OrphanedOwnership {
        nft_fingerprint,
        routing,
    })
}

fn validate_orphan_routing(ownership: &RoutingOwnership) -> Result<(), PrivilegedError> {
    for rule in &ownership.rules {
        let table_slot = rule.table.checked_sub(TABLE_BASE);
        let priority_slot = rule.priority.checked_sub(RULE_PRIORITY_BASE);
        let mark_slot = rule.mark.checked_sub(MARK_BASE);
        if table_slot != priority_slot || table_slot != mark_slot {
            return Err(PrivilegedError::new(
                "ambiguous RouteLane ownership: policy rule does not match a reserved lane tuple",
            ));
        }
        if ownership
            .rules
            .iter()
            .filter(|candidate| candidate.family == rule.family && candidate.table == rule.table)
            .count()
            != 1
        {
            return Err(PrivilegedError::new(
                "ambiguous RouteLane ownership: routing table has multiple policy rules",
            ));
        }
    }
    for route in &ownership.routes {
        if ownership
            .routes
            .iter()
            .filter(|candidate| {
                candidate.family == route.family
                    && candidate.table == route.table
                    && candidate.terminal == route.terminal
            })
            .count()
            != 1
        {
            return Err(PrivilegedError::new(
                "ambiguous RouteLane ownership: routing table has multiple default routes",
            ));
        }
    }
    Ok(())
}

fn validate_orphan_nft_document(document: &serde_json::Value) -> Result<(), PrivilegedError> {
    let objects = document
        .get("nftables")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| PrivilegedError::new("RouteLane nftables ownership document is invalid"))?;
    let modern = objects.iter().any(|o| {
        o["chain"]["name"]
            .as_str()
            .is_some_and(|name| name.starts_with(POLICY_SEAL_PREFIX))
    });
    if modern && !has_valid_policy_seal(document) {
        return Err(PrivilegedError::new(
            "RouteLane nftables ownership content seal does not match the complete live table",
        ));
    }
    if modern && objects.iter().any(|o| o["chain"]["name"] == "guard") {
        return Err(PrivilegedError::new(
            "RouteLane temporary guard requires its exact transaction journal for recovery",
        ));
    }
    let mut tables = 0;
    let mut outputs = 0;
    let mut markers = 0;
    let mut chains = BTreeSet::new();
    for object in objects {
        if object.get("metainfo").is_some() {
            continue;
        }
        let (kind, value) = ["table", "chain", "rule"]
            .into_iter()
            .find_map(|kind| object.get(kind).map(|value| (kind, value)))
            .ok_or_else(|| {
                PrivilegedError::new("RouteLane nftables table contains an unknown object")
            })?;
        if value.get("family").and_then(serde_json::Value::as_str) != Some("inet")
            || (kind != "table"
                && value.get("table").and_then(serde_json::Value::as_str) != Some("routelane"))
            || (kind == "table"
                && value.get("name").and_then(serde_json::Value::as_str) != Some("routelane"))
        {
            return Err(PrivilegedError::new(
                "RouteLane nftables ownership document crosses its reserved table boundary",
            ));
        }
        match kind {
            "table" => tables += 1,
            "chain" => {
                let name = value
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| PrivilegedError::new("RouteLane nftables chain has no name"))?;
                if !chains.insert(name.to_owned()) {
                    return Err(PrivilegedError::new(
                        "RouteLane nftables chain is duplicated",
                    ));
                }
                if modern && !modern_owned_chain(value) {
                    return Err(PrivilegedError::new(
                        "RouteLane nftables chain does not match an exact owned hook or selector",
                    ));
                }
                if name == "output" {
                    outputs += 1;
                } else if valid_state_chain_name(name) {
                    markers += 1;
                } else if modern {
                    // The modern layout is accepted only as a complete,
                    // unchanged sealed document, never by table name alone.
                } else {
                    return Err(PrivilegedError::new(
                        "RouteLane nftables table contains an unrecognized ownership marker",
                    ));
                }
            }
            "rule" => {
                let name = value.get("chain").and_then(serde_json::Value::as_str);
                let permitted = if modern {
                    name.is_some_and(|name| {
                        chains.contains(name)
                            && !valid_state_chain_name(name)
                            && !name.starts_with(POLICY_SEAL_PREFIX)
                    })
                } else {
                    name == Some("output")
                };
                if !permitted {
                    return Err(PrivilegedError::new(
                        "RouteLane nftables rule is outside the owned output chain",
                    ));
                }
            }
            _ => unreachable!(),
        }
    }
    if tables != 1 || outputs != 1 || markers != 1 {
        return Err(PrivilegedError::new(
            "RouteLane nftables table has no unique validated ownership marker",
        ));
    }
    if modern
        && [
            "restore4",
            "restore6",
            "prerouting",
            "egress_guard",
            "source_nat",
        ]
        .iter()
        .any(|name| !chains.contains(*name))
    {
        return Err(PrivilegedError::new(
            "RouteLane nftables policy is missing a required owned chain",
        ));
    }
    Ok(())
}

fn modern_owned_chain(chain: &serde_json::Value) -> bool {
    let Some(name) = chain["name"].as_str() else {
        return false;
    };
    let expected = match name {
        "output" => Some(("route", "output", -150)),
        "prerouting" => Some(("filter", "prerouting", -150)),
        "egress_guard" => Some(("filter", "postrouting", 99)),
        "source_nat" => Some(("nat", "postrouting", 100)),
        "guard" => Some(("route", "output", -149)),
        _ => None,
    };
    if let Some((kind, hook, priority)) = expected {
        return chain["type"] == kind
            && chain["hook"] == hook
            && chain["prio"] == priority
            && chain["policy"] == "accept";
    }
    let regular = name == "restore4"
        || name == "restore6"
        || valid_state_chain_name(name)
        || name.strip_prefix(POLICY_SEAL_PREFIX).is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
        || ["select4_", "select6_"].iter().any(|prefix| {
            name.strip_prefix(prefix).is_some_and(|mark| {
                mark.len() == 8
                    && u32::from_str_radix(mark, 16)
                        .is_ok_and(|m| (MARK_BASE..=MARK_MAX).contains(&m))
            })
        });
    regular
        && ["type", "hook", "prio", "policy"]
            .iter()
            .all(|key| chain.get(key).is_none())
}

fn valid_state_chain_name(name: &str) -> bool {
    let Some((generation, fingerprint)) = name
        .strip_prefix("state_g")
        .and_then(|value| value.split_once('_'))
    else {
        return false;
    };
    generation.parse::<u64>().is_ok_and(|value| value > 0)
        && fingerprint.len() == 16
        && fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn orphan_journal(ownership: OrphanedOwnership) -> StoredAppliedState {
    StoredAppliedState {
        active: None,
        active_nft_fingerprint: None,
        pending: None,
        pending_expectations: None,
        cleanup_pending: false,
        restore_receipt: None,
        orphan_cleanup: Some(ownership),
        ..StoredAppliedState::default()
    }
}

fn resume_orphan_cleanup(
    executor: &mut impl CommandExecutor,
    store: &mut impl AppliedStateStore,
) -> Result<(), PrivilegedError> {
    let record = store
        .load()?
        .ok_or_else(|| PrivilegedError::new("orphan cleanup journal is missing"))?;
    validate_orphan_journal(&record)?;
    let mut ownership = record.orphan_cleanup.expect("validated orphan journal");
    let captured =
        capture_snapshot(executor).map_err(|error| PrivilegedError::new(error.to_string()))?;
    let live = live_routing_ownership(&captured)?;
    if !live.routes.is_subset(&ownership.routing.routes)
        || !live.rules.is_subset(&ownership.routing.rules)
    {
        return Err(PrivilegedError::new(
            "orphan cleanup journal conflicts with live routing resources",
        ));
    }
    ownership.routing = live;
    store.save(&orphan_journal(ownership.clone()))?;

    for rule in ownership.routing.rules.clone() {
        let family = if rule.family == 4 {
            IpFamily::V4
        } else {
            IpFamily::V6
        };
        let command = ip_command(
            family,
            [
                "rule".to_owned(),
                "del".to_owned(),
                "fwmark".to_owned(),
                format!("{:#010x}", rule.mark),
                "lookup".to_owned(),
                rule.table.to_string(),
                "priority".to_owned(),
                rule.priority.to_string(),
            ],
        );
        run_required(
            executor,
            &command,
            "remove verified orphan RouteLane policy rule",
        )
        .map_err(|error| PrivilegedError::new(error.to_string()))?;
        ownership.routing.rules.remove(&rule);
        store.save(&orphan_journal(ownership.clone()))?;
    }
    for route in ownership.routing.routes.clone() {
        let family = if route.family == 4 {
            IpFamily::V4
        } else {
            IpFamily::V6
        };
        let command = ip_command(
            family,
            [
                "route".to_owned(),
                "flush".to_owned(),
                "table".to_owned(),
                route.table.to_string(),
            ],
        );
        run_required(
            executor,
            &command,
            "flush verified orphan RouteLane routing table",
        )
        .map_err(|error| PrivilegedError::new(error.to_string()))?;
        ownership
            .routing
            .routes
            .retain(|value| value.family != route.family || value.table != route.table);
        store.save(&orphan_journal(ownership.clone()))?;
    }
    if let Some(expected) = ownership.nft_fingerprint.as_deref() {
        let document =
            read_nft_table(executor).map_err(|error| PrivilegedError::new(error.to_string()))?;
        let live = nft_document_fingerprint(&document)
            .map_err(|error| PrivilegedError::new(error.to_string()))?;
        if live != expected {
            return Err(PrivilegedError::new(
                "orphan cleanup journal does not exactly own the live inet/routelane table",
            ));
        }
        run_nft_batch(
            executor,
            b"delete table inet routelane\n".to_vec(),
            "delete verified orphan RouteLane nftables table",
        )
        .map_err(|error| PrivilegedError::new(error.to_string()))?;
        ownership.nft_fingerprint = None;
        store.save(&orphan_journal(ownership))?;
    }
    let captured =
        capture_snapshot(executor).map_err(|error| PrivilegedError::new(error.to_string()))?;
    if captured.snapshot.has_nft_table("inet", "routelane")
        || live_routing_ownership(&captured)? != RoutingOwnership::default()
    {
        return Err(PrivilegedError::new(
            "verified orphan RouteLane resources remain after cleanup",
        ));
    }
    store.clear()
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
        if object.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(PrivilegedError::new(
                "live RouteLane routing-table entry has foreign semantics",
            ));
        }
        let resource = if route
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| matches!(kind, "unreachable" | "7"))
        {
            live_terminal_route(&route, family, table)?
        } else {
            live_unicast_route(&route, family, table)?
        };
        if !ownership.routes.insert(resource) {
            return Err(PrivilegedError::new(
                "live RouteLane routing-table entry is duplicated",
            ));
        }
    }
    Ok(())
}

fn live_terminal_route(
    route: &serde_json::Value,
    family: u8,
    table: u32,
) -> Result<RouteOwnership, PrivilegedError> {
    // IPv4 reports no device; IPv6 attaches reject routes to loopback.
    let device_is_exact = if family == 4 {
        route.get("dev").is_none()
    } else {
        absent_or_exact_string(route.get("dev"), "lo")
    };
    if route.get("dst").and_then(serde_json::Value::as_str) != Some("default")
        || json_u32(route.get("metric")) != Some(TERMINAL_ROUTE_METRIC)
        || route.get("gateway").is_some()
        || !device_is_exact
        // iproute2 prints enums symbolically, or numerically with `-N`.
        || !route
            .get("protocol")
            .is_none_or(|value| matches!(value.as_str(), Some("boot" | "3")))
        || !route
            .get("scope")
            .is_none_or(|value| matches!(value.as_str(), Some("global" | "0")))
        || route.get("prefsrc").is_some()
        || !absent_or_empty_array(route.get("flags"))
        || (family == 4 && route.get("pref").is_some())
        || (family == 6 && !absent_or_exact_string(route.get("pref"), "medium"))
    {
        return Err(PrivilegedError::new(
            "live RouteLane terminal route has foreign semantics",
        ));
    }
    Ok(RouteOwnership {
        family,
        table,
        gateway: None,
        interface: String::new(),
        terminal: true,
    })
}

fn live_unicast_route(
    route: &serde_json::Value,
    family: u8,
    table: u32,
) -> Result<RouteOwnership, PrivilegedError> {
    if route.get("dst").and_then(serde_json::Value::as_str) != Some("default")
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
    Ok(RouteOwnership {
        family,
        table,
        gateway: route
            .get("gateway")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        interface: route
            .get("dev")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| PrivilegedError::new("live RouteLane route is missing its interface"))?
            .to_owned(),
        terminal: false,
    })
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
    let document = read_nft_table_once(executor)?;
    if !super::nft_document::has_symbolic_ifindices(&document) {
        return Ok(document);
    }
    // Never resolve a current dump with stale stored name→index assumptions.
    // Bracket the actual dump with immutable-identity observations; fail closed
    // if a device is renamed/replaced during this read-only proof.
    let before = read_nft_link_bindings()?;
    let mut document = read_nft_table_once(executor)?;
    let after = read_nft_link_bindings()?;
    if before != after {
        return Err(RunnerError::new("interface identities changed during exact nft ownership inspection; retry with fresh topology"));
    }
    super::nft_document::resolve_dump_ifindices(&mut document, &before)?;
    Ok(document)
}

fn read_nft_link_bindings() -> Result<BTreeMap<String, super::InterfaceIdentity>, RunnerError> {
    let links = run_topology_query(super::topology::TopologyQuery::Links)?;
    let topology = super::parse_topology(super::TopologyInput {
        links: &links,
        addresses: b"[]",
        routes_v4: b"[]",
        routes_v6: b"[]",
        rules_v4: b"[]",
        rules_v6: b"[]",
    })
    .map_err(|e| RunnerError::new(e.to_string()))?;
    Ok(topology.interface_identities())
}

fn read_nft_table_once(
    executor: &mut impl CommandExecutor,
) -> Result<serde_json::Value, RunnerError> {
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

fn json_u32(value: Option<&serde_json::Value>) -> Option<u32> {
    match value? {
        serde_json::Value::Number(number) => number.as_u64()?.try_into().ok(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// Runs one typed routing step. Removals are skipped when the exact resource is
/// already absent, so a kernel-dropped route or an interrupted removal can resume.
fn run_routing_step(
    executor: &mut impl CommandExecutor,
    step: &RoutingStep,
    context: &str,
) -> Result<(), RunnerError> {
    let present = match step {
        RoutingStep::DeleteRoute {
            family,
            table,
            route,
        } => live_family_ownership(executor, *family)?
            .routes
            .contains(&route_ownership(*family, *table, route)),
        RoutingStep::DeleteRule {
            family,
            priority,
            table,
            mark,
        } => live_family_ownership(executor, *family)?
            .rules
            .contains(&rule_ownership(*family, *priority, *table, *mark)),
        RoutingStep::FlushTable { family, table } => {
            let command = ip_command(*family, ["-j", "-N", "route", "show", "table", "all"]);
            let routes = run_required(executor, &command, "inspect RouteLane routing table")?;
            route_table_present(&routes, *table)?
        }
        RoutingStep::ReplaceRoute { .. } | RoutingStep::AddRule { .. } => true,
    };
    if present {
        run_required(executor, &step.command(), context)?;
    }
    Ok(())
}

fn live_family_ownership(
    executor: &mut impl CommandExecutor,
    family: IpFamily,
) -> Result<RoutingOwnership, RunnerError> {
    let routes = run_required(
        executor,
        &ip_command(family, ["-j", "-N", "route", "show", "table", "all"]),
        "inspect RouteLane routes",
    )?;
    let rules = run_required(
        executor,
        &ip_command(family, ["-j", "-N", "rule", "show"]),
        "inspect RouteLane policy rules",
    )?;
    let mut ownership = RoutingOwnership::default();
    let number = family_number(family);
    collect_live_routes(&routes, number, &mut ownership)
        .and_then(|()| collect_live_rules(&rules, number, &mut ownership))
        .map_err(|error| RunnerError::new(error.to_string()))?;
    Ok(ownership)
}

fn remove_plan_policy_rules(
    executor: &mut impl CommandExecutor,
    plans: &[KernelPlan],
) -> Result<(), RunnerError> {
    for step in rollback_rule_order(plans) {
        let RoutingStep::DeleteRule {
            family,
            priority,
            table,
            mark,
        } = step
        else {
            unreachable!("rollback rule order yields rule removals");
        };
        let rule = rule_ownership(family, priority, table, mark);
        for _ in 0..3 {
            if !live_family_ownership(executor, family)?
                .rules
                .contains(&rule)
            {
                break;
            }
            run_required(
                executor,
                &step.command(),
                "remove RouteLane policy rule during rollback",
            )?;
        }
        if live_family_ownership(executor, family)?
            .rules
            .contains(&rule)
        {
            return Err(RunnerError::new(
                "too many duplicate RouteLane policy rules during rollback",
            ));
        }
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
    InvalidApplicationCgroup(String),
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
            Self::InvalidApplicationCgroup(path) => {
                write!(formatter, "invalid RouteLane application cgroup {path:?}")
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
        if let CompiledTarget::Cgroup(path) = &rule.target {
            if !is_routelane_application_cgroup(path) {
                return Err(ValidationError::InvalidApplicationCgroup(path.clone()));
            }
        }
        if let CompiledAction::Mark(mark) = rule.action {
            if !marks.contains(&mark) {
                return Err(ValidationError::UnknownMark(mark));
            }
        }
    }

    Ok(())
}

fn is_routelane_application_cgroup(path: &str) -> bool {
    let Some(rule_id) = path.strip_prefix("routelane.apps/") else {
        return false;
    };
    !rule_id.is_empty()
        && rule_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
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

    pub async fn caller_uid(&self, sender: &str) -> Result<u32, AuthorizationError> {
        let sender_name = zbus::names::BusName::try_from(sender)
            .map_err(|error| AuthorizationError::UidLookup(error.to_string()))?;
        let dbus = zbus::fdo::DBusProxy::new(&self.connection)
            .await
            .map_err(|error| AuthorizationError::UidLookup(error.to_string()))?;
        dbus.get_connection_unix_user(sender_name)
            .await
            .map_err(|error| AuthorizationError::UidLookup(error.to_string()))
    }
}

impl Authorizer for SystemPolkitAuthorizer {
    type Future<'a> = Pin<Box<dyn Future<Output = Result<(), AuthorizationError>> + Send + 'a>>;

    fn authorize<'a>(&'a self, sender: &'a str) -> Self::Future<'a> {
        Box::pin(async move {
            let _caller_uid = self.caller_uid(sender).await?;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanRecoveryResponse {
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
    fn recover_orphaned(&mut self) -> Result<OrphanRecoveryResponse, PrivilegedError>;
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

    pub async fn recover_orphaned_resources(
        &self,
        sender: Option<&str>,
    ) -> Result<OrphanRecoveryResponse, ServiceError> {
        self.authorize(sender).await?;
        self.run_backend(PrivilegedBackend::recover_orphaned).await
    }

    pub async fn authorize_mutation(&self, sender: Option<&str>) -> Result<(), ServiceError> {
        self.authorize(sender).await.map(|_| ())
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
        execute_invocation_with_timeout, nft_document_fingerprint, render_guarded_ruleset,
        render_owned_ruleset, render_transaction_ruleset, run_nft_batch, validate_privileged_plan,
        validate_root_owned_file_attributes, AppliedStateStore, ApplyResponse, AuthorizationError,
        Authorizer, CleanupResponse, CommandExecutor, CommandInvocation, CommandOutput,
        CommandPrivilegedBackend, InspectResponse, InterfaceSettingReceipt, OrphanRecoveryResponse,
        PrivilegedBackend, PrivilegedError, PrivilegedService, RestoreReceipt, RestoreResponse,
        ServiceError, StoredAppliedState, ValidationError, IP_BINARY, MAX_PLAN_JSON_BYTES,
        NFT_BINARY,
    };
    use crate::backend::{
        CommandSpec, CompiledAction, CompiledRule, CompiledTarget, IpFamily, KernelPlan,
        LaneAvailability, LanePlan, Program, ResourceState, RouteMode, TableMode, MARK_BASE,
        RULE_PRIORITY_BASE, TABLE_BASE, TERMINAL_ROUTE_METRIC,
    };

    fn fixture_lane(id: &str, offset: u32) -> LanePlan {
        LanePlan {
            identity: None,
            source_v4: vec![],
            source_v6: vec![],
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
            primary: Default::default(),
            local_networks: vec![],
            local_addresses: vec![],
            generation: 1,
            profile_id: ProfileId::from("default"),
            lanes: vec![fixture_lane("privacy", 0)],
            rules: vec![CompiledRule {
                family: crate::backend::IpFamily::V4,
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
    fn privileged_nft_renderer_delegates_mark_to_terminal_selection_after_counter() {
        let mut plan = fixture_valid_plan();
        let rule = plan.rules.remove(0);

        assert_eq!(
            serde_json::Value::Array(super::super::nft_document::explicit_rule(&plan, &rule)),
            serde_json::json!([
                {
                    "match": {
                        "op": "==",
                        "left": {"payload": {"protocol": "ip", "field": "daddr"}},
                        "right": {"prefix": {"addr": "203.0.113.0", "len": 24}}
                    }
                },
                {"counter": null},
                {"goto": {"target":"select4_524c0001"}}
            ])
        );
    }

    #[test]
    fn privileged_nft_renderer_matches_application_cgroup_before_marking() {
        let rule = CompiledRule {
            family: crate::backend::IpFamily::V4,
            id: RuleId::from("browser"),
            priority: 1,
            target: CompiledTarget::Cgroup("routelane.apps/browser".to_owned()),
            action: CompiledAction::Mark(MARK_BASE),
        };

        assert_eq!(
            super::super::nft_document::explicit_rule(&fixture_valid_plan(), &rule)[1],
            serde_json::json!({
                "match": {
                    "op": "==",
                    "left": {"socket": {"key": "cgroupv2", "level": 2}},
                    "right": "routelane.apps/browser"
                }
            })
        );
    }

    #[test]
    fn privileged_plan_rejects_an_application_cgroup_outside_routelane() {
        let mut plan = fixture_valid_plan();
        plan.rules[0].target = CompiledTarget::Cgroup("../user.slice".to_owned());

        assert_eq!(
            validate_privileged_plan(&plan),
            Err(ValidationError::InvalidApplicationCgroup(
                "../user.slice".to_owned()
            ))
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
    fn orphan_recovery_accepts_current_policy_only_with_unchanged_content_seal() {
        let valid = super::expected_nft_document(Some(&fixture_valid_plan()), None, false);
        let without_metadata = serde_json::json!({"nftables": valid["nftables"].as_array().unwrap()
            .iter().filter(|o| !o.as_object().unwrap().is_empty()).cloned().collect::<Vec<_>>()});
        assert!(super::validate_orphan_nft_document(&without_metadata).is_ok());
        let mut changed = without_metadata.clone();
        let expression = changed["nftables"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .filter_map(|o| o.get_mut("rule"))
            .find(|r| r["chain"] == "output")
            .unwrap();
        expression["expr"] = serde_json::json!([{"accept":null}]);
        assert!(super::validate_orphan_nft_document(&changed).is_err());
    }

    #[test]
    fn new_nft_chains_are_verified_not_just_table_markers() {
        let expected = super::expected_nft_document(Some(&fixture_valid_plan()), None, false);
        let mut changed = expected.clone();
        let hook = changed["nftables"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .filter_map(|o| o.get_mut("chain"))
            .find(|c| c["name"] == "prerouting")
            .unwrap();
        hook["prio"] = serde_json::json!(-100);
        assert_ne!(
            nft_document_fingerprint(&expected).unwrap(),
            nft_document_fingerprint(&changed).unwrap()
        );
        assert!(super::validate_orphan_nft_document(&changed).is_err());
    }

    #[test]
    fn live_ifindex_dump_is_bound_to_observed_device_not_old_name_assumptions() {
        use std::collections::BTreeMap;
        let identity = crate::backend::InterfaceIdentity {
            ifindex: 2,
            name: "eth0".into(),
            kind: "ether".into(),
            hardware_address: None,
        };
        let mut by_name = serde_json::json!({"nftables":[{"rule":{"expr":[
            {"match":{"left":{"meta":{"key":"oif"}},"op":"==","right":"eth0"}}
        ]}}]});
        let expected = serde_json::json!({"nftables":[{"rule":{"expr":[
            {"match":{"left":{"meta":{"key":"oif"}},"op":"==","right":2}}
        ]}}]});
        super::super::nft_document::resolve_dump_ifindices(
            &mut by_name,
            &BTreeMap::from([("eth0".into(), identity.clone())]),
        )
        .unwrap();
        assert_eq!(by_name, expected);
        let mut removed = serde_json::json!({"nftables":[{"rule":{"expr":[
            {"match":{"left":{"meta":{"key":"oif"}},"op":"==","right":"2"}}
        ]}}]});
        super::super::nft_document::resolve_dump_ifindices(&mut removed, &BTreeMap::new()).unwrap();
        assert_eq!(removed, expected);
        let mut replacement = identity;
        replacement.ifindex = 9;
        let mut changed = serde_json::json!({"nftables":[{"rule":{"expr":[
            {"match":{"left":{"meta":{"key":"oif"}},"op":"==","right":"eth0"}}
        ]}}]});
        super::super::nft_document::resolve_dump_ifindices(
            &mut changed,
            &BTreeMap::from([("eth0".into(), replacement)]),
        )
        .unwrap();
        assert_ne!(
            changed, expected,
            "a current name must resolve to its current index, never an old assumed binding"
        );
    }

    #[test]
    fn numeric_only_interface_names_cannot_fake_local_iif_zero_in_json_proof() {
        use std::collections::BTreeMap;
        let identity = crate::backend::InterfaceIdentity {
            ifindex: 9,
            name: "0".into(),
            kind: "dummy".into(),
            hardware_address: None,
        };
        let mut document = serde_json::json!({"nftables":[{"rule":{"expr":[
            {"match":{"left":{"meta":{"key":"iif"}},"op":"==","right":"0"}}
        ]}}]});
        assert!(super::super::nft_document::resolve_dump_ifindices(
            &mut document,
            &BTreeMap::from([("0".into(), identity)])
        )
        .is_err());
    }

    #[test]
    fn reject_uses_explicit_kernel_canonical_type_and_code() {
        // nft 1.0.9 round-trip: an unspecified reject gains a family-specific
        // type/code. Omitting it makes an otherwise correct commit look foreign.
        for (network, family, reject_type) in [
            ("192.0.2.99/32", IpFamily::V4, "icmp"),
            ("2001:db8:dead::/48", IpFamily::V6, "icmpv6"),
        ] {
            let rule = CompiledRule {
                family,
                id: RuleId::from("block"),
                priority: 1,
                target: CompiledTarget::Network(network.parse().unwrap()),
                action: CompiledAction::Block,
            };
            let generated = serde_json::json!({"rule":{"expr":
                super::super::nft_document::explicit_rule(&fixture_valid_plan(), &rule)}});
            let reject = generated["rule"]["expr"]
                .as_array()
                .unwrap()
                .last()
                .unwrap();
            assert_eq!(
                reject,
                &serde_json::json!({"reject": {
                    "type": reject_type, "expr": "port-unreachable"
                }})
            );
            let mut changed = generated.clone();
            changed["rule"]["expr"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap()["reject"]["expr"] = serde_json::json!("admin-prohibited");
            assert_ne!(
                nft_document_fingerprint(&generated).unwrap(),
                nft_document_fingerprint(&changed).unwrap(),
                "different reject policies must not become the same ownership proof"
            );
        }
        let guarded = super::expected_nft_document(None, Some(&fixture_valid_plan()), true);
        let guard_reject = guarded["nftables"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|object| object.get("rule"))
            .find(|rule| {
                rule["chain"] == "guard"
                    && rule["expr"].as_array().is_some_and(|expressions| {
                        expressions
                            .iter()
                            .any(|expression| expression.get("reject").is_some())
                    })
            })
            .unwrap();
        assert_eq!(
            guard_reject["expr"].as_array().unwrap().last().unwrap(),
            &serde_json::json!({"reject": {"type": "icmpx", "expr": "port-unreachable"}})
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
                family: crate::backend::IpFamily::V4,
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
        /// Everything beyond the legacy IPv4 table-20000 unicast/rule flags.
        extra_routes: Vec<(IpFamily, serde_json::Value)>,
        extra_rules: Vec<(IpFamily, serde_json::Value)>,
    }

    fn fake_flag_route(spec: &CommandSpec) -> bool {
        spec.family == Some(IpFamily::V4)
            && spec.args.windows(2).any(|pair| pair == ["table", "20000"])
            && !spec.args.iter().any(|argument| argument == "unreachable")
    }

    fn fake_route_json(spec: &CommandSpec) -> serde_json::Value {
        let value_after = |key: &str| {
            spec.args
                .windows(2)
                .find(|pair| pair[0] == key)
                .map(|pair| pair[1].clone())
        };
        let table: u32 = value_after("table").unwrap().parse().unwrap();
        let v6 = spec.family == Some(IpFamily::V6);
        if spec.args.iter().any(|argument| argument == "unreachable") {
            let mut route = serde_json::json!({
                "type": "7",
                "dst": "default",
                "table": table,
                "metric": value_after("metric").unwrap().parse::<u32>().unwrap(),
                "flags": [],
            });
            if v6 {
                route["dev"] = serde_json::json!("lo");
                route["pref"] = serde_json::json!("medium");
            }
            return route;
        }
        let mut route = serde_json::json!({
            "dst": "default",
            "dev": value_after("dev").unwrap(),
            "table": table,
            "flags": [],
        });
        match value_after("via") {
            Some(gateway) => route["gateway"] = serde_json::json!(gateway),
            None => route["scope"] = serde_json::json!("link"),
        }
        if v6 {
            route["metric"] = serde_json::json!(1024);
            route["pref"] = serde_json::json!("medium");
        }
        route
    }

    fn fake_same_route(left: &serde_json::Value, right: &serde_json::Value) -> bool {
        left["table"] == right["table"] && left.get("type") == right.get("type")
    }

    fn fake_rule_json(spec: &CommandSpec) -> serde_json::Value {
        let value_after = |key: &str| {
            spec.args
                .windows(2)
                .find(|pair| pair[0] == key)
                .map(|pair| pair[1].clone())
                .unwrap()
        };
        serde_json::json!({
            "priority": value_after("priority").parse::<u32>().unwrap(),
            "src": "all",
            "fwmark": value_after("fwmark"),
            "table": value_after("lookup").parse::<u32>().unwrap(),
        })
    }

    fn fake_flag_rule(spec: &CommandSpec) -> bool {
        spec.family == Some(IpFamily::V4)
            && spec
                .args
                .windows(2)
                .any(|pair| pair == ["priority", "10000"])
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
                        body.extend(
                            state
                                .extra_routes
                                .iter()
                                .filter(|(family, _)| Some(*family) == spec.family)
                                .map(|(_, route)| route.clone()),
                        );
                        Ok(CommandOutput::success(serde_json::to_vec(&body).unwrap()))
                    }
                    [json, numeric, "rule", "show"] if *json == "-j" && *numeric == "-N" => {
                        let mut body = if state.has_rule && spec.family == Some(IpFamily::V4) {
                            vec![serde_json::json!({
                                "priority": 10_000,
                                "src": "all",
                                "fwmark": "0x524c0001",
                                "table": 20_000,
                            })]
                        } else {
                            vec![]
                        };
                        body.extend(
                            state
                                .extra_rules
                                .iter()
                                .filter(|(family, _)| Some(*family) == spec.family)
                                .map(|(_, rule)| rule.clone()),
                        );
                        Ok(CommandOutput::success(serde_json::to_vec(&body).unwrap()))
                    }
                    ["route", "replace", ..] if fake_flag_route(spec) => {
                        state.has_route = true;
                        state.gatewayless_route =
                            !spec.args.iter().any(|argument| argument == "via");
                        Ok(CommandOutput::success(vec![]))
                    }
                    ["route", "replace", ..] => {
                        let family = spec.family.unwrap();
                        let route = fake_route_json(spec);
                        state.extra_routes.retain(|(existing_family, existing)| {
                            *existing_family != family || !fake_same_route(existing, &route)
                        });
                        state.extra_routes.push((family, route));
                        Ok(CommandOutput::success(vec![]))
                    }
                    ["route", "del", ..] if fake_flag_route(spec) => {
                        state.has_route = false;
                        state.gatewayless_route = false;
                        Ok(CommandOutput::success(vec![]))
                    }
                    ["route", "del", ..] => {
                        let family = spec.family.unwrap();
                        let route = fake_route_json(spec);
                        state.extra_routes.retain(|(existing_family, existing)| {
                            *existing_family != family || !fake_same_route(existing, &route)
                        });
                        Ok(CommandOutput::success(vec![]))
                    }
                    ["rule", "add", ..] if fake_flag_rule(spec) => {
                        state.has_rule = true;
                        Ok(CommandOutput::success(vec![]))
                    }
                    ["rule", "add", ..] => {
                        let rule = fake_rule_json(spec);
                        state.extra_rules.push((spec.family.unwrap(), rule));
                        Ok(CommandOutput::success(vec![]))
                    }
                    ["rule", "del", ..] if fake_flag_rule(spec) => {
                        state.has_rule = false;
                        Ok(CommandOutput::success(vec![]))
                    }
                    ["rule", "del", ..] => {
                        let family = spec.family.unwrap();
                        let rule = fake_rule_json(spec);
                        if let Some(index) = state
                            .extra_rules
                            .iter()
                            .position(|existing| existing == &(family, rule.clone()))
                        {
                            state.extra_rules.remove(index);
                        }
                        Ok(CommandOutput::success(vec![]))
                    }
                    ["route", "flush", "table", table] => {
                        if state.fail_route_flush {
                            return Ok(CommandOutput::failure(
                                "injected route-table flush failure",
                            ));
                        }
                        let table: u32 = table.parse().unwrap();
                        let family = spec.family.unwrap();
                        if family == IpFamily::V4 && table == 20_000 {
                            state.has_route = false;
                            state.gatewayless_route = false;
                        }
                        state.extra_routes.retain(|(existing_family, route)| {
                            *existing_family != family || route["table"] != table
                        });
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
    fn recovery_cleans_an_exact_live_generation_after_its_journal_is_lost() {
        let state = Arc::new(Mutex::new(FakeKernelState::default()));
        let executor = FakeCommandExecutor {
            state: Arc::clone(&state),
        };
        let plan = fixture_valid_plan();
        CommandPrivilegedBackend::new(executor.clone())
            .apply(&plan)
            .unwrap();

        let mut restarted_without_journal = CommandPrivilegedBackend::new(executor);
        let result = restarted_without_journal.recover_orphaned().unwrap();

        assert!(result.cleaned);
        let state = state.lock().unwrap();
        assert!(!state.has_route);
        assert!(!state.has_rule);
        assert!(!state.has_table);
    }

    #[test]
    fn recovery_rejects_a_routelane_table_without_a_valid_state_marker() {
        let state = Arc::new(Mutex::new(FakeKernelState::default()));
        let executor = FakeCommandExecutor {
            state: Arc::clone(&state),
        };
        CommandPrivilegedBackend::new(executor.clone())
            .apply(&fixture_valid_plan())
            .unwrap();
        {
            let mut state = state.lock().unwrap();
            state.marker = Some("state_invalid".to_owned());
            state.nft_objects = None;
        }

        let error = CommandPrivilegedBackend::new(executor)
            .recover_orphaned()
            .unwrap_err();

        assert!(error.to_string().contains("ownership marker"));
        let state = state.lock().unwrap();
        assert!(state.has_route);
        assert!(state.has_rule);
        assert!(state.has_table);
    }

    #[test]
    fn recovery_accepts_valid_policy_rules_after_routes_were_already_removed() {
        let state = Arc::new(Mutex::new(FakeKernelState::default()));
        let executor = FakeCommandExecutor {
            state: Arc::clone(&state),
        };
        CommandPrivilegedBackend::new(executor.clone())
            .apply(&fixture_valid_plan())
            .unwrap();
        state.lock().unwrap().has_route = false;

        let recovered = CommandPrivilegedBackend::new(executor)
            .recover_orphaned()
            .unwrap();

        assert!(recovered.cleaned);
        let state = state.lock().unwrap();
        assert!(!state.has_rule);
        assert!(!state.has_table);
    }

    #[test]
    fn recovery_accepts_valid_routes_after_policy_rules_were_already_removed() {
        let state = Arc::new(Mutex::new(FakeKernelState::default()));
        let executor = FakeCommandExecutor {
            state: Arc::clone(&state),
        };
        CommandPrivilegedBackend::new(executor.clone())
            .apply(&fixture_valid_plan())
            .unwrap();
        state.lock().unwrap().has_rule = false;

        let recovered = CommandPrivilegedBackend::new(executor)
            .recover_orphaned()
            .unwrap();

        assert!(recovered.cleaned);
        let state = state.lock().unwrap();
        assert!(!state.has_route);
        assert!(!state.has_table);
    }

    #[test]
    fn cleanup_accepts_exact_rules_after_routes_were_already_removed() {
        let state = Arc::new(Mutex::new(FakeKernelState::default()));
        let executor = FakeCommandExecutor {
            state: Arc::clone(&state),
        };
        let mut backend = CommandPrivilegedBackend::new(executor);
        backend.apply(&fixture_valid_plan()).unwrap();
        state.lock().unwrap().has_route = false;

        let cleaned = backend.cleanup().unwrap();

        assert!(cleaned.cleaned);
        let state = state.lock().unwrap();
        assert!(!state.has_rule);
        assert!(!state.has_route);
        assert!(!state.has_table);
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
                orphan_cleanup: None,
                ..StoredAppliedState::default()
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
                orphan_cleanup: None,
                ..StoredAppliedState::default()
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
                orphan_cleanup: None,
                ..StoredAppliedState::default()
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
    fn unavailable_mark_has_terminal_route() {
        let state = Arc::new(Mutex::new(FakeKernelState::default()));
        let mut backend = CommandPrivilegedBackend::new(FakeCommandExecutor {
            state: Arc::clone(&state),
        });
        let mut plan = fixture_two_lane_plan();
        plan.lanes[1].availability = LaneAvailability::Unavailable;
        backend.apply(&plan).unwrap();

        {
            let kernel = state.lock().unwrap();
            assert!(kernel.has_route && kernel.has_rule);
            for table in [20_000, 20_001] {
                for family in [IpFamily::V4, IpFamily::V6] {
                    assert!(
                        kernel.extra_routes.iter().any(|(f, route)| *f == family
                            && route["table"] == table
                            && route["type"] == "7"
                            && route["metric"] == TERMINAL_ROUTE_METRIC),
                        "table {table} {family:?} must fail closed"
                    );
                }
                assert!(kernel
                    .extra_rules
                    .iter()
                    .any(|(f, rule)| *f == IpFamily::V6 && rule["table"] == table));
            }
            assert!(
                kernel
                    .extra_rules
                    .iter()
                    .any(|(f, rule)| *f == IpFamily::V4 && rule["table"] == 20_001),
                "an unavailable lane's mark still resolves to its terminal table"
            );
            assert!(
                !kernel
                    .extra_routes
                    .iter()
                    .any(|(_, route)| route.get("dev").is_some_and(|dev| dev == "wg1")),
                "an unavailable lane never receives a unicast exit"
            );
        }

        backend.cleanup().unwrap();
        let kernel = state.lock().unwrap();
        assert!(!kernel.has_route && !kernel.has_rule && !kernel.has_table);
        assert!(kernel.extra_routes.is_empty() && kernel.extra_rules.is_empty());
    }

    #[test]
    fn terminal_route_with_foreign_metric_is_not_owned() {
        let mut ownership = super::RoutingOwnership::default();
        let foreign = serde_json::json!([{
            "type": "unreachable",
            "dst": "default",
            "table": 20_000,
            "metric": 4_294_967_295u32,
            "flags": [],
        }]);
        let error =
            super::collect_live_routes(&serde_json::to_vec(&foreign).unwrap(), 4, &mut ownership)
                .unwrap_err();
        assert!(error
            .to_string()
            .contains("terminal route has foreign semantics"));

        let owned = serde_json::json!([{
            "type": "7",
            "dst": "default",
            "dev": "lo",
            "table": "20000",
            "metric": TERMINAL_ROUTE_METRIC,
            "flags": [],
            "pref": "medium",
        }]);
        super::collect_live_routes(&serde_json::to_vec(&owned).unwrap(), 6, &mut ownership)
            .unwrap();
        assert!(ownership.routes.iter().all(|route| route.terminal));
    }

    #[test]
    fn higher_generation_reuses_identical_policy_rule_without_deleting_it() {
        let state = Arc::new(Mutex::new(FakeKernelState::default()));
        let executor = FakeCommandExecutor {
            state: Arc::clone(&state),
        };
        let mut backend = CommandPrivilegedBackend::new(executor);
        let first = fixture_valid_plan();
        let rule_adds = |state: &FakeKernelState| {
            state
                .commands
                .iter()
                .filter(|command| {
                    command
                        .args
                        .starts_with(&["rule".to_owned(), "add".to_owned()])
                })
                .count()
        };
        backend.apply(&first).unwrap();
        // One rule per lane×family: the IPv6 rule guards the terminal route.
        assert_eq!(rule_adds(&state.lock().unwrap()), 2);
        let mut second = first.clone();
        second.generation = 2;

        backend.apply(&second).unwrap();
        let state = state.lock().unwrap();

        assert!(state.has_rule);
        assert_eq!(
            rule_adds(&state),
            2,
            "identical rules are reused, not re-added"
        );
        assert!(!state.commands.iter().any(|command| command
            .args
            .starts_with(&["rule".to_owned(), "del".to_owned()])));
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
        rollback_prefix.rules.remove(&super::rule_ownership(
            IpFamily::V4,
            pending.lanes[0].rule_priority,
            pending.lanes[0].table,
            pending.lanes[0].mark,
        ));

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
        let unicast = |table, gateway: &str, interface: &str| super::RouteOwnership {
            family: 4,
            table,
            gateway: Some(gateway.to_owned()),
            interface: interface.to_owned(),
            terminal: false,
        };
        let terminal = |family, table| super::RouteOwnership {
            family,
            table,
            gateway: None,
            interface: String::new(),
            terminal: true,
        };
        let rule = |family, offset: u32| super::RuleOwnership {
            family,
            priority: 10_000 + offset,
            table: 20_000 + offset,
            mark: 0x524c0001 + offset,
        };
        // The old IPv4 rule is already gone; its IPv6 rule and table remain.
        let live = super::RoutingOwnership {
            routes: [
                unicast(20_000, "10.0.0.1", "wg0"),
                terminal(4, 20_000),
                terminal(6, 20_000),
                unicast(20_001, "10.0.1.1", "wg1"),
                terminal(4, 20_001),
                terminal(6, 20_001),
            ]
            .into_iter()
            .collect(),
            rules: [rule(6, 0), rule(4, 1), rule(6, 1)].into_iter().collect(),
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
            orphan_cleanup: None,
            ..StoredAppliedState::default()
        };
        assert!(
            serde_json::to_vec(&journal).unwrap().len() < super::MAX_STATE_BYTES as usize,
            "a valid maximum-lane pending journal must fit the bounded state file"
        );

        let mut rollback_prefix = super::plan_routing_ownership(&pending);
        rollback_prefix.rules.remove(&super::rule_ownership(
            IpFamily::V4,
            pending.lanes[0].rule_priority,
            pending.lanes[0].table,
            pending.lanes[0].mark,
        ));
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
        fail_commit_generation: Arc<Mutex<Option<u64>>>,
        fail_setting_journal: Arc<Mutex<bool>>,
    }

    impl AppliedStateStore for FakeAppliedStateStore {
        fn load(&mut self) -> Result<Option<StoredAppliedState>, PrivilegedError> {
            Ok(self.state.lock().unwrap().clone())
        }

        fn save(&mut self, state: &StoredAppliedState) -> Result<(), PrivilegedError> {
            if !state.pending_interface_settings.is_empty()
                && *self.fail_setting_journal.lock().unwrap()
            {
                return Err(PrivilegedError::new(
                    "injected setting journal fsync failure",
                ));
            }
            let mut failure = self.fail_commit_generation.lock().unwrap();
            if failure.is_some()
                && state.pending.is_none()
                && state
                    .active
                    .as_ref()
                    .is_some_and(|plan| Some(plan.generation) == *failure)
            {
                *failure = None;
                return Err(PrivilegedError::new(
                    "injected committed journal fsync failure",
                ));
            }
            *self.state.lock().unwrap() = Some(state.clone());
            Ok(())
        }

        fn clear(&mut self) -> Result<(), PrivilegedError> {
            *self.state.lock().unwrap() = None;
            Ok(())
        }
    }

    #[test]
    fn pending_commit_failure_restores_exact_previous_state() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let mut backend = CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(&kernel),
            },
            store.clone(),
        );
        let previous = fixture_valid_plan();
        backend.apply(&previous).unwrap();
        let journal_before = store.state.lock().unwrap().clone();
        let nft_before = super::live_nft_fingerprint(&mut backend.executor).unwrap();
        let mut candidate = previous.clone();
        candidate.generation = 2;
        candidate.rules[0].target = CompiledTarget::Network("198.51.100.0/24".parse().unwrap());
        *store.fail_commit_generation.lock().unwrap() = Some(2);
        let error = backend.apply(&candidate).unwrap_err();
        assert!(error.to_string().contains("state commit failed"));
        assert!(error.to_string().contains("rollback: ok"));
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        assert_eq!(
            super::live_nft_fingerprint(&mut backend.executor).unwrap(),
            nft_before
        );
        assert_eq!(backend.inspect().unwrap().applied_plan, Some(previous));
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
                orphan_cleanup: None,
                ..StoredAppliedState::default()
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
                orphan_cleanup: None,
                ..StoredAppliedState::default()
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
                    orphan_cleanup: None,
                    ..StoredAppliedState::default()
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
                orphan_cleanup: None,
                ..StoredAppliedState::default()
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
                orphan_cleanup: None,
                ..StoredAppliedState::default()
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
                orphan_cleanup: None,
                ..StoredAppliedState::default()
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
            orphan_cleanup: None,
            ..StoredAppliedState::default()
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

        fn recover_orphaned(&mut self) -> Result<OrphanRecoveryResponse, PrivilegedError> {
            self.events.lock().unwrap().push("recover_orphaned");
            Ok(OrphanRecoveryResponse {
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

        fn recover_orphaned(&mut self) -> Result<OrphanRecoveryResponse, PrivilegedError> {
            Ok(OrphanRecoveryResponse {
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
    async fn orphan_recovery_requires_mutation_authorization() {
        let (service, events) = fixture_service(Err(AuthorizationError::Denied));

        let error = service
            .recover_orphaned_resources(Some(":1.42"))
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

    #[derive(Default)]
    struct FakeRpfState {
        identity: Option<crate::backend::InterfaceIdentity>,
        src_valid_mark: u8,
        writes: Vec<u8>,
    }

    #[derive(Clone, Default)]
    struct FakeRpf(Arc<Mutex<FakeRpfState>>);

    impl FakeRpf {
        fn strict(identity: &crate::backend::InterfaceIdentity) -> Self {
            Self(Arc::new(Mutex::new(FakeRpfState {
                identity: Some(identity.clone()),
                ..FakeRpfState::default()
            })))
        }
        fn value(&self) -> u8 {
            self.0.lock().unwrap().src_valid_mark
        }
        fn writes(&self) -> Vec<u8> {
            self.0.lock().unwrap().writes.clone()
        }
    }

    impl super::MarkAwareRpfBackend for FakeRpf {
        fn identity(
            &mut self,
            name: &str,
        ) -> Result<crate::backend::InterfaceIdentity, PrivilegedError> {
            self.0
                .lock()
                .unwrap()
                .identity
                .clone()
                .filter(|identity| identity.name == name)
                .ok_or_else(|| PrivilegedError::new("interface no longer exists"))
        }
        fn rp_filter(&mut self, _: &str) -> Result<u8, PrivilegedError> {
            Ok(1)
        }
        fn src_valid_mark(&mut self, name: &str) -> Result<u8, PrivilegedError> {
            Ok(if name == "all" {
                0
            } else {
                self.0.lock().unwrap().src_valid_mark
            })
        }
        fn set_src_valid_mark(
            &mut self,
            identity: &crate::backend::InterfaceIdentity,
            value: u8,
        ) -> Result<(), PrivilegedError> {
            assert_eq!(identity.name, "wg0", "only the lane interface may change");
            let mut state = self.0.lock().unwrap();
            state.src_valid_mark = value;
            state.writes.push(value);
            Ok(())
        }
    }

    fn lane_identity() -> crate::backend::InterfaceIdentity {
        crate::backend::InterfaceIdentity {
            ifindex: 7,
            name: "wg0".to_owned(),
            kind: "wireguard".to_owned(),
            hardware_address: None,
        }
    }

    fn identified_plan() -> KernelPlan {
        let mut plan = fixture_valid_plan();
        plan.lanes[0].identity = Some(lane_identity());
        plan
    }

    fn rpf_backend(
        kernel: &Arc<Mutex<FakeKernelState>>,
        store: &FakeAppliedStateStore,
        rpf: &FakeRpf,
    ) -> CommandPrivilegedBackend<FakeCommandExecutor, FakeAppliedStateStore> {
        CommandPrivilegedBackend::with_store(
            FakeCommandExecutor {
                state: Arc::clone(kernel),
            },
            store.clone(),
        )
        .with_interface_settings(rpf.clone())
    }

    #[test]
    fn interface_setting_receipt_is_committed_then_restored_by_cleanup() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let rpf = FakeRpf::strict(&lane_identity());
        let mut backend = rpf_backend(&kernel, &store, &rpf);

        backend.apply(&identified_plan()).unwrap();
        assert_eq!(rpf.value(), 1);
        let journal = store.state.lock().unwrap().clone().unwrap();
        assert_eq!(journal.interface_settings.len(), 1);
        assert_eq!(journal.interface_settings[0].identity, lane_identity());
        assert!(journal.pending_interface_settings.is_empty());

        let mut second = identified_plan();
        second.generation = 2;
        backend.apply(&second).unwrap();
        assert_eq!(rpf.writes(), vec![1], "an owned setting is not rewritten");

        backend.cleanup().unwrap();
        assert_eq!(rpf.value(), 0);
        assert_eq!(rpf.writes(), vec![1, 0]);
        assert!(store.state.lock().unwrap().is_none());
    }

    #[test]
    fn cleanup_reports_replaced_interface_without_writing_its_setting() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let rpf = FakeRpf::strict(&lane_identity());
        let mut backend = rpf_backend(&kernel, &store, &rpf);
        backend.apply(&identified_plan()).unwrap();
        // Another device now carries the name; its value is not ours to restore.
        rpf.0.lock().unwrap().identity.as_mut().unwrap().ifindex = 9;

        let cleaned = backend.cleanup().unwrap();
        assert!(cleaned.cleaned);
        assert!(cleaned
            .diagnostics
            .iter()
            .any(|line| line.contains("wg0 src_valid_mark left unchanged")));
        assert_eq!(rpf.writes(), vec![1]);
        assert_eq!(rpf.value(), 1);
        assert!(store.state.lock().unwrap().is_none());
    }

    #[test]
    fn failed_activation_restores_journaled_interface_setting() {
        let kernel = Arc::new(Mutex::new(FakeKernelState {
            fail_activation: true,
            ..FakeKernelState::default()
        }));
        let store = FakeAppliedStateStore::default();
        let rpf = FakeRpf::strict(&lane_identity());
        let mut backend = rpf_backend(&kernel, &store, &rpf);

        assert!(backend.apply(&identified_plan()).is_err());
        assert_eq!(rpf.value(), 0);
        assert_eq!(rpf.writes(), vec![1, 0]);
        assert!(store
            .state
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|journal| journal.interface_settings.is_empty()
                && journal.pending_interface_settings.is_empty()));
    }

    #[test]
    fn setting_journal_failure_means_zero_setting_writes() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        *store.fail_setting_journal.lock().unwrap() = true;
        let rpf = FakeRpf::strict(&lane_identity());
        let mut backend = rpf_backend(&kernel, &store, &rpf);

        let error = backend.apply(&identified_plan()).unwrap_err();
        assert!(error.to_string().contains("setting journal"));
        assert_eq!(rpf.writes(), Vec::<u8>::new());
        let kernel = kernel.lock().unwrap();
        assert!(!kernel.has_table && !kernel.has_route && !kernel.has_rule);
    }

    #[test]
    fn commit_failure_restores_interface_setting_and_previous_state() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let rpf = FakeRpf::strict(&lane_identity());
        let mut backend = rpf_backend(&kernel, &store, &rpf);
        let previous = fixture_valid_plan();
        backend.apply(&previous).unwrap();
        let journal_before = store.state.lock().unwrap().clone();
        let mut candidate = identified_plan();
        candidate.generation = 2;
        *store.fail_commit_generation.lock().unwrap() = Some(2);

        let error = backend.apply(&candidate).unwrap_err();
        assert!(error.to_string().contains("state commit failed"));
        assert_eq!(rpf.value(), 0);
        assert_eq!(*store.state.lock().unwrap(), journal_before);
        assert_eq!(backend.inspect().unwrap().applied_plan, Some(previous));
    }

    #[test]
    fn interrupted_setting_write_is_restored_by_recovery() {
        let kernel = Arc::new(Mutex::new(FakeKernelState::default()));
        let store = FakeAppliedStateStore::default();
        let rpf = FakeRpf::strict(&lane_identity());
        let plan = identified_plan();
        let receipt = InterfaceSettingReceipt {
            identity: lane_identity(),
            previous: 0,
            applied: 1,
        };
        // Crash after the journaled write, before any kernel mutation.
        rpf.0.lock().unwrap().src_valid_mark = 1;
        *store.state.lock().unwrap() = Some(StoredAppliedState {
            pending_expectations: Some(build_pending_expectations(None, &plan).unwrap()),
            pending: Some(plan),
            pending_interface_settings: vec![receipt],
            ..StoredAppliedState::default()
        });
        let mut backend = rpf_backend(&kernel, &store, &rpf);

        backend.cleanup().unwrap();
        assert_eq!(rpf.value(), 0);
        assert!(store.state.lock().unwrap().is_none());
    }

    fn legacy_plan() -> crate::backend::legacy_journal::LegacyPlanV1 {
        use crate::backend::legacy_journal::{LegacyAvailabilityV1, LegacyLaneV1, LegacyPlanV1};
        LegacyPlanV1 {
            generation: 3,
            profile_id: ProfileId::from("default"),
            lanes: vec![LegacyLaneV1 {
                id: LaneId::from("privacy"),
                interface: "wg0".to_owned(),
                route_v4: Some(RouteMode::Via("10.0.0.1".parse().unwrap())),
                route_v6: None,
                availability: LegacyAvailabilityV1::Available,
                table: TABLE_BASE,
                mark: MARK_BASE,
                rule_priority: RULE_PRIORITY_BASE,
                failure_policy: LaneFailurePolicy::Inactive,
            }],
            rules: vec![],
        }
    }

    fn legacy_backend(
        tamper: bool,
    ) -> (
        Arc<Mutex<FakeKernelState>>,
        FakeAppliedStateStore,
        CommandPrivilegedBackend<FakeCommandExecutor, FakeAppliedStateStore>,
    ) {
        let legacy = legacy_plan();
        let kernel = Arc::new(Mutex::new(FakeKernelState {
            has_route: true,
            has_rule: true,
            has_table: true,
            marker: Some(legacy.state_chain_name()),
            ..FakeKernelState::default()
        }));
        let mut executor = FakeCommandExecutor {
            state: Arc::clone(&kernel),
        };
        let document = super::read_nft_table_once(&mut executor).unwrap();
        let fingerprint = crate::backend::legacy_journal::legacy_nft_fingerprint(&document);
        kernel.lock().unwrap().output_tampered = tamper;
        kernel.lock().unwrap().commands.clear();
        let unversioned = serde_json::json!({
            "active": legacy,
            "active_nft_fingerprint": fingerprint,
            "pending": null,
            "pending_expectations": null,
            "cleanup_pending": false,
            "restore_receipt": null,
        });
        let store = FakeAppliedStateStore::default();
        *store.state.lock().unwrap() =
            Some(super::decode_journal(&serde_json::to_vec(&unversioned).unwrap()).unwrap());
        let backend = CommandPrivilegedBackend::with_store(executor, store.clone());
        (kernel, store, backend)
    }

    #[test]
    fn old_journal_can_clean_but_cannot_arm_runtime() {
        let (kernel, store, mut backend) = legacy_backend(false);

        let error = backend.apply(&fixture_valid_plan()).unwrap_err();
        assert!(error.to_string().contains("cleanup-only"));
        assert_eq!(mutation_count(&kernel.lock().unwrap()), 0);
        let inspection = backend.inspect().unwrap();
        assert_eq!(inspection.applied_plan, None);
        assert!(inspection
            .diagnostics
            .iter()
            .any(|line| line.contains("schema-1")));
        assert!(backend.recover_orphaned().is_err());

        assert!(backend.cleanup().unwrap().cleaned);
        let kernel = kernel.lock().unwrap();
        assert!(!kernel.has_route && !kernel.has_rule && !kernel.has_table);
        assert!(store.state.lock().unwrap().is_none());
    }

    #[test]
    fn changed_legacy_table_is_not_cleaned() {
        let (kernel, store, mut backend) = legacy_backend(true);
        assert!(backend.cleanup().is_err());
        assert_eq!(mutation_count(&kernel.lock().unwrap()), 0);
        assert!(store.state.lock().unwrap().is_some());
    }

    #[test]
    fn journal_versions_are_explicit_and_strict() {
        let current = serde_json::to_vec(&StoredAppliedState::default()).unwrap();
        assert!(String::from_utf8_lossy(&current).contains("\"journal_version\":2"));
        assert_eq!(
            super::decode_journal(&current).unwrap(),
            StoredAppliedState::default()
        );
        let future =
            br#"{"journal_version":3,"active":null,"active_nft_fingerprint":null,"pending":null}"#;
        assert!(super::decode_journal(future).is_err());
        // An unversioned record carrying current-schema fields is not schema 1.
        let mut plan = serde_json::to_value(fixture_valid_plan()).unwrap();
        plan["lanes"][0]["identity"] = serde_json::Value::Null;
        let mixed = serde_json::json!({
            "active": plan,
            "active_nft_fingerprint": "x",
            "pending": null,
        });
        assert!(super::decode_journal(&serde_json::to_vec(&mixed).unwrap()).is_err());
    }
}
