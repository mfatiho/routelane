use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use routelane::backend::{
    application_cgroup_rule_ids, capture_system_topology, drive_runtime,
    forward_kernel_notifications, validate_privileged_plan, ApplicationCgroupManager, KernelPlan,
    MonitorTiming, PrivilegedService, ProcessIdentity, RuntimeActivation, RuntimePhase,
    RuntimeRefresher, RuntimeStatus, ServiceError, SystemPolkitAuthorizer, SystemPrivilegedBackend,
    TopologySnapshot, MAX_PLAN_JSON_BYTES, RUNTIME_PROTOCOL_VERSION,
};
use routelane::domain::RuleSource;
use serde::Serialize;
use zbus::object_server::SignalEmitter;

#[cfg(feature = "root-integration")]
use std::future::{ready, Ready};
#[cfg(feature = "root-integration")]
use std::io::{BufRead, Write};

#[cfg(feature = "root-integration")]
use routelane::backend::{
    AuthorizationError, Authorizer, CommandPrivilegedBackend, FileAppliedStateStore,
    ProcessCommandExecutor,
};
#[cfg(feature = "root-integration")]
use serde::Deserialize;

const BUS_NAME: &str = "io.github.routelane.Helper";
const OBJECT_PATH: &str = "/io/github/routelane/Helper";

type HelperCore = PrivilegedService<SystemPolkitAuthorizer, SystemPrivilegedBackend>;

struct DbusHelper {
    core: Arc<HelperCore>,
    authorizer: SystemPolkitAuthorizer,
    cgroups: ApplicationCgroupManager,
}

/// Runtime authority interface. Activation and restore are authorized per
/// call; automatic refresh has no method here at all.
struct DbusHelper2 {
    core: Arc<HelperCore>,
    authorizer: SystemPolkitAuthorizer,
    cgroups: ApplicationCgroupManager,
    started: Instant,
}

#[zbus::interface(name = "io.github.routelane.Helper2")]
impl DbusHelper2 {
    /// Read-only capability query: the daemon checks it before it saves a
    /// desired profile or asks for any mutation. Needs no authorization.
    async fn get_runtime_protocol(&self) -> u32 {
        RUNTIME_PROTOCOL_VERSION
    }

