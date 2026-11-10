//! Packaging contracts: the deb and rpm packages ship the same owned files
//! with the same modes and lifecycle, the helper stays a hardened root
//! service, and upgrades never touch private config or the ownership journal.

use std::fs;
use std::path::Path;

use routelane::backend::RUNTIME_PROTOCOL_VERSION;
use routelane::ipc::IPC_PROTOCOL_VERSION;

const HELPER_PATH: &str = "/usr/lib/routelane/routelane-helper";

/// Every path either package installs, with its mode.
const OWNED_FILES: &[(&str, &str)] = &[
    ("/usr/bin/routelane", "0755"),
    ("/usr/bin/routelane-daemon", "0755"),
    ("/usr/bin/routelanectl", "0755"),
    (HELPER_PATH, "0755"),
    ("/usr/lib/systemd/user/routelane-daemon.service", "0644"),
    ("/usr/lib/systemd/system/routelane-helper.service", "0644"),
    ("/usr/lib/tmpfiles.d/routelane.conf", "0644"),
    (
        "/usr/share/dbus-1/system-services/io.github.routelane.Helper.service",
        "0644",
    ),
    (
        "/usr/share/dbus-1/system.d/io.github.routelane.Helper.conf",
        "0644",
    ),
    (
        "/usr/share/applications/io.github.routelane.desktop",
        "0644",
    ),
    (
        "/usr/share/polkit-1/actions/io.github.routelane.policy",
        "0644",
    ),
];

fn read(path: &str) -> String {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    fs::read_to_string(&full).unwrap_or_else(|error| panic!("{}: {error}", full.display()))
}

fn deb_script() -> String {
    read("packaging/deb/build-deb.sh")
}

fn rpm_spec() -> String {
    read("packaging/rpm/routelane.spec")
}

/// `(destination, mode)` of every `install -m MODE SRC DEST` in the deb
/// build script; continuation lines are joined first.
fn deb_installs() -> Vec<(String, String)> {
    let script = deb_script().replace("\\\n", " ");
    script
        .lines()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            let ["install", "-m", mode, _source, destination] = words.as_slice() else {
                return None;
            };
            let destination = destination.trim_matches('"');
            let path = destination.strip_prefix("${PACKAGE_ROOT}")?;
            (!path.starts_with("/DEBIAN/")).then(|| (path.to_owned(), (*mode).to_owned()))
        })
        .collect()
}

/// `(destination, mode)` of every `install -D -m MODE SRC %{buildroot}DEST`
/// in the rpm spec's `%install` section.
fn rpm_installs() -> Vec<(String, String)> {
    let spec = rpm_spec().replace("\\\n", " ");
    spec.lines()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            let ["install", "-D", "-m", mode, _source, destination] = words.as_slice() else {
                return None;
            };
            let path = destination.strip_prefix("%{buildroot}")?;
            Some((path.to_owned(), (*mode).to_owned()))
        })
        .collect()
}

/// Files listed in the spec's final `%files` section (attributes stripped,
/// `%dir` entries excluded).
fn rpm_files() -> Vec<String> {
    let spec = rpm_spec();
    let section = spec
        .split("\n%files\n")
        .nth(1)
        .expect("spec ends with a %files section");
    section
        .lines()
        .filter(|line| !line.starts_with("%dir"))
        .filter_map(|line| line.split_whitespace().last())
        .filter(|path| path.starts_with('/'))
        .map(str::to_owned)
        .collect()
}

fn sorted(mut entries: Vec<(String, String)>) -> Vec<(String, String)> {
    entries.sort();
    entries
}

fn expected() -> Vec<(String, String)> {
    sorted(
        OWNED_FILES
            .iter()
            .map(|(path, mode)| ((*path).to_owned(), (*mode).to_owned()))
            .collect(),
    )
}

fn unit_values<'a>(unit: &'a str, key: &str) -> Vec<&'a str> {
    unit.lines()
        .filter_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .collect()
}

