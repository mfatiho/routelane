//! Real-kernel acceptance for selective routing (plan Task 9).
//!
//! The topology adds a directly connected lab bridge (10.65.201.0/24,
//! fd00:65::/64, never a lane) to the three uplinks. Lane B is the OS
//! default; lane A is RouteLane's primary. Every check opens a new TCP
//! connection or UDP socket, and endpoints report the source they observed,
//! so the path is proven at the receiver rather than by `ip route get`. A
//! foreign nftables observer in the client namespace counts which packets
//! left with a mark.
//!
//! `ROUTELANE_TEST_SELECTIVE_BASELINE=1` compiles every plan as
//! `UsePrimary` (the pre-selective default) for RED evidence only.

use std::env;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use routelane::backend::{CompiledAction, KernelPlan, MARK_BASE};
use routelane::domain::UnmatchedTraffic;
use serde_json::{json, Value};

use super::namespace_routing::{
    assert_managed_resources_absent, checked_output, rule, run_ip, HelperProcess, RunNames,
};
use super::primary_failover::{
    assert_echo, foreign_sentinels, install_foreign_sentinels, run_worker_scenario, scenario_plan,
    tcp_connect, tcp_exchange, udp_exchange, vinn_rule, EchoServer, Routing, RULE_TARGET,
    SCENARIO_ENV,
};

const WORKER: &str = "selective_routing::selective_worker";
const BASELINE_ENV: &str = "ROUTELANE_TEST_SELECTIVE_BASELINE";
const OLD_HELPER_ENV: &str = "ROUTELANE_TEST_OLD_HELPER";
const LAB_V4: &str = "10.65.201.60";
const LAB_V6: &str = "fd00:65::60";
const LAB_SOURCE_V4: &str = "10.65.201.1";
const LAB_SOURCE_V6: &str = "fd00:65::1";
/// Reachable only through lane B, the OS default; never through lane A.
const UNMATCHED: &str = "198.51.100.10";
/// Reachable only through lane A (the primary).
const LANE_A_TARGET: &str = "203.0.113.10";
const DIRECT_TARGET: &str = "203.0.113.11";
const FOREIGN_MARK: &str = "0x1234";
/// Reachable through lane A (a managed rule) and through lane B, the OS
/// default, so a leak during a rollback shows up as lane B's source address.
const SHARED_ROUTED: &str = "198.18.0.10";

fn lab_and_uplinks(names: &RunNames) -> Vec<EchoServer> {
    vec![
        EchoServer::spawn(&names.lab, &["10.65.201.60:7000", "[fd00:65::60]:7000"]),
        EchoServer::spawn(&names.sinks[0], &["203.0.113.10:7000", "203.0.113.11:7000"]),
        EchoServer::spawn(&names.sinks[1], &["198.51.100.10:7000"]),
        EchoServer::spawn(&names.sinks[2], &["192.0.2.10:7000"]),
    ]
}

#[test]
fn selective_lab_and_unmatched_dual_stack_keep_system_exit() {
    run_worker_scenario(WORKER, "lab_and_unmatched", lab_and_uplinks);
}

#[test]
fn explicit_routes_use_both_lanes() {
    run_worker_scenario(WORKER, "explicit_routes", lab_and_uplinks);
}

#[test]
fn selective_switch_failure_preserves_lab() {
    run_worker_scenario(WORKER, "switch_failure", lab_and_uplinks);
}

#[test]
fn foreign_vpn_and_forwarded_container_keep_state() {
    run_worker_scenario(WORKER, "foreign_and_forwarded", lab_and_uplinks);
}

#[test]
fn v2_upgrade_requires_exact_cleanup_then_reauthorization() {
    run_worker_scenario(WORKER, "v2_upgrade", lab_and_uplinks);
}

#[test]
fn rollback_keeps_the_previous_managed_scope_guarded() {
    run_worker_scenario(WORKER, "rollback_guard", |names| {
        let mut servers = lab_and_uplinks(names);
        servers.push(EchoServer::spawn(&names.sinks[0], &["198.18.0.10:7000"]));
        servers.push(EchoServer::spawn(&names.sinks[1], &["198.18.0.10:7000"]));
        servers
    });
}