    async fn activate_runtime(
        &self,
        activation_json: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<String> {
        let sender = header.sender().map(|sender| sender.as_str());
        let uid = self.caller_uid(sender).await?;
        let topology = capture_topology().await.map_err(zbus::fdo::Error::Failed)?;
        let cgroups = self.cgroups.clone();
        let response = self
            .core
            .activate_runtime(
                sender,
                activation_json,
                uid,
                topology,
                self.started.elapsed(),
                move |activation| prepare_application_cgroups(&cgroups, activation, uid),
            )
            .await
            .map_err(to_fdo_error)?;
        announce(&emitter, &response.status).await;
        serialize_response(&response)
    }

    async fn restore_runtime(
        &self,
        receipt: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<String> {
        let sender = header.sender().map(|sender| sender.as_str());
        let uid = self.caller_uid(sender).await?;
        let status = self
            .core
            .restore_runtime(sender, receipt.to_owned(), uid)
            .await
            .map_err(to_fdo_error)?;
        announce(&emitter, &status).await;
        serialize_response(&status)
    }

    async fn inspect(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        inspect_with_observed_networks(&self.core, header.sender().map(|sender| sender.as_str()))
            .await
    }

    async fn cleanup(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<String> {
        let response = self
            .core
            .cleanup(header.sender().map(|sender| sender.as_str()))
            .await
            .map_err(to_fdo_error)?;
        if let Ok(status) = self.core.runtime_status().await {
            announce(&emitter, &status).await;
        }
        serialize_response(&response)
    }

    #[zbus(signal)]
    async fn runtime_changed(emitter: &SignalEmitter<'_>, status_json: &str) -> zbus::Result<()>;
}

impl DbusHelper2 {
    async fn caller_uid(&self, sender: Option<&str>) -> zbus::fdo::Result<u32> {
        let sender =
            sender.ok_or_else(|| zbus::fdo::Error::AccessDenied("missing sender".to_owned()))?;
        self.authorizer
            .caller_uid(sender)
            .await
            .map_err(|error| zbus::fdo::Error::AccessDenied(error.to_string()))
    }
}

async fn announce(emitter: &SignalEmitter<'_>, status: &RuntimeStatus) {
    if let Ok(json) = serde_json::to_string(status) {
        if let Err(error) = DbusHelper2::runtime_changed(emitter, &json).await {
            eprintln!("routelane-helper could not announce runtime status: {error}");
        }
    }
}

/// Read-only Inspect plus the directly connected networks from a fresh root
/// topology capture (off the async worker). A failed or unobserved capture
/// leaves them `None`, so a preview can never present unknown facts as safe.
async fn inspect_with_observed_networks(
    core: &HelperCore,
    sender: Option<&str>,
) -> zbus::fdo::Result<String> {
    let mut response = core.inspect(sender).await.map_err(to_fdo_error)?;
    match capture_topology().await {
        Ok(topology) if topology.is_observed() => {
            response.observed_connected_networks = Some(topology.connected_networks());
        }
        Ok(_) => response
            .diagnostics
            .push("HELPER_TOPOLOGY_UNAVAILABLE: connected networks were not observed".into()),
        Err(error) => response
            .diagnostics
            .push(format!("HELPER_TOPOLOGY_UNAVAILABLE: {error}")),
    }
    serialize_response(&response)
}

async fn capture_topology() -> Result<TopologySnapshot, String> {
    tokio::task::spawn_blocking(capture_system_topology)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

/// Application cgroups must exist before nftables can match them.
fn prepare_application_cgroups(
    cgroups: &ApplicationCgroupManager,
    activation: &RuntimeActivation,
    uid: u32,
) -> Result<(), ServiceError> {
    for rule in &activation.profile.rules {
        if matches!(rule.source, RuleSource::Application(_)) {
            cgroups
                .prepare(&rule.id, uid)
                .map_err(|error| ServiceError::Backend(error.to_string()))?;
        }
    }
    Ok(())
}

/// Kernel-event refresh glue. It holds no authorizer.
struct HelperRefresher {
    core: Arc<HelperCore>,
    connection: zbus::Connection,
    started: Instant,
    last: Mutex<Option<RuntimeStatus>>,
}

impl HelperRefresher {
    async fn publish(&self, status: &RuntimeStatus) {
        let changed = {
            let mut last = self.last.lock().unwrap();
            let changed = last.as_ref() != Some(status);
            *last = Some(status.clone());
            changed
        };
        if !changed {
            return;
        }
        if let Ok(iface) = self
            .connection
            .object_server()
            .interface::<_, DbusHelper2>(OBJECT_PATH)
            .await
        {
            announce(iface.signal_emitter(), status).await;
        }
    }
}

impl RuntimeRefresher for HelperRefresher {
    fn refresh(&self) -> Pin<Box<dyn Future<Output = Option<RuntimeStatus>> + Send + '_>> {
        Box::pin(async move {
            let status = self.core.runtime_status().await.ok()?;
            if status.phase == RuntimePhase::Inactive {
                self.publish(&status).await;
                return Some(status);
            }
            let status = match capture_topology().await {
                Ok(topology) => self
                    .core
                    .refresh_runtime(topology, self.started.elapsed())
                    .await
                    .ok()?,
                Err(error) => self.core.degrade_runtime(error).await.ok()?,
            };
            self.publish(&status).await;
            Some(status)
        })
    }

    fn degrade(&self, reason: String) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if let Ok(status) = self.core.degrade_runtime(reason).await {
                self.publish(&status).await;
            }
        })
    }
}

/// Subscribes to kernel notifications before the initial snapshot, which also
/// resumes a journaled runtime after a helper restart.
async fn run_runtime_monitor(refresher: Arc<HelperRefresher>) {
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let start_forwarder = move || {
        tokio::spawn(forward_kernel_notifications(sender.clone()));
    };
    start_forwarder();
    drive_runtime(
        &*refresher,
        receiver,
        MonitorTiming::default(),
        start_forwarder,
    )
    .await;
}

#[zbus::interface(name = "io.github.routelane.Helper1")]
impl DbusHelper {
    async fn apply_plan(
        &self,
        plan_json: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        let sender = header.sender().map(|sender| sender.as_str());
        self.core
            .authorize_mutation(sender)
            .await
            .map_err(to_fdo_error)?;
        if plan_json.len() > MAX_PLAN_JSON_BYTES {
            return Err(zbus::fdo::Error::InvalidArgs(
                "kernel plan exceeds the maximum size".to_owned(),
            ));
        }
        let plan: KernelPlan = serde_json::from_str(plan_json)
            .map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?;
        validate_privileged_plan(&plan)
            .map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?;
        let sender =
            sender.ok_or_else(|| zbus::fdo::Error::AccessDenied("missing sender".to_owned()))?;
        let uid = self
            .authorizer
            .caller_uid(sender)
            .await
            .map_err(|error| zbus::fdo::Error::AccessDenied(error.to_string()))?;
        for rule_id in application_cgroup_rule_ids(&plan.rules)
            .map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?
        {
            self.cgroups
                .prepare(&rule_id, uid)
                .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        }
        let response = self
            .core
            .apply_plan(Some(sender), plan_json)
            .await
            .map_err(to_fdo_error)?;
        serialize_response(&response)
    }

