use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};

use ipnet::IpNet;
use routelane::backend::{
    CompiledAction, CompiledRule, CompiledTarget, KernelPlan, LaneAvailability, LanePlan,
    MARK_BASE, RULE_PRIORITY_BASE, TABLE_BASE,
};
use routelane::domain::{LaneFailurePolicy, LaneId, ProfileId, RuleId};
use serde_json::{json, Value};

const CLIENT: &str = "routelane-test-client";
const GATEWAYS: [&str; 3] = [
    "routelane-test-gateway-a",
    "routelane-test-gateway-b",
    "routelane-test-gateway-c",
];
const SINKS: [&str; 3] = [
    "routelane-test-sink-a",
    "routelane-test-sink-b",
    "routelane-test-sink-c",
];
const NAMESPACES: [&str; 7] = [
    CLIENT,
    GATEWAYS[0],
    GATEWAYS[1],
    GATEWAYS[2],
    SINKS[0],
    SINKS[1],
    SINKS[2],
];
const VETHS: [&str; 14] = [
    "rlt-ca",
    "rlt-ga-c",
    "rlt-ga-s",
    "rlt-sa",
    "rlt-cb",
    "rlt-gb-c",
    "rlt-gb-s",
    "rlt-sb",
    "rlt-cc",
    "rlt-gc-c",
    "rlt-gc-s",
    "rlt-sc",
    "rlt-pre-a",
    "rlt-pre-b",
];
const WORKER_ENV: &str = "ROUTELANE_ROOT_INTEGRATION_WORKER";
const HELPER: &str = env!("CARGO_BIN_EXE_routelane-helper");

