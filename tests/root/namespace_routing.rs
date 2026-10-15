use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};

use ipnet::IpNet;
use routelane::backend::{
    CompiledAction, CompiledRule, CompiledTarget, KernelPlan, LaneAvailability, LanePlan,
    RouteMode, MARK_BASE, RULE_PRIORITY_BASE, TABLE_BASE,
};
use routelane::domain::{LaneFailurePolicy, LaneId, ProfileId, RuleId};
use serde_json::{json, Value};

const WORKER_ENV: &str = "ROUTELANE_ROOT_INTEGRATION_WORKER";
const HELPER: &str = env!("CARGO_BIN_EXE_routelane-helper");
const O_NOFOLLOW: i32 = 0o400000;

#[derive(Clone, Debug)]
struct RunNames {
    token: String,
    client: String,
    gateways: [String; 3],
    sinks: [String; 3],
    client_interfaces: [String; 3],
    gateway_client_interfaces: [String; 3],
    gateway_sink_interfaces: [String; 3],
    sink_interfaces: [String; 3],
}

impl RunNames {
    fn from_env() -> Result<Self, String> {
        let token = env::var("ROUTELANE_TEST_TOKEN")
            .map_err(|_| "ROUTELANE_TEST_TOKEN is required".to_owned())?;
        Self::from_token(token)
    }

