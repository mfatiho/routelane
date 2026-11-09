//! Read-only installation checks. All system access is isolated behind DoctorProbe.

use std::fmt::Write;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde::Serialize;

use crate::ipc::{socket_path, DaemonClient, DaemonEnvelope, DaemonRequest, DaemonResult};

const HELPER_DIR: &str = "/usr/lib/routelane";
const HELPER: &str = "/usr/lib/routelane/routelane-helper";
const POLICY: &str = "/usr/share/polkit-1/actions/io.github.routelane.policy";
const ACTION: &str = "io.github.routelane.apply";
const BUS_NAME: &str = "io.github.routelane.Helper";
const CONTROLLERS: &str = "/sys/fs/cgroup/cgroup.controllers";
const APP_CGROUP: &str = "/sys/fs/cgroup/routelane.apps";
const CLIENTS: &[&str] = &[
    "/usr/bin/routelane",
    "/usr/bin/routelane-daemon",
    "/usr/bin/routelanectl",
];
const INTEGRATION: &[&str] = &[
    "/usr/lib/systemd/system/routelane-helper.service",
    "/usr/lib/systemd/user/routelane-daemon.service",
    "/usr/share/dbus-1/system-services/io.github.routelane.Helper.service",
    "/usr/share/dbus-1/system.d/io.github.routelane.Helper.conf",
    POLICY,
    "/usr/lib/tmpfiles.d/routelane.conf",
];
const REINSTALL: &str = "Reinstall the native RouteLane package with your package manager.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub status: CheckStatus,
    pub detail: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FileFacts {
    pub is_file: bool,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
}

#[derive(Debug, Clone)]
pub struct CommandResult {
    pub success: bool,
    pub output: String,
}

#[derive(Debug, Clone)]
pub struct DbusFacts {
    pub activatable: bool,
    pub has_owner: bool,
}

pub trait DoctorProbe {
    fn metadata(&self, path: &Path) -> Option<FileFacts>;
    fn resolved_metadata(&self, path: &Path) -> Option<FileFacts>;
    fn read_to_string(&self, path: &Path) -> Option<String>;
    fn run(&self, program: &str, args: &[&str]) -> Option<CommandResult>;
    fn dbus_helper(&self) -> Result<DbusFacts, String>;
    fn daemon_status(&self) -> Result<String, String>;
}

/// Uses lstat/stat, version/action queries, bus introspection, and daemon GetStatus only.
/// Doctor itself sends only GetStatus to the daemon, never calls a helper method,
/// and changes nothing. The daemon uses its normal read-only helper Inspect path,
/// as for routelanectl status and the GUI, which may D-Bus-activate the helper.
/// Call from a synchronous context (or tokio::task::spawn_blocking).
pub struct SystemProbe;

