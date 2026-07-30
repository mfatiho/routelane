use std::path::Path;
use std::time::Duration;

use routelane::domain::Profile;
use routelane::ipc::{
    socket_path, DaemonClient, DaemonEnvelope, DaemonRequest, DaemonResponse, DaemonResult,
};

const EXIT_USAGE: i32 = 2;
const EXIT_CONFIG: i32 = 3;
const EXIT_DAEMON: i32 = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ERROR_CHARS: usize = 768;
const USAGE: &str =
    "Usage:\n  routelanectl status [--json]\n  routelanectl apply-profile <config.json>\n  routelanectl deactivate";

#[tokio::main]
async fn main() {
    let code = match run(std::env::args().skip(1).collect()).await {
        Ok(output) => {
            println!("{output}");
            0
        }
        Err(error) => {
            eprintln!("{}", bounded(&error.message));
            error.code
        }
    };
    if code != 0 {
        std::process::exit(code);
    }
}

async fn run(args: Vec<String>) -> Result<String, CliError> {
    let (request, output) = parse_command(&args)?;
    let path = socket_path().map_err(|error| CliError::daemon(error.to_string()))?;
    let client = DaemonClient::new(path);
    let envelope = DaemonEnvelope::new(uuid::Uuid::new_v4().to_string(), request);
    let response = tokio::time::timeout(REQUEST_TIMEOUT, client.send(envelope))
        .await
        .map_err(|_| CliError::daemon("IPC_TIMEOUT: daemon request timed out"))?
        .map_err(|error| CliError::daemon(error.to_string()))?;
    format_response(response, output)
}

#[derive(Debug, Clone, Copy)]
enum OutputFormat {
    StatusText,
    StatusJson,
    Applied,
    Deactivated,
}

fn parse_command(args: &[String]) -> Result<(DaemonRequest, OutputFormat), CliError> {
    match args {
        [command] if command == "status" => {
            Ok((DaemonRequest::GetStatus, OutputFormat::StatusText))
        }
        [command, option] if command == "status" && option == "--json" => {
            Ok((DaemonRequest::GetStatus, OutputFormat::StatusJson))
        }
        [command, path] if command == "apply-profile" => Ok((
            DaemonRequest::ApplyProfile(read_profile(Path::new(path))?),
            OutputFormat::Applied,
        )),
        [command] if command == "deactivate" => {
            Ok((DaemonRequest::Deactivate, OutputFormat::Deactivated))
        }
        _ => Err(CliError::usage()),
    }
}

fn read_profile(path: &Path) -> Result<Profile, CliError> {
    let bytes = std::fs::read(path).map_err(|error| {
        CliError::config(format!(
            "could not read profile {}: {error}",
            path.display()
        ))
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|error| CliError::config(format!("invalid profile JSON: {error}")))
}

fn format_response(response: DaemonResponse, output: OutputFormat) -> Result<String, CliError> {
    if let Some(error) = response.error {
        return Err(CliError::daemon(error.to_string()));
    }
    match (output, response.result) {
        (OutputFormat::StatusText, Some(DaemonResult::Status(status))) => Ok(format!(
            "state={:?} generation={} applied_profile={}",
            status.health.state,
            status.generation,
            status
                .applied_profile_id
                .as_ref()
                .map(|id| id.as_str())
                .unwrap_or("-")
        )),
        (OutputFormat::StatusJson, Some(DaemonResult::Status(status))) => {
            serde_json::to_string(&status)
                .map_err(|error| CliError::daemon(format!("IPC_JSON_ENCODE_FAILED: {error}")))
        }
        (OutputFormat::Applied, Some(DaemonResult::Applied(result))) => {
            Ok(format!("applied generation={}", result.generation))
        }
        (OutputFormat::Deactivated, Some(DaemonResult::Deactivated)) => {
            Ok("deactivated".to_owned())
        }
        _ => Err(CliError::daemon(
            "IPC_INVALID_RESPONSE: daemon returned an unexpected result",
        )),
    }
}

fn bounded(message: &str) -> String {
    let mut chars = message.chars();
    let prefix = chars.by_ref().take(MAX_ERROR_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

struct CliError {
    code: i32,
    message: String,
}

impl CliError {
    fn usage() -> Self {
        Self {
            code: EXIT_USAGE,
            message: USAGE.to_owned(),
        }
    }

    fn config(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_CONFIG,
            message: message.into(),
        }
    }

    fn daemon(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_DAEMON,
            message: message.into(),
        }
    }
}