#[test]
fn selective_worker() {
    let Ok(scenario) = env::var(SCENARIO_ENV) else {
        return;
    };
    let names = RunNames::from_env().expect("validated run-specific names");
    match scenario.as_str() {
        "lab_and_unmatched" => lab_and_unmatched(&names),
        "explicit_routes" => explicit_routes(&names),
        "switch_failure" => switch_failure(&names),
        "foreign_and_forwarded" => foreign_and_forwarded(&names),
        "v2_upgrade" => v2_upgrade(&names),
        "rollback_guard" => rollback_guard(&names),
        other => panic!("unknown selective scenario {other}"),
    }
}

/// Runs inside the lab namespace: one new TCP exchange, reply on stdout.
#[test]
fn lab_probe_worker() {
    let Ok(target) = env::var("ROUTELANE_TEST_LAB_PROBE") else {
        return;
    };
    let mut stream = tcp_connect(&target).expect("forwarded connection from the lab host");
    println!("PROBE {}", tcp_exchange(&mut stream, "forwarded"));
}

fn mode() -> UnmatchedTraffic {
    if env::var_os(BASELINE_ENV).is_some() {
        UnmatchedTraffic::UsePrimary
    } else {
        UnmatchedTraffic::PreserveSystem
    }
}

fn plan(
    generation: u64,
    names: &RunNames,
    rules: Vec<routelane::backend::CompiledRule>,
) -> KernelPlan {
    let mut plan = scenario_plan(generation, names, 0, &[], rules);
    plan.unmatched_traffic = mode();
    plan
}

/// One new TCP connection and one new UDP socket; the source the endpoint saw.
fn fresh_source(target: &str) -> Result<String, String> {
    let mut stream = tcp_connect(target).map_err(|error| format!("{target}: {error}"))?;
    let reply = tcp_exchange(&mut stream, "fresh-tcp");
    let udp = udp_exchange(target, "fresh-udp").ok_or_else(|| format!("{target}: no UDP reply"))?;
    let source = |reply: &str| {
        reply
            .strip_prefix("peer=")
            .and_then(|rest| rest.split_once(' '))
            .map(|(source, _)| source.to_owned())
    };
    match (source(&reply), source(&udp)) {
        (Some(tcp), Some(udp)) if tcp == udp => Ok(tcp),
        other => Err(format!("{target}: inconsistent replies {other:?}")),
    }
}

/// A foreign table that RouteLane must leave alone: counts, after every
/// RouteLane hook, which locally generated packets left with a mark.
struct MarkObserver;

impl MarkObserver {
    const TABLE: &'static str = "rltest_observer";

    fn install(targets: &[&str]) -> Self {
        let mut batch = format!(
            "add table inet {t}\n\
             add chain inet {t} out {{ type filter hook postrouting priority 300; policy accept; }}\n",
            t = Self::TABLE
        );
        for target in targets {
            let family = if target.contains(':') { "ip6" } else { "ip" };
            batch.push_str(&format!(
                "add rule inet {t} out {family} daddr {target} meta mark != 0 counter comment \"marked {target}\"\n\
                 add rule inet {t} out {family} daddr {target} counter comment \"seen {target}\"\n",
                t = Self::TABLE
            ));
        }
        nft_batch(&batch);
        Self
    }

    fn counter(&self, label: &str) -> u64 {
        let output = checked_output(
            Command::new("/usr/sbin/nft").args(["-j", "list", "table", "inet", Self::TABLE]),
            "read the mark observer",
        );
        let document: Value = serde_json::from_slice(&output.stdout).unwrap();
        document["nftables"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item.get("rule"))
            .filter(|rule| rule["comment"] == label)
            .flat_map(|rule| rule["expr"].as_array().unwrap().iter())
            .find_map(|expr| {
                expr.get("counter")
                    .and_then(|counter| counter["packets"].as_u64())
            })
            .unwrap_or_else(|| panic!("observer counter {label} is missing"))
    }

    fn marked(&self, target: &str) -> u64 {
        self.counter(&format!("marked {target}"))
    }

    fn seen(&self, target: &str) -> u64 {
        self.counter(&format!("seen {target}"))
    }
}

impl Drop for MarkObserver {
    fn drop(&mut self) {
        let _ = Command::new("/usr/sbin/nft")
            .args(["delete", "table", "inet", Self::TABLE])
            .status();
    }
}

