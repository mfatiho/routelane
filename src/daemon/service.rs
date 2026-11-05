use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::IpAddr;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::process::Command;
use tokio::sync::{broadcast, Mutex};

use crate::application::{matching_processes, RunningProcess};
use crate::backend::{
    policy_hash, AppliedKernelState, ApplyResponse, CleanupResponse, InspectResponse, KernelPlan,
    OrphanRecoveryResponse, PlanHash, ResolutionSnapshot, RestoreResponse, RuntimeActivation,
    RuntimeApplyResponse, RuntimePhase, RuntimeStatus, TopologySnapshot, RUNTIME_PROTOCOL_VERSION,
};
use crate::dns::periodic::resolve_domain;
use crate::domain::{
    normalize_lane_names, validate_profile, HealthState, Profile, ProfileId, RuleId, RuleSource,
    RuleTarget,
};
use crate::health::{
    evaluate_health, kernel_resources_match_plan, Diagnostic, DiagnosticCode, DiagnosticSubject,
    HealthReport, Severity, SuggestedAction,
};
use crate::ipc::{
    ApplicationAdoptionResult, ApplicationLaunchResult, ApplyProfileResult, DaemonEnvelope,
    DaemonEvent, DaemonRequest, DaemonResponse, DaemonResult, IpcError, OrphanRecoveryResult,
    IPC_PROTOCOL_VERSION,
};

use super::DaemonState;

const EVENT_CAPACITY: usize = 32;
const MAX_CONFIG_BYTES: u64 = 1_048_576;
const HELPER_BUS_NAME: &str = "io.github.routelane.Helper";
const HELPER_OBJECT_PATH: &str = "/io/github/routelane/Helper";
const HELPER_INTERFACE: &str = "io.github.routelane.Helper1";
const HELPER2_INTERFACE: &str = "io.github.routelane.Helper2";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonError {
    code: String,
    message: String,
}

