use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;
use tokio::sync::{broadcast, Mutex};

use crate::backend::{
    compile_profile, AppliedKernelState, ApplyResponse, CleanupResponse, InspectResponse,
    KernelPlan, ResolutionSnapshot, RestoreResponse, TopologySnapshot,
};
use crate::dns::periodic::resolve_domain;
use crate::domain::{
    normalize_lane_names, validate_profile, HealthState, Profile, ProfileId, RuleId, RuleTarget,
};
use crate::health::{
    evaluate_health, Diagnostic, DiagnosticCode, DiagnosticSubject, HealthReport, Severity,
    SuggestedAction,
};
use crate::ipc::{
    ApplyProfileResult, DaemonEnvelope, DaemonEvent, DaemonRequest, DaemonResponse, DaemonResult,
    IpcError, IPC_PROTOCOL_VERSION,
};

use super::DaemonState;

const EVENT_CAPACITY: usize = 32;
const MAX_CONFIG_BYTES: u64 = 1_048_576;
const HELPER_BUS_NAME: &str = "io.github.routelane.Helper";
const HELPER_OBJECT_PATH: &str = "/io/github/routelane/Helper";
const HELPER_INTERFACE: &str = "io.github.routelane.Helper1";
const IP_BINARY: &str = "/usr/sbin/ip";

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

    fn planner(message: impl Into<String>) -> Self {
        Self::new("DAEMON_PLAN_FAILED", message)
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
                let profiles = self
                    .state
                    .lock()
                    .await
                    .desired
                    .clone()
                    .into_iter()
                    .collect();
                Ok(DaemonResult::Profiles(profiles))
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

    pub async fn apply_profile(&self, profile: Profile) -> Result<ApplyProfileResult, DaemonError> {
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
        if let Err(error) = self.config.save(Some(&profile)) {
            self.record_failure(error.clone()).await;
            return Err(error);
        }

        let (candidate_generation, previous_plan) = {
            let mut state = self.state.lock().await;
            state.desired = Some(profile.clone());
            state.health = HealthReport::applying();
            (
                state.generation + 1,
                state.applied.as_ref().map(|applied| applied.plan().clone()),
            )
        };
        let topology = match self.topology.snapshot(&profile).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.record_failure(error.clone()).await;
                return Err(error);
            }
        };
        let resolutions = match self.resolutions.snapshot(&profile).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.record_failure(error.clone()).await;
                return Err(error);
            }
        };
        let plan = match compile_profile(&profile, &topology, &resolutions, candidate_generation) {
            Ok(plan) => plan,
            Err(error) => {
                let error = DaemonError::planner(format!("{error:?}"));
                self.record_failure(error.clone()).await;
                return Err(error);
            }
        };
        let response = match self.helper.apply(&plan).await {
            Ok(response) => response,
            Err(error) => {
                self.record_failure(error.clone()).await;
                return Err(error);
            }
        };
        if response.generation != plan.generation || response.plan_hash != plan.content_hash() {
            let error = DaemonError::new(
                "DAEMON_HELPER_RESPONSE_MISMATCH",
                "privileged helper response did not match the requested generation and plan hash",
            );
            self.restore_failed_candidate(&plan, previous_plan.as_ref())
                .await?;
            self.record_rolled_back_failure(error.clone()).await;
            return Err(error);
        }

        let health = match self.inspect_plan_health(&profile, &plan).await {
            Ok(report) if report.state == HealthState::Healthy => report,
            Ok(report) => {
                let error = DaemonError::new(
                    "DAEMON_HEALTH_VERIFICATION_FAILED",
                    "live kernel state did not match the applied plan",
                );
                self.restore_failed_candidate(&plan, previous_plan.as_ref())
                    .await?;
                self.record_verification_failure(error.clone(), report)
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
                self.restore_failed_candidate(&plan, previous_plan.as_ref())
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
            state.applied = Some(AppliedKernelState::from_plan(&plan));
            state.applied_profile = Some(profile);
            state.generation = plan.generation;
            state.health = health.clone();
            state.last_error = None;
        }
        let _ = self.events.send(DaemonEvent::StateChanged {
            generation: plan.generation,
            health,
        });
        Ok(result)
    }

    pub async fn set_rule_enabled(
        &self,
        profile_id: ProfileId,
        rule_id: RuleId,
        enabled: bool,
    ) -> Result<ApplyProfileResult, DaemonError> {
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
        if let Err(error) = self.helper.cleanup().await {
            self.record_failure(error.clone()).await;
            return Err(error);
        }
        let generation = {
            let mut state = self.state.lock().await;
            state.applied = None;
            state.applied_profile = None;
            state.health = HealthReport::inactive();
            state.last_error = None;
            state.generation
        };
        let _ = self.events.send(DaemonEvent::StateChanged {
            generation,
            health: HealthReport::inactive(),
        });
        Ok(())
    }

    async fn record_failure(&self, error: DaemonError) {
        let ipc_error = IpcError::from(error.clone());
        let (generation, health) = {
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
            (state.generation, health)
        };
        let _ = self.events.send(DaemonEvent::OperationFailed {
            generation,
            health,
            error: ipc_error,
        });
    }

    async fn record_rolled_back_failure(&self, error: DaemonError) {
        let ipc_error = IpcError::from(error.clone());
        let (generation, health) = {
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
            (state.generation, health)
        };
        let _ = self.events.send(DaemonEvent::OperationFailed {
            generation,
            health,
            error: ipc_error,
        });
    }

    async fn restore_failed_candidate(
        &self,
        failed_candidate: &KernelPlan,
        previous: Option<&KernelPlan>,
    ) -> Result<(), DaemonError> {
        let restored = match self.helper.restore(failed_candidate, previous).await {
            Ok(restored) => restored,
            Err(error) => {
                let error = DaemonError::new(
                    "DAEMON_KERNEL_ROLLBACK_FAILED",
                    format!("failed to restore kernel state after health check: {error}"),
                );
                self.record_kernel_restore_failure(error.clone()).await;
                return Err(error);
            }
        };
        if restored.generation != previous.map(|plan| plan.generation)
            || restored.plan_hash != previous.map(KernelPlan::content_hash)
        {
            let error = DaemonError::new(
                "DAEMON_KERNEL_ROLLBACK_FAILED",
                "privileged helper restore response did not match the previous plan",
            );
            self.record_kernel_restore_failure(error.clone()).await;
            return Err(error);
        }
        Ok(())
    }

    async fn record_kernel_restore_failure(&self, error: DaemonError) {
        let ipc_error = IpcError::from(error);
        let (generation, health) = {
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
            (state.generation, health)
        };
        let _ = self.events.send(DaemonEvent::OperationFailed {
            generation,
            health,
            error: ipc_error,
        });
    }

    async fn record_verification_failure(&self, error: DaemonError, mut report: HealthReport) {
        let ipc_error = IpcError::from(error);
        let (generation, health) = {
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
            (state.generation, report)
        };
        let _ = self.events.send(DaemonEvent::OperationFailed {
            generation,
            health,
            error: ipc_error,
        });
    }

    async fn inspect_plan_health(
        &self,
        profile: &Profile,
        plan: &KernelPlan,
    ) -> Result<HealthReport, DaemonError> {
        let topology = self.topology.snapshot(profile).await?;
        let inspected = self.helper.inspect().await?;
        Ok(evaluate_health(
            Some(plan),
            &inspected.snapshot.with_topology(&topology),
        ))
    }

    async fn current_status(&self) -> crate::ipc::DaemonStatus {
        let (base, profile, plan) = {
            let state = self.state.lock().await;
            (
                state.clone(),
                state.applied_profile.clone(),
                state.applied.as_ref().map(|applied| applied.plan().clone()),
            )
        };
        let health = match (profile.as_ref(), plan.as_ref()) {
            (_, None) => HealthReport::inactive(),
            (Some(profile), Some(plan)) => match self.inspect_plan_health(profile, plan).await {
                Ok(report) => report,
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
            (None, Some(_)) => HealthReport::failure(
                HealthState::Failed,
                DiagnosticCode::RoutingResourceConflict,
                DiagnosticSubject::HealthInspection,
                "Applied policy has no corresponding profile",
                "daemon state is missing the profile required for topology inspection",
                Some(SuggestedAction::new("Reapply a saved profile.")),
            ),
        };
        let mut status = base.status();
        status.health = health;
        status
    }
}