    async fn inspect(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        inspect_with_observed_networks(&self.core, header.sender().map(|sender| sender.as_str()))
            .await
    }

    async fn restore_plan(
        &self,
        failed_candidate_json: &str,
        previous_json: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        let response = self
            .core
            .restore_plan(
                header.sender().map(|sender| sender.as_str()),
                failed_candidate_json,
                (!previous_json.is_empty()).then_some(previous_json),
            )
            .await
            .map_err(to_fdo_error)?;
        serialize_response(&response)
    }

    async fn cleanup(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        let response = self
            .core
            .cleanup(header.sender().map(|sender| sender.as_str()))
            .await
            .map_err(to_fdo_error)?;
        serialize_response(&response)
    }

    async fn recover_orphaned_resources(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        let sender = header.sender().map(|sender| sender.as_str());
        self.core
            .authorize_mutation(sender)
            .await
            .map_err(to_fdo_error)?;
        let cgroups = self
            .cgroups
            .verified_orphan_directories()
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        let response = self
            .core
            .recover_orphaned_resources(sender)
            .await
            .map_err(to_fdo_error)?;
        self.cgroups
            .remove_verified_orphan_directories(&cgroups)
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        serialize_response(&response)
    }

    async fn prepare_application_cgroup(
        &self,
        rule_id: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        let sender = header.sender().map(|sender| sender.as_str());
        self.core
            .authorize_mutation(sender)
            .await
            .map_err(to_fdo_error)?;
        let sender =
            sender.ok_or_else(|| zbus::fdo::Error::AccessDenied("missing sender".into()))?;
        let uid = self
            .authorizer
            .caller_uid(sender)
            .await
            .map_err(|error| zbus::fdo::Error::AccessDenied(error.to_string()))?;
        let path = self
            .cgroups
            .prepare(&routelane::domain::RuleId::from(rule_id), uid)
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        serialize_response(&path)
    }

