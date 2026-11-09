use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use routelane::backend::{RoutingPreview, RoutingWarning};
use routelane::config::{load_config_file, ConfigV4};
use routelane::doctor::{any_failed, render_text, run_checks, SystemProbe};
use routelane::domain::{validate_profile, Profile, UnmatchedTraffic};
use routelane::ipc::{socket_path, DaemonClient, DaemonEnvelope, DaemonRequest, DaemonResult};

const EXIT_USAGE: i32 = 2;
const EXIT_CONFIG: i32 = 3;
const EXIT_DAEMON: i32 = 4;
const EXIT_DOCTOR: i32 = 5;
/// The routing impact needs `--acknowledge-routing-impact`; nothing was sent
/// for activation and no file was changed.
const EXIT_ACKNOWLEDGEMENT: i32 = 6;
const ACKNOWLEDGE: &str = "--acknowledge-routing-impact";
/// Warnings listed before the summary count, keeping stderr bounded.
const LISTED_WARNINGS: usize = 3;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ERROR_CHARS: usize = 768;
const USAGE: &str = "Usage:\n  routelanectl status [--json]\n  routelanectl apply-profile <config.json> [--profile <id-or-name>] [--acknowledge-routing-impact]\n  routelanectl deactivate\n  routelanectl doctor [--json]";