#[test]
fn readme_documents_selective_scope_and_protocol_versions() {
    let readme = read("README.md");
    for fact in [
        "--acknowledge-routing-impact",
        "PreserveSystem",
        "UsePrimary",
        "DAEMON_HELPER_PROTOCOL_MISMATCH",
        "IPC_UNSUPPORTED_PROTOCOL_VERSION",
        "DAEMON_LEGACY_CLEANUP_REQUIRED",
        "DAEMON_PREVIEW_UNAVAILABLE",
        "docs/selective-routing-verification.md",
        "schema version 4",
        "IPC protocol 4",
        "runtime protocol 3",
        "helper journal version 3",
        "exit code 6",
        "Ubuntu 22.04/24.04/26.04",
        "Debian 12/13",
        "Fedora 43/44",
        "usage-settings.json",
        "events-v1.jsonl",
        "never stops or throttles traffic",
    ] {
        assert!(readme.contains(fact), "README must document {fact}");
    }
    assert_eq!(IPC_PROTOCOL_VERSION, 4);
    assert_eq!(RUNTIME_PROTOCOL_VERSION, 3);
    assert_eq!(routelane::config::CURRENT_SCHEMA_VERSION, 4);
}

#[test]
fn packages_ship_helper2_policy_and_consistent_versions() {
    // Each package installs every owned file exactly once, with one mode.
    assert_eq!(sorted(deb_installs()), expected(), "deb file list");
    assert_eq!(sorted(rpm_installs()), expected(), "rpm %install list");
    let mut files = rpm_files();
    files.sort();
    let mut owned: Vec<String> = OWNED_FILES.iter().map(|(p, _)| (*p).to_owned()).collect();
    owned.sort();
    assert_eq!(files, owned, "rpm %files list");
    assert!(rpm_spec().contains("%dir %attr(0755, root, root) /usr/lib/routelane\n"));

    // policykit-1 is transitional from Ubuntu 24.04/Debian 12 (and pulls in
    // pkexec); Ubuntu 22.04 has only policykit-1.
    assert!(deb_script().contains(
        "Depends: libgtk-4-1, libadwaita-1-0, nftables, iproute2, polkitd | policykit-1, dbus, systemd\n"
    ));
    assert!(rpm_spec().contains("Requires:       polkit\n"));

    // Daemon IPC and helper runtime have independent protocol generations.
    assert_eq!((IPC_PROTOCOL_VERSION, RUNTIME_PROTOCOL_VERSION), (4, 3));

    // Both packages take their version from Cargo.toml the same way.
    let cargo_version = read("Cargo.toml")
        .lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .map(|rest| rest.trim_end_matches('"').to_owned())
        .unwrap();
    assert_eq!(cargo_version, env!("CARGO_PKG_VERSION"));
    let rpm_build = read("packaging/rpm/build-rpm.sh");
    assert!(rpm_build.contains("$1 ~ /^version[[:space:]]*=/"));
    assert!(deb_script().contains("$1 ~ /^version[[:space:]]*=/"));
    assert!(rpm_build.contains("--define \"routelane_version ${VERSION}\""));
    assert!(rpm_spec().contains("Version:        %{routelane_version}"));

    // The helper stays root; the bus name is owned only by root.
    let unit = read("data/routelane-helper.service");
    assert_eq!(unit_values(&unit, "User"), ["root"]);
    assert_eq!(unit_values(&unit, "ExecStart"), [HELPER_PATH]);
    let activation = read("data/io.github.routelane.Helper.service");
    assert!(activation.contains("User=root"));
    assert!(activation.contains(&format!("Exec={HELPER_PATH}")));

    // The bus policy lets anyone reach Helper1/Helper2, each exactly once;
    // authorization is enforced per method (Polkit), never by the bus.
    let policy = read("data/io.github.routelane.Helper.conf");
    for interface in ["io.github.routelane.Helper1", "io.github.routelane.Helper2"] {
        assert_eq!(
            policy
                .matches(&format!("send_interface=\"{interface}\""))
                .count(),
            1,
            "{interface}"
        );
    }
    assert_eq!(policy.matches("<allow own=").count(), 1);
    assert!(
        policy.contains("<policy user=\"root\">\n    <allow own=\"io.github.routelane.Helper\"/>")
    );
    assert!(!policy.contains("send_type=\"method_call\"/>"));
    assert!(!policy.contains("eavesdrop"));
    let helper = read("src/bin/routelane_helper.rs");
    assert!(helper.contains("#[zbus::interface(name = \"io.github.routelane.Helper2\")]"));
    let actions = read("data/io.github.routelane.policy");
    assert!(actions.contains("<allow_any>no</allow_any>"));
    assert!(actions.contains("<allow_inactive>no</allow_inactive>"));
    assert!(actions.contains("<allow_active>auth_admin_keep</allow_active>"));
}