fn nft_batch(batch: &str) {
    let mut nft = Command::new("/usr/sbin/nft")
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    nft.stdin
        .take()
        .unwrap()
        .write_all(batch.as_bytes())
        .unwrap();
    assert!(nft.wait().unwrap().success(), "nft batch failed:\n{batch}");
}

fn evidence(line: impl std::fmt::Display) {
    eprintln!("evidence: {line}");
}

fn assert_lab_reachable(when: &str) {
    for (target, expected) in [(LAB_V4, LAB_SOURCE_V4), (LAB_V6, LAB_SOURCE_V6)] {
        let lab_received_source = fresh_source(target)
            .unwrap_or_else(|error| panic!("{when}: fresh lab session failed: {error}"));
        assert_eq!(
            lab_received_source.parse::<std::net::IpAddr>().unwrap(),
            expected.parse::<std::net::IpAddr>().unwrap(),
            "{when}: lab saw the bridge address"
        );
    }
}

fn lab_and_unmatched(names: &RunNames) {
    let mut routing = Routing::start(names);
    let observer = MarkObserver::install(&[LAB_V4, LAB_V6, UNMATCHED, RULE_TARGET]);
    routing.apply(&plan(1, names, vec![vinn_rule()]));

    assert_lab_reachable("selective");
    let fresh_lab_ssh_succeeded = tcp_connect(LAB_V4)
        .map(|mut stream| tcp_exchange(&mut stream, "ssh-like").ends_with("ssh-like"))
        .unwrap_or(false);
    assert!(fresh_lab_ssh_succeeded);

    // Unmatched public traffic keeps the OS default (lane B), unmarked.
    let unmatched = fresh_source(UNMATCHED).expect("unmatched traffic keeps the OS route");
    assert_eq!(unmatched, "10.10.2.2", "the OS default uplink carried it");
    // The explicit rule still steers its own traffic to lane C.
    let routed = fresh_source(RULE_TARGET).expect("explicit route");
    assert_eq!(routed, "10.10.3.2");

    for target in [LAB_V4, LAB_V6, UNMATCHED] {
        let observed_unmatched_mark = observer.marked(target);
        assert!(observer.seen(target) > 0, "{target} was observed");
        assert_eq!(observed_unmatched_mark, 0, "{target} left unmarked");
    }
    assert!(
        observer.marked(RULE_TARGET) > 0,
        "the observer does see RouteLane marks"
    );
    evidence(format!(
        "lab v4/v6 reached from the bridge; unmatched via 10.10.2.2; rule via 10.10.3.2; marked rule packets {}",
        observer.marked(RULE_TARGET)
    ));
    routing.cleanup();
}

fn explicit_routes(names: &RunNames) {
    let mut routing = Routing::start(names);
    let rules = vec![
        rule(
            "lane-a-site",
            5,
            &format!("{LANE_A_TARGET}/32"),
            CompiledAction::Mark(MARK_BASE),
        ),
        rule(
            "direct-site",
            6,
            &format!("{DIRECT_TARGET}/32"),
            CompiledAction::Direct,
        ),
        vinn_rule(),
    ];
    routing.apply(&plan(1, names, rules));
    for (target, lane) in [(LANE_A_TARGET, 0), (DIRECT_TARGET, 0), (RULE_TARGET, 2)] {
        let mut stream = tcp_connect(target).expect("explicit route");
        assert_echo(&tcp_exchange(&mut stream, "explicit"), lane, "explicit");
        let reply = udp_exchange(target, "explicit-udp").expect("explicit UDP route");
        assert_echo(&reply, lane, "explicit-udp");
    }
    assert_eq!(
        fresh_source(UNMATCHED).expect("unmatched stays on the OS route"),
        "10.10.2.2"
    );
    evidence("lane A, Direct (primary A) and lane C rules each used their lane; unmatched via B");
    routing.cleanup();
}