impl DoctorProbe for SystemProbe {
    fn metadata(&self, path: &Path) -> Option<FileFacts> {
        let metadata = std::fs::symlink_metadata(path).ok()?;
        Some(FileFacts {
            is_file: metadata.is_file(),
            is_dir: metadata.is_dir(),
            is_symlink: metadata.file_type().is_symlink(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode(),
        })
    }

    fn resolved_metadata(&self, path: &Path) -> Option<FileFacts> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(FileFacts {
            is_file: metadata.is_file(),
            is_dir: metadata.is_dir(),
            is_symlink: metadata.file_type().is_symlink(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode(),
        })
    }

    fn read_to_string(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn run(&self, program: &str, args: &[&str]) -> Option<CommandResult> {
        match Command::new(program).args(args).output() {
            Ok(output) => Some(CommandResult {
                success: output.status.success(),
                output: format!(
                    "{} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => Some(CommandResult {
                success: false,
                output: error.to_string(),
            }),
        }
    }

    fn dbus_helper(&self) -> Result<DbusFacts, String> {
        with_timeout(async {
            let connection = zbus::Connection::system()
                .await
                .map_err(|e| e.to_string())?;
            let proxy = zbus::fdo::DBusProxy::new(&connection)
                .await
                .map_err(|e| e.to_string())?;
            let names = proxy
                .list_activatable_names()
                .await
                .map_err(|e| e.to_string())?;
            let name = zbus::names::BusName::try_from(BUS_NAME).map_err(|e| e.to_string())?;
            let has_owner = proxy
                .name_has_owner(name)
                .await
                .map_err(|e| e.to_string())?;
            Ok(DbusFacts {
                activatable: names.iter().any(|name| name.as_str() == BUS_NAME),
                has_owner,
            })
        })
    }

    /// Sends only GetStatus, without calling helper methods or changing anything.
    /// The daemon's normal read-only helper Inspect path (also used by routelanectl
    /// status and the GUI) may D-Bus-activate the helper while answering.
    fn daemon_status(&self) -> Result<String, String> {
        with_timeout(async {
            let path = socket_path().map_err(|e| e.to_string())?;
            let envelope =
                DaemonEnvelope::new(uuid::Uuid::new_v4().to_string(), DaemonRequest::GetStatus);
            let response = DaemonClient::new(path)
                .send(envelope)
                .await
                .map_err(|e| e.to_string())?;
            if let Some(error) = response.error {
                return Err(error.to_string());
            }
            match response.result {
                Some(DaemonResult::Status(status)) => Ok(format!(
                    "state={:?} generation={} applied_profile={}",
                    status.health.state,
                    status.generation,
                    status
                        .applied_profile_id
                        .as_ref()
                        .map_or("-", |id| id.as_str())
                )),
                _ => Err("daemon returned an unexpected response to GetStatus".to_owned()),
            }
        })
    }
}

fn with_timeout<T>(
    future: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), future)
            .await
            .map_err(|_| "check timed out after 2 seconds".to_owned())?
    })
}

fn check(id: &'static str, status: CheckStatus, detail: impl Into<String>, hint: &str) -> Check {
    Check {
        id,
        status,
        detail: detail.into(),
        hint: (status != CheckStatus::Pass).then(|| hint.to_owned()),
    }
}