    async fn adopt_application_process(
        &self,
        rule_id: &str,
        pid: u32,
        start_time: u64,
        executable_path: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.core
            .authorize_mutation(header.sender().map(|sender| sender.as_str()))
            .await
            .map_err(to_fdo_error)?;
        self.cgroups
            .adopt(
                &routelane::domain::RuleId::from(rule_id),
                &ProcessIdentity::new(pid, start_time, executable_path.into()),
            )
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        serialize_response(&serde_json::json!({"adopted": true}))
    }
}

fn serialize_response(response: &impl Serialize) -> zbus::fdo::Result<String> {
    serde_json::to_string(response).map_err(|error| zbus::fdo::Error::Failed(error.to_string()))
}

fn to_fdo_error(error: ServiceError) -> zbus::fdo::Error {
    match error {
        ServiceError::MissingSender | ServiceError::Authorization(_) => {
            zbus::fdo::Error::AccessDenied(error.to_string())
        }
        ServiceError::PlanTooLarge { .. }
        | ServiceError::InvalidPlanJson(_)
        | ServiceError::Validation(_) => zbus::fdo::Error::InvalidArgs(error.to_string()),
        ServiceError::Backend(_) | ServiceError::StatePoisoned => {
            zbus::fdo::Error::Failed(error.to_string())
        }
    }
}

#[tokio::main]
async fn main() {
    if unsafe { libc::getuid() } != 0 {
        eprintln!("routelane-helper must run as root");
        std::process::exit(1);
    }
    #[cfg(feature = "root-integration")]
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new("--root-integration-stdio"))
        && std::env::args_os().len() == 2
    {
        if let Err(error) = run_root_integration_stdio().await {
            eprintln!("routelane-helper root integration transport failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    if std::env::args_os().len() != 1 {
        eprintln!("usage: routelane-helper");
        std::process::exit(2);
    }
    if let Err(error) = run_dbus_service().await {
        eprintln!("routelane-helper D-Bus service failed: {error}");
        std::process::exit(1);
    }
}

async fn run_dbus_service() -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;
    let authorizer = SystemPolkitAuthorizer::new(connection.clone());
    let started = Instant::now();
    let core = Arc::new(PrivilegedService::new(
        authorizer.clone(),
        SystemPrivilegedBackend::default(),
    ));
    let helper = DbusHelper {
        core: Arc::clone(&core),
        authorizer: authorizer.clone(),
        cgroups: ApplicationCgroupManager::system(),
    };
    let helper2 = DbusHelper2 {
        core: Arc::clone(&core),
        authorizer,
        cgroups: ApplicationCgroupManager::system(),
        started,
    };
    connection.object_server().at(OBJECT_PATH, helper).await?;
    connection.object_server().at(OBJECT_PATH, helper2).await?;
    connection.request_name(BUS_NAME).await?;
    tokio::spawn(run_runtime_monitor(Arc::new(HelperRefresher {
        core,
        connection: connection.clone(),
        started,
        last: Mutex::new(None),
    })));
    std::future::pending::<()>().await;
    Ok(())
}

#[cfg(feature = "root-integration")]
#[derive(Debug, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
enum RootIntegrationRequest {
    Apply {
        plan_json: String,
    },
    Inspect,
    Restore {
        failed_candidate_json: String,
        previous_json: Option<String>,
    },
    Cleanup,
    RecoverOrphanedResources,
    SetFailPoint {
        value: Option<String>,
    },
    /// Test-only: runtime activation with an explicit caller and clock.
    ActivateRuntime {
        activation_json: String,
        uid: u32,
        now_ms: u64,
    },
    /// Test-only stand-in for a kernel notification; production has no
    /// callable refresh.
    RefreshRuntime {
        now_ms: u64,
    },
    RestoreRuntime {
        receipt: String,
        uid: u32,
    },
    Exit,
}

#[cfg(feature = "root-integration")]
struct RootIntegrationAuthorizer;

#[cfg(feature = "root-integration")]
impl Authorizer for RootIntegrationAuthorizer {
    type Future<'a> = Ready<Result<(), AuthorizationError>>;

