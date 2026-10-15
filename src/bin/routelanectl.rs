use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use routelane::config::{load_config_file, ConfigV3};
use routelane::domain::{validate_profile, Profile};
use routelane::ipc::{
    socket_path, DaemonClient, DaemonEnvelope, DaemonRequest, DaemonResponse, DaemonResult,
};

const EXIT_USAGE: i32 = 2;
const EXIT_CONFIG: i32 = 3;
const EXIT_DAEMON: i32 = 4;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ERROR_CHARS: usize = 768;
const USAGE: &str = "Usage:\n  routelanectl status [--json]\n  routelanectl apply-profile <config.json> [--profile <id-or-name>]\n  routelanectl deactivate";

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
    let deadline = tokio::time::Instant::now() + OPERATION_TIMEOUT;
    let (request, output) = parse_command(&args, deadline).await?;
    let path = socket_path().map_err(|error| CliError::daemon(error.to_string()))?;
    let client = DaemonClient::new(path);
    let envelope = DaemonEnvelope::new(uuid::Uuid::new_v4().to_string(), request);
    let response = tokio::time::timeout_at(deadline, client.send(envelope))
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

async fn parse_command(
    args: &[String],
    deadline: tokio::time::Instant,
) -> Result<(DaemonRequest, OutputFormat), CliError> {
    match args {
        [command] if command == "status" => {
            Ok((DaemonRequest::GetStatus, OutputFormat::StatusText))
        }
        [command, option] if command == "status" && option == "--json" => {
            Ok((DaemonRequest::GetStatus, OutputFormat::StatusJson))
        }
        [command, path] if command == "apply-profile" => {
            let profile = load_selected_profile(Path::new(path), None, deadline).await?;
            Ok((DaemonRequest::ApplyProfile(profile), OutputFormat::Applied))
        }
        [command, path, flag, selector] if command == "apply-profile" && flag == "--profile" => {
            let profile =
                load_selected_profile(Path::new(path), Some(selector.as_str()), deadline).await?;
            Ok((DaemonRequest::ApplyProfile(profile), OutputFormat::Applied))
        }
        [command] if command == "deactivate" => {
            Ok((DaemonRequest::Deactivate, OutputFormat::Deactivated))
        }
        _ => Err(CliError::usage()),
    }
}

async fn load_selected_profile(
    path: &Path,
    selector: Option<&str>,
    deadline: tokio::time::Instant,
) -> Result<Profile, CliError> {
    let path = path.to_owned();
    let document = tokio::time::timeout_at(
        deadline,
        tokio::task::spawn_blocking(move || load_config_file(&path)),
    )
    .await
    .map_err(|_| CliError::config("configuration read timed out"))?
    .map_err(|error| CliError::config(format!("configuration reader failed: {error}")))?
    .map_err(|error| CliError::config(error.to_string()))?;
    select_profile(document, selector)
}

fn select_profile(mut document: ConfigV3, selector: Option<&str>) -> Result<Profile, CliError> {
    validate_profile_identities(&document)?;
    let selected_index = match selector {
        Some(selector) => {
            let matches = document
                .profiles
                .iter()
                .enumerate()
                .filter(|(_, profile)| profile.id.as_str() == selector || profile.name == selector)
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [index] => *index,
                [] => {
                    return Err(CliError::config(format!(
                        "profile selector '{selector}' was not found"
                    )));
                }
                _ => {
                    return Err(CliError::config(format!(
                        "profile selector '{selector}' is ambiguous"
                    )));
                }
            }
        }
        None => match document.profiles.len() {
            1 => 0,
            0 => return Err(CliError::config("configuration contains no profiles")),
            _ => {
                return Err(CliError::config(
                    "configuration contains multiple profiles; use --profile <id-or-name>",
                ));
            }
        },
    };

    let mut profile = document.profiles.swap_remove(selected_index);
    profile.enabled = true;
    validate_profile(&profile)
        .map_err(|error| CliError::config(format!("invalid profile: {error:?}")))?;
    Ok(profile)
}

fn validate_profile_identities(document: &ConfigV3) -> Result<(), CliError> {
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for profile in &document.profiles {
        if !ids.insert(profile.id.as_str()) {
            return Err(CliError::config(format!(
                "duplicate profile id '{}'",
                profile.id.as_str()
            )));
        }
        if !names.insert(profile.name.as_str()) {
            return Err(CliError::config(format!(
                "duplicate profile name '{}'",
                profile.name
            )));
        }
    }
    Ok(())
}

fn format_response(response: DaemonResponse, output: OutputFormat) -> Result<String, CliError> {
    if let Some(error) = response.error {
        return Err(CliError::daemon(error.to_string()));
    }
    match (output, response.result) {
        (OutputFormat::StatusText, Some(DaemonResult::Status(status))) => Ok(format!(
            "state={:?} generation={} applied_profile={}\n{}",
            status.health.state,
            status.generation,
            status
                .applied_profile_id
                .as_ref()
                .map(|id| id.as_str())
                .unwrap_or("-"),
            primary_summary(&status.primary)
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

/// Preferred and effective exit per family. The effective exit is chosen
/// from local link, address and route evidence only.
fn primary_summary(primary: &routelane::backend::RuntimeStatus) -> String {
    use routelane::backend::FamilyExit;
    let Some(decision) = primary.decision.as_ref() else {
        return format!("primary phase={:?}", primary.phase);
    };
    let family = |preferred: &Option<routelane::domain::LaneId>, effective: &FamilyExit| {
        format!(
            "{}->{}",
            preferred.as_ref().map_or("-", |id| id.as_str()),
            match effective {
                FamilyExit::Lane(id) => id.as_str(),
                FamilyExit::Blocked => "blocked",
            }
        )
    };
    format!(
        "primary phase={:?} ipv4={} ipv6={}\nnote=effective exits follow local link evidence; internet reachability is not verified",
        primary.phase,
        family(&decision.preferred.v4, &decision.effective.v4),
        family(&decision.preferred.v6, &decision.effective.v6),
    )
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