#[test]
fn real_helper_routes_three_lanes_and_preserves_generation_one_on_activate_failure() {
    require_root_capabilities();
    let _topology = NamespaceHarness::create();

    let current_exe = env::current_exe().expect("resolve root integration test binary");
    let output = checked_output(
        Command::new("/usr/sbin/ip")
            .args(["netns", "exec", CLIENT])
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

    let generation_one = plan(1, "203.0.113.10/32");
    let generation_two = plan(2, "203.0.113.11/32");
    let production_state_before = state_fingerprint("/run/routelane/applied-plan.json");
    let state_directory = tempfile::Builder::new()
        .prefix("routelane-test-state-")
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

    assert_probe_reaches_only("203.0.113.10", 0);
    assert_probe_reaches_only("198.51.100.10", 1);
    assert_probe_reaches_only("192.0.2.10", 2);
    assert_probe_reaches_only("198.18.0.10", 1);

    let routed_before = nft_counter("203.0.113.10");
    assert_probe_reaches_only("203.0.113.10", 0);
    assert!(nft_counter("203.0.113.10") > routed_before);

    let block_before = nft_counter("192.0.2.99");
    let sink_before = sink_rx_packets();
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
    assert_eq!(sink_rx_packets(), sink_before);
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
    let counter_after_rollback = nft_counter("203.0.113.10");
    assert_probe_reaches_only("203.0.113.10", 0);
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

    let cleaned = helper.ok(json!({"method": "cleanup"}));
    assert_eq!(cleaned["cleaned"], true);
    let empty = helper.ok(json!({"method": "inspect"}));
    assert_eq!(empty["generation"], Value::Null);
    assert_eq!(empty["routelane_table_present"], false);
    assert!(
        !isolated_state.exists(),
        "Cleanup must remove the isolated helper journal"
    );
    assert_eq!(
        state_fingerprint("/run/routelane/applied-plan.json"),
        production_state_before,
        "root integration helper must leave the production journal unchanged"
    );
}

fn require_root_capabilities() {
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

    let preflight = "routelane-test-preflight";
    let _ = Command::new("/usr/sbin/ip")
        .args(["netns", "del", preflight])
        .output();
    let created = command_output(Command::new("/usr/sbin/ip").args(["netns", "add", preflight]));
    assert!(
        created.status.success(),
        "ROOT_INTEGRATION_SKIP: network namespace capability unavailable: {}",
        String::from_utf8_lossy(&created.stderr).trim()
    );
    let _preflight = ExplicitNamespace(preflight);
    let nft = command_output(Command::new("/usr/sbin/ip").args([
        "netns",
        "exec",
        preflight,
        "/usr/sbin/nft",
        "list",
        "ruleset",
    ]));
    assert!(
        nft.status.success(),
        "ROOT_INTEGRATION_SKIP: nftables capability unavailable inside namespace: {}",
        String::from_utf8_lossy(&nft.stderr).trim()
    );
    let forwarding = command_output(Command::new("/usr/sbin/ip").args([
        "netns",
        "exec",
        preflight,
        "/usr/sbin/sysctl",
        "-q",
        "-w",
        "net.ipv4.ip_forward=1",
    ]));
    assert!(
        forwarding.status.success(),
        "ROOT_INTEGRATION_SKIP: IPv4 forwarding is unavailable inside namespace: {}",
        String::from_utf8_lossy(&forwarding.stderr).trim()
    );
    let veth = command_output(Command::new("/usr/sbin/ip").args([
        "link",
        "add",
        "rlt-pre-a",
        "type",
        "veth",
        "peer",
        "name",
        "rlt-pre-b",
    ]));
    let _ = Command::new("/usr/sbin/ip")
        .args(["link", "del", "rlt-pre-a"])
        .output();
    assert!(
        veth.status.success(),
        "ROOT_INTEGRATION_SKIP: veth capability unavailable: {}",
        String::from_utf8_lossy(&veth.stderr).trim()
    );
}

struct ExplicitNamespace(&'static str);

impl Drop for ExplicitNamespace {
    fn drop(&mut self) {
        let _ = Command::new("/usr/sbin/ip")
            .args(["netns", "del", self.0])
            .output();
    }
}

struct NamespaceHarness;

impl NamespaceHarness {
    fn create() -> Self {
        cleanup_explicit_objects();
        for namespace in NAMESPACES {
            run_ip(["netns", "add", namespace]);
            run_ip(["-n", namespace, "link", "set", "lo", "up"]);
        }

        for lane in 0..3 {
            create_lane(lane);
        }
        run_ip([
            "-n",
            CLIENT,
            "route",
            "add",
            "default",
            "via",
            "10.10.2.1",
            "dev",
            "rlt-cb",
        ]);
        Self
    }
}

impl Drop for NamespaceHarness {
    fn drop(&mut self) {
        cleanup_explicit_objects();
    }
}

fn create_lane(lane: usize) {
    let letter = ["a", "b", "c"][lane];
    let client_if = ["rlt-ca", "rlt-cb", "rlt-cc"][lane];
    let gateway_client_if = ["rlt-ga-c", "rlt-gb-c", "rlt-gc-c"][lane];
    let gateway_sink_if = ["rlt-ga-s", "rlt-gb-s", "rlt-gc-s"][lane];
    let sink_if = ["rlt-sa", "rlt-sb", "rlt-sc"][lane];
    let client_address = format!("10.10.{}.2/24", lane + 1);
    let client_gateway = format!("10.10.{}.1", lane + 1);
    let gateway_client_address = format!("{client_gateway}/24");
    let gateway_sink_address = format!("10.20.{}.1/24", lane + 1);
    let sink_address = format!("10.20.{}.2/24", lane + 1);
    let sink_gateway = format!("10.20.{}.1", lane + 1);
    let sink_ip = format!("10.20.{}.2", lane + 1);
    let client_network = format!("10.10.{}.0/24", lane + 1);
    let targets: &[&str] = match lane {
        0 => &["203.0.113.10/32", "203.0.113.11/32"],
        1 => &["198.51.100.10/32", "198.18.0.10/32"],
        _ => &["192.0.2.10/32", "192.0.2.99/32"],
    };

    run_ip([
        "link",
        "add",
        client_if,
        "type",
        "veth",
        "peer",
        "name",
        gateway_client_if,
    ]);
    run_ip(["link", "set", client_if, "netns", CLIENT]);
    run_ip(["link", "set", gateway_client_if, "netns", GATEWAYS[lane]]);
    run_ip([
        "link",
        "add",
        gateway_sink_if,
        "type",
        "veth",
        "peer",
        "name",
        sink_if,
    ]);
    run_ip(["link", "set", gateway_sink_if, "netns", GATEWAYS[lane]]);
    run_ip(["link", "set", sink_if, "netns", SINKS[lane]]);

    configure_interface(CLIENT, client_if, &client_address);
    configure_interface(GATEWAYS[lane], gateway_client_if, &gateway_client_address);
    configure_interface(GATEWAYS[lane], gateway_sink_if, &gateway_sink_address);
    configure_interface(SINKS[lane], sink_if, &sink_address);

    checked_output(
        Command::new("/usr/sbin/ip").args([
            "netns",
            "exec",
            GATEWAYS[lane],
            "/usr/sbin/sysctl",
            "-q",
            "-w",
            "net.ipv4.ip_forward=1",
        ]),
        &format!("enable forwarding for lane {letter}"),
    );
    run_ip([
        "-n",
        SINKS[lane],
        "route",
        "add",
        &client_network,
        "via",
        &sink_gateway,
        "dev",
        sink_if,
    ]);
    for target in targets {
        run_ip(["-n", SINKS[lane], "addr", "add", target, "dev", "lo"]);
        let target_ip = target.trim_end_matches("/32");
        run_ip([
            "-n",
            GATEWAYS[lane],
            "route",
            "add",
            target,
            "via",
            &sink_ip,
            "dev",
            gateway_sink_if,
        ]);
        if target_ip == "198.18.0.10" || target_ip == "192.0.2.99" {
            run_ip([
                "-n",
                CLIENT,
                "route",
                "add",
                target,
                "via",
                &client_gateway,
                "dev",
                client_if,
            ]);
        }
    }
}

fn configure_interface(namespace: &str, interface: &str, address: &str) {
    run_ip(["-n", namespace, "addr", "add", address, "dev", interface]);
    run_ip(["-n", namespace, "link", "set", interface, "up"]);
}

fn cleanup_explicit_objects() {
    for namespace in NAMESPACES {
        let _ = Command::new("/usr/sbin/ip")
            .args(["netns", "del", namespace])
            .output();
    }
    for veth in VETHS {
        let _ = Command::new("/usr/sbin/ip")
            .args(["link", "del", veth])
            .output();
    }
}

fn plan(generation: u64, lane_a_target: &str) -> KernelPlan {
    KernelPlan {
        generation,
        profile_id: ProfileId::from("root-integration"),
        lanes: vec![
            lane("lane-a", "rlt-ca", "10.10.1.1", 0),
            lane("lane-b", "rlt-cb", "10.10.2.1", 1),
            lane("lane-c", "rlt-cc", "10.10.3.1", 2),
        ],
        rules: vec![
            rule(
                "route-a",
                10,
                lane_a_target,
                CompiledAction::Mark(MARK_BASE),
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
        ],
    }
}

fn lane(id: &str, interface: &str, gateway: &str, offset: u32) -> LanePlan {
    LanePlan {
        id: LaneId::from(id),
        interface: interface.to_owned(),
        gateway_v4: Some(gateway.parse().unwrap()),
        gateway_v6: None,
        availability: LaneAvailability::Available,
        table: TABLE_BASE + offset,
        mark: MARK_BASE + offset,
        rule_priority: RULE_PRIORITY_BASE + offset,
        failure_policy: LaneFailurePolicy::Inactive,
    }
}

fn rule(id: &str, priority: u32, target: &str, action: CompiledAction) -> CompiledRule {
    CompiledRule {
        id: RuleId::from(id),
        priority,
        target: CompiledTarget::Network(target.parse::<IpNet>().unwrap()),
        action,
    }
}

fn assert_probe_reaches_only(target: &str, expected_sink: usize) {
    let before = sink_rx_packets();
    let output =
        command_output(Command::new("/usr/bin/ping").args(["-n", "-c", "1", "-W", "2", target]));
    assert!(
        output.status.success(),
        "probe to {target} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let after = sink_rx_packets();
    for lane in 0..3 {
        if lane == expected_sink {
            assert!(
                after[lane] > before[lane],
                "sink {} did not receive probe for {target}",
                SINKS[lane]
            );
        } else {
            assert_eq!(
                after[lane], before[lane],
                "sink {} unexpectedly received probe for {target}",
                SINKS[lane]
            );
        }
    }
}

fn sink_rx_packets() -> [u64; 3] {
    std::array::from_fn(|lane| {
        let output = checked_output(
            Command::new("/usr/sbin/ip").args([
                "netns",
                "exec",
                SINKS[lane],
                "/usr/sbin/ip",
                "-j",
                "-s",
                "link",
                "show",
                "dev",
                ["rlt-sa", "rlt-sb", "rlt-sc"][lane],
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