impl DaemonError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn helper(message: impl Into<String>) -> Self {
        Self::new("DAEMON_HELPER_FAILED", message)
    }

    pub fn protocol_mismatch() -> Self {
        Self::new(
            "DAEMON_HELPER_PROTOCOL_MISMATCH",
            "the installed routelane-helper does not provide runtime protocol 2; install matching RouteLane daemon and helper versions",
        )
    }

    pub fn authorization(message: impl Into<String>) -> Self {
        Self::new("DAEMON_NEEDS_AUTHORIZATION", message)
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn validation(message: impl Into<String>) -> Self {
        Self::new("DAEMON_PROFILE_INVALID", message)
    }

    fn topology(message: impl Into<String>) -> Self {
        Self::new("DAEMON_TOPOLOGY_FAILED", message)
    }

    fn resolution(message: impl Into<String>) -> Self {
        Self::new("DAEMON_RESOLUTION_FAILED", message)
    }

    fn config(message: impl Into<String>) -> Self {
        Self::new("DAEMON_CONFIG_FAILED", message)
    }
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for DaemonError {}

impl From<DaemonError> for IpcError {
    fn from(error: DaemonError) -> Self {
        Self::new(error.code, error.message)
    }
}

#[async_trait]
pub trait TopologyProvider: Send + Sync {
    async fn snapshot(&self, profile: &Profile) -> Result<TopologySnapshot, DaemonError>;
}

#[async_trait]
pub trait ResolutionProvider: Send + Sync {
    async fn snapshot(&self, profile: &Profile) -> Result<ResolutionSnapshot, DaemonError>;
}

#[async_trait]
pub trait PrivilegedHelperClient: Send + Sync {
    async fn apply(&self, plan: &KernelPlan) -> Result<ApplyResponse, DaemonError>;
    async fn restore(
        &self,
        failed_candidate: &KernelPlan,
        previous: Option<&KernelPlan>,
    ) -> Result<RestoreResponse, DaemonError>;
    async fn inspect(&self) -> Result<InspectResponse, DaemonError>;
    async fn cleanup(&self) -> Result<CleanupResponse, DaemonError>;
    async fn recover_orphaned_resources(&self) -> Result<OrphanRecoveryResponse, DaemonError>;
    async fn prepare_application_cgroup(&self, _rule_id: &RuleId) -> Result<PathBuf, DaemonError> {
        Err(DaemonError::helper(
            "application cgroup operations are unavailable",
        ))
    }
    async fn adopt_application_process(
        &self,
        _rule_id: &RuleId,
        _process: &RunningProcess,
    ) -> Result<(), DaemonError> {
        Err(DaemonError::helper(
            "application cgroup operations are unavailable",
        ))
    }
    /// Arms helper-owned runtime authority (Helper2). A helper without it is
    /// an older, incompatible peer.
    async fn activate_runtime(
        &self,
        _activation: &RuntimeActivation,
    ) -> Result<RuntimeApplyResponse, DaemonError> {
        Err(DaemonError::protocol_mismatch())
    }
    async fn restore_runtime(&self, _receipt: &str) -> Result<RuntimeStatus, DaemonError> {
        Err(DaemonError::protocol_mismatch())
    }
}

pub trait ConfigRepository: Send + Sync {
    fn load(&self) -> Result<Option<Profile>, DaemonError>;
    fn save(&self, desired: Option<&Profile>) -> Result<(), DaemonError>;
}

pub struct DaemonService<T, R, H, C>
where
    T: TopologyProvider,
    R: ResolutionProvider,
    H: PrivilegedHelperClient,
    C: ConfigRepository,
{
    state: Mutex<DaemonState>,
    mutation: Mutex<()>,
    reconciliation: Mutex<ReconciliationGate>,
    reconciliation_attempts: AtomicU64,
    topology: T,
    resolutions: R,
    helper: H,
    config: C,
    events: broadcast::Sender<DaemonEvent>,
}

impl<T, R, H, C> DaemonService<T, R, H, C>
where
    T: TopologyProvider,
    R: ResolutionProvider,
    H: PrivilegedHelperClient,
    C: ConfigRepository,
{
    pub fn new(topology: T, resolutions: R, helper: H, config: C) -> Result<Self, DaemonError> {
        let desired = config.load()?;
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Ok(Self {
            state: Mutex::new(DaemonState::new(desired)),
            mutation: Mutex::new(()),
            reconciliation: Mutex::new(ReconciliationGate::default()),
            reconciliation_attempts: AtomicU64::new(0),
            topology,
            resolutions,
            helper,
            config,
            events,
        })
    }

    pub async fn state(&self) -> DaemonState {
        self.state.lock().await.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.events.subscribe()
    }

    pub async fn handle(&self, envelope: DaemonEnvelope) -> DaemonResponse {
        let request_id = envelope.request_id.clone();
        if envelope.protocol_version != IPC_PROTOCOL_VERSION {
            return DaemonResponse::failure(
                request_id,
                IpcError::new(
                    "IPC_UNSUPPORTED_PROTOCOL_VERSION",
                    format!(
                        "protocol version {} is unsupported; expected {IPC_PROTOCOL_VERSION}",
                        envelope.protocol_version
                    ),
                ),
            );
        }
        if envelope.request_id.is_empty() {
            return DaemonResponse::failure(
                request_id,
                IpcError::new("IPC_INVALID_REQUEST_ID", "request_id must not be empty"),
            );
        }

        let result = match envelope.request {
            DaemonRequest::GetStatus => Ok(DaemonResult::Status(self.current_status().await)),
            DaemonRequest::ListProfiles => {
                let _ = self.ensure_reconciled().await;
                let partial_profiles = self
                    .state
                    .lock()
                    .await
                    .desired
                    .clone()
                    .into_iter()
                    .collect();
                Ok(DaemonResult::Profiles(partial_profiles))
            }
            DaemonRequest::ApplyProfile(profile) => {
                self.apply_profile(profile).await.map(DaemonResult::Applied)
            }
            DaemonRequest::SetRuleEnabled {
                profile_id,
                rule_id,
                enabled,
            } => self
                .set_rule_enabled(profile_id, rule_id, enabled)
                .await
                .map(DaemonResult::Applied),
            DaemonRequest::LaunchApplication {
                profile_id,
                rule_id,
            } => self
                .launch_application(profile_id, rule_id)
                .await
                .map(DaemonResult::ApplicationLaunched),
            DaemonRequest::AdoptApplicationProcesses {
                profile_id,
                rule_id,
            } => self
                .adopt_application_processes(profile_id, rule_id)
                .await
                .map(DaemonResult::ApplicationProcessesAdopted),
            DaemonRequest::RecoverOrphanedResources => self
                .recover_orphaned_resources()
                .await
                .map(DaemonResult::OrphanedResourcesRecovered),
            DaemonRequest::Deactivate => {
                self.deactivate().await.map(|()| DaemonResult::Deactivated)
            }
            DaemonRequest::Subscribe => Ok(DaemonResult::Subscribed),
        };

        match result {
            Ok(result) => DaemonResponse::success(request_id, result),
            Err(error) => DaemonResponse::failure(request_id, error.into()),
        }
    }

    pub async fn launch_application(
        &self,
        profile_id: ProfileId,
        rule_id: RuleId,
    ) -> Result<ApplicationLaunchResult, DaemonError> {
        let _mutation = self.mutation.lock().await;
        self.ensure_reconciled().await.result?;
        let selector = self.application_selector(&profile_id, &rule_id).await?;
        let program = selector
            .launch
            .as_ref()
            .map_or(selector.executable_path.as_path(), |launch| {
                launch.executable.as_path()
            });
        let cgroup = self.helper.prepare_application_cgroup(&rule_id).await?;
        let control = cgroup.join("cgroup.procs");
        let mut command = Command::new(program);
        if let Some(launch) = &selector.launch {
            command.args(&launch.arguments);
        }
        unsafe {
            command.pre_exec(move || {
                let mut file = OpenOptions::new().write(true).open(&control)?;
                writeln!(file, "{}", libc::getpid())?;
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|error| DaemonError::helper(format!("start application: {error}")))?;
        let pid = child
            .id()
            .ok_or_else(|| DaemonError::helper("started application has no PID"))?;
        tokio::spawn(async move {
            if let Err(error) = child.wait().await {
                log::warn!("wait for launched application {pid}: {error}");
            }
        });
        Ok(ApplicationLaunchResult { pid })
    }

    pub async fn adopt_application_processes(
        &self,
        profile_id: ProfileId,
        rule_id: RuleId,
    ) -> Result<ApplicationAdoptionResult, DaemonError> {
        let _mutation = self.mutation.lock().await;
        self.ensure_reconciled().await.result?;
        let selector = self.application_selector(&profile_id, &rule_id).await?;
        let processes = matching_processes(&selector);
        let mut adopted_count = 0;
        let mut skipped_count = 0;
        for process in processes {
            match self
                .helper
                .adopt_application_process(&rule_id, &process)
                .await
            {
                Ok(()) => adopted_count += 1,
                Err(error) if error.code() == "DAEMON_NEEDS_AUTHORIZATION" => return Err(error),
                Err(_) => skipped_count += 1,
            }
        }
        Ok(ApplicationAdoptionResult {
            adopted_count,
            skipped_count,
        })
    }

    async fn application_selector(
        &self,
        profile_id: &ProfileId,
        rule_id: &RuleId,
    ) -> Result<crate::domain::ApplicationSelector, DaemonError> {
        let state = self.state.lock().await;
        if state
            .applied
            .as_ref()
            .is_none_or(|applied| &applied.plan().profile_id != profile_id)
        {
            return Err(DaemonError::new("DAEMON_APPLICATION_INACTIVE", "Enable the application's routing profile before launching or adopting its processes."));
        }
        let profile = state
            .applied_profile
            .as_ref()
            .filter(|profile| &profile.id == profile_id)
            .ok_or_else(|| DaemonError::validation("application profile is unavailable"))?;
        let rule = profile
            .rules
            .iter()
            .find(|rule| &rule.id == rule_id && rule.enabled)
            .ok_or_else(|| DaemonError::validation("application rule is unavailable"))?;
        let selector = match &rule.source {
            RuleSource::Application(selector) => selector.clone(),
            RuleSource::Any => {
                return Err(DaemonError::validation("rule is not an application rule"));
            }
        };
        drop(state);
        // Both launch and adoption must reject shared sandbox launchers. Matching
        // /usr/bin/snap or flatpak could otherwise select unrelated applications.
        for program in std::iter::once(&selector.executable_path)
            .chain(selector.launch.as_ref().map(|launch| &launch.executable))
        {
            let resolved = fs::canonicalize(program).map_err(|error| {
                DaemonError::validation(format!("application executable is unavailable: {error}"))
            })?;
            if matches!(
                resolved.file_name().and_then(|name| name.to_str()),
                Some("snap" | "flatpak")
            ) {
                return Err(DaemonError::new(
                    "DAEMON_APPLICATION_SANDBOX_UNSUPPORTED",
                    "Snap/Flatpak application routing is not supported. Select an application installed directly on the host.",
                ));
            }
        }
        Ok(selector)
    }

    pub async fn apply_profile(&self, profile: Profile) -> Result<ApplyProfileResult, DaemonError> {
        self.ensure_reconciled().await.result?;
        let _mutation = self.mutation.lock().await;
        self.apply_profile_locked(profile).await
    }

    async fn apply_profile_locked(
        &self,
        profile: Profile,
    ) -> Result<ApplyProfileResult, DaemonError> {
        if let Err(error) = validate_profile(&profile) {
            let error = DaemonError::validation(format!("{error:?}"));
            self.record_failure(error.clone()).await;
            return Err(error);
        }
        // Saved desired configuration is distinct from what becomes active.
        if let Err(error) = self.config.save(Some(&profile)) {
            self.record_failure(error.clone()).await;
            return Err(error);
        }

        let expected_generation = {
            let mut state = self.state.lock().await;
            state.desired = Some(profile.clone());
            state.health = HealthReport::applying();
            state.generation
        };
        let resolutions = match self.resolutions.snapshot(&profile).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.record_failure(error.clone()).await;
                return Err(error);
            }
        };
        let activation = RuntimeActivation {
            protocol_version: RUNTIME_PROTOCOL_VERSION,
            profile: profile.clone(),
            resolutions: resolutions.clone(),
            expected_generation,
        };
        // The helper captures topology, compiles and owns the plan and its
        // generation; the daemon never sends a preview compiled from older
        // evidence.
        let response = match self.helper.activate_runtime(&activation).await {
            Ok(response) => response,
            Err(error) => {
                self.record_failure(error.clone()).await;
                return Err(error);
            }
        };
        let expected_policy = policy_hash(&profile, &resolutions);
        let plan = response.plan;
        let matches_request = response
            .status
            .grant
            .as_ref()
            .is_some_and(|grant| grant.policy_hash == expected_policy)
            && plan.profile_id == profile.id
            && plan.generation > expected_generation
            && response.status.generation == plan.generation;
        if !matches_request {
            let error = DaemonError::new(
                "DAEMON_HELPER_RESPONSE_MISMATCH",
                "privileged helper armed a runtime that does not match the requested profile",
            );
            self.restore_runtime_receipt(&response.rollback_receipt)
                .await?;
            self.record_rolled_back_failure(error.clone()).await;
            return Err(error);
        }

        let verified = match self
            .inspect_runtime(Some(&profile), &plan, &expected_policy)
            .await
        {
            Ok(verified) if kernel_resources_match_plan(&verified.health) => verified,
            Ok(verified) => {
                let error = DaemonError::new(
                    "DAEMON_HEALTH_VERIFICATION_FAILED",
                    "live kernel state did not match the applied plan",
                );
                self.restore_runtime_receipt(&response.rollback_receipt)
                    .await?;
                self.record_verification_failure(error.clone(), verified.health)
                    .await;
                return Err(error);
            }
            Err(error) => {
                let error = if error.code() == "DAEMON_NEEDS_AUTHORIZATION" {
                    error
                } else {
                    DaemonError::new(
                        "DAEMON_HEALTH_INSPECTION_FAILED",
                        format!("could not inspect live kernel health: {}", error.message()),
                    )
                };
                self.restore_runtime_receipt(&response.rollback_receipt)
                    .await?;
                self.record_rolled_back_failure(error.clone()).await;
                return Err(error);
            }
        };

        let result = ApplyProfileResult {
            generation: plan.generation,
            plan_hash: plan.content_hash(),
        };
        {
            let mut state = self.state.lock().await;
            state.applied = Some(AppliedKernelState::from_plan(&verified.plan));
            state.applied_profile = Some(profile);
            state.generation = verified.plan.generation;
            state.health = verified.health.clone();
            state.primary = verified.runtime.clone();
            state.last_error = None;
            let sequence = advance_event_sequence(&mut state);
            let _ = self.events.send(DaemonEvent::StateChanged {
                sequence,
                generation: verified.plan.generation,
                health: verified.health,
            });
        }
        Ok(result)
    }

    /// Inspects and pairs the helper's state with a plan it just reported.
    /// A newer generation of the same grant (an automatic refresh in between)
    /// is accepted and verified instead of being treated as a mismatch.
    async fn inspect_runtime(
        &self,
        profile: Option<&Profile>,
        plan: &KernelPlan,
        expected_policy: &PlanHash,
    ) -> Result<VerifiedRuntime, DaemonError> {
        let inspected = self.helper.inspect().await?;
        let same_grant = inspected
            .runtime
            .grant
            .as_ref()
            .is_some_and(|grant| grant.policy_hash == *expected_policy);
        let live = inspected.applied_plan.clone().filter(|live| {
            Some(live.generation) == inspected.generation
                && Some(live.content_hash()) == inspected.plan_hash
                && inspected.runtime.generation == live.generation
                && (live == plan || (same_grant && live.generation > plan.generation))
        });
        let Some(live) = live else {
            return Err(DaemonError::new(
                "DAEMON_HELPER_RESPONSE_MISMATCH",
                "helper inspection does not pair with the reported runtime generation",
            ));
        };
        let snapshot = match profile {
            Some(profile) => {
                let topology = self.topology.snapshot(profile).await?;
                inspected.snapshot.with_topology(&topology)
            }
            None => inspected.snapshot,
        };
        Ok(VerifiedRuntime {
            health: evaluate_health(Some(&live), &snapshot),
            plan: live,
            runtime: inspected.runtime,
        })
    }

    async fn restore_runtime_receipt(&self, receipt: &str) -> Result<(), DaemonError> {
        if let Err(error) = self.helper.restore_runtime(receipt).await {
            // A newer helper commit makes the receipt ineligible; it is never
            // overwritten from here.
            let error = DaemonError::new(
                "DAEMON_KERNEL_ROLLBACK_FAILED",
                format!("failed to restore the previous runtime: {error}"),
            );
            self.record_kernel_restore_failure(error.clone()).await;
            return Err(error);
        }
        Ok(())
    }

    /// A helper `RuntimeChanged` announcement (or a resync after the helper
    /// restarted). Stale generations are ignored; the status is re-read and
    /// paired with Inspect before it is published.
    pub async fn on_runtime_changed(&self, announced: RuntimeStatus) {
        self.follow_runtime(Some(announced)).await;
    }

    /// Re-reads the helper after it (re)appeared on the bus or at startup.
    pub async fn resync_runtime(&self) {
        self.follow_runtime(None).await;
    }

    async fn follow_runtime(&self, announced: Option<RuntimeStatus>) {
        let _mutation = self.mutation.lock().await;
        let (profile, current_generation) = {
            let state = self.state.lock().await;
            (state.applied_profile.clone(), state.generation)
        };
        if announced
            .as_ref()
            .is_some_and(|status| status.generation < current_generation)
        {
            return;
        }
        let announced_policy = announced
            .as_ref()
            .map(|status| status.grant.as_ref().map(|grant| grant.policy_hash));
        let inspected = match self.helper.inspect().await {
            Ok(inspected) => inspected,
            Err(error) => {
                self.record_failure(DaemonError::new(
                    "DAEMON_HEALTH_INSPECTION_FAILED",
                    format!("could not inspect helper after a runtime change: {error}"),
                ))
                .await;
                return;
            }
        };
        let paired = inspected.applied_plan.clone().filter(|live| {
            Some(live.generation) == inspected.generation
                && Some(live.content_hash()) == inspected.plan_hash
                && inspected.runtime.generation == live.generation
                && announced_policy.is_none_or(|policy| {
                    inspected
                        .runtime
                        .grant
                        .as_ref()
                        .map(|grant| grant.policy_hash)
                        == policy
                })
        });
        let mut state = self.state.lock().await;
        if inspected.runtime.generation < state.generation {
            return;
        }
        let health = match &paired {
            Some(plan) => {
                let snapshot = match &profile {
                    Some(profile) => match self.topology.snapshot(profile).await {
                        Ok(topology) => inspected.snapshot.with_topology(&topology),
                        Err(_) => inspected.snapshot.clone(),
                    },
                    None => inspected.snapshot.clone(),
                };
                let mut health = evaluate_health(Some(plan), &snapshot);
                if inspected.runtime.phase != RuntimePhase::Armed
                    && health.state == HealthState::Healthy
                {
                    health = HealthReport::from_parts(
                        HealthState::Degraded,
                        health.lanes,
                        health.diagnostics,
                    );
                }
                state.applied = Some(AppliedKernelState::from_plan(plan));
                state.generation = plan.generation;
                health
            }
            None if inspected.applied_plan.is_none() && inspected.runtime.grant.is_none() => {
                state.applied = None;
                state.applied_profile = None;
                HealthReport::inactive()
            }
            None => HealthReport::from_parts(HealthState::Degraded, Default::default(), vec![]),
        };
        state.health = health;
        state.primary = inspected.runtime;
        let sequence = advance_event_sequence(&mut state);
        let _ = self.events.send(DaemonEvent::PrimaryChanged {
            sequence,
            generation: state.generation,
            primary: state.primary.clone(),
        });
    }

    pub async fn set_rule_enabled(
        &self,
        profile_id: ProfileId,
        rule_id: RuleId,
        enabled: bool,
    ) -> Result<ApplyProfileResult, DaemonError> {
        self.ensure_reconciled().await.result?;
        let _mutation = self.mutation.lock().await;
        let mut profile =
            self.state.lock().await.desired.clone().ok_or_else(|| {
                DaemonError::new("DAEMON_PROFILE_NOT_FOUND", "no profile is stored")
            })?;
        if profile.id != profile_id {
            return Err(DaemonError::new(
                "DAEMON_PROFILE_NOT_FOUND",
                "requested profile is not stored",
            ));
        }
        let rule = profile
            .rules
            .iter_mut()
            .find(|rule| rule.id == rule_id)
            .ok_or_else(|| {
                DaemonError::new("DAEMON_RULE_NOT_FOUND", "requested rule is not stored")
            })?;
        rule.enabled = enabled;
        self.apply_profile_locked(profile).await
    }

    pub async fn deactivate(&self) -> Result<(), DaemonError> {
        let _mutation = self.mutation.lock().await;
        if let Err(error) = self.ensure_reconciled().await.result {
            // A validated plan with missing live resources must remain removable.
            // The authorized helper still checks exact ownership before deleting.
            if error.code() != "DAEMON_LEGACY_CLEANUP_REQUIRED"
                && (error.code() != "DAEMON_RECONCILIATION_FAILED"
                    || self.state.lock().await.applied.is_none())
            {
                return Err(error);
            }
        }
        if let Err(error) = self.helper.cleanup().await {
            self.record_failure(error.clone()).await;
            return Err(error);
        }
        {
            let mut gate = self.reconciliation.lock().await;
            gate.completed = true;
            gate.last_result = Some(Ok(()));
        }
        {
            let mut state = self.state.lock().await;
            state.applied = None;
            state.applied_profile = None;
            // Cleanup removed the helper's current plan, so its next activation
            // expects generation zero. event_sequence still orders UI updates.
            state.generation = 0;
            state.health = HealthReport::inactive();
            state.primary = RuntimeStatus::default();
            state.last_error = None;
            let generation = state.generation;
            let sequence = advance_event_sequence(&mut state);
            let _ = self.events.send(DaemonEvent::StateChanged {
                sequence,
                generation,
                health: HealthReport::inactive(),
            });
        }
        Ok(())
    }

    pub async fn recover_orphaned_resources(&self) -> Result<OrphanRecoveryResult, DaemonError> {
        let _mutation = self.mutation.lock().await;
        let recoverable = self
            .state
            .lock()
            .await
            .last_error
            .as_ref()
            .is_some_and(|error| error.code == "DAEMON_ORPHANED_RESOURCES");
        if !recoverable {
            return Err(DaemonError::new(
                "DAEMON_ORPHAN_RECOVERY_UNAVAILABLE",
                "orphan recovery requires a detected orphaned-resource conflict",
            ));
        }
        let recovered = self.helper.recover_orphaned_resources().await?;
        {
            let mut gate = self.reconciliation.lock().await;
            *gate = ReconciliationGate::default();
            self.reconciliation_attempts.store(0, Ordering::Release);
        }
        {
            let mut state = self.state.lock().await;
            state.applied = None;
            state.applied_profile = None;
            state.generation = 0;
            state.last_error = None;
        }
        self.ensure_reconciled().await.result?;
        Ok(OrphanRecoveryResult {
            cleaned: recovered.cleaned,
        })
    }

    async fn record_failure(&self, error: DaemonError) {
        let ipc_error = IpcError::from(error.clone());
        {
            let mut state = self.state.lock().await;
            let health_state = if error.code == "DAEMON_NEEDS_AUTHORIZATION" {
                HealthState::NeedsAuthorization
            } else if state.applied.is_some() {
                HealthState::Degraded
            } else {
                HealthState::Failed
            };
            let health = HealthReport::from_parts(health_state, Default::default(), vec![]);
            state.health = health.clone();
            state.last_error = Some(ipc_error.clone());
            let generation = state.generation;
            let sequence = advance_event_sequence(&mut state);
            let _ = self.events.send(DaemonEvent::OperationFailed {
                sequence,
                generation,
                health,
                error: ipc_error,
            });
        }
    }

    async fn record_rolled_back_failure(&self, error: DaemonError) {
        let ipc_error = IpcError::from(error.clone());
        {
            let mut state = self.state.lock().await;
            let health_state = if error.code == "DAEMON_NEEDS_AUTHORIZATION" {
                HealthState::NeedsAuthorization
            } else if state.applied.is_some() {
                HealthState::Degraded
            } else {
                HealthState::Failed
            };
            let health = HealthReport::failure(
                health_state,
                DiagnosticCode::PolicyApplyRolledBack,
                DiagnosticSubject::Policy,
                "RouteLane restored the previous policy",
                format!("{}: {}", error.code, error.message),
                Some(SuggestedAction::new(
                    "Review the technical detail and retry the operation.",
                )),
            );
            state.health = health.clone();
            state.last_error = Some(ipc_error.clone());
            let generation = state.generation;
            let sequence = advance_event_sequence(&mut state);
            let _ = self.events.send(DaemonEvent::OperationFailed {
                sequence,
                generation,
                health,
                error: ipc_error,
            });
        }
    }

    async fn record_kernel_restore_failure(&self, error: DaemonError) {
        let ipc_error = IpcError::from(error);
        {
            let mut state = self.state.lock().await;
            let health = HealthReport::failure(
                HealthState::Failed,
                DiagnosticCode::RoutingResourceConflict,
                DiagnosticSubject::Policy,
                "RouteLane could not restore the previous kernel policy",
                ipc_error.message.clone(),
                Some(SuggestedAction::new(
                    "Stop policy changes and inspect privileged helper state.",
                )),
            );
            state.health = health.clone();
            state.last_error = Some(ipc_error.clone());
            let generation = state.generation;
            let sequence = advance_event_sequence(&mut state);
            let _ = self.events.send(DaemonEvent::OperationFailed {
                sequence,
                generation,
                health,
                error: ipc_error,
            });
        }
    }

    async fn record_verification_failure(&self, error: DaemonError, mut report: HealthReport) {
        let ipc_error = IpcError::from(error);
        {
            let mut state = self.state.lock().await;
            let health_state = if state.applied.is_some() {
                HealthState::Degraded
            } else {
                HealthState::Failed
            };
            report.state = health_state;
            report.diagnostics.push(Diagnostic::new(
                DiagnosticCode::PolicyApplyRolledBack,
                "Applied policy failed live verification",
                ipc_error.message.clone(),
                if health_state == HealthState::Failed {
                    Severity::Error
                } else {
                    Severity::Warning
                },
                DiagnosticSubject::Policy,
                Some(SuggestedAction::new(
                    "Reapply the profile after resolving the reported health diagnostics.",
                )),
            ));
            report = HealthReport::from_parts(report.state, report.lanes, report.diagnostics);
            state.health = report.clone();
            state.last_error = Some(ipc_error.clone());
            let generation = state.generation;
            let sequence = advance_event_sequence(&mut state);
            let _ = self.events.send(DaemonEvent::OperationFailed {
                sequence,
                generation,
                health: report,
                error: ipc_error,
            });
        }
    }

    async fn inspect_plan_health(
        &self,
        profile: Option<&Profile>,
        plan: &KernelPlan,
    ) -> Result<(HealthReport, RuntimeStatus), DaemonError> {
        let inspected = self.helper.inspect().await?;
        let mut runtime = inspected.runtime;
        // A runtime is reported Armed only for the exact plan the daemon verified.
        let unpaired = inspected.generation != Some(plan.generation)
            || inspected.plan_hash != Some(plan.content_hash())
            || runtime.generation != plan.generation;
        if unpaired && runtime.phase == RuntimePhase::Armed {
            runtime.phase = RuntimePhase::Degraded;
            runtime.last_error =
                Some("helper state does not pair with the verified applied plan".to_owned());
        }
        let snapshot = match profile {
            Some(profile) => {
                let topology = self.topology.snapshot(profile).await?;
                inspected.snapshot.with_topology(&topology)
            }
            None => inspected.snapshot,
        };
        Ok((evaluate_health(Some(plan), &snapshot), runtime))
    }

    async fn current_status(&self) -> crate::ipc::DaemonStatus {
        let reconciliation = self.ensure_reconciled().await;
        if reconciliation.first_wave || reconciliation.result.is_err() {
            return self.state.lock().await.status();
        }
        let (base, profile, plan) = {
            let state = self.state.lock().await;
            (
                state.clone(),
                state.applied_profile.clone(),
                state.applied.as_ref().map(|applied| applied.plan().clone()),
            )
        };
        let mut primary = base.primary.clone();
        let health = match plan.as_ref() {
            None => HealthReport::inactive(),
            Some(plan) => match self.inspect_plan_health(profile.as_ref(), plan).await {
                Ok((report, runtime)) => {
                    primary = runtime;
                    if primary.phase != RuntimePhase::Armed
                        && primary.grant.is_some()
                        && report.state == HealthState::Healthy
                    {
                        HealthReport::from_parts(
                            HealthState::Degraded,
                            report.lanes,
                            report.diagnostics,
                        )
                    } else {
                        report
                    }
                }
                Err(error) => {
                    let state = if error.code() == "DAEMON_NEEDS_AUTHORIZATION" {
                        HealthState::NeedsAuthorization
                    } else {
                        HealthState::Degraded
                    };
                    HealthReport::failure(
                        state,
                        DiagnosticCode::NftablesFeatureMissing,
                        DiagnosticSubject::HealthInspection,
                        "RouteLane could not inspect live policy health",
                        error.to_string(),
                        Some(SuggestedAction::new(
                            "Authorize or restore the privileged helper, then retry.",
                        )),
                    )
                }
            },
        };
        let mut status = base.status();
        status.health = health;
        status.primary = primary;
        status
    }

    async fn ensure_reconciled(&self) -> ReconciliationOutcome {
        let observed_attempts = self.reconciliation_attempts.load(Ordering::Acquire);
        let mut gate = self.reconciliation.lock().await;
        if gate.attempts > observed_attempts {
            return ReconciliationOutcome {
                result: gate
                    .last_result
                    .clone()
                    .expect("a completed reconciliation attempt has a result"),
                first_wave: true,
            };
        }
        if gate.completed {
            return ReconciliationOutcome {
                result: Ok(()),
                first_wave: false,
            };
        }

        let result = self.reconcile_from_helper().await;
        gate.attempts = gate
            .attempts
            .checked_add(1)
            .expect("reconciliation attempt counter exhausted");
        gate.completed = result.is_ok();
        gate.last_result = Some(result.clone());
        let completed_attempts = gate.attempts;
        drop(gate);
        self.reconciliation_attempts
            .store(completed_attempts, Ordering::Release);
        ReconciliationOutcome {
            result,
            first_wave: true,
        }
    }

    async fn reconcile_from_helper(&self) -> Result<(), DaemonError> {
        let inspected = match self.helper.inspect().await {
            Ok(inspected) => inspected,
            Err(error) => {
                let error = if error.code() == "DAEMON_NEEDS_AUTHORIZATION" {
                    error
                } else {
                    DaemonError::new(
                        "DAEMON_RECONCILIATION_FAILED",
                        format!(
                            "could not inspect persisted helper state: {}",
                            error.message()
                        ),
                    )
                };
                let health_state = if error.code() == "DAEMON_NEEDS_AUTHORIZATION" {
                    HealthState::NeedsAuthorization
                } else {
                    HealthState::Failed
                };
                let health = HealthReport::failure(
                    health_state,
                    DiagnosticCode::RoutingResourceConflict,
                    DiagnosticSubject::HealthInspection,
                    "RouteLane could not reconcile persisted routing state",
                    error.to_string(),
                    Some(SuggestedAction::new(
                        "Authorize or repair the privileged helper before changing policy.",
                    )),
                );
                let mut state = self.state.lock().await;
                state.health = health;
                state.last_error = Some(IpcError::from(error.clone()));
                return Err(error);
            }
        };

        if inspected.diagnostics.iter().any(|d| d.starts_with("DAEMON_LEGACY_CLEANUP_REQUIRED:")) {
            return self.record_reconciliation_drift_with_code(None,
                "DAEMON_LEGACY_CLEANUP_REQUIRED",
                "validated legacy RouteLane state requires authorized cleanup before activation").await;
        }
        let Some(plan) = inspected.applied_plan else {
            if inspected.generation.is_some()
                || inspected.plan_hash.is_some()
                || inspected.routelane_table_present
            {
                return self
                    .record_reconciliation_drift_with_code(
                        None,
                        "DAEMON_ORPHANED_RESOURCES",
                        "helper reported owned resources without a validated applied plan",
                    )
                    .await;
            }
            let mut state = self.state.lock().await;
            state.applied = None;
            state.applied_profile = None;
            state.generation = 0;
            state.health = HealthReport::inactive();
            state.primary = inspected.runtime;
            state.last_error = None;
            return Ok(());
        };

        if inspected.generation != Some(plan.generation)
            || inspected.plan_hash != Some(plan.content_hash())
        {
            return self
                .record_reconciliation_drift(
                    Some(&plan),
                    "helper applied-plan metadata does not match the validated plan",
                )
                .await;
        }

        let health = evaluate_health(Some(&plan), &inspected.snapshot);
        let applied_profile = {
            let state = self.state.lock().await;
            state
                .desired
                .as_ref()
                .filter(|profile| profile.id == plan.profile_id)
                .cloned()
        };
        {
            let mut state = self.state.lock().await;
            state.applied = Some(AppliedKernelState::from_plan(&plan));
            state.applied_profile = applied_profile;
            state.generation = plan.generation;
            state.health = health.clone();
            // Runtime authority comes only from the helper, never user config.
            state.primary = inspected.runtime;
            state.last_error = None;
        }
        if kernel_resources_match_plan(&health) {
            Ok(())
        } else {
            self.record_reconciliation_drift(
                Some(&plan),
                "persisted helper plan does not exactly match live kernel state",
            )
            .await
        }
    }

    async fn record_reconciliation_drift(
        &self,
        plan: Option<&KernelPlan>,
        detail: &str,
    ) -> Result<(), DaemonError> {
        self.record_reconciliation_drift_with_code(plan, "DAEMON_RECONCILIATION_FAILED", detail)
            .await
    }

    async fn record_reconciliation_drift_with_code(
        &self,
        plan: Option<&KernelPlan>,
        code: &str,
        detail: &str,
    ) -> Result<(), DaemonError> {
        let error = DaemonError::new(code, detail);
        let mut state = self.state.lock().await;
        if let Some(plan) = plan {
            state.applied = Some(AppliedKernelState::from_plan(plan));
            state.generation = plan.generation;
        }
        state.health = HealthReport::failure(
            HealthState::Failed,
            DiagnosticCode::RoutingResourceConflict,
            DiagnosticSubject::HealthInspection,
            "Persisted routing state differs from the live kernel",
            detail,
            Some(SuggestedAction::new(
                "Inspect RouteLane-owned routes and rules before applying another profile.",
            )),
        );
        state.last_error = Some(IpcError::from(error.clone()));
        Err(error)
    }
}

struct VerifiedRuntime {
    plan: KernelPlan,
    health: HealthReport,
    runtime: RuntimeStatus,
}

#[derive(Debug, Clone)]
struct ReconciliationOutcome {
    result: Result<(), DaemonError>,
    first_wave: bool,
}

#[derive(Debug, Default)]
struct ReconciliationGate {
    attempts: u64,
    completed: bool,
    last_result: Option<Result<(), DaemonError>>,
}

fn advance_event_sequence(state: &mut DaemonState) -> u64 {
    state.event_sequence = state
        .event_sequence
        .checked_add(1)
        .expect("daemon event sequence exhausted");
    state.event_sequence
}

#[derive(Debug, Clone, Default)]
pub struct SystemTopologyProvider;

#[async_trait]
impl TopologyProvider for SystemTopologyProvider {
    async fn snapshot(&self, _profile: &Profile) -> Result<TopologySnapshot, DaemonError> {
        tokio::task::spawn_blocking(crate::backend::capture_system_topology)
            .await
            .map_err(|error| {
                DaemonError::topology(format!("kernel topology worker failed: {error}"))
            })?
            .map_err(|error| DaemonError::topology(error.to_string()))
    }
}

#[derive(Debug, Clone, Default)]
pub struct SystemResolutionProvider;

#[async_trait]
impl ResolutionProvider for SystemResolutionProvider {
    async fn snapshot(&self, profile: &Profile) -> Result<ResolutionSnapshot, DaemonError> {
        let mut snapshot = ResolutionSnapshot::empty();
        for rule in profile.rules.iter().filter(|rule| rule.enabled) {
            let RuleTarget::Domain(domain) = &rule.target else {
                continue;
            };
            let addresses = resolve_domain(domain)
                .await
                .map_err(|error| DaemonError::resolution(error.to_string()))?
                .into_iter()
                .map(ip_host_network)
                .collect();
            snapshot.insert(rule.id.clone(), addresses);
        }
        Ok(snapshot)
    }
}

fn ip_host_network(address: IpAddr) -> ipnet::IpNet {
    ipnet::IpNet::from(address)
}

#[derive(Clone)]
pub struct SystemPrivilegedHelperClient {
    connection: zbus::Connection,
}

impl SystemPrivilegedHelperClient {
    pub async fn connect() -> Result<Self, DaemonError> {
        let connection = zbus::Connection::system()
            .await
            .map_err(|error| DaemonError::helper(format!("connect to system bus: {error}")))?;
        Ok(Self { connection })
    }

    async fn proxy(&self) -> Result<zbus::Proxy<'_>, DaemonError> {
        zbus::Proxy::new(
            &self.connection,
            HELPER_BUS_NAME,
            HELPER_OBJECT_PATH,
            HELPER_INTERFACE,
        )
        .await
        .map_err(|error| DaemonError::helper(format!("create helper proxy: {error}")))
    }

    async fn runtime_proxy(&self) -> Result<zbus::Proxy<'static>, DaemonError> {
        zbus::Proxy::new(
            &self.connection,
            HELPER_BUS_NAME,
            HELPER_OBJECT_PATH,
            HELPER2_INTERFACE,
        )
        .await
        .map_err(|error| DaemonError::helper(format!("create runtime helper proxy: {error}")))
    }

    /// Forwards helper `RuntimeChanged` announcements, and a resync whenever
    /// the helper (re)appears on the bus. Subscription happens before the
    /// caller takes its initial snapshot.
    /// `None` asks for a resync from Inspect (helper owner changed).
    pub async fn runtime_changes(
        &self,
    ) -> Result<tokio::sync::mpsc::Receiver<Option<RuntimeStatus>>, DaemonError> {
        let proxy = self.runtime_proxy().await?;
        let mut signals = proxy
            .receive_signal("RuntimeChanged")
            .await
            .map_err(|error| DaemonError::helper(format!("subscribe to helper: {error}")))?;
        let mut owners = proxy
            .receive_owner_changed()
            .await
            .map_err(|error| DaemonError::helper(format!("watch helper owner: {error}")))?;
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            loop {
                let change = tokio::select! {
                    message = next_item(&mut signals) => {
                        let Some(message) = message else { return };
                        let decoded = message
                            .body()
                            .deserialize::<String>()
                            .ok()
                            .and_then(|json| serde_json::from_str::<RuntimeStatus>(&json).ok());
                        match decoded {
                            Some(status) => Some(status),
                            None => continue,
                        }
                    }
                    owner = next_item(&mut owners) => {
                        if owner.is_none() { return }
                        // Resync from Inspect; a missing helper is not "inactive".
                        None
                    }
                };
                if sender.send(change).await.is_err() {
                    return;
                }
            }
        });
        Ok(receiver)
    }
}

