use routelane::backend::{
    application_cgroup_rule_ids, validate_privileged_plan, ApplicationCgroupManager, KernelPlan,
    PrivilegedService, ProcessIdentity, ServiceError, SystemPolkitAuthorizer,
    SystemPrivilegedBackend, MAX_PLAN_JSON_BYTES,
};
use serde::Serialize;

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
    core: HelperCore,
    authorizer: SystemPolkitAuthorizer,
    cgroups: ApplicationCgroupManager,
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
        let response = self
            .core
            .inspect(header.sender().map(|sender| sender.as_str()))
            .await
            .map_err(to_fdo_error)?;
        serialize_response(&response)
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
    let helper = DbusHelper {
        core: PrivilegedService::new(authorizer.clone(), SystemPrivilegedBackend::default()),
        authorizer,
        cgroups: ApplicationCgroupManager::system(),
    };
    connection.object_server().at(OBJECT_PATH, helper).await?;
    connection.request_name(BUS_NAME).await?;
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
            .with_interface_settings(routelane::backend::SystemMarkAwareRpfBackend),
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