    fn authorize<'a>(&'a self, _sender: &'a str) -> Self::Future<'a> {
        ready(Ok(()))
    }
}

#[cfg(feature = "root-integration")]
async fn run_root_integration_stdio() -> Result<(), Box<dyn std::error::Error>> {
    const TEST_SENDER: &str = ":routelane-root-integration";
    let state_directory = std::env::var_os("ROUTELANE_TEST_STATE_DIRECTORY")
        .ok_or("ROUTELANE_TEST_STATE_DIRECTORY is required")?;
    let store = FileAppliedStateStore::isolated_root_integration(std::path::PathBuf::from(
        state_directory,
    ))?;
    let service = PrivilegedService::new(
        RootIntegrationAuthorizer,
        CommandPrivilegedBackend::with_store(ProcessCommandExecutor, store)
            .with_interface_settings(routelane::backend::SystemMarkAwareRpfBackend)
            .with_runtime_environment(routelane::backend::SystemRuntimeEnvironment),
    );
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();

    for line in stdin.lock().lines() {
        let response = match serde_json::from_str::<RootIntegrationRequest>(&line?) {
            Ok(RootIntegrationRequest::Apply { plan_json }) => service
                .apply_plan(Some(TEST_SENDER), &plan_json)
                .await
                .map(|response| serde_json::json!({"ok": response}))
                .unwrap_or_else(|error| serde_json::json!({"error": error.to_string()})),
            Ok(RootIntegrationRequest::Inspect) => service
                .inspect(Some(TEST_SENDER))
                .await
                .map(|response| serde_json::json!({"ok": response}))
                .unwrap_or_else(|error| serde_json::json!({"error": error.to_string()})),
            Ok(RootIntegrationRequest::Restore {
                failed_candidate_json,
                previous_json,
            }) => service
                .restore_plan(
                    Some(TEST_SENDER),
                    &failed_candidate_json,
                    previous_json.as_deref(),
                )
                .await
                .map(|response| serde_json::json!({"ok": response}))
                .unwrap_or_else(|error| serde_json::json!({"error": error.to_string()})),
            Ok(RootIntegrationRequest::Cleanup) => service
                .cleanup(Some(TEST_SENDER))
                .await
                .map(|response| serde_json::json!({"ok": response}))
                .unwrap_or_else(|error| serde_json::json!({"error": error.to_string()})),
            Ok(RootIntegrationRequest::RecoverOrphanedResources) => service
                .recover_orphaned_resources(Some(TEST_SENDER))
                .await
                .map(|response| serde_json::json!({"ok": response}))
                .unwrap_or_else(|error| serde_json::json!({"error": error.to_string()})),
            Ok(RootIntegrationRequest::SetFailPoint { value }) => {
                if value
                    .as_deref()
                    .is_some_and(|value| value != "activate_nft")
                {
                    serde_json::json!({"error": "unsupported root integration fail point"})
                } else {
                    match value.as_deref() {
                        Some(value) => std::env::set_var("ROUTELANE_TEST_FAIL_POINT", value),
                        None => std::env::remove_var("ROUTELANE_TEST_FAIL_POINT"),
                    }
                    serde_json::json!({"ok": {"fail_point": value}})
                }
            }
            Ok(RootIntegrationRequest::ActivateRuntime {
                activation_json,
                uid,
                now_ms,
            }) => match capture_topology().await {
                Ok(topology) => service
                    .activate_runtime(
                        Some(TEST_SENDER),
                        &activation_json,
                        uid,
                        topology,
                        std::time::Duration::from_millis(now_ms),
                        |_| Ok(()),
                    )
                    .await
                    .map(|response| serde_json::json!({"ok": response}))
                    .unwrap_or_else(|error| serde_json::json!({"error": error.to_string()})),
                Err(error) => serde_json::json!({"error": error}),
            },
            Ok(RootIntegrationRequest::RefreshRuntime { now_ms }) => match capture_topology().await
            {
                Ok(topology) => service
                    .refresh_runtime(topology, std::time::Duration::from_millis(now_ms))
                    .await
                    .map(|response| serde_json::json!({"ok": response}))
                    .unwrap_or_else(|error| serde_json::json!({"error": error.to_string()})),
                Err(error) => serde_json::json!({"error": error}),
            },
            Ok(RootIntegrationRequest::RestoreRuntime { receipt, uid }) => service
                .restore_runtime(Some(TEST_SENDER), receipt, uid)
                .await
                .map(|response| serde_json::json!({"ok": response}))
                .unwrap_or_else(|error| serde_json::json!({"error": error.to_string()})),
            Ok(RootIntegrationRequest::Exit) => break,
            Err(error) => serde_json::json!({"error": format!("invalid request: {error}")}),
        };
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    std::env::remove_var("ROUTELANE_TEST_FAIL_POINT");
    Ok(())
}