#[test]
fn service_keeps_kernel_tunable_protection() {
    let unit = read("data/routelane-helper.service");
    assert!(unit.contains("ProtectKernelTunables=yes"));
    assert!(!unit.contains("ProtectKernelTunables=no"));
    for key in [
        "NoNewPrivileges=yes",
        "ProtectSystem=strict",
        "ProtectKernelModules=yes",
        "ProtectControlGroups=yes",
    ] {
        assert!(unit.contains(key), "{key}");
    }
    // Only Task 6's interface-setting subtree and the dedicated application
    // cgroup are writable; nothing broader under /proc/sys or /sys.
    let mut writable = unit_values(&unit, "ReadWritePaths");
    writable.sort_unstable();
    assert_eq!(
        writable,
        ["-/sys/fs/cgroup/routelane.apps", "/proc/sys/net/ipv4/conf"]
    );
    assert_eq!(
        unit_values(&unit, "RestrictAddressFamilies"),
        ["AF_UNIX AF_NETLINK"]
    );
    // Packages install the unit verbatim; neither ships a drop-in.
    for packaging in [deb_script(), rpm_spec()] {
        assert!(!packaging.contains("routelane-helper.service.d"));
        assert!(!packaging.contains("ProtectKernelTunables"));
    }
}

#[test]
fn package_upgrade_preserves_private_config_and_journal() {
    // A helper restart during upgrade keeps the ownership journal.
    let unit = read("data/routelane-helper.service");
    assert_eq!(unit_values(&unit, "RuntimeDirectory"), ["routelane"]);
    assert_eq!(unit_values(&unit, "RuntimeDirectoryPreserve"), ["yes"]);

    let deb = deb_script();
    let postinst = read("packaging/deb/postinst");
    let spec = rpm_spec();
    for script in [&deb, &postinst, &spec] {
        // Lifecycle scripts never run a command that deletes config, journal
        // or kernel state (the removal notice may name such commands).
        let commands: Vec<&str> = script.lines().map(str::trim_start).collect();
        for forbidden in [
            "rm ",
            "nft delete",
            "nft flush",
            "ip rule del",
            "ip route flush",
            "conntrack",
        ] {
            assert!(
                !commands.iter().any(|line| line.starts_with(forbidden)),
                "{forbidden}"
            );
        }
        assert!(!script.contains("/run/routelane"));
        assert!(!script.contains("rm -rf ~"));
        // Upgrades restart only what is already running.
        assert!(!script.contains("systemctl restart"));
    }
    assert!(postinst.contains("systemctl try-restart routelane-helper.service"));
    assert!(spec.contains("systemctl try-restart routelane-helper.service"));
    // Neither package owns the journal directory, so no upgrade removes it.
    assert!(!rpm_files().iter().any(|path| path.starts_with("/run")));
    assert!(!deb_installs()
        .iter()
        .any(|(path, _)| path.starts_with("/run")));
    // Stop and disable happen only on removal, not on upgrade.
    let preun = spec
        .split("\n%preun")
        .nth(1)
        .and_then(|rest| rest.split("\n%").next())
        .expect("spec has %preun");
    assert!(preun.contains("if [ \"$1\" -eq 0 ]; then"));
    assert!(deb.contains("    remove)\n        systemctl stop routelane-helper.service"));
}

#[test]
fn native_packages_build_all_binaries_with_the_lockfile() {
    for script in ["packaging/deb/build-deb.sh", "packaging/rpm/build-rpm.sh"] {
        assert!(
            read(script)
                .lines()
                .any(|line| line == "cargo build --release --bins --locked"),
            "{script}"
        );
    }
}