fn secure_root_file(facts: &FileFacts) -> bool {
    facts.is_file && !facts.is_symlink && facts.uid == 0 && facts.mode & 0o022 == 0
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn run_checks(probe: &dyn DoctorProbe) -> Vec<Check> {
    use CheckStatus::{Fail, Pass, Warn};
    let mut checks = Vec::with_capacity(9);
    let helper = probe.metadata(Path::new(HELPER));
    let directory = probe.metadata(Path::new(HELPER_DIR));
    let safe_helper = helper
        .as_ref()
        .is_some_and(|facts| secure_root_file(facts) && facts.gid == 0 && facts.mode & 0o100 != 0);
    let safe_dir = directory.as_ref().is_some_and(|facts| {
        facts.is_dir && !facts.is_symlink && facts.uid == 0 && facts.mode & 0o022 == 0
    });
    checks.push(check(
        "helper_binary",
        if safe_helper && safe_dir { Pass } else { Fail },
        if safe_helper && safe_dir {
            format!(
                "{HELPER} root:root {:04o}; parent directory is secure",
                helper.unwrap().mode & 0o7777
            )
        } else {
            format!(
                "{HELPER} or {HELPER_DIR} is missing or has unsafe type, ownership, or permissions"
            )
        },
        REINSTALL,
    ));

    let invalid_clients: Vec<_> = CLIENTS
        .iter()
        .filter(|path| {
            !probe
                .metadata(Path::new(path))
                .is_some_and(|f| f.is_file && f.mode & 0o111 != 0)
        })
        .copied()
        .collect();
    checks.push(check(
        "client_binaries",
        if invalid_clients.is_empty() {
            Pass
        } else {
            Fail
        },
        if invalid_clients.is_empty() {
            CLIENTS.join(", ")
        } else {
            format!(
                "missing or non-executable regular files: {}",
                invalid_clients.join(", ")
            )
        },
        REINSTALL,
    ));

    let invalid_integration: Vec<_> = INTEGRATION
        .iter()
        .filter(|path| {
            !probe
                .metadata(Path::new(path))
                .as_ref()
                .is_some_and(secure_root_file)
        })
        .copied()
        .collect();
    checks.push(check(
        "integration_files",
        if invalid_integration.is_empty() {
            Pass
        } else {
            Fail
        },
        if invalid_integration.is_empty() {
            "all six integration files are root-owned and not group/other writable".to_owned()
        } else {
            format!(
                "missing or unsafe integration files: {}",
                invalid_integration.join(", ")
            )
        },
        REINSTALL,
    ));

    let policy_contains_action = probe
        .read_to_string(Path::new(POLICY))
        .is_some_and(|s| s.contains(ACTION));
    let (status, detail, hint) = if !policy_contains_action {
        (
            Fail,
            format!("{POLICY} is unreadable or does not contain {ACTION}"),
            REINSTALL,
        )
    } else {
        match probe.run("pkaction", &["--action-id", ACTION]) {
            Some(result) if result.success => (Pass, format!("{ACTION} is registered"), ""),
            Some(result) => (
                Fail,
                format!(
                    "pkaction rejected {ACTION}: {}",
                    single_line(&result.output)
                ),
                REINSTALL,
            ),
            None => (
                Warn,
                format!("{ACTION} is in the policy; pkaction is unavailable"),
                "Install the distribution's Polkit tools, then rerun routelanectl doctor.",
            ),
        }
    };
    checks.push(check("polkit_action", status, detail, hint));

    let (status, detail) = match probe.dbus_helper() {
        Ok(facts) => (
            if facts.activatable { Pass } else { Fail },
            format!(
                "{BUS_NAME} activatable={} currently_has_owner={}",
                facts.activatable, facts.has_owner
            ),
        ),
        Err(error) => (
            Fail,
            format!("system bus check failed: {}", single_line(&error)),
        ),
    };
    checks.push(check("dbus_activation", status, detail,
        "Ensure the system D-Bus service is available and reinstall the RouteLane package's D-Bus activation files."));

    let mut tools_ok = true;
    let mut versions = Vec::new();
    for (program, argument) in [("/usr/sbin/nft", "--version"), ("/usr/sbin/ip", "-V")] {
        if !probe
            .resolved_metadata(Path::new(program))
            .is_some_and(|f| f.is_file && f.mode & 0o111 != 0)
        {
            tools_ok = false;
            versions.push(format!(
                "{program} missing or not an executable regular file"
            ));
        } else {
            let version = probe.run(program, &[argument]);
            versions.push(match version {
                Some(result) if result.success => {
                    format!("{program}: {}", single_line(&result.output))
                }
                _ => format!("{program} exists (version unavailable)"),
            });
        }
    }
    checks.push(check(
        "kernel_tools",
        if tools_ok { Pass } else { Fail },
        versions.join("; "),
        "Install nftables and iproute2 (Debian/Ubuntu) or nftables and iproute (Fedora).",
    ));

    let controllers_exist = probe.metadata(Path::new(CONTROLLERS)).is_some();
    checks.push(check("cgroup_v2", if controllers_exist { Pass } else { Fail },
        format!("{CONTROLLERS} {}", if controllers_exist { "exists" } else { "missing" }),
        "Boot with a unified cgroup v2 hierarchy; consult your distribution's boot configuration documentation."));
    let apps_exist = probe
        .metadata(Path::new(APP_CGROUP))
        .is_some_and(|facts| facts.is_dir);
    checks.push(check(
        "app_cgroup",
        if apps_exist { Pass } else { Warn },
        format!(
            "{APP_CGROUP} {}",
            if apps_exist {
                "exists"
            } else {
                "missing; only application rules are affected"
            }
        ),
        "sudo systemd-tmpfiles --create /usr/lib/tmpfiles.d/routelane.conf",
    ));

    let (status, detail) = match probe.daemon_status() {
        Ok(summary) => (Pass, single_line(&summary)),
        Err(error) => (
            Warn,
            format!("GetStatus unavailable: {}", single_line(&error)),
        ),
    };
    checks.push(check("user_daemon", status, detail,
        "Open the GUI (it starts the daemon), or run systemctl --user start routelane-daemon.service."));
    checks
}

pub fn render_text(checks: &[Check]) -> String {
    let mut output = String::new();
    let mut passed = 0;
    let mut warnings = 0;
    let mut failed = 0;
    for check in checks {
        let status = match check.status {
            CheckStatus::Pass => {
                passed += 1;
                "PASS"
            }
            CheckStatus::Warn => {
                warnings += 1;
                "WARN"
            }
            CheckStatus::Fail => {
                failed += 1;
                "FAIL"
            }
        };
        writeln!(
            output,
            "{status}  {}  {}",
            check.id,
            single_line(&check.detail)
        )
        .unwrap();
        if check.status != CheckStatus::Pass {
            if let Some(hint) = &check.hint {
                writeln!(output, "      fix: {}", single_line(hint)).unwrap();
            }
        }
    }
    write!(
        output,
        "doctor: {passed} passed, {warnings} warnings, {failed} failed"
    )
    .unwrap();
    output
}

pub fn any_failed(checks: &[Check]) -> bool {
    checks.iter().any(|check| check.status == CheckStatus::Fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct FakeProbe {
        files: HashMap<String, FileFacts>,
        resolved_files: HashMap<String, Option<FileFacts>>,
        policy: Option<String>,
        action: Option<CommandResult>,
        versions: Option<CommandResult>,
        dbus: Result<DbusFacts, String>,
        daemon: Result<String, String>,
    }

    fn file() -> FileFacts {
        FileFacts {
            is_file: true,
            is_dir: false,
            is_symlink: false,
            uid: 0,
            gid: 0,
            mode: 0o755,
        }
    }

    fn directory() -> FileFacts {
        FileFacts {
            is_file: false,
            is_dir: true,
            ..file()
        }
    }

    impl FakeProbe {
        fn healthy() -> Self {
            let mut files = HashMap::new();
            for path in CLIENTS.iter().chain(INTEGRATION).copied().chain([
                HELPER,
                CONTROLLERS,
                "/usr/sbin/nft",
                "/usr/sbin/ip",
            ]) {
                files.insert(path.to_owned(), file());
            }
            files.insert(HELPER_DIR.to_owned(), directory());
            files.insert(APP_CGROUP.to_owned(), directory());
            Self {
                files,
                resolved_files: HashMap::new(),
                policy: Some(format!("<action id=\"{ACTION}\">")),
                action: Some(CommandResult {
                    success: true,
                    output: ACTION.to_owned(),
                }),
                versions: Some(CommandResult {
                    success: true,
                    output: "tool version 1\n".to_owned(),
                }),
                dbus: Ok(DbusFacts {
                    activatable: true,
                    has_owner: false,
                }),
                daemon: Ok("state=Inactive generation=0".to_owned()),
            }
        }
    }

    impl DoctorProbe for FakeProbe {
        fn metadata(&self, path: &Path) -> Option<FileFacts> {
            self.files.get(path.to_str().unwrap()).cloned()
        }
        fn resolved_metadata(&self, path: &Path) -> Option<FileFacts> {
            self.resolved_files
                .get(path.to_str().unwrap())
                .cloned()
                .unwrap_or_else(|| self.metadata(path).filter(|facts| !facts.is_symlink))
        }
        fn read_to_string(&self, path: &Path) -> Option<String> {
            assert_eq!(path, Path::new(POLICY));
            self.policy.clone()
        }
        fn run(&self, program: &str, args: &[&str]) -> Option<CommandResult> {
            match program {
                "pkaction" => {
                    assert_eq!(args, ["--action-id", ACTION]);
                    self.action.clone()
                }
                "/usr/sbin/nft" => {
                    assert_eq!(args, ["--version"]);
                    self.versions.clone()
                }
                "/usr/sbin/ip" => {
                    assert_eq!(args, ["-V"]);
                    self.versions.clone()
                }
                _ => panic!("unexpected command {program}"),
            }
        }
        fn dbus_helper(&self) -> Result<DbusFacts, String> {
            self.dbus.clone()
        }
        fn daemon_status(&self) -> Result<String, String> {
            self.daemon.clone()
        }
    }

    fn assert_status(probe: &FakeProbe, id: &str, status: CheckStatus) {
        let checks = run_checks(probe);
        let check = checks.iter().find(|check| check.id == id).unwrap();
        assert_eq!(check.status, status, "{check:?}");
        assert_eq!(check.hint.is_some(), status != CheckStatus::Pass);
        if let Some(hint) = &check.hint {
            assert!(!hint.is_empty());
        }
    }

    #[test]
    fn doctor_passes_all_checks_in_fixed_order_without_activating_helper() {
        let probe = FakeProbe::healthy();
        let checks = run_checks(&probe);
        assert_eq!(
            checks.iter().map(|c| c.id).collect::<Vec<_>>(),
            [
                "helper_binary",
                "client_binaries",
                "integration_files",
                "polkit_action",
                "dbus_activation",
                "kernel_tools",
                "cgroup_v2",
                "app_cgroup",
                "user_daemon"
            ]
        );
        assert!(checks
            .iter()
            .all(|c| c.status == CheckStatus::Pass && c.hint.is_none()));
        assert!(!any_failed(&checks));
        assert!(checks[4].detail.contains("currently_has_owner=false"));
        assert!(checks[5].detail.contains("tool version 1"));
        assert!(render_text(&checks).ends_with("doctor: 9 passed, 0 warnings, 0 failed"));
    }

    #[test]
    fn doctor_helper_rejects_missing_files_and_unsafe_ownership_types_and_modes() {
        for path in [HELPER, HELPER_DIR] {
            let mut probe = FakeProbe::healthy();
            probe.files.remove(path);
            assert_status(&probe, "helper_binary", CheckStatus::Fail);
            for variant in 0..7 {
                let mut probe = FakeProbe::healthy();
                let facts = probe.files.get_mut(path).unwrap();
                match variant {
                    0 => facts.uid = 1000,
                    1 => facts.mode |= 0o020,
                    2 => facts.mode |= 0o002,
                    3 => facts.is_symlink = true,
                    4 => {
                        facts.is_dir = false;
                        facts.is_file = false;
                    }
                    5 if path == HELPER => facts.gid = 1000,
                    6 if path == HELPER => facts.mode &= !0o100,
                    _ => continue,
                }
                assert_status(&probe, "helper_binary", CheckStatus::Fail);
            }
        }
    }

    #[test]
    fn doctor_clients_and_integration_fail_for_each_missing_or_invalid_file() {
        for (paths, id) in [
            (CLIENTS, "client_binaries"),
            (INTEGRATION, "integration_files"),
        ] {
            for path in paths {
                let mut probe = FakeProbe::healthy();
                probe.files.remove(*path);
                assert_status(&probe, id, CheckStatus::Fail);
                probe.files.insert((*path).to_owned(), directory());
                assert_status(&probe, id, CheckStatus::Fail);
            }
        }
        for path in INTEGRATION {
            for variant in 0..4 {
                let mut probe = FakeProbe::healthy();
                let facts = probe.files.get_mut(*path).unwrap();
                match variant {
                    0 => facts.uid = 1000,
                    1 => facts.mode |= 0o020,
                    2 => facts.mode |= 0o002,
                    _ => facts.is_symlink = true,
                }
                assert_status(&probe, "integration_files", CheckStatus::Fail);
            }
        }
    }

    #[test]
    fn doctor_client_binaries_reject_non_executable_regular_files() {
        for path in CLIENTS {
            let mut probe = FakeProbe::healthy();
            probe.files.get_mut(*path).unwrap().mode = 0o644;
            assert_status(&probe, "client_binaries", CheckStatus::Fail);
        }
    }

    #[test]
    fn doctor_polkit_covers_missing_policy_action_failure_and_missing_tool() {
        let mut probe = FakeProbe::healthy();
        probe.policy = None;
        assert_status(&probe, "polkit_action", CheckStatus::Fail);
        probe.policy = Some("<policyconfig/>".to_owned());
        assert_status(&probe, "polkit_action", CheckStatus::Fail);
        probe.policy = Some(ACTION.to_owned());
        probe.action.as_mut().unwrap().success = false;
        assert_status(&probe, "polkit_action", CheckStatus::Fail);
        probe.action = None;
        assert_status(&probe, "polkit_action", CheckStatus::Warn);
        probe.policy = None;
        assert_status(&probe, "polkit_action", CheckStatus::Fail);
    }

    #[test]
    fn doctor_dbus_covers_owned_unowned_unregistered_and_unreachable_bus() {
        let mut probe = FakeProbe::healthy();
        probe.dbus = Ok(DbusFacts {
            activatable: true,
            has_owner: true,
        });
        assert_status(&probe, "dbus_activation", CheckStatus::Pass);
        assert!(run_checks(&probe)[4]
            .detail
            .contains("currently_has_owner=true"));
        for has_owner in [true, false] {
            probe.dbus = Ok(DbusFacts {
                activatable: false,
                has_owner,
            });
            assert_status(&probe, "dbus_activation", CheckStatus::Fail);
        }
        probe.dbus = Err("bus unavailable".to_owned());
        assert_status(&probe, "dbus_activation", CheckStatus::Fail);
    }

    #[test]
    fn doctor_kernel_tools_require_both_paths_but_versions_are_optional() {
        for path in ["/usr/sbin/nft", "/usr/sbin/ip"] {
            let mut probe = FakeProbe::healthy();
            probe.files.remove(path);
            assert_status(&probe, "kernel_tools", CheckStatus::Fail);
        }
        let mut probe = FakeProbe::healthy();
        probe.versions.as_mut().unwrap().success = false;
        assert_status(&probe, "kernel_tools", CheckStatus::Pass);
        probe.versions = None;
        assert_status(&probe, "kernel_tools", CheckStatus::Pass);
        assert!(run_checks(&probe)[5].detail.contains("version unavailable"));
    }

    #[test]
    fn doctor_kernel_tools_reject_dangling_symlinks() {
        for path in ["/usr/sbin/nft", "/usr/sbin/ip"] {
            let mut probe = FakeProbe::healthy();
            probe.files.insert(
                path.to_owned(),
                FileFacts {
                    is_file: false,
                    is_symlink: true,
                    mode: 0o777,
                    ..file()
                },
            );
            probe.resolved_files.insert(path.to_owned(), None);
            assert_status(&probe, "kernel_tools", CheckStatus::Fail);
        }
    }

    #[test]
    fn doctor_kernel_tools_accept_symlinks_to_executable_regular_files() {
        for path in ["/usr/sbin/nft", "/usr/sbin/ip"] {
            let mut probe = FakeProbe::healthy();
            probe.files.insert(
                path.to_owned(),
                FileFacts {
                    is_file: false,
                    is_symlink: true,
                    mode: 0o777,
                    ..file()
                },
            );
            probe.resolved_files.insert(path.to_owned(), Some(file()));
            assert_status(&probe, "kernel_tools", CheckStatus::Pass);
        }
    }

    #[test]
    fn doctor_kernel_tools_reject_non_executable_regular_files() {
        for path in ["/usr/sbin/nft", "/usr/sbin/ip"] {
            let mut probe = FakeProbe::healthy();
            probe.files.get_mut(path).unwrap().mode = 0o644;
            assert_status(&probe, "kernel_tools", CheckStatus::Fail);
        }
    }

    #[test]
    fn doctor_cgroups_and_daemon_cover_failures_and_actionable_warnings() {
        let mut probe = FakeProbe::healthy();
        probe.files.remove(CONTROLLERS);
        assert_status(&probe, "cgroup_v2", CheckStatus::Fail);
        probe.files.remove(APP_CGROUP);
        assert_status(&probe, "app_cgroup", CheckStatus::Warn);
        probe.files.insert(APP_CGROUP.to_owned(), file());
        assert_status(&probe, "app_cgroup", CheckStatus::Warn);
        probe.daemon = Err("timed out".to_owned());
        assert_status(&probe, "user_daemon", CheckStatus::Warn);
        let checks = run_checks(&probe);
        assert!(any_failed(&checks));
        assert!(render_text(&checks).ends_with("doctor: 6 passed, 2 warnings, 1 failed"));
        assert_eq!(render_text(&checks).matches("      fix: ").count(), 3);
        let json = serde_json::to_value(&checks).unwrap();
        assert_eq!(json[6]["status"], "FAIL");
        assert_eq!(json[7]["status"], "WARN");
        assert_eq!(json[0]["status"], "PASS");
        probe.files.insert(CONTROLLERS.to_owned(), file());
        assert!(
            !any_failed(&run_checks(&probe)),
            "warnings alone must exit successfully"
        );
    }
}
