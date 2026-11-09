//! Exercise detection and planning without running package managers or builds.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

struct Installer {
    temp: TempDir,
    bin: PathBuf,
}

impl Installer {
    fn new(os_release: &str) -> Self {
        let temp = TempDir::new().unwrap();
        let bin = temp.path().join("bin");
        fs::create_dir(&bin).unwrap();
        fs::write(temp.path().join("os-release"), os_release).unwrap();
        let fixture = Self { temp, bin };
        for tool in [
            "id",
            "dpkg-query",
            "rpm",
            "cargo",
            "rustc",
            "sudo",
            "apt",
            "apt-get",
            "dnf",
            "routelanectl",
            "systemctl",
            "rustup",
            "curl",
        ] {
            fixture.stub(
                tool,
                r#"
printf '%s' "${0##*/}" >> "$INSTALL_TEST_LOG"
printf ' <%s>' "$@" >> "$INSTALL_TEST_LOG"
printf '\n' >> "$INSTALL_TEST_LOG"
case "${0##*/}" in
    id) echo "${INSTALL_TEST_UID:-1000}" ;;
    rustc) echo "rustc ${INSTALL_TEST_RUST:-1.87.0} (test)" ;;
    dpkg-query)
        case " ${INSTALL_TEST_INSTALLED:-} " in
            *" $3 "*) echo 'install ok installed' ;;
            *) exit 1 ;;
        esac ;;
    rpm)
        case " ${INSTALL_TEST_INSTALLED:-} " in
            *" $2 "*) exit 0 ;;
            *) exit 1 ;;
        esac ;;
    *) echo "Unexpected execution: $0 $*" >&2; exit 99 ;;