    fn from_token(token: String) -> Result<Self, String> {
        if token.len() != 6 || !token.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(
                "root integration token must be exactly six ASCII alphanumerics".to_owned(),
            );
        }
        let namespace = |role: &str| format!("routelane-test-{token}-{role}");
        let interface = |role: &str| {
            let name = format!("rl{token}{role}");
            assert!(name.len() <= 15);
            name
        };
        let client = namespace("client");
        let gateways = [
            namespace("gateway-a"),
            namespace("gateway-b"),
            namespace("gateway-c"),
        ];
        let sinks = [
            namespace("sink-a"),
            namespace("sink-b"),
            namespace("sink-c"),
        ];
        let client_interfaces = [interface("ca"), interface("cb"), interface("cc")];
        let gateway_client_interfaces = [interface("gac"), interface("gbc"), interface("gcc")];
        let gateway_sink_interfaces = [interface("gas"), interface("gbs"), interface("gcs")];
        let sink_interfaces = [interface("sa"), interface("sb"), interface("sc")];
        Ok(Self {
            token,
            client,
            gateways,
            sinks,
            client_interfaces,
            gateway_client_interfaces,
            gateway_sink_interfaces,
            sink_interfaces,
        })
    }

    fn namespaces(&self) -> Vec<String> {
        std::iter::once(self.client.clone())
            .chain(self.gateways.iter().cloned())
            .chain(self.sinks.iter().cloned())
            .collect()
    }

    fn creations(&self) -> Vec<Creation> {
        let mut creations = self
            .namespaces()
            .into_iter()
            .map(Creation::Namespace)
            .collect::<Vec<_>>();
        for lane in 0..3 {
            creations.push(Creation::VethPair(
                self.client_interfaces[lane].clone(),
                self.gateway_client_interfaces[lane].clone(),
            ));
            creations.push(Creation::VethPair(
                self.gateway_sink_interfaces[lane].clone(),
                self.sink_interfaces[lane].clone(),
            ));
        }
        creations
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MainRouteSpec {
    prefix: &'static str,
    gateway: &'static str,
    interface: String,
}

fn planned_client_main_routes(names: &RunNames) -> Vec<MainRouteSpec> {
    vec![
        MainRouteSpec {
            prefix: "198.18.0.10/32",
            gateway: "10.10.2.1",
            interface: names.client_interfaces[1].clone(),
        },
        MainRouteSpec {
            prefix: "192.0.2.99/32",
            gateway: "10.10.3.1",
            interface: names.client_interfaces[2].clone(),
        },
    ]
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum KernelObject {
    Namespace(String),
    Veth(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Creation {
    Namespace(String),
    VethPair(String, String),
}

impl Creation {
    fn objects(&self) -> Vec<KernelObject> {
        match self {
            Self::Namespace(name) => vec![KernelObject::Namespace(name.clone())],
            Self::VethPair(left, right) => vec![
                KernelObject::Veth(left.clone()),
                KernelObject::Veth(right.clone()),
            ],
        }
    }
}

trait ProvisionBackend {
    fn exists(&mut self, object: &KernelObject) -> Result<bool, String>;
    fn create(&mut self, creation: &Creation) -> Result<Vec<KernelObject>, String>;
    fn record(&mut self, object: &KernelObject) -> Result<(), String>;
    fn delete(&mut self, object: &KernelObject);
}

fn provision_owned(
    desired: &[Creation],
    backend: &mut impl ProvisionBackend,
) -> Result<Vec<KernelObject>, String> {
    for object in desired.iter().flat_map(Creation::objects) {
        if backend.exists(&object)? {
            return Err(format!("root integration object collision: {object:?}"));
        }
    }

    let mut owned = Vec::new();
    for creation in desired {
        let created = match backend.create(creation) {
            Ok(created) => created,
            Err(error) => {
                cleanup_recorded(&mut owned, backend);
                return Err(error);
            }
        };
        for object in created {
            owned.push(object.clone());
            if let Err(error) = backend.record(&object) {
                cleanup_recorded(&mut owned, backend);
                return Err(error);
            }
        }
    }
    Ok(owned)
}

fn cleanup_recorded(owned: &mut Vec<KernelObject>, backend: &mut impl ProvisionBackend) {
    for object in owned.drain(..).rev() {
        backend.delete(&object);
    }
}

struct SystemProvisionBackend {
    registry: File,
}

impl SystemProvisionBackend {
    fn from_env(names: &RunNames) -> Result<Self, String> {
        let path = env::var("ROUTELANE_TEST_REGISTRY")
            .map_err(|_| "ROUTELANE_TEST_REGISTRY is required".to_owned())?;
        let expected = format!("/tmp/routelane-test-registry.{}", names.token);
        if path != expected {
            return Err("root integration registry path does not match the run token".to_owned());
        }
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| format!("inspect registry: {error}"))?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
        {
            return Err(
                "registry must be a root-owned, non-symlink, single-link 0600 file".to_owned(),
            );
        }
        let registry = OpenOptions::new()
            .append(true)
            .custom_flags(O_NOFOLLOW)
            .open(path)
            .map_err(|error| format!("open registry without following symlinks: {error}"))?;
        Ok(Self { registry })
    }

    fn clear_registry(&mut self) {
        self.registry
            .set_len(0)
            .expect("truncate owned-resource registry after cleanup");
        self.registry
            .sync_all()
            .expect("sync cleared owned-resource registry");
    }
}

impl ProvisionBackend for SystemProvisionBackend {
    fn exists(&mut self, object: &KernelObject) -> Result<bool, String> {
        let output = match object {
            KernelObject::Namespace(_) => {
                command_output(Command::new("/usr/sbin/ip").args(["netns", "list"]))
            }
            KernelObject::Veth(name) => {
                let output = command_output(
                    Command::new("/usr/sbin/ip").args(["link", "show", "dev", name]),
                );
                return Ok(output.status.success());
            }
        };
        if !output.status.success() {
            return Err(format!(
                "list network namespaces for collision preflight: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let KernelObject::Namespace(name) = object else {
            unreachable!()
        };
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .any(|existing| existing == name))
    }

    fn create(&mut self, creation: &Creation) -> Result<Vec<KernelObject>, String> {
        let output = match creation {
            Creation::Namespace(name) => {
                command_output(Command::new("/usr/sbin/ip").args(["netns", "add", name]))
            }
            Creation::VethPair(left, right) => command_output(
                Command::new("/usr/sbin/ip")
                    .args(["link", "add", left, "type", "veth", "peer", "name", right]),
            ),
        };
        if output.status.success() {
            Ok(creation.objects())
        } else {
            Err(format!(
                "create exact root integration object {creation:?}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    fn record(&mut self, object: &KernelObject) -> Result<(), String> {
        match object {
            KernelObject::Namespace(name) => writeln!(self.registry, "namespace {name}"),
            KernelObject::Veth(name) => writeln!(self.registry, "veth {name}"),
        }
        .and_then(|()| self.registry.sync_data())
        .map_err(|error| format!("record owned root integration object: {error}"))
    }

    fn delete(&mut self, object: &KernelObject) {
        let _ = match object {
            KernelObject::Namespace(name) => Command::new("/usr/sbin/ip")
                .args(["netns", "del", name])
                .output(),
            KernelObject::Veth(name) => Command::new("/usr/sbin/ip")
                .args(["link", "del", name])
                .output(),
        };
    }
}

#[test]
fn real_helper_routes_three_lanes_and_preserves_generation_one_on_activate_failure() {
    let names = require_root_capabilities();
    let _topology = NamespaceHarness::create(names.clone());

    let current_exe = env::current_exe().expect("resolve root integration test binary");
    let output = checked_output(
        Command::new("/usr/sbin/ip")
            .args(["netns", "exec", &names.client])
            .arg(current_exe)
            .args([
                "--exact",
                "namespace_routing::root_worker_exercises_real_helper_transport",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(WORKER_ENV, "1"),
        "run RouteLane worker inside client namespace",
    );
    assert!(
        output.status.success(),
        "namespace worker failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn root_worker_exercises_real_helper_transport() {
    if env::var_os(WORKER_ENV).is_none() {
        return;
    }

    let names = RunNames::from_env().expect("validated run-specific names");
    let generation_one = plan(1, "203.0.113.10/32", &names, false);
    let generation_two = plan(2, "203.0.113.11/32", &names, true);
    let production_state_before = state_fingerprint("/run/routelane/applied-plan.json");
    let state_directory = tempfile::Builder::new()
        .prefix(&format!("routelane-test-{}-state-", names.token))
        .tempdir_in("/tmp")
        .expect("create isolated helper journal directory");
    fs::set_permissions(state_directory.path(), fs::Permissions::from_mode(0o700))
        .expect("secure isolated helper journal directory");
    let isolated_state = state_directory.path().join("applied-plan.json");
    let mut helper = HelperProcess::spawn(state_directory.path());

    let applied = helper.ok(json!({
        "method": "apply",
        "plan_json": serde_json::to_string(&generation_one).unwrap(),
    }));
    assert_eq!(applied["generation"], 1);
    let isolated_metadata =
        fs::symlink_metadata(&isolated_state).expect("helper wrote isolated applied state");
    assert!(isolated_metadata.is_file());
    assert_eq!(isolated_metadata.mode() & 0o777, 0o600);
    assert_eq!(isolated_metadata.uid(), 0);
    assert_eq!(
        state_fingerprint("/run/routelane/applied-plan.json"),
        production_state_before,
        "root integration helper must not touch the production journal"
    );

    let inspected = helper.ok(json!({"method": "inspect"}));
    assert_eq!(inspected["generation"], 1);
    assert_eq!(inspected["routelane_table_present"], true);
    // Strict RPF needs per-lane mark awareness; loose RPF needs no write.
    let strict = env::var_os("ROUTELANE_TEST_STRICT_RPF").is_some();
    assert_lane_mark_awareness(&names, u8::from(strict));

    assert_probe_reaches_only("203.0.113.10", 0, &names, None);
    assert_probe_reaches_only("198.51.100.10", 1, &names, None);
    assert_probe_reaches_only("192.0.2.10", 2, &names, None);
    let direct_before = nft_counter("198.18.0.10");
    assert_probe_reaches_only("198.18.0.10", 1, &names, Some(MARK_BASE));
    assert!(nft_counter("198.18.0.10") > direct_before);

    let routed_before = nft_counter("203.0.113.10");
    assert_probe_reaches_only("203.0.113.10", 0, &names, None);
    assert!(nft_counter("203.0.113.10") > routed_before);

    let block_before = nft_counter("192.0.2.99");
    let sink_before = sink_rx_packets(&names);
    let blocked = command_output(Command::new("/usr/bin/ping").args([
        "-n",
        "-c",
        "1",
        "-W",
        "1",
        "192.0.2.99",
    ]));
    assert!(
        !blocked.status.success(),
        "block target unexpectedly replied"
    );
    assert_eq!(sink_rx_packets(&names), sink_before);
    assert!(nft_counter("192.0.2.99") > block_before);

    helper.ok(json!({"method": "set_fail_point", "value": "activate_nft"}));
    let error = helper.error(json!({
        "method": "apply",
        "plan_json": serde_json::to_string(&generation_two).unwrap(),
    }));
    assert!(error.contains("activate nftables"), "{error}");
    assert!(error.contains("root integration fail point"), "{error}");
    helper.ok(json!({"method": "set_fail_point", "value": null}));

    let after_rollback = helper.ok(json!({"method": "inspect"}));
    assert_eq!(after_rollback["generation"], 1);
    assert_generation_one_routing_restored(&names);
    let counter_after_rollback = nft_counter("203.0.113.10");
    assert_probe_reaches_only("203.0.113.10", 0, &names, None);
    assert!(nft_counter("203.0.113.10") > counter_after_rollback);

    helper.ok(json!({
        "method": "apply",
        "plan_json": serde_json::to_string(&generation_two).unwrap(),
    }));
    let restored = helper.ok(json!({
        "method": "restore",
        "failed_candidate_json": serde_json::to_string(&generation_two).unwrap(),
        "previous_json": serde_json::to_string(&generation_one).unwrap(),
    }));
    assert_eq!(restored["generation"], 1);
    assert_eq!(helper.ok(json!({"method": "inspect"}))["generation"], 1);

    // Cleanup restores exactly the journaled interface settings.
    assert_eq!(helper.ok(json!({"method": "cleanup"}))["cleaned"], true);
    assert_managed_resources_absent();
    assert_lane_mark_awareness(&names, 0);
    helper.ok(json!({
        "method": "apply",
        "plan_json": serde_json::to_string(&generation_one).unwrap(),
    }));
    assert_lane_mark_awareness(&names, u8::from(strict));
    assert_link_loss_fails_closed(&names);

    drop(helper);
    fs::remove_file(&isolated_state).expect("simulate a lost validated helper journal");
    let mut helper = HelperProcess::spawn(state_directory.path());
    let orphaned = helper.ok(json!({"method": "inspect"}));
    assert_eq!(orphaned["generation"], Value::Null);
    assert_eq!(orphaned["routelane_table_present"], true);

    let cleaned = helper.ok(json!({"method": "recover_orphaned_resources"}));
    assert_eq!(cleaned["cleaned"], true);
    assert_managed_resources_absent();
    // A lost journal also loses its setting receipts: orphan recovery cannot
    // prove the previous value and leaves the interface setting untouched.
    assert_lane_mark_awareness(&names, u8::from(strict));
    let empty = helper.ok(json!({"method": "inspect"}));
    assert_eq!(empty["generation"], Value::Null);
    assert_eq!(empty["routelane_table_present"], false);
    assert!(
        !isolated_state.exists(),
        "orphan recovery must remove the isolated helper journal"
    );
    assert_eq!(
        state_fingerprint("/run/routelane/applied-plan.json"),
        production_state_before,
        "root integration helper must leave the production journal unchanged"
    );
}

/// The kernel drops a lane's unicast default when its link goes down; the
/// owned terminal route must then reject marked traffic instead of letting it
/// fall through to the main table.
fn assert_link_loss_fails_closed(names: &RunNames) {
    let lane_c = &names.client_interfaces[2];
    let mark = format!("{:#010x}", MARK_BASE + 2);
    let before = command_output(Command::new("/usr/sbin/ip").args([
        "route",
        "get",
        "192.0.2.10",
        "mark",
        &mark,
    ]));
    assert!(
        before.status.success(),
        "lane C is routable before link loss"
    );
    run_ip(["link", "set", lane_c, "down"]);
    for family in ["-4", "-6"] {
        let table = command_output(
            Command::new("/usr/sbin/ip")
                .args([family, "-j", "-N", "route", "show", "table", "20002"]),
        );
        let routes: Value = serde_json::from_slice(&table.stdout).unwrap();
        let mut terminal =
            json!({"type": "7", "dst": "default", "metric": 4_294_967_294u32, "flags": []});
        if family == "-6" {
            terminal["dev"] = json!("lo");
            terminal["pref"] = json!("medium");
        }
        assert_eq!(
            routes,
            json!([terminal]),
            "{family} table keeps only its terminal route after link loss"
        );
    }
    let after = command_output(Command::new("/usr/sbin/ip").args([
        "route",
        "get",
        "192.0.2.10",
        "mark",
        &mark,
    ]));
    assert!(
        !after.status.success(),
        "marked lane C traffic must not fall through after link loss: {}",
        String::from_utf8_lossy(&after.stdout)
    );
    run_ip(["link", "set", lane_c, "up"]);
}

fn assert_lane_mark_awareness(names: &RunNames, expected: u8) {
    for interface in &names.client_interfaces[..3] {
        let path = format!("/proc/sys/net/ipv4/conf/{interface}/src_valid_mark");
        let value = fs::read_to_string(&path).expect("read client interface src_valid_mark");
        assert_eq!(
            value.trim().parse::<u8>().unwrap(),
            expected,
            "{interface} src_valid_mark"
        );
    }
    let global = fs::read_to_string("/proc/sys/net/ipv4/conf/all/src_valid_mark").unwrap();
    assert_eq!(global.trim(), "0", "the global setting is never written");
}

fn require_root_capabilities() -> RunNames {
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "ROOT_INTEGRATION_SKIP: root network integration requires UID 0; use scripts/run-root-integration.sh"
    );
    for binary in [
        "/usr/sbin/ip",
        "/usr/sbin/nft",
        "/usr/bin/ping",
        "/usr/sbin/sysctl",
        HELPER,
    ] {
        assert!(
            std::path::Path::new(binary).is_file(),
            "ROOT_INTEGRATION_SKIP: required executable is unavailable: {binary}"
        );
    }
    let names = RunNames::from_env().expect("script supplied safe run-specific names");
    let _registry = SystemProvisionBackend::from_env(&names)
        .expect("script supplied secure exclusive registry");
    names
}

struct NamespaceHarness {
    names: RunNames,
    backend: SystemProvisionBackend,
    owned: Vec<KernelObject>,
}

impl NamespaceHarness {
    fn create(names: RunNames) -> Self {
        let mut backend =
            SystemProvisionBackend::from_env(&names).expect("open secure owned-resource registry");
        let owned = provision_owned(&names.creations(), &mut backend)
            .unwrap_or_else(|error| panic!("provision collision-safe topology: {error}"));
        let harness = Self {
            names,
            backend,
            owned,
        };
        for namespace in harness.names.namespaces() {
            run_ip(["-n", &namespace, "link", "set", "lo", "up"]);
        }
        for lane in 0..3 {
            create_lane(lane, &harness.names);
        }
        run_ip([
            "-n",
            &harness.names.client,
            "route",
            "add",
            "default",
            "via",
            "10.10.2.1",
            "dev",
            &harness.names.client_interfaces[1],
        ]);
        for route in planned_client_main_routes(&harness.names) {
            run_ip([
                "-n",
                &harness.names.client,
                "route",
                "add",
                route.prefix,
                "via",
                route.gateway,
                "dev",
                &route.interface,
            ]);
        }
        if env::var_os("ROUTELANE_TEST_STRICT_RPF").is_some() {
            // Test-only setup, exclusively inside the run-owned client netns.
            // Production must preserve all/default/rp_filter and touch only
            // src_valid_mark on an authorized, journaled interface.
            for interface in std::iter::once("all")
                .chain(harness.names.client_interfaces.iter().map(String::as_str))
            {
                let rpf = format!("net.ipv4.conf.{interface}.rp_filter=1");
                let mark = format!("net.ipv4.conf.{interface}.src_valid_mark=0");
                checked_output(
                    Command::new("/usr/sbin/ip").args([
                        "netns",
                        "exec",
                        &harness.names.client,
                        "/usr/sbin/sysctl",
                        "-q",
                        "-w",
                        &rpf,
                        &mark,
                    ]),
                    "configure isolated strict-RPF fixture",
                );
            }
        }
        harness
    }
}

impl Drop for NamespaceHarness {
    fn drop(&mut self) {
        cleanup_recorded(&mut self.owned, &mut self.backend);
        self.backend.clear_registry();
    }
}

fn create_lane(lane: usize, names: &RunNames) {
    let letter = ["a", "b", "c"][lane];
    let client_if = &names.client_interfaces[lane];
    let gateway_client_if = &names.gateway_client_interfaces[lane];
    let gateway_sink_if = &names.gateway_sink_interfaces[lane];
    let sink_if = &names.sink_interfaces[lane];
    let client_address = format!("10.10.{}.2/24", lane + 1);
    let gateway_client_address = format!("10.10.{}.1/24", lane + 1);
    let gateway_sink_address = format!("10.20.{}.1/24", lane + 1);
    let sink_address = format!("10.20.{}.2/24", lane + 1);
    let sink_gateway = format!("10.20.{}.1", lane + 1);
    let sink_ip = format!("10.20.{}.2", lane + 1);
    let client_network = format!("10.10.{}.0/24", lane + 1);
    let targets: &[&str] = match lane {
        0 => &["203.0.113.10/32", "203.0.113.11/32", "198.18.0.10/32"],
        1 => &["198.51.100.10/32", "198.18.0.10/32"],
        _ => &["192.0.2.10/32", "192.0.2.99/32"],
    };

    run_ip(["link", "set", client_if, "netns", &names.client]);
    run_ip([
        "link",
        "set",
        gateway_client_if,
        "netns",
        &names.gateways[lane],
    ]);
    run_ip([
        "link",
        "set",
        gateway_sink_if,
        "netns",
        &names.gateways[lane],
    ]);
    run_ip(["link", "set", sink_if, "netns", &names.sinks[lane]]);

    configure_interface(&names.client, client_if, &client_address);
    configure_interface(
        &names.gateways[lane],
        gateway_client_if,
        &gateway_client_address,
    );
    configure_interface(
        &names.gateways[lane],
        gateway_sink_if,
        &gateway_sink_address,
    );
    configure_interface(&names.sinks[lane], sink_if, &sink_address);
    if lane == 0 {
        run_ip([
            "-n",
            &names.gateways[lane],
            "addr",
            "add",
            "10.10.1.254/24",
            "dev",
            gateway_client_if,
        ]);
    }

    checked_output(
        Command::new("/usr/sbin/ip").args([
            "netns",
            "exec",
            &names.gateways[lane],
            "/usr/sbin/sysctl",
            "-q",
            "-w",
            "net.ipv4.ip_forward=1",
        ]),
        &format!("enable forwarding for lane {letter}"),
    );
    run_ip([
        "-n",
        &names.sinks[lane],
        "route",
        "add",
        &client_network,
        "via",
        &sink_gateway,
        "dev",
        sink_if,
    ]);
    for target in targets {
        run_ip(["-n", &names.sinks[lane], "addr", "add", target, "dev", "lo"]);
        run_ip([
            "-n",
            &names.gateways[lane],
            "route",
            "add",
            target,
            "via",
            &sink_ip,
            "dev",
            gateway_sink_if,
        ]);
    }
}

fn configure_interface(namespace: &str, interface: &str, address: &str) {
    run_ip(["-n", namespace, "addr", "add", address, "dev", interface]);
    run_ip(["-n", namespace, "link", "set", interface, "up"]);
}

fn plan(
    generation: u64,
    lane_a_target: &str,
    names: &RunNames,
    generation_two_resources: bool,
) -> KernelPlan {
    let topology = routelane::backend::capture_system_topology()
        .expect("capture real client namespace interface evidence");
    let lane_a_offset = if generation_two_resources { 10 } else { 0 };
    let lane_a_gateway = if generation_two_resources {
        "10.10.1.254"
    } else {
        "10.10.1.1"
    };
    KernelPlan {
        primary: routelane::backend::PrimaryDecision {
            preferred: routelane::backend::FamilyPair {
                v4: Some("lane-b".into()),
                v6: None,
            },
            effective: routelane::backend::FamilyPair {
                v4: routelane::backend::FamilyExit::Lane("lane-b".into()),
                v6: routelane::backend::FamilyExit::Blocked,
            },
        },
        local_networks: topology.connected_networks(),
        local_addresses: topology.assigned_addresses(),
        generation,
        profile_id: ProfileId::from("root-integration"),
        lanes: vec![
            lane(
                "lane-a",
                &names.client_interfaces[0],
                lane_a_gateway,
                lane_a_offset,
                &topology,
            ),
            lane(
                "lane-b",
                &names.client_interfaces[1],
                "10.10.2.1",
                1,
                &topology,
            ),
            lane(
                "lane-c",
                &names.client_interfaces[2],
                "10.10.3.1",
                2,
                &topology,
            ),
        ],
        rules: vec![
            rule(
                "route-a",
                10,
                lane_a_target,
                CompiledAction::Mark(MARK_BASE + lane_a_offset),
            ),
            rule(
                "route-b",
                20,
                "198.51.100.10/32",
                CompiledAction::Mark(MARK_BASE + 1),
            ),
            rule(
                "route-c",
                30,
                "192.0.2.10/32",
                CompiledAction::Mark(MARK_BASE + 2),
            ),
            rule("direct-b", 40, "198.18.0.10/32", CompiledAction::Direct),
            rule("block-c", 50, "192.0.2.99/32", CompiledAction::Block),
            CompiledRule {
                family: routelane::backend::IpFamily::V4,
                id: RuleId::from("catch-all-direct"),
                priority: 100,
                target: CompiledTarget::Any,
                action: CompiledAction::Direct,
            },
        ],
    }
}

fn lane(
    id: &str,
    interface: &str,
    gateway: &str,
    offset: u32,
    topology: &routelane::backend::TopologySnapshot,
) -> LanePlan {
    let observed = topology
        .interface(interface)
        .expect("fixture interface must exist in client namespace");
    LanePlan {
        identity: observed.identity.clone(),
        source_v4: observed.source_v4.clone(),
        source_v6: observed.source_v6.clone(),
        id: LaneId::from(id),
        interface: interface.to_owned(),
        route_v4: Some(RouteMode::Via(gateway.parse().unwrap())),
        route_v6: None,
        availability: LaneAvailability::Available,
        table: TABLE_BASE + offset,
        mark: MARK_BASE + offset,
        rule_priority: RULE_PRIORITY_BASE + offset,
        failure_policy: LaneFailurePolicy::Inactive,
    }
}

fn rule(id: &str, priority: u32, target: &str, action: CompiledAction) -> CompiledRule {
    CompiledRule {
        family: routelane::backend::IpFamily::V4,
        id: RuleId::from(id),
        priority,
        target: CompiledTarget::Network(target.parse::<IpNet>().unwrap()),
        action,
    }
}

fn assert_probe_reaches_only(
    target: &str,
    expected_sink: usize,
    names: &RunNames,
    initial_mark: Option<u32>,
) {
    let before = sink_rx_packets(names);
    let mut command = Command::new("/usr/bin/ping");
    command.args(["-n", "-c", "1", "-W", "2"]);
    if let Some(mark) = initial_mark {
        command.args(["-m", &mark.to_string()]);
    }
    let output = command_output(command.arg(target));
    assert!(
        output.status.success(),
        "probe to {target} failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let after = sink_rx_packets(names);
    for lane in 0..3 {
        if lane == expected_sink {
            assert!(
                after[lane] > before[lane],
                "sink {} did not receive probe for {target}",
                names.sinks[lane]
            );
        } else {
            assert_eq!(
                after[lane], before[lane],
                "sink {} unexpectedly received probe for {target}",
                names.sinks[lane]
            );
        }
    }
}

fn sink_rx_packets(names: &RunNames) -> [u64; 3] {
    std::array::from_fn(|lane| {
        let output = checked_output(
            Command::new("/usr/sbin/ip").args([
                "netns",
                "exec",
                &names.sinks[lane],
                "/usr/sbin/ip",
                "-j",
                "-s",
                "link",
                "show",
                "dev",
                &names.sink_interfaces[lane],
            ]),
            "read sink receive counters",
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        value[0]["stats64"]["rx"]["packets"]
            .as_u64()
            .expect("ip -j -s link exposes RX packet counter")
    })
}

fn nft_counter(target: &str) -> u64 {
    let output = checked_output(
        Command::new("/usr/sbin/nft").args(["-j", "list", "chain", "inet", "routelane", "output"]),
        "read RouteLane nftables counters",
    );
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    for object in document["nftables"].as_array().unwrap() {
        let Some(expressions) = object["rule"]["expr"].as_array() else {
            continue;
        };
        let matches_target = expressions.iter().any(|expression| {
            expression["match"]["right"].as_str() == Some(target)
                || expression["match"]["right"]["prefix"]["addr"].as_str() == Some(target)
        });
        if !matches_target {
            continue;
        }
        return expressions
            .iter()
            .find_map(|expression| expression["counter"]["packets"].as_u64())
            .expect("matched RouteLane rule has a packet counter");
    }
    panic!("no RouteLane nftables rule found for {target}");
}

fn assert_generation_one_routing_restored(names: &RunNames) {
    let routes = ip_json(&["-j", "-4", "route", "show", "table", "all"]);
    assert!(routes.as_array().unwrap().iter().any(|route| {
        json_u32(&route["table"]) == Some(TABLE_BASE)
            && route["dst"].as_str() == Some("default")
            && route["gateway"].as_str() == Some("10.10.1.1")
            && route["dev"].as_str() == Some(names.client_interfaces[0].as_str())
    }));
    assert!(
        routes
            .as_array()
            .unwrap()
            .iter()
            .all(|route| json_u32(&route["table"]) != Some(TABLE_BASE + 10)),
        "generation-two route table survived rollback"
    );

    let rules = ip_json(&["-j", "-4", "rule", "show"]);
    assert!(rule_present(
        &rules,
        RULE_PRIORITY_BASE,
        TABLE_BASE,
        MARK_BASE
    ));
    assert!(!rule_present(
        &rules,
        RULE_PRIORITY_BASE + 10,
        TABLE_BASE + 10,
        MARK_BASE + 10
    ));
}

fn assert_managed_resources_absent() {
    let routes = ip_json(&["-j", "-4", "route", "show", "table", "all"]);
    assert!(
        routes.as_array().unwrap().iter().all(|route| {
            json_u32(&route["table"])
                .is_none_or(|table| !(TABLE_BASE..=routelane::backend::TABLE_MAX).contains(&table))
        }),
        "Cleanup left a route in the managed table range"
    );
    let rules = ip_json(&["-j", "-4", "rule", "show"]);
    assert!(
        rules.as_array().unwrap().iter().all(|rule| {
            let priority_is_managed = json_u32(&rule["priority"]).is_some_and(|priority| {
                (RULE_PRIORITY_BASE..=routelane::backend::RULE_PRIORITY_MAX).contains(&priority)
            });
            let mark_is_managed = json_u32(&rule["fwmark"])
                .is_some_and(|mark| (MARK_BASE..=routelane::backend::MARK_MAX).contains(&mark));
            !priority_is_managed && !mark_is_managed
        }),
        "Cleanup left a policy rule in a managed priority or mark range"
    );
    let tables = checked_output(
        Command::new("/usr/sbin/nft").args(["-j", "list", "tables"]),
        "list nftables tables after cleanup",
    );
    let tables: Value = serde_json::from_slice(&tables.stdout).unwrap();
    assert!(
        tables["nftables"].as_array().unwrap().iter().all(|object| {
            object["table"]["family"].as_str() != Some("inet")
                || object["table"]["name"].as_str() != Some("routelane")
        }),
        "Cleanup left the inet/routelane table"
    );
}

fn ip_json(args: &[&str]) -> Value {
    let output = checked_output(
        Command::new("/usr/sbin/ip").args(args),
        "read IP JSON state",
    );
    serde_json::from_slice(&output.stdout).expect("ip command returned JSON")
}

fn rule_present(rules: &Value, priority: u32, table: u32, mark: u32) -> bool {
    rules.as_array().unwrap().iter().any(|rule| {
        json_u32(&rule["priority"]) == Some(priority)
            && json_u32(&rule["table"]) == Some(table)
            && json_u32(&rule["fwmark"]) == Some(mark)
    })
}

fn json_u32(value: &Value) -> Option<u32> {
    value.as_u64().map(|value| value as u32).or_else(|| {
        let text = value.as_str()?.split('/').next()?;
        text.strip_prefix("0x")
            .map(|hex| u32::from_str_radix(hex, 16).ok())
            .unwrap_or_else(|| text.parse().ok())
    })
}

struct HelperProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl HelperProcess {
    fn spawn(state_directory: &std::path::Path) -> Self {
        let mut child = Command::new(HELPER)
            .arg("--root-integration-stdio")
            .env("ROUTELANE_TEST_STATE_DIRECTORY", state_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn feature-gated real routelane-helper");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn request(&mut self, request: Value) -> Value {
        serde_json::to_writer(&mut self.stdin, &request).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
        let mut response = String::new();
        self.stdout.read_line(&mut response).unwrap();
        assert!(
            !response.is_empty(),
            "routelane-helper closed test transport before responding"
        );
        serde_json::from_str(&response).unwrap()
    }

    fn ok(&mut self, request: Value) -> Value {
        let response = self.request(request);
        assert!(response.get("error").is_none(), "helper error: {response}");
        response["ok"].clone()
    }

    fn error(&mut self, request: Value) -> String {
        let response = self.request(request);
        response["error"]
            .as_str()
            .unwrap_or_else(|| panic!("expected helper error response, got {response}"))
            .to_owned()
    }
}

impl Drop for HelperProcess {
    fn drop(&mut self) {
        let _ = self.stdin.write_all(b"{\"method\":\"exit\"}\n");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }
}

#[derive(Debug, PartialEq, Eq)]
struct StateFingerprint {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

fn state_fingerprint(path: &str) -> Option<StateFingerprint> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Some(StateFingerprint {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("inspect production helper journal: {error}"),
    }
}

fn run_ip<const N: usize>(args: [&str; N]) {
    checked_output(
        Command::new("/usr/sbin/ip").args(args),
        "configure namespace topology",
    );
}

fn command_output(command: &mut Command) -> Output {
    command.output().expect("start integration command")
}

fn checked_output(command: &mut Command, context: &str) -> Output {
    let output = command_output(command);
    assert!(
        output.status.success(),
        "{context} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    output
}

#[cfg(test)]
mod ownership_tests {
    use std::io::{BufRead, BufReader, Read};
    use std::process::{Command, Stdio};

    use super::{
        planned_client_main_routes, provision_owned, Creation, KernelObject, ProvisionBackend,
        RunNames,
    };

    #[derive(Default)]
    struct FakeBackend {
        collision: Option<KernelObject>,
        fail_at: Option<usize>,
        creates: Vec<Creation>,
        records: Vec<KernelObject>,
        deletes: Vec<KernelObject>,
    }

    impl ProvisionBackend for FakeBackend {
        fn exists(&mut self, object: &KernelObject) -> Result<bool, String> {
            Ok(self.collision.as_ref() == Some(object))
        }

        fn create(&mut self, creation: &Creation) -> Result<Vec<KernelObject>, String> {
            if self.fail_at == Some(self.creates.len()) {
                return Err("injected partial creation failure".to_owned());
            }
            self.creates.push(creation.clone());
            Ok(creation.objects())
        }

        fn record(&mut self, object: &KernelObject) -> Result<(), String> {
            self.records.push(object.clone());
            Ok(())
        }

        fn delete(&mut self, object: &KernelObject) {
            self.deletes.push(object.clone());
        }
    }

    fn desired() -> Vec<Creation> {
        vec![
            Creation::Namespace("routelane-test-token-client".to_owned()),
            Creation::VethPair("rltoken-ca".to_owned(), "rltoken-ga".to_owned()),
        ]
    }

    #[test]
    fn collision_fails_before_create_record_or_delete() {
        let mut backend = FakeBackend {
            collision: Some(KernelObject::Namespace(
                "routelane-test-token-client".to_owned(),
            )),
            ..FakeBackend::default()
        };

        let error = provision_owned(&desired(), &mut backend).unwrap_err();

        assert!(error.contains("collision"));
        assert!(backend.creates.is_empty());
        assert!(backend.records.is_empty());
        assert!(backend.deletes.is_empty());
    }

    #[test]
    fn partial_creation_deletes_only_objects_recorded_as_owned() {
        let mut backend = FakeBackend {
            fail_at: Some(1),
            ..FakeBackend::default()
        };

        let error = provision_owned(&desired(), &mut backend).unwrap_err();

        assert!(error.contains("partial creation"));
        assert_eq!(
            backend.records,
            vec![KernelObject::Namespace(
                "routelane-test-token-client".to_owned()
            )]
        );
        assert_eq!(backend.deletes, backend.records);
    }

    #[test]
    fn direct_prefix_has_exactly_one_main_route_via_lane_b() {
        let names = RunNames::from_token("ABC123".to_owned()).unwrap();

        let direct = planned_client_main_routes(&names)
            .into_iter()
            .filter(|route| route.prefix == "198.18.0.10/32")
            .collect::<Vec<_>>();

        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].gateway, "10.10.2.1");
        assert_eq!(direct[0].interface, names.client_interfaces[1]);
    }

    #[test]
    fn term_cleans_registry_exits_143_and_never_runs_sentinel() {
        let script = format!(
            "{}/scripts/run-root-integration.sh",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut child = Command::new("/bin/sh")
            .arg(script)
            .env("ROUTELANE_SHELL_SIGNAL_TEST", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn real root integration runner");
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        stdout
            .read_line(&mut ready)
            .expect("read signal-test readiness");
        assert!(
            ready.starts_with("READY /tmp/routelane-test-registry."),
            "runner did not enter its real signal-test hook: {ready:?}"
        );
        let registry = ready
            .trim()
            .strip_prefix("READY ")
            .expect("ready line includes registry path")
            .to_owned();

        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
        let status = child.wait().expect("wait for signalled runner");
        let mut remaining_stdout = String::new();
        stdout.read_to_string(&mut remaining_stdout).unwrap();

        assert_eq!(status.code(), Some(143));
        assert!(!std::path::Path::new(&registry).exists());
        assert!(!remaining_stdout.contains("SENTINEL"));
    }
}