#[async_trait]
impl PrivilegedHelperClient for SystemPrivilegedHelperClient {
    async fn activate_runtime(
        &self,
        activation: &RuntimeActivation,
    ) -> Result<RuntimeApplyResponse, DaemonError> {
        let activation_json = serde_json::to_string(activation)
            .map_err(|error| DaemonError::helper(format!("encode activation: {error}")))?;
        let proxy = self.runtime_proxy().await?;
        let response_json: String = proxy
            .call("ActivateRuntime", &(activation_json.as_str(),))
            .await
            .map_err(|error| runtime_call_error("ActivateRuntime", error))?;
        serde_json::from_str(&response_json).map_err(|error| {
            DaemonError::helper(format!("decode ActivateRuntime response: {error}"))
        })
    }

    async fn restore_runtime(&self, receipt: &str) -> Result<RuntimeStatus, DaemonError> {
        let proxy = self.runtime_proxy().await?;
        let response_json: String = proxy
            .call("RestoreRuntime", &(receipt,))
            .await
            .map_err(|error| runtime_call_error("RestoreRuntime", error))?;
        serde_json::from_str(&response_json).map_err(|error| {
            DaemonError::helper(format!("decode RestoreRuntime response: {error}"))
        })
    }

    async fn apply(&self, plan: &KernelPlan) -> Result<ApplyResponse, DaemonError> {
        let plan_json = serde_json::to_string(plan)
            .map_err(|error| DaemonError::helper(format!("encode kernel plan: {error}")))?;
        let proxy = self.proxy().await?;
        let response_json: String = proxy
            .call("ApplyPlan", &(plan_json.as_str(),))
            .await
            .map_err(|error| helper_call_error("ApplyPlan", error))?;
        serde_json::from_str(&response_json)
            .map_err(|error| DaemonError::helper(format!("decode ApplyPlan response: {error}")))
    }