esac
"#,
            );
        }
        fixture
    }

    fn stub(&self, name: &str, body: &str) {
        let path = self.bin.join(name);
        fs::write(&path, format!("#!/bin/bash\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn run(&self, options: &[&str], variables: &[(&str, &str)]) -> Output {
        let mut paths = vec![self.bin.clone()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let output = Command::new("bash")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install-from-source.sh"))
            .arg("--dry-run")
            .args(options)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("ROUTELANE_OS_RELEASE", self.temp.path().join("os-release"))
            .env("INSTALL_TEST_LOG", self.temp.path().join("commands"))
            .env("INSTALL_TEST_UID", "1000")
            .env("INSTALL_TEST_RUST", "1.87.0")
            .env("INSTALL_TEST_INSTALLED", "")
            .envs(variables.iter().copied())
            .output()
            .unwrap();
        let log = self.log();
        for forbidden in [
            "sudo",
            "apt",
            "apt-get",
            "dnf",
            "cargo",
            "routelanectl",
            "systemctl",
            "rustup",
            "curl",
        ] {
            assert!(
                !log.lines()
                    .any(|line| line.split_whitespace().next() == Some(forbidden)),
                "dry-run executed {forbidden}: {log}"
            );
        }
        output
    }

    fn log(&self) -> String {
        fs::read_to_string(self.temp.path().join("commands")).unwrap_or_default()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn deb_dry_run_lists_dependencies_and_all_planned_commands() {
    let fixture = Installer::new("ID=ubuntu\nVERSION_ID=24.04\n");
    let output = fixture.run(&[], &[]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let text = stdout(&output);
    for line in [
        "Missing build dependencies: build-essential pkg-config libgtk-4-dev libadwaita-1-dev",
        "[dry-run] sudo apt-get update",
        "[dry-run] sudo apt-get install -y build-essential pkg-config libgtk-4-dev libadwaita-1-dev",
        "[dry-run] bash packaging/deb/build-deb.sh",
        "[dry-run] routelanectl doctor",
    ] {
        assert!(text.contains(line), "{text}");
    }
    let install = text
        .lines()
        .find(|line| line.starts_with("[dry-run] sudo apt install -y "))
        .unwrap();
    assert!(install
        .trim_start_matches("[dry-run] sudo apt install -y ")
        .starts_with('/'));
    for package in [
        "build-essential",
        "pkg-config",
        "libgtk-4-dev",
        "libadwaita-1-dev",
    ] {
        assert!(fixture
            .log()
            .contains(&format!("dpkg-query <-W> <-f=${{Status}}> <{package}>")));
    }
}

#[test]
fn rpm_dry_run_lists_dependencies_and_uses_dnf() {
    let fixture = Installer::new("ID=fedora\nVERSION_ID=44\n");
    let output = fixture.run(&["--yes"], &[]);
    assert!(output.status.success(), "{output:?}");
    let text = stdout(&output);
    assert!(text.contains("[dry-run] sudo dnf install -y gcc pkgconf-pkg-config gtk4-devel libadwaita-devel rpm-build"));
    assert!(text.contains("[dry-run] bash packaging/rpm/build-rpm.sh"));
    assert!(text.contains("[dry-run] routelanectl doctor"));
    assert_eq!(text.matches("[dry-run] sudo dnf install -y ").count(), 2);
    assert!(!text.contains("apt"));
    for package in [
        "gcc",
        "pkgconf-pkg-config",
        "gtk4-devel",
        "libadwaita-devel",
        "rpm-build",
    ] {
        assert!(fixture.log().contains(&format!("rpm <-q> <{package}>")));
    }
}

#[test]
fn installed_dependencies_are_excluded_from_install_commands() {
    for (os, installed, missing) in [
        (
            "ID=debian\nVERSION_ID=13\n",
            "build-essential libgtk-4-dev",
            "pkg-config libadwaita-1-dev",
        ),
        (
            "ID=fedora\nVERSION_ID=43\n",
            "gcc gtk4-devel rpm-build",
            "pkgconf-pkg-config libadwaita-devel",
        ),
    ] {
        let fixture = Installer::new(os);
        let output = fixture.run(&[], &[("INSTALL_TEST_INSTALLED", installed)]);
        assert!(output.status.success(), "{output:?}");
        assert!(stdout(&output).contains(&format!("Missing build dependencies: {missing}\n")));
    }
    let fixture = Installer::new("ID=debian\nVERSION_ID=12\n");
    let output = fixture.run(
        &[],
        &[(
            "INSTALL_TEST_INSTALLED",
            "build-essential pkg-config libgtk-4-dev libadwaita-1-dev",
        )],
    );
    assert!(output.status.success());
    assert!(!stdout(&output).contains("apt-get"));
}

#[test]
fn skip_deps_and_build_only_omit_queries_install_and_doctor() {
    let fixture = Installer::new("ID=debian\nVERSION_ID=12\n");
    let output = fixture.run(&["--skip-deps", "--build-only"], &[]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        stdout(&output),
        "[dry-run] bash packaging/deb/build-deb.sh\n"
    );
    assert!(!fixture.log().contains("dpkg-query"));
}

#[test]
fn id_like_selects_family_and_unvalidated_releases_warn() {
    for (os, family) in [
        ("ID=mint\nID_LIKE=\"ubuntu debian\"\nVERSION_ID=99\n", "deb"),
        ("ID=derivative\nID_LIKE='fedora'\nVERSION_ID=99\n", "rpm"),
        ("ID=ubuntu\nVERSION_ID=99.04\n", "deb"),
    ] {
        let fixture = Installer::new(os);
        let output = fixture.run(&["--skip-deps", "--build-only"], &[]);
        assert!(output.status.success(), "{output:?}");
        assert!(stdout(&output).contains(&format!("packaging/{family}/build-{family}.sh")));
        assert!(String::from_utf8_lossy(&output.stderr).contains("not a validated release"));
    }
}

#[test]
fn unsupported_distribution_and_root_have_usage_exit_code() {
    let fixture = Installer::new("ID=arch\nVERSION_ID=rolling\n");
    let output = fixture.run(&[], &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("README manual build"));
    assert!(!stdout(&output).contains("[dry-run]"));
    let fixture = Installer::new("ID=ubuntu\nVERSION_ID=24.04\n");
    let output = fixture.run(&[], &[("INSTALL_TEST_UID", "0")]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("run as your user; sudo is used only for the package manager"));
    assert_eq!(fixture.log(), "id <-u>\n");
}

#[test]
fn rust_must_meet_cargo_msrv_and_never_runs_rustup() {
    for (version, expected) in [
        ("1.86.9", 3),
        ("invalid", 3),
        ("1.87.0", 0),
        ("1.100.0", 0),
        ("2.0.0", 0),
    ] {
        let fixture = Installer::new("ID=ubuntu\nVERSION_ID=24.04\n");
        let output = fixture.run(
            &["--skip-deps", "--build-only"],
            &[("INSTALL_TEST_RUST", version)],
        );
        assert_eq!(output.status.code(), Some(expected), "{output:?}");
        if expected == 3 {
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains("Rust >= 1.87 is required"));
            assert!(
                error.contains("curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh")
            );
            assert!(error.contains("rustup update stable"));
            assert!(!stdout(&output).contains("[dry-run]"));
        }
    }
}

#[test]
fn help_and_unknown_options_are_handled_before_detection() {
    let fixture = Installer::new("ID=arch\n");
    let output = fixture.run(&["--help"], &[]);
    assert!(output.status.success());
    for option in [
        "--yes",
        "--dry-run",
        "--skip-deps",
        "--build-only",
        "--help",
    ] {
        assert!(stdout(&output).contains(option));
    }
    let output = fixture.run(&["--unknown"], &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
    assert!(fixture.log().is_empty());
}

#[test]
fn os_release_is_data_and_is_not_executed() {
    let fixture =
        Installer::new("ID=ubuntu\nVERSION_ID=24.04\nIGNORED=$(sudo touch /should-not-exist)\n");
    let output = fixture.run(&["--skip-deps", "--build-only"], &[]);
    assert!(output.status.success(), "{output:?}");
}
