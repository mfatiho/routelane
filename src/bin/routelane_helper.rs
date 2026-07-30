use routelane::backend::{
    PrivilegedService, ServiceError, SystemPolkitAuthorizer, SystemPrivilegedBackend,
};
use serde::Serialize;

const BUS_NAME: &str = "io.github.routelane.Helper";
const OBJECT_PATH: &str = "/io/github/routelane/Helper";

type HelperCore = PrivilegedService<SystemPolkitAuthorizer, SystemPrivilegedBackend>;

struct DbusHelper {
    core: HelperCore,
}

#[zbus::interface(name = "io.github.routelane.Helper1")]
impl DbusHelper {
    async fn apply_plan(
        &self,
        plan_json: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        let response = self
            .core
            .apply_plan(header.sender().map(|sender| sender.as_str()), plan_json)
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
    let helper = DbusHelper {
        core: PrivilegedService::new(
            SystemPolkitAuthorizer::new(connection.clone()),
            SystemPrivilegedBackend::default(),
        ),
    };
    connection.object_server().at(OBJECT_PATH, helper).await?;
    connection.request_name(BUS_NAME).await?;
    std::future::pending::<()>().await;
    Ok(())
}