    async fn restore(
        &self,
        failed_candidate: &KernelPlan,
        previous: Option<&KernelPlan>,
    ) -> Result<RestoreResponse, DaemonError> {
        let failed_candidate_json = serde_json::to_string(failed_candidate)
            .map_err(|error| DaemonError::helper(format!("encode failed candidate: {error}")))?;
        let previous_json = previous
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| DaemonError::helper(format!("encode previous plan: {error}")))?;
        let previous_json = previous_json.as_deref().unwrap_or("");
        let proxy = self.proxy().await?;
        let response_json: String = proxy
            .call(
                "RestorePlan",
                &(failed_candidate_json.as_str(), previous_json),
            )
            .await
            .map_err(|error| helper_call_error("RestorePlan", error))?;
        serde_json::from_str(&response_json)
            .map_err(|error| DaemonError::helper(format!("decode RestorePlan response: {error}")))
    }

    async fn inspect(&self) -> Result<InspectResponse, DaemonError> {
        let proxy = self.proxy().await?;
        let response_json: String = proxy
            .call("Inspect", &())
            .await
            .map_err(|error| helper_call_error("Inspect", error))?;
        serde_json::from_str(&response_json)
            .map_err(|error| DaemonError::helper(format!("decode Inspect response: {error}")))
    }

    async fn cleanup(&self) -> Result<CleanupResponse, DaemonError> {
        let proxy = self.proxy().await?;
        let response_json: String = proxy
            .call("Cleanup", &())
            .await
            .map_err(|error| helper_call_error("Cleanup", error))?;
        serde_json::from_str(&response_json)
            .map_err(|error| DaemonError::helper(format!("decode Cleanup response: {error}")))
    }

    async fn recover_orphaned_resources(&self) -> Result<OrphanRecoveryResponse, DaemonError> {
        let proxy = self.proxy().await?;
        let response_json: String = proxy
            .call("RecoverOrphanedResources", &())
            .await
            .map_err(|error| helper_call_error("RecoverOrphanedResources", error))?;
        serde_json::from_str(&response_json).map_err(|error| {
            DaemonError::helper(format!("decode RecoverOrphanedResources response: {error}"))
        })
    }

    async fn prepare_application_cgroup(&self, rule_id: &RuleId) -> Result<PathBuf, DaemonError> {
        let proxy = self.proxy().await?;
        let response_json: String = proxy
            .call("PrepareApplicationCgroup", &(rule_id.as_str(),))
            .await
            .map_err(|error| helper_call_error("PrepareApplicationCgroup", error))?;
        serde_json::from_str(&response_json).map_err(|error| {
            DaemonError::helper(format!("decode PrepareApplicationCgroup response: {error}"))
        })
    }

    async fn adopt_application_process(
        &self,
        rule_id: &RuleId,
        process: &RunningProcess,
    ) -> Result<(), DaemonError> {
        let proxy = self.proxy().await?;
        let _: String = proxy
            .call(
                "AdoptApplicationProcess",
                &(
                    rule_id.as_str(),
                    process.pid,
                    process.start_time,
                    process.executable_path.to_string_lossy().as_ref(),
                ),
            )
            .await
            .map_err(|error| helper_call_error("AdoptApplicationProcess", error))?;
        Ok(())
    }
}