#[tokio::main]
async fn main() {
    let code = match run(std::env::args().skip(1).collect()).await {
        Ok((output, code)) => {
            println!("{output}");
            code
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

async fn run(args: Vec<String>) -> Result<(String, i32), CliError> {
    if args.first().is_some_and(|command| command == "doctor") {
        let json = match args.as_slice() {
            [_] => false,
            [_, option] if option == "--json" => true,
            _ => return Err(CliError::usage()),
        };
        // The synchronous probe owns short-lived runtimes for its bus/socket checks.
        let checks = tokio::task::spawn_blocking(|| run_checks(&SystemProbe))
            .await
            .map_err(|error| CliError {
                code: EXIT_DOCTOR,
                message: format!("doctor failed: {error}"),
            })?;
        let code = if any_failed(&checks) { EXIT_DOCTOR } else { 0 };
        let output = if json {
            serde_json::to_string_pretty(&checks).map_err(|error| CliError {
                code: EXIT_DOCTOR,
                message: format!("doctor JSON encoding failed: {error}"),
            })?
        } else {
            render_text(&checks)
        };
        return Ok((output, code));
    }
    let deadline = tokio::time::Instant::now() + OPERATION_TIMEOUT;
    let (request, output) = match parse_command(&args)? {
        Command::Request(request, output) => (request, output),
        Command::Apply {
            path,
            selector,
            acknowledged,
        } => {
            let (profile, migrated) =
                load_selected_profile(Path::new(&path), selector.as_deref(), deadline).await?;
            // Read-only first. Any failure, including an unavailable preview,
            // stops here: nothing is applied without a known impact.
            let preview = match send(
                DaemonRequest::PreviewProfile(profile.clone()),
                deadline,
                "IPC_TIMEOUT: daemon preview timed out; nothing was applied",
            )
            .await?
            {
                DaemonResult::Preview(preview) => preview,
                _ => return Err(CliError::daemon(INVALID_RESPONSE)),
            };
            let reasons = acknowledgement_reasons(&profile, migrated, &preview);
            if !reasons.is_empty() && !acknowledged {
                return Err(CliError {
                    code: EXIT_ACKNOWLEDGEMENT,
                    message: refusal(&path, &reasons),
                });
            }
            // Sent once; a timeout is reported, never replayed.
            (DaemonRequest::ApplyProfile(profile), OutputFormat::Applied)
        }
    };
    let result = send(request, deadline, "IPC_TIMEOUT: daemon request timed out").await?;
    format_result(result, output).map(|output| (output, 0))
}

const INVALID_RESPONSE: &str = "IPC_INVALID_RESPONSE: daemon returned an unexpected result";

async fn send(
    request: DaemonRequest,
    deadline: tokio::time::Instant,
    timeout_message: &'static str,
) -> Result<DaemonResult, CliError> {
    let path = socket_path().map_err(|error| CliError::daemon(error.to_string()))?;
    let client = DaemonClient::new(path);
    let envelope = DaemonEnvelope::new(uuid::Uuid::new_v4().to_string(), request);
    let response = tokio::time::timeout_at(deadline, client.send(envelope))
        .await
        .map_err(|_| CliError::daemon(timeout_message))?
        .map_err(|error| CliError::daemon(error.to_string()))?;
    if let Some(error) = response.error {
        return Err(CliError::daemon(error.to_string()));
    }
    response
        .result
        .ok_or_else(|| CliError::daemon(INVALID_RESPONSE))
}

/// Why this activation needs `--acknowledge-routing-impact`, if at all.
fn acknowledgement_reasons(
    profile: &Profile,
    migrated: bool,
    preview: &RoutingPreview,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if migrated {
        reasons.push(format!(
            "this configuration was migrated from an older schema; check its routing mode (unmatched traffic: {:?})",
            profile.routing.unmatched_traffic
        ));
    }
    if profile.routing.unmatched_traffic == UnmatchedTraffic::UsePrimary {
        reasons.push(
            "mode UsePrimary sends all new traffic that no rule matches through the primary connection"
                .to_owned(),
        );
    }
    let total = preview.warnings.len();
    for warning in preview.warnings.iter().take(LISTED_WARNINGS) {
        reasons.push(match warning {
            RoutingWarning::LocalNetworkRule { rule_id, network } => format!(
                "rule '{}' covers connected network {network}; local or lab access there follows that rule",
                rule_id.as_str()
            ),
            RoutingWarning::AllTrafficRule { rule_id } => {
                format!("rule '{}' matches all traffic", rule_id.as_str())
            }
        });
    }
    if total > LISTED_WARNINGS {
        reasons.push(format!("{} more rule warnings", total - LISTED_WARNINGS));
    }
    reasons
}

fn refusal(path: &str, reasons: &[String]) -> String {
    let mut message = format!(
        "routing impact needs acknowledgement; nothing was applied and {path} was not changed:"
    );
    for reason in reasons {
        message.push_str("\n- ");
        message.push_str(reason);
    }
    message.push_str(&format!("\nre-run with {ACKNOWLEDGE} to proceed"));
    message
}

#[derive(Debug, Clone, Copy)]
enum OutputFormat {
    StatusText,
    StatusJson,
    Applied,
    Deactivated,
}

enum Command {
    Request(DaemonRequest, OutputFormat),
    Apply {
        path: String,
        selector: Option<String>,
        acknowledged: bool,
    },
}

fn parse_command(args: &[String]) -> Result<Command, CliError> {
    match args {
        [command] if command == "status" => Ok(Command::Request(
            DaemonRequest::GetStatus,
            OutputFormat::StatusText,
        )),
        [command, option] if command == "status" && option == "--json" => Ok(Command::Request(
            DaemonRequest::GetStatus,
            OutputFormat::StatusJson,
        )),
        [command, path, options @ ..] if command == "apply-profile" => {
            let mut selector = None;
            let mut acknowledged = false;
            let mut options = options.iter();
            while let Some(option) = options.next() {
                match option.as_str() {
                    "--profile" if selector.is_none() => {
                        selector = Some(options.next().ok_or_else(CliError::usage)?.clone());
                    }
                    ACKNOWLEDGE if !acknowledged => acknowledged = true,
                    _ => return Err(CliError::usage()),
                }
            }
            Ok(Command::Apply {
                path: path.clone(),
                selector,
                acknowledged,
            })
        }
        [command] if command == "deactivate" => Ok(Command::Request(
            DaemonRequest::Deactivate,
            OutputFormat::Deactivated,
        )),
        _ => Err(CliError::usage()),
    }
}

/// The selected profile and whether the document carries the migration
/// notice (`routing_notice_pending`).
async fn load_selected_profile(
    path: &Path,
    selector: Option<&str>,
    deadline: tokio::time::Instant,
) -> Result<(Profile, bool), CliError> {
    let path = path.to_owned();
    let document = tokio::time::timeout_at(
        deadline,
        tokio::task::spawn_blocking(move || load_config_file(&path)),
    )
    .await
    .map_err(|_| CliError::config("configuration read timed out"))?
    .map_err(|error| CliError::config(format!("configuration reader failed: {error}")))?
    .map_err(|error| CliError::config(error.to_string()))?;
    let migrated = document.routing_notice_pending;
    select_profile(document, selector).map(|profile| (profile, migrated))
}

fn select_profile(mut document: ConfigV4, selector: Option<&str>) -> Result<Profile, CliError> {
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

fn validate_profile_identities(document: &ConfigV4) -> Result<(), CliError> {
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

fn format_result(result: DaemonResult, output: OutputFormat) -> Result<String, CliError> {
    match (output, result) {
        (OutputFormat::StatusText, DaemonResult::Status(status)) => Ok(format!(
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
        (OutputFormat::StatusJson, DaemonResult::Status(status)) => serde_json::to_string(&status)
            .map_err(|error| CliError::daemon(format!("IPC_JSON_ENCODE_FAILED: {error}"))),
        (OutputFormat::Applied, DaemonResult::Applied(result)) => {
            Ok(format!("applied generation={}", result.generation))
        }
        (OutputFormat::Deactivated, DaemonResult::Deactivated) => Ok("deactivated".to_owned()),
        _ => Err(CliError::daemon(INVALID_RESPONSE)),
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