#[derive(Debug, Clone, Default)]
pub struct SystemTopologyProvider;

#[async_trait]
impl TopologyProvider for SystemTopologyProvider {
    async fn snapshot(&self, _profile: &Profile) -> Result<TopologySnapshot, DaemonError> {
        #[derive(Deserialize)]
        struct Link {
            ifname: String,
        }
        #[derive(Deserialize)]
        struct Route<A> {
            dev: Option<String>,
            gateway: Option<A>,
        }

        let links: Vec<Link> = run_ip_json(&["-j", "link", "show"]).await?;
        let routes_v4: Vec<Route<Ipv4Addr>> =
            run_ip_json(&["-j", "-4", "route", "show", "default"]).await?;
        let routes_v6: Vec<Route<Ipv6Addr>> =
            run_ip_json(&["-j", "-6", "route", "show", "default"]).await?;
        let mut interfaces = links
            .into_iter()
            .map(|link| (link.ifname, (None, None)))
            .collect::<BTreeMap<_, _>>();
        for route in routes_v4 {
            if let (Some(interface), Some(gateway)) = (route.dev, route.gateway) {
                if let Some(entry) = interfaces.get_mut(&interface) {
                    entry.0 = Some(gateway);
                }
            }
        }
        for route in routes_v6 {
            if let (Some(interface), Some(gateway)) = (route.dev, route.gateway) {
                if let Some(entry) = interfaces.get_mut(&interface) {
                    entry.1 = Some(gateway);
                }
            }
        }

        let mut snapshot = TopologySnapshot::empty();
        for (interface, (gateway_v4, gateway_v6)) in interfaces {
            snapshot.insert(interface, gateway_v4, gateway_v6);
        }
        Ok(snapshot)
    }
}

async fn run_ip_json<T>(arguments: &[&str]) -> Result<T, DaemonError>
where
    T: serde::de::DeserializeOwned,
{
    let output = Command::new(IP_BINARY)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| {
            DaemonError::topology(format!("could not start typed ip command: {error}"))
        })?;
    if !output.status.success() {
        return Err(DaemonError::topology(format!(
            "typed ip command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| DaemonError::topology(format!("invalid ip JSON output: {error}")))
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
}

#[async_trait]
impl PrivilegedHelperClient for SystemPrivilegedHelperClient {
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
        let mut desired: Option<Profile> = serde_json::from_slice(&json)
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
        let json = serde_json::to_vec(&desired)
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