/// `StreamExt::next` without a new dependency (zbus re-exports the trait).
async fn next_item<S>(stream: &mut S) -> Option<S::Item>
where
    S: zbus::export::futures_core::Stream + Unpin,
{
    std::future::poll_fn(|context| std::pin::Pin::new(&mut *stream).poll_next(context)).await
}

/// An older helper without Helper2 is an incompatible peer, not a transient
/// failure: say what to do.
fn runtime_call_error(operation: &str, error: zbus::Error) -> DaemonError {
    let unknown = match &error {
        zbus::Error::MethodError(name, _, _) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.UnknownMethod"
                | "org.freedesktop.DBus.Error.UnknownInterface"
                | "org.freedesktop.DBus.Error.UnknownObject"
        ),
        zbus::Error::FDO(error) => matches!(
            error.as_ref(),
            zbus::fdo::Error::UnknownMethod(_)
                | zbus::fdo::Error::UnknownInterface(_)
                | zbus::fdo::Error::UnknownObject(_)
        ),
        _ => false,
    };
    if unknown {
        DaemonError::protocol_mismatch()
    } else {
        helper_call_error(operation, error)
    }
}

fn helper_call_error(operation: &str, error: zbus::Error) -> DaemonError {
    let authorization_denied = match &error {
        zbus::Error::MethodError(name, _, _) => {
            name.as_str() == "org.freedesktop.DBus.Error.AccessDenied"
        }
        zbus::Error::FDO(error) => {
            matches!(error.as_ref(), zbus::fdo::Error::AccessDenied(_))
        }
        _ => false,
    };
    let message = format!("helper {operation} call: {error}");
    if authorization_denied {
        DaemonError::authorization(message)
    } else {
        DaemonError::helper(message)
    }
}

