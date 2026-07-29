use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::domain::LaneId;

use super::{
    apply_plan as apply_kernel_plan, build_policy_commands, build_snapshot_commands,
    detect_resource_conflicts, detect_resource_conflicts_for_update, render_ruleset,
    AppliedKernelState, CommandSpec, CompiledAction, IpFamily, KernelPlan, KernelRunner,
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
        let mut command = Command::new(invocation.program);
        command
            .args(&invocation.args)
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
        if let Some(input) = invocation.stdin {
            child
                .stdin
                .as_mut()
                .ok_or_else(|| RunnerError::new("typed command stdin pipe is unavailable"))?
                .write_all(input)
                .map_err(|error| {
                    RunnerError::new(format!("failed to write typed command stdin: {error}"))
                })?;
        }
        let output = child.wait_with_output().map_err(|error| {
            RunnerError::new(format!("failed to wait for typed command: {error}"))
        })?;

        Ok(CommandOutput {
            success: output.status.success(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredAppliedState {
    pub active: Option<KernelPlan>,
    pub active_nft_fingerprint: Option<String>,
    pub pending: Option<KernelPlan>,
    #[serde(default)]
    pub cleanup_pending: bool,
}

impl StoredAppliedState {
    fn committed(active: Option<KernelPlan>, active_nft_fingerprint: Option<String>) -> Self {
        Self {
            active,
            active_nft_fingerprint,
            pending: None,
            cleanup_pending: false,
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
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
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
        let previous = self.current.clone();
        let previous_nft_fingerprint = self.current_nft_fingerprint.clone();
        preflight_plan(&mut self.executor, previous.as_ref(), plan)?;
        self.store.save(&StoredAppliedState {
            active: previous.as_ref().map(|state| state.plan().clone()),
            active_nft_fingerprint: previous_nft_fingerprint.clone(),
            pending: Some(plan.clone()),
            cleanup_pending: false,
        })?;

        let mut runner = CommandKernelRunner::new(&mut self.executor, previous.clone());
        let applied = match apply_kernel_plan(&mut runner, previous.as_ref(), plan) {
            Ok(applied) => applied,
            Err(error) => {
                let persistence = persist_committed(
                    &mut self.store,
                    previous.as_ref(),
                    previous_nft_fingerprint.as_deref(),
                );
                return match persistence {
                    Ok(()) => Err(PrivilegedError::new(error.to_string())),
                    Err(persistence) => Err(PrivilegedError::new(format!(
                        "{error}; failed to clear pending state: {persistence}"
                    ))),
                };
            }
        };

        let applied_nft_fingerprint = match live_nft_fingerprint(&mut *runner.executor) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                let rollback = runner.restore(previous.as_ref());
                let state_repair = persist_committed(
                    &mut self.store,
                    previous.as_ref(),
                    previous_nft_fingerprint.as_deref(),
                );
                return Err(PrivilegedError::new(format!(
                    "kernel apply completed but exact nftables state could not be recorded: {error}; rollback: {}; state repair: {}",
                    result_label(&rollback),
                    result_label(&state_repair),
                )));
            }
        };

        if let Err(persistence) = persist_committed(
            &mut self.store,
            Some(&applied),
            Some(&applied_nft_fingerprint),
        ) {
            let rollback = runner.restore(previous.as_ref());
            let state_repair = persist_committed(
                &mut self.store,
                previous.as_ref(),
                previous_nft_fingerprint.as_deref(),
            );
            return Err(PrivilegedError::new(format!(
                "kernel apply completed but state commit failed: {persistence}; rollback: {}; state repair: {}",
                result_label(&rollback),
                result_label(&state_repair),
            )));
        }

        self.current = Some(applied);
        self.current_nft_fingerprint = Some(applied_nft_fingerprint);
        Ok(ApplyResponse::from_plan(plan, vec![]))
    }

    fn inspect(&mut self) -> Result<InspectResponse, PrivilegedError> {
        self.ensure_reconciled()?;
        let captured = capture_snapshot(&mut self.executor)
            .map_err(|error| PrivilegedError::new(error.to_string()))?;
        Ok(InspectResponse {
            generation: self.current.as_ref().map(|state| state.generation),
            plan_hash: self.current.as_ref().map(|state| state.plan_hash),
            routelane_table_present: captured.snapshot.has_nft_table("inet", "routelane"),
            diagnostics: vec![],
        })
    }

    fn cleanup(&mut self) -> Result<CleanupResponse, PrivilegedError> {
        self.ensure_reconciled()?;
        let Some(current) = self.current.clone() else {
            self.store.clear()?;
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
            pending: None,
            cleanup_pending: true,
        })?;
        let mut runner = CommandKernelRunner::new(&mut self.executor, Some(current));
        runner
            .snapshot()
            .and_then(|_| runner.restore(None))
            .map_err(|error| PrivilegedError::new(error.to_string()))?;
        self.store.clear()?;
        self.current = None;
        self.current_nft_fingerprint = None;
        Ok(CleanupResponse {
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
    fn ensure_reconciled(&mut self) -> Result<(), PrivilegedError> {
        if self.initialized {
            return Ok(());
        }

        let Some(record) = self.store.load()? else {
            self.initialized = true;
            return Ok(());
        };
        validate_stored_plan(record.active.as_ref(), "active")?;
        validate_stored_plan(record.pending.as_ref(), "pending")?;
        validate_stored_fingerprint_pair(
            record.active.as_ref(),
            record.active_nft_fingerprint.as_deref(),
        )?;

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
            self.store.clear()?;
            self.current = None;
            self.current_nft_fingerprint = None;
            self.initialized = true;
            return Ok(());
        }

        if let Some(pending) = &record.pending {
            verify_pending_table_ownership(
                &mut self.executor,
                active.as_ref(),
                record.active_nft_fingerprint.as_deref(),
                pending,
            )?;
            let mut runner = CommandKernelRunner::new(&mut self.executor, active.clone());
            runner.next = Some(pending.clone());
            runner
                .snapshot()
                .and_then(|_| runner.restore(active.as_ref()))
                .map_err(|error| {
                    PrivilegedError::new(format!("recover pending kernel transaction: {error}"))
                })?;
            let restored_nft_fingerprint = match active.as_ref() {
                Some(_) => Some(
                    live_nft_fingerprint(&mut self.executor)
                        .map_err(|error| PrivilegedError::new(error.to_string()))?,
                ),
                None => None,
            };
            persist_committed(
                &mut self.store,
                active.as_ref(),
                restored_nft_fingerprint.as_deref(),
            )?;
            self.current = active;
            self.current_nft_fingerprint = restored_nft_fingerprint;
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
                let mut runner = CommandKernelRunner::new(&mut self.executor, Some(active.clone()));
                runner
                    .snapshot()
                    .and_then(|_| runner.restore(Some(&active)))
                    .map_err(|error| {
                        PrivilegedError::new(format!(
                            "restore incomplete persisted generation: {error}"
                        ))
                    })?;
            }
            let live_fingerprint = live_nft_fingerprint(&mut self.executor)
                .map_err(|error| PrivilegedError::new(error.to_string()))?;
            self.current = Some(active);
            self.current_nft_fingerprint = Some(live_fingerprint);
        } else {
            self.store.clear()?;
            self.current_nft_fingerprint = None;
        }

        self.initialized = true;
        Ok(())
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

fn persist_committed(
    store: &mut impl AppliedStateStore,
    state: Option<&AppliedKernelState>,
    nft_fingerprint: Option<&str>,
) -> Result<(), PrivilegedError> {
    match (state, nft_fingerprint) {
        (Some(state), Some(fingerprint)) => store.save(&StoredAppliedState::committed(
            Some(state.plan().clone()),
            Some(fingerprint.to_owned()),
        )),
        (None, None) => store.clear(),
        _ => Err(PrivilegedError::new(
            "cannot persist an active plan without its exact nftables fingerprint",
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
        if let Some(gateway) = lane.gateway_v4 {
            if route_present(
                &ipv4_routes,
                lane.table,
                &gateway.to_string(),
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
        if let Some(gateway) = lane.gateway_v6 {
            if route_present(
                &ipv6_routes,
                lane.table,
                &gateway.to_string(),
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
        let mut batch = String::new();
        if !self.table_present {
            batch.push_str("add table inet routelane\n");
        }
        batch.push_str(&transaction_guard_batch(next));
        run_nft_batch(
            self.executor,
            batch.into_bytes(),
            "install temporary RouteLane guard",
        )?;
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

fn guard_batch() -> String {
    format!(
        "add chain inet routelane guard {{ type route hook output priority mangle + 1; policy accept; }}\n\
         add rule inet routelane guard meta mark >= 0x{MARK_BASE:08x} meta mark <= 0x{MARK_MAX:08x} reject\n"
    )
}

fn transaction_guard_batch(pending: &KernelPlan) -> String {
    format!(
        "add chain inet routelane {}\n{}",
        transaction_chain_name(pending),
        guard_batch()
    )
}

fn render_guarded_ruleset(plan: &KernelPlan) -> Result<Vec<u8>, RunnerError> {
    render_owned_ruleset(plan, TableMode::Replace, true)
}

fn render_owned_ruleset(
    plan: &KernelPlan,
    mode: TableMode,
    include_guard: bool,
) -> Result<Vec<u8>, RunnerError> {
    let rendered =
        render_ruleset(plan, mode).map_err(|error| RunnerError::new(error.to_string()))?;
    let table_line = "add table inet routelane\n";
    if !rendered.contains(table_line) {
        return Err(RunnerError::new(
            "generated nftables ruleset omitted fixed RouteLane table",
        ));
    }
    let marker = format!("add chain inet routelane {}\n", state_chain_name(plan));
    let guard = include_guard.then(guard_batch).unwrap_or_default();
    Ok(rendered
        .replacen(table_line, &format!("{table_line}{marker}{guard}"), 1)
        .into_bytes())
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
    let command = CommandSpec {
        program: Program::Nft,
        family: None,
        args: vec!["-f".to_owned(), "-".to_owned()],
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
        if let Some(gateway) = lane.gateway_v4 {
            if !route_present(
                &ipv4_routes,
                lane.table,
                &gateway.to_string(),
                &lane.interface,
            ) || !rule_present(&ipv4_rules, lane.rule_priority, lane.table, lane.mark)
            {
                return Ok(false);
            }
        }
        if let Some(gateway) = lane.gateway_v6 {
            if !route_present(
                &ipv6_routes,
                lane.table,
                &gateway.to_string(),
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
    if let Some(active) = active {
        detect_resource_conflicts(active.plan(), &captured.snapshot).map_err(|conflict| {
            PrivilegedError::new(format!(
                "cleanup journal conflicts with live kernel resources: {conflict:?}"
            ))
        })?;
    }
    if !captured.snapshot.has_nft_table("inet", "routelane") {
        return Ok(());
    }
    let (active, fingerprint) = active.zip(active_nft_fingerprint).ok_or_else(|| {
        PrivilegedError::new("cleanup journal cannot own a live inet/routelane table")
    })?;
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
) -> Result<(), PrivilegedError> {
    let captured =
        capture_snapshot(executor).map_err(|error| PrivilegedError::new(error.to_string()))?;
    match active {
        Some(active) => {
            detect_resource_conflicts_for_update(pending, active.plan(), &captured.snapshot)
        }
        None => detect_resource_conflicts(pending, &captured.snapshot),
    }
    .map_err(|conflict| {
        PrivilegedError::new(format!(
            "pending journal conflicts with live kernel resources: {conflict:?}"
        ))
    })?;
    if !captured.snapshot.has_nft_table("inet", "routelane") {
        return Ok(());
    }

    if let Some(active) = active {
        if nft_marker_present(executor, active.plan())
            .map_err(|error| PrivilegedError::new(error.to_string()))?
        {
            let expected = active_nft_fingerprint.ok_or_else(|| {
                PrivilegedError::new("pending journal is missing the active nftables fingerprint")
            })?;
            let live = live_nft_fingerprint(executor)
                .map_err(|error| PrivilegedError::new(error.to_string()))?;
            if live == expected {
                return Ok(());
            }
            if nft_chain_present(executor, &transaction_chain_name(pending))
                .map_err(|error| PrivilegedError::new(error.to_string()))?
            {
                return Ok(());
            }
            return Err(PrivilegedError::new(
                "pending journal active marker is present but the live inet/routelane table differs from the exact committed state",
            ));
        }
    }

    if nft_marker_present(executor, pending)
        .map_err(|error| PrivilegedError::new(error.to_string()))?
    {
        return Ok(());
    }
    if nft_chain_present(executor, &transaction_chain_name(pending))
        .map_err(|error| PrivilegedError::new(error.to_string()))?
    {
        return Ok(());
    }
    Err(PrivilegedError::new(
        "pending journal does not own the live inet/routelane table",
    ))
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
    let mut document = read_nft_table(executor)?;
    normalize_nft_document(&mut document);
    let canonical = serde_json::to_vec(&document)
        .map_err(|error| RunnerError::new(format!("encode canonical nftables table: {error}")))?;
    Ok(blake3::hash(&canonical).to_hex().to_string())
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
            if let Some(counter) = object
                .get_mut("counter")
                .and_then(serde_json::Value::as_object_mut)
            {
                counter.remove("packets");
                counter.remove("bytes");
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

fn route_present(routes: &[serde_json::Value], table: u32, gateway: &str, interface: &str) -> bool {
    routes.iter().any(|route| {
        route.get("dst").and_then(serde_json::Value::as_str) == Some("default")
            && json_u32(route.get("table")) == Some(table)
            && route.get("gateway").and_then(serde_json::Value::as_str) == Some(gateway)
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
        if lane.gateway_v4.is_some() && !plan_has_policy_rule(next, IpFamily::V4, lane) {
            delete_one_rule_if_present(executor, IpFamily::V4, lane)?;
        }
        if lane.gateway_v6.is_some() && !plan_has_policy_rule(next, IpFamily::V6, lane) {
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
                IpFamily::V4 => lane.gateway_v4.is_some(),
                IpFamily::V6 => lane.gateway_v6.is_some(),
            }
    })
}

fn plan_has_route(plan: &KernelPlan, family: IpFamily, expected: &super::LanePlan) -> bool {
    plan.lanes.iter().any(|lane| {
        lane.availability == super::LaneAvailability::Available
            && lane.table == expected.table
            && lane.interface == expected.interface
            && match family {
                IpFamily::V4 => lane.gateway_v4 == expected.gateway_v4,
                IpFamily::V6 => lane.gateway_v6 == expected.gateway_v6,
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
        if lane.gateway_v4.is_some() && !keys.contains(&ipv4) {
            keys.push(ipv4);
        }
        let ipv6 = (IpFamily::V6, lane.table);
        if lane.gateway_v6.is_some() && !keys.contains(&ipv6) {
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
            if lane.gateway_v4.is_some() {
                let rule = (IpFamily::V4, lane.rule_priority, lane.table, lane.mark);
                if !rules.contains(&rule) {
                    rules.push(rule);
                }
            }
            if lane.gateway_v6.is_some() {
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
                gateway_v4: None,
                gateway_v6: None,
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
            gateway_v4: None,
            gateway_v6: None,
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
    pub routelane_table_present: bool,
    pub diagnostics: Vec<String>,
}

impl InspectResponse {
    pub fn empty() -> Self {
        Self {
            generation: None,
            plan_hash: None,
            routelane_table_present: false,
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
    backend: Mutex<B>,
}

impl<A, B> PrivilegedService<A, B>
where
    A: Authorizer,
    B: PrivilegedBackend,
{
    pub fn new(authorizer: A, backend: B) -> Self {
        Self {
            authorizer,
            backend: Mutex::new(backend),
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

        self.backend
            .lock()
            .map_err(|_| ServiceError::StatePoisoned)?
            .apply(&plan)
            .map_err(|error| ServiceError::Backend(error.to_string()))
    }

    pub async fn inspect(&self, sender: Option<&str>) -> Result<InspectResponse, ServiceError> {
        self.authorize(sender).await?;
        self.backend
            .lock()
            .map_err(|_| ServiceError::StatePoisoned)?
            .inspect()
            .map_err(|error| ServiceError::Backend(error.to_string()))
    }

    pub async fn cleanup(&self, sender: Option<&str>) -> Result<CleanupResponse, ServiceError> {
        self.authorize(sender).await?;
        self.backend
            .lock()
            .map_err(|_| ServiceError::StatePoisoned)?
            .cleanup()
            .map_err(|error| ServiceError::Backend(error.to_string()))
    }

    async fn authorize<'a>(&self, sender: Option<&'a str>) -> Result<&'a str, ServiceError> {
        let sender = sender.ok_or(ServiceError::MissingSender)?;
        self.authorizer
            .authorize(sender)
            .await
            .map_err(ServiceError::Authorization)?;
        Ok(sender)
    }
}

#[cfg(test)]
mod tests {
    use std::future::{ready, Ready};
    use std::sync::{Arc, Mutex};

    use crate::domain::{LaneFailurePolicy, LaneId, ProfileId, RuleId};

    use super::{
        authorization_result, command_invocation, run_nft_batch, transaction_guard_batch,
        validate_privileged_plan, AppliedStateStore, ApplyResponse, AuthorizationError, Authorizer,
        CleanupResponse, CommandExecutor, CommandOutput, CommandPrivilegedBackend, InspectResponse,
        PrivilegedBackend, PrivilegedError, PrivilegedService, ServiceError, StoredAppliedState,
        ValidationError, IP_BINARY, MAX_PLAN_JSON_BYTES, NFT_BINARY,
    };
    use crate::backend::{
        CommandSpec, CompiledAction, CompiledRule, CompiledTarget, IpFamily, KernelPlan,
        LaneAvailability, LanePlan, Program, MARK_BASE, RULE_PRIORITY_BASE, TABLE_BASE,
    };

    fn fixture_lane(id: &str, offset: u32) -> LanePlan {
        LanePlan {
            id: LaneId::from(id),
            interface: format!("wg{offset}"),
            gateway_v4: Some(format!("10.0.{offset}.1").parse().unwrap()),
            gateway_v6: None,
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
        has_rule: bool,
        has_table: bool,
        marker: Option<String>,
        transaction_marker: Option<String>,
        has_guard: bool,
        output_tampered: bool,
        fail_activation: bool,
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
                        let body = if state.has_route && spec.family == Some(IpFamily::V4) {
                            serde_json::json!([{
                                "dst": "default",
                                "gateway": "10.0.0.1",
                                "dev": "wg0",
                                "table": 20_000,
                                "flags": [],
                            }])
                        } else {
                            serde_json::json!([])
                        };
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
                        state.has_route = false;
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
                Program::Nft if spec.args == ["-f", "-"] => {
                    let batch = std::str::from_utf8(spec.stdin.as_deref().unwrap()).unwrap();
                    if state.fail_activation && batch.contains("chain inet routelane output") {
                        return Ok(CommandOutput::failure("injected nft activation failure"));
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
                        state.marker = None;
                        state.transaction_marker = None;
                        state.has_guard = false;
                    }
                    if batch.contains("add table inet routelane") {
                        state.has_table = true;
                        state.has_guard = batch.contains("add chain inet routelane guard");
                        state.output_tampered = false;
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
                    .and_then(|batch| std::str::from_utf8(batch).ok())
                    .is_some_and(|batch| batch.contains("chain inet routelane output"))
        }));
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
                cleanup_pending: false,
            });
        }
        let mut crash_executor = FakeCommandExecutor {
            state: Arc::clone(&kernel),
        };
        run_nft_batch(
            &mut crash_executor,
            transaction_guard_batch(&pending).into_bytes(),
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
    fn restarted_backend_rejects_marker_present_but_tampered_nft_output() {
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
        let error = restarted.inspect().unwrap_err();

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
            cleanup_pending: false,
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

    #[tokio::test]
    async fn inspect_and_cleanup_are_authorized_before_backend_access() {
        let (denied, denied_events) = fixture_service(Err(AuthorizationError::Challenge));
        assert_eq!(
            denied.inspect(Some(":1.42")).await,
            Err(ServiceError::Authorization(AuthorizationError::Challenge))
        );
        assert_eq!(
            denied.cleanup(Some(":1.42")).await,
            Err(ServiceError::Authorization(AuthorizationError::Challenge))
        );
        assert_eq!(
            *denied_events.lock().unwrap(),
            vec!["authorize", "authorize"]
        );

        let (allowed, allowed_events) = fixture_service(Ok(()));
        allowed.inspect(Some(":1.42")).await.unwrap();
        allowed.cleanup(Some(":1.42")).await.unwrap();
        assert_eq!(
            *allowed_events.lock().unwrap(),
            vec!["authorize", "inspect", "authorize", "cleanup"]
        );
    }
}