fn switch_failure(names: &RunNames) {
    let mut routing = Routing::start(names);
    routing.apply(&plan(1, names, vec![vinn_rule()]));
    assert_lab_reachable("before the transition");

    let next = plan(
        2,
        names,
        vec![rule(
            "new-site",
            5,
            &format!("{DIRECT_TARGET}/32"),
            CompiledAction::Mark(MARK_BASE),
        )],
    );
    let (helper, state) = routing.helper();
    let pause = state.join("transition-paused");
    helper.ok(json!({
        "method": "set_fail_point",
        "value": "activate_nft",
        "pause_file": pause.to_str().unwrap(),
    }));
    std::thread::scope(|scope| {
        let pause_file = pause.clone();
        let transition = scope.spawn(move || {
            helper.error(json!({
                "method": "apply",
                "plan_json": serde_json::to_string(&next).unwrap(),
            }))
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        while !pause_file.exists() {
            assert!(Instant::now() < deadline, "the transition never paused");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Mid-transition, guard installed: unmanaged traffic still flows,
        // both generations' managed scopes are held back.
        assert_lab_reachable("during the guarded transition");
        assert_eq!(
            fresh_source(UNMATCHED).expect("unmatched traffic flows mid-transition"),
            "10.10.2.2"
        );
        for managed in [RULE_TARGET, DIRECT_TARGET] {
            assert!(
                tcp_connect(managed).is_err(),
                "{managed} is held back by the guard"
            );
        }
        evidence("guarded transition: lab and unmatched flowed, both managed scopes held back");
        std::fs::remove_file(&pause_file).unwrap();
        let error = transition.join().unwrap();
        assert!(error.contains("fail point"), "injected failure: {error}");
    });
    let (helper, _) = routing.helper();
    helper.ok(json!({"method": "set_fail_point", "value": null}));
    let inspected = helper.ok(json!({"method": "inspect"}));
    assert_eq!(
        inspected["generation"], 1,
        "the previous generation is restored"
    );
    assert!(
        !checked_output(
            Command::new("/usr/sbin/nft").args(["list", "table", "inet", "routelane"]),
            "read the restored table",
        )
        .stdout
        .windows(b"chain guard".len())
        .any(|window| window == b"chain guard"),
        "the temporary guard is gone"
    );
    assert_eq!(
        fresh_source(RULE_TARGET).expect("restored route"),
        "10.10.3.2"
    );
    assert_lab_reachable("after rollback");
    routing.cleanup();
}

fn rollback_guard(names: &RunNames) {
    let mut routing = Routing::start(names);
    let managed = || {
        vec![rule(
            "shared-site",
            5,
            &format!("{SHARED_ROUTED}/32"),
            CompiledAction::Mark(MARK_BASE),
        )]
    };
    routing.apply(&plan(1, names, managed()));
    assert_eq!(
        fresh_source(SHARED_ROUTED).expect("managed route before the transition"),
        "10.10.1.2"
    );
    let next = plan(2, names, vec![vinn_rule()]);
    let (helper, state) = routing.helper();
    let pause = state.join("rollback-paused");
    helper.ok(json!({
        "method": "set_fail_point",
        "value": "activate_nft",
        "restore_pause_file": pause.to_str().unwrap(),
    }));
    std::thread::scope(|scope| {
        let pause_file = pause.clone();
        let transition = scope.spawn(move || {
            helper.error(json!({
                "method": "apply",
                "plan_json": serde_json::to_string(&next).unwrap(),
            }))
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        while !pause_file.exists() {
            assert!(Instant::now() < deadline, "the rollback never paused");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Mid-rollback, no RouteLane policy rule remains. The previous
        // generation's managed destination must be held back, never sent
        // out through the OS default; unmanaged traffic still flows.
        let leaked = fresh_source(SHARED_ROUTED);
        assert_lab_reachable("during the rollback");
        let unmatched = fresh_source(UNMATCHED);
        std::fs::remove_file(&pause_file).unwrap();
        let error = transition.join().unwrap();
        assert!(error.contains("fail point"), "injected failure: {error}");
        assert!(
            leaked.is_err(),
            "managed destination escaped during the rollback via {leaked:?}"
        );
        assert_eq!(
            unmatched.expect("unmatched traffic flows mid-rollback"),
            "10.10.2.2"
        );
        evidence("rollback: managed destination held back, lab and unmatched flowed");
    });
    let (helper, _) = routing.helper();
    helper.ok(json!({"method": "set_fail_point", "value": null}));
    assert_eq!(helper.ok(json!({"method": "inspect"}))["generation"], 1);
    assert_eq!(
        fresh_source(SHARED_ROUTED).expect("previous route restored"),
        "10.10.1.2"
    );
    routing.cleanup();
}

fn foreign_and_forwarded(names: &RunNames) {
    install_foreign_sentinels();
    // A foreign VPN: its own mark, policy rule and table, set before RouteLane.
    nft_batch(&format!(
        "add table ip rltest_vpn\n\
         add chain ip rltest_vpn vpnmark {{ type route hook output priority -300; policy accept; }}\n\
         add rule ip rltest_vpn vpnmark ip daddr {UNMATCHED} meta mark set {FOREIGN_MARK} counter\n\
         add rule ip rltest_vpn vpnmark ip daddr {RULE_TARGET} meta mark set {FOREIGN_MARK} counter\n"
    ));
    run_ip([
        "rule",
        "add",
        "priority",
        "9100",
        "fwmark",
        FOREIGN_MARK,
        "lookup",
        "1234",
    ]);
    run_ip([
        "route",
        "add",
        "default",
        "via",
        "10.10.2.1",
        "dev",
        &names.client_interfaces[1],
        "table",
        "1234",
    ]);
    // A forwarded VM/container behind the lab bridge.
    checked_output(
        Command::new("/usr/sbin/sysctl").args(["-q", "-w", "net.ipv4.ip_forward=1"]),
        "enable forwarding in the client namespace",
    );
    for (namespace, gateway, interface) in [
        (
            &names.gateways[1],
            "10.10.2.2",
            &names.gateway_client_interfaces[1],
        ),
        (&names.sinks[1], "10.20.2.1", &names.sink_interfaces[1]),
    ] {
        run_ip([
            "-n",
            namespace,
            "route",
            "add",
            "10.65.201.0/24",
            "via",
            gateway,
            "dev",
            interface,
        ]);
    }
    let foreign_state_before = (foreign_sentinels(), foreign_vpn_state());

    let mut routing = Routing::start(names);
    let observer = MarkObserver::install(&[UNMATCHED, RULE_TARGET]);
    routing.apply(&plan(1, names, vec![vinn_rule()]));

    // Unmatched foreign-marked traffic is left exactly as the VPN set it.
    assert_eq!(
        fresh_source(UNMATCHED).expect("foreign-marked unmatched traffic passes"),
        "10.10.2.2"
    );
    assert_eq!(
        observer.marked(UNMATCHED),
        observer.seen(UNMATCHED),
        "every packet kept the foreign mark"
    );
    // A foreign mark on an explicit RouteLane route is an unsupported
    // conflict: rejected, never re-marked.
    assert!(tcp_connect(RULE_TARGET).is_err(), "conflict is rejected");
    // Forwarded lab traffic gets no RouteLane mark and no NAT.
    let probe = checked_output(
        Command::new("/usr/sbin/ip")
            .args(["netns", "exec", &names.lab])
            .arg(env::current_exe().unwrap())
            .args([
                "--exact",
                "selective_routing::lab_probe_worker",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("ROUTELANE_TEST_LAB_PROBE", UNMATCHED),
        "forwarded probe from the lab host",
    );
    let probe = String::from_utf8_lossy(&probe.stdout);
    assert!(
        probe.contains("PROBE peer=10.65.201.60 forwarded"),
        "forwarded traffic kept its own source: {probe}"
    );
    routing.cleanup();
    drop(observer);
    let foreign_state_after = (foreign_sentinels(), foreign_vpn_state());
    assert_eq!(foreign_state_after, foreign_state_before);
    evidence("foreign VPN mark kept on unmatched, rejected on explicit route; forwarded lab traffic unNATed");
}

/// The VPN fixture without its counters (they legitimately change).
fn foreign_vpn_state() -> Value {
    let read = |args: &[&str]| -> Value {
        let output = checked_output(Command::new(args[0]).args(&args[1..]), "read foreign state");
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let mut nft = read(&["/usr/sbin/nft", "-j", "list", "table", "ip", "rltest_vpn"]);
    for item in nft["nftables"].as_array_mut().unwrap() {
        if let Some(rule) = item.get_mut("rule") {
            rule.as_object_mut().unwrap().remove("handle");
            for expr in rule["expr"].as_array_mut().unwrap() {
                if let Some(counter) = expr.get_mut("counter") {
                    *counter = json!(null);
                }
            }
        }
        for key in ["table", "chain"] {
            if let Some(object) = item.get_mut(key) {
                object.as_object_mut().unwrap().remove("handle");
            }
        }
    }
    json!({
        "nft": nft,
        "rules": read(&["/usr/sbin/ip", "-j", "rule", "show", "priority", "9100"]),
        "table": read(&["/usr/sbin/ip", "-j", "route", "show", "table", "1234"]),
        "forward": checked_output(
            Command::new("/usr/sbin/sysctl").args(["-n", "net.ipv4.ip_forward"]),
            "read forwarding",
        ).stdout,
    })
}

fn v2_upgrade(names: &RunNames) {
    let old_helper = env::var_os(OLD_HELPER_ENV).unwrap_or_else(|| {
        panic!("NOT VERIFIED: {OLD_HELPER_ENV} must name a journal-v2 helper build")
    });
    let sentinels = install_foreign_sentinels();
    let state = tempfile::Builder::new()
        .prefix(&format!("routelane-test-{}-state-", names.token))
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(
        state.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();

    // The released helper arms its broad plan and journals schema 2.
    let mut legacy_plan = scenario_plan(1, names, 0, &[], vec![vinn_rule()]);
    legacy_plan.unmatched_traffic = UnmatchedTraffic::UsePrimary;
    let mut legacy_json = serde_json::to_value(&legacy_plan).unwrap();
    legacy_json
        .as_object_mut()
        .unwrap()
        .remove("unmatched_traffic");
    {
        let mut old = HelperProcess::spawn_binary(std::path::Path::new(&old_helper), state.path());
        old.ok(json!({"method": "apply", "plan_json": legacy_json.to_string()}));
    }
    let journal = std::fs::read_dir(state.path())
        .unwrap()
        .filter_map(Result::ok)
        .find_map(|entry| {
            let text = std::fs::read_to_string(entry.path()).ok()?;
            text.contains("\"journal_version\":2").then_some(text)
        });
    assert!(journal.is_some(), "the old helper wrote a schema-2 journal");
    assert_eq!(
        fresh_source(RULE_TARGET).expect("old rule is live"),
        "10.10.3.2"
    );

    // The current helper refuses to activate over it and reads it only.
    let mut current = HelperProcess::spawn(state.path());
    let inspected = current.ok(json!({"method": "inspect"}));
    assert!(
        inspected["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic
                .as_str()
                .is_some_and(|text| text.starts_with("DAEMON_LEGACY_CLEANUP_REQUIRED"))),
        "inspection names the required cleanup: {inspected}"
    );
    let refused = current.error(json!({
        "method": "apply",
        "plan_json": serde_json::to_string(&plan(2, names, vec![])).unwrap(),
    }));
    assert!(
        refused.contains("cleanup") || refused.contains("schema-2"),
        "modern activation is refused before cleanup: {refused}"
    );
    assert_eq!(
        fresh_source(RULE_TARGET).expect("refusal changed nothing"),
        "10.10.3.2"
    );

    // Exact cleanup removes only what the journal owns; then a new
    // authorization may arm the modern policy.
    assert_eq!(current.ok(json!({"method": "cleanup"}))["cleaned"], true);
    assert_managed_resources_absent();
    current.ok(json!({
        "method": "apply",
        "plan_json": serde_json::to_string(&plan(1, names, vec![vinn_rule()])).unwrap(),
    }));
    assert_lab_reachable("after the upgrade");
    assert_eq!(
        fresh_source(UNMATCHED).expect("selective after the upgrade"),
        if mode() == UnmatchedTraffic::PreserveSystem {
            "10.10.2.2"
        } else {
            "unreachable through the primary"
        }
    );
    assert_eq!(current.ok(json!({"method": "cleanup"}))["cleaned"], true);
    assert_managed_resources_absent();
    drop(current);
    assert_eq!(foreign_sentinels(), sentinels, "foreign state unchanged");
    evidence("schema-2 journal: inspect read-only, modern apply refused, exact cleanup, then modern apply");
}