#[derive(Debug, Clone)]
pub struct AtomicJsonConfigRepository {
    path: PathBuf,
}

impl AtomicJsonConfigRepository {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn from_environment() -> Result<Self, DaemonError> {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(path) => PathBuf::from(path),
            None => {
                let home = std::env::var_os("HOME").ok_or_else(|| {
                    DaemonError::config("neither XDG_CONFIG_HOME nor HOME is set")
                })?;
                PathBuf::from(home).join(".config")
            }
        };
        Ok(Self::new(base.join("routelane/desired-profile.json")))
    }
}

impl ConfigRepository for AtomicJsonConfigRepository {
    fn load(&self) -> Result<Option<Profile>, DaemonError> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(DaemonError::config(format!(
                    "open {}: {error}",
                    self.path.display()
                )));
            }
        };
        let metadata = file
            .metadata()
            .map_err(|error| DaemonError::config(format!("inspect config file: {error}")))?;
        validate_owned_private_file(&metadata)?;
        if metadata.len() > MAX_CONFIG_BYTES {
            return Err(DaemonError::config("desired profile file is too large"));
        }
        let mut json = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut json)
            .map_err(|error| DaemonError::config(format!("read desired profile: {error}")))?;
        if json.len() as u64 > MAX_CONFIG_BYTES {
            return Err(DaemonError::config("desired profile file is too large"));
        }
        let mut desired = crate::config::decode_desired_profile(&json)
            .map_err(|error| DaemonError::config(format!("decode desired profile: {error}")))?;
        if let Some(profile) = &mut desired {
            normalize_lane_names(profile);
            validate_profile(profile).map_err(|error| {
                DaemonError::config(format!("validate desired profile: {error:?}"))
            })?;
        }
        Ok(desired)
    }

    fn save(&self, desired: Option<&Profile>) -> Result<(), DaemonError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| DaemonError::config("desired profile path has no parent"))?;
        ensure_owned_private_directory(parent)?;
        validate_existing_destination(&self.path)?;
        let json = crate::config::encode_desired_profile(desired)
            .map_err(|error| DaemonError::config(format!("encode desired profile: {error}")))?;
        if json.len() as u64 > MAX_CONFIG_BYTES {
            return Err(DaemonError::config("desired profile is too large"));
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| DaemonError::config(format!("read system clock: {error}")))?
            .as_nanos();
        let temporary = parent.join(format!(
            ".desired-profile.{}.{nonce}.tmp",
            std::process::id()
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|error| DaemonError::config(format!("create temp config: {error}")))?;
            file.write_all(&json)
                .map_err(|error| DaemonError::config(format!("write temp config: {error}")))?;
            file.sync_all()
                .map_err(|error| DaemonError::config(format!("sync temp config: {error}")))?;
            fs::rename(&temporary, &self.path).map_err(|error| {
                DaemonError::config(format!("replace desired profile: {error}"))
            })?;
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| DaemonError::config(format!("sync config directory: {error}")))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn ensure_owned_private_directory(path: &Path) -> Result<(), DaemonError> {
    fs::create_dir_all(path)
        .map_err(|error| DaemonError::config(format!("create config directory: {error}")))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| DaemonError::config(format!("inspect config directory: {error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || metadata.uid() != effective_uid()
    {
        return Err(DaemonError::config(
            "config directory must be a real directory owned by the daemon user",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| DaemonError::config(format!("secure config directory: {error}")))
}

fn validate_existing_destination(path: &Path) -> Result<(), DaemonError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_owned_private_file(&metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DaemonError::config(format!(
            "inspect existing desired profile: {error}"
        ))),
    }
}

fn validate_owned_private_file(metadata: &fs::Metadata) -> Result<(), DaemonError> {
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != effective_uid()
        || metadata.mode() & 0o077 != 0
    {
        return Err(DaemonError::config(
            "desired profile must be a user-owned, non-symlink 0600 file",
        ));
    }
    Ok(())
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    unsafe { libc::geteuid() }
}
