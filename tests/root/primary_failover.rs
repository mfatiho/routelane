//! Real-kernel acceptance for primary/backup routing (plan Task 7).
//!
//! Every scenario runs in run-owned namespaces: an outer test provisions the
//! topology and echo endpoints, then a worker inside the client namespace
//! drives the real feature-gated helper. Endpoints report the source address
//! they observed, so each assertion proves the chosen uplink, and the reply
//! payload proves the return path reached the original client socket.
//!
//! `ROUTELANE_TEST_LEGACY_BASELINE=1` replaces the helper with the schema-1
//! mark-only semantics (`tests/fixtures/nftables/legacy-primary-baseline.nft`)
//! for baseline RED evidence only.

use std::env;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::fd::AsRawFd;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use routelane::backend::{
    capture_system_topology, CompiledAction, CompiledTarget, FamilyExit, FamilyPair, KernelPlan,
    LaneAvailability, PrimaryDecision, RouteMode, MARK_BASE,
};
use routelane::domain::ProfileId;
use serde_json::{json, Value};

use super::namespace_routing::{
    assert_managed_resources_absent, checked_output, command_output, lane,
    planned_client_main_routes, require_root_capabilities, rule, run_ip, HelperProcess,
    NamespaceHarness, RunNames, WORKER_ENV,
};

const ECHO_PORT: u16 = 7000;
const SERVER_ENV: &str = "ROUTELANE_TEST_ECHO_BIND";
const SEND_ENV: &str = "ROUTELANE_TEST_UDP_SEND";
pub(super) const SCENARIO_ENV: &str = "ROUTELANE_TEST_SCENARIO";
const LEGACY_ENV: &str = "ROUTELANE_TEST_LEGACY_BASELINE";
/// Reachable through sink A only: the primary (non-OS-default) uplink.
pub(super) const PRIMARY_TARGET: &str = "203.0.113.10";
/// Reachable through sink C only: the explicit rule ("VINN") uplink.
pub(super) const RULE_TARGET: &str = "192.0.2.10";
/// Reachable through both sink A and sink C: primary and backup.
const SHARED_TARGET: &str = "198.51.100.77";
const V6_TARGET: &str = "2001:db8:ffff::10";
/// Reachable through sink A over IPv6 with a global gateway.
const V6_A_TARGET: &str = "2001:db8:fffa::10";
const LANE_IDS: [&str; 3] = ["lane-a", "lane-b", "lane-c"];
const LANE_GATEWAYS: [&str; 3] = ["10.10.1.1", "10.10.2.1", "10.10.3.1"];
pub(super) const LANE_SOURCES: [&str; 3] = ["10.10.1.2", "10.10.2.2", "10.10.3.2"];

#[test]
fn primary_and_rule_paths_have_correct_source_and_return() {
    run_scenario("source_and_return", |names| {
        vec![
            EchoServer::spawn(&names.sinks[0], &["203.0.113.10:7000"]),
            EchoServer::spawn(&names.sinks[2], &["192.0.2.10:7000"]),
        ]
    });
}

#[test]
fn backup_tcp_session_survives_new_flow_failback() {
    run_scenario("failback", |names| {
        for lane in [0, 2] {
            add_sink_target(names, lane, SHARED_TARGET);
        }
        vec![
            EchoServer::spawn(&names.sinks[0], &["198.51.100.77:7000"]),
            EchoServer::spawn(&names.sinks[2], &["198.51.100.77:7000"]),
        ]
    });
}

#[test]
fn ipv6_missing_family_has_no_escape() {
    run_scenario("ipv6_no_escape", |names| {
        provision_lane_b_ipv6(names);
        vec![EchoServer::spawn(
            &names.sinks[1],
            &["[2001:db8:ffff::10]:7000"],
        )]
    });
}

#[test]
fn ipv6_only_lane_carries_its_family_with_its_source() {
    run_scenario("ipv6_lane", |names| {
        provision_lane_b_ipv6(names);
        vec![
            EchoServer::spawn(&names.sinks[0], &["203.0.113.10:7000"]),
            EchoServer::spawn(&names.sinks[1], &["[2001:db8:ffff::10]:7000"]),
        ]
    });
}

#[test]
fn runtime_refresh_fails_over_and_back_without_breaking_backup_sessions() {
    run_scenario("runtime_refresh", |names| {
        for lane in [0, 2] {
            add_sink_target(names, lane, SHARED_TARGET);
        }
        provision_lane_a_ipv6(names);
        vec![
            EchoServer::spawn(
                &names.sinks[0],
                &["198.51.100.77:7000", "[2001:db8:fffa::10]:7000"],
            ),
            EchoServer::spawn(&names.sinks[2], &["198.51.100.77:7000"]),
        ]
    });
}

#[test]
fn gui_closed_link_loss_fails_over_without_authorization_prompt() {
    run_scenario("loss_through_refresh", |names| {
        for lane in [0, 2] {
            add_sink_target(names, lane, SHARED_TARGET);
        }
        for lane in [1, 2] {
            install_observer(
                &names.gateways[lane],
                &format!("ip daddr {SHARED_TARGET}"),
                "escape",
            );
        }
        vec![
            EchoServer::spawn(&names.sinks[0], &["198.51.100.77:7000"]),
            EchoServer::spawn(&names.sinks[2], &["198.51.100.77:7000"]),
        ]
    });
}

#[test]
fn strict_and_loose_rpf_accept_only_correct_paths() {
    run_scenario("rpf", |names| {
        run_ip([
            "-n",
            &names.gateways[0],
            "addr",
            "add",
            &format!("{RULE_TARGET}/32"),
            "dev",
            "lo",
        ]);
        vec![EchoServer::spawn(&names.sinks[0], &["203.0.113.10:7000"])]
    });
}

#[test]
fn local_dhcp_dns_and_foreign_resources_survive_cleanup() {
    run_scenario("local_controls", |names| {
        vec![
            EchoServer::spawn(&names.gateways[0], &["0.0.0.0:67"]),
            EchoServer::spawn(&names.gateways[1], &["10.10.2.1:53"]),
        ]
    });
}

#[test]
fn loss_signals_fail_closed_without_escape() {
    run_scenario("loss_signals", |names| {
        for lane in [1, 2] {
            install_observer(
                &names.gateways[lane],
                &format!("ip daddr {{ {PRIMARY_TARGET}, {RULE_TARGET} }}"),
                "escape",
            );
        }
        vec![
            EchoServer::spawn(&names.sinks[0], &["203.0.113.10:7000"]),
            EchoServer::spawn(&names.sinks[2], &["192.0.2.10:7000"]),
        ]
    });
}

fn run_scenario(scenario: &str, setup: impl FnOnce(&RunNames) -> Vec<EchoServer>) {
    run_worker_scenario("primary_failover::client_worker", scenario, setup);
}

/// Provisions the topology and endpoints, then runs `worker` (a test path in
/// this binary) inside the client namespace with `scenario` selected.
pub(super) fn run_worker_scenario(
    worker: &str,
    scenario: &str,
    setup: impl FnOnce(&RunNames) -> Vec<EchoServer>,
) {
    let names = require_root_capabilities();
    let _topology = NamespaceHarness::create(names.clone());
    let _servers = setup(&names);
    let output = command_output(
        Command::new("/usr/sbin/ip")
            .args(["netns", "exec", &names.client])
            .arg(env::current_exe().expect("resolve root integration test binary"))
            .args(["--exact", worker, "--nocapture", "--test-threads=1"])
            .env(WORKER_ENV, "1")
            .env(SCENARIO_ENV, scenario),
    );
    assert!(
        output.status.success(),
        "{scenario} worker failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for evidence in String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter(|line| line.starts_with("evidence:"))
    {
        eprintln!("{scenario} {evidence}");
    }
}

#[test]
fn client_worker() {
    let Ok(scenario) = env::var(SCENARIO_ENV) else {
        return;
    };
    let names = RunNames::from_env().expect("validated run-specific names");
    let mut routing = Routing::start(&names);
    match scenario.as_str() {
        "source_and_return" => source_and_return(&names, &mut routing),
        "failback" => failback(&names, &mut routing),
        "ipv6_no_escape" => ipv6_no_escape(&names, &mut routing),
        "ipv6_lane" => ipv6_lane(&names, &mut routing),
        "rpf" => reverse_path_filtering(&names, &mut routing),
        "runtime_refresh" => runtime_refresh(&names, &mut routing),
        "loss_through_refresh" => loss_through_refresh(&names, &mut routing),
        "local_controls" => local_controls(&names, &mut routing),
        "loss_signals" => loss_signals(&names, &mut routing),
        other => panic!("unknown root scenario {other}"),
    }
}

fn source_and_return(names: &RunNames, routing: &mut Routing) {
    routing.apply(&scenario_plan(1, names, 0, &[], vec![vinn_rule()]));
    for (target, lane) in [(PRIMARY_TARGET, 0), (RULE_TARGET, 2)] {
        let mut stream = tcp_connect(target).expect("connect through the chosen uplink");
        assert_echo(
            &tcp_exchange(&mut stream, "tcp-request"),
            lane,
            "tcp-request",
        );
        let reply = udp_exchange(target, "udp-request").expect("UDP reply on the client socket");
        assert_echo(&reply, lane, "udp-request");
    }
    routing.cleanup();
}

fn failback(names: &RunNames, routing: &mut Routing) {
    let primary = &names.client_interfaces[0];
    run_ip(["link", "set", primary, "down"]);
    routing.apply(&scenario_plan(1, names, 2, &[0], vec![]));
    let mut backup_session = tcp_connect(SHARED_TARGET).expect("connect over the backup");
    assert_echo(
        &tcp_exchange(&mut backup_session, "before-failback"),
        2,
        "before-failback",
    );

    run_ip(["link", "set", primary, "up"]);
    wait_for_connected_route(primary);
    routing.apply(&scenario_plan(2, names, 0, &[], vec![]));
    // The established backup session keeps its lane; only new flows move.
    assert_echo(
        &tcp_exchange(&mut backup_session, "after-failback"),
        2,
        "after-failback",
    );
    let mut new_session = tcp_connect(SHARED_TARGET).expect("connect over the primary");
    assert_echo(&tcp_exchange(&mut new_session, "new-flow"), 0, "new-flow");
    let reply = udp_exchange(SHARED_TARGET, "new-datagram").expect("UDP over the primary");
    assert_echo(&reply, 0, "new-datagram");
    drop(backup_session);
    drop(new_session);
    routing.cleanup();
}

fn ipv6_no_escape(names: &RunNames, routing: &mut Routing) {
    let mut control = tcp_connect(V6_TARGET).expect("IPv6 works through the OS default");
    assert_eq!(
        tcp_exchange(&mut control, "control"),
        "peer=2001:db8:2::2 control"
    );
    drop(control);

    routing.apply(&scenario_plan(1, names, 0, &[], vec![]));
    let forwarded = || gateway_counter(names, 1, "v6_target");
    let before = forwarded();
    assert!(
        tcp_connect(V6_TARGET).is_err(),
        "IPv6 must not escape through the OS default when no approved lane has IPv6"
    );
    assert_eq!(udp_exchange(V6_TARGET, "escape"), None);
    assert_eq!(forwarded(), before, "no IPv6 packet left through lane B");

    routing.cleanup();
    let mut restored = tcp_connect(V6_TARGET).expect("cleanup restores OS IPv6 routing");
    assert_eq!(
        tcp_exchange(&mut restored, "restored"),
        "peer=2001:db8:2::2 restored"
    );
}

/// Lane B becomes an IPv6-only lane and the IPv6 exit; lane A stays the IPv4
/// exit. Each family must leave through its own lane with that lane's source.
fn ipv6_lane(names: &RunNames, routing: &mut Routing) {
    let mut plan = scenario_plan(1, names, 0, &[], vec![]);
    plan.lanes[1].route_v4 = None;
    plan.lanes[1].route_v6 = Some(RouteMode::Via("2001:db8:2::1".parse().unwrap()));
    plan.primary.preferred.v6 = Some(LANE_IDS[1].into());
    plan.primary.effective.v6 = FamilyExit::Lane(LANE_IDS[1].into());
    routing.apply(&plan);

    let mut v6 = tcp_connect(V6_TARGET).expect("IPv6 through its lane");
    assert_eq!(tcp_exchange(&mut v6, "v6-tcp"), "peer=2001:db8:2::2 v6-tcp");
    assert_eq!(
        udp_exchange(V6_TARGET, "v6-udp").as_deref(),
        Some("peer=2001:db8:2::2 v6-udp")
    );
    let mut v4 = tcp_connect(PRIMARY_TARGET).expect("IPv4 through lane A");
    assert_echo(&tcp_exchange(&mut v4, "v4-tcp"), 0, "v4-tcp");
    drop((v6, v4));
    routing.cleanup();
}

/// The helper's own runtime: one activation, then kernel-evidence refreshes.
/// Lane A (primary, dual-stack with a global IPv6 gateway) fails and returns
/// while long-lived TCP and UDP flows keep running on backup lane C.
fn runtime_refresh(names: &RunNames, routing: &mut Routing) {
    let Routing::Helper { helper, .. } = routing else {
        panic!("runtime refresh needs the real helper");
    };
    let primary = names.client_interfaces[0].clone();
    // What DHCP/SLAAC would restore after the link returns: the kernel drops
    // global IPv6 addresses and every route through the interface on down.
    let restore_lane_a_defaults = || {
        run_ip([
            "-6",
            "addr",
            "replace",
            "2001:db8:1::2/64",
            "dev",
            &primary,
            "nodad",
        ]);
        run_ip([
            "route",
            "replace",
            "default",
            "via",
            "10.10.1.1",
            "dev",
            &primary,
            "metric",
            "100",
        ]);
        run_ip([
            "-6",
            "route",
            "replace",
            "default",
            "via",
            "2001:db8:1::1",
            "dev",
            &primary,
            "metric",
            "100",
        ]);
    };
    restore_lane_a_defaults();
    run_ip([
        "route",
        "add",
        "default",
        "via",
        "10.10.3.1",
        "dev",
        &names.client_interfaces[2],
        "metric",
        "200",
    ]);
    let activation = runtime_activation(names);
    let armed = helper.ok(json!({
        "method": "activate_runtime",
        "activation_json": serde_json::to_string(&activation).unwrap(),
        "uid": 0,
        "now_ms": 0,
    }));
    assert_eq!(armed["status"]["phase"], "Armed");
    assert_effective(&armed["status"], "lane-a", Some("lane-a"));
    let mut session = tcp_connect(SHARED_TARGET).expect("primary before outage");
    assert_echo(&tcp_exchange(&mut session, "armed"), 0, "armed");
    let mut v6 = tcp_connect(V6_A_TARGET).expect("IPv6 over the primary");
    assert_eq!(tcp_exchange(&mut v6, "v6"), "peer=2001:db8:1::2 v6");
    drop((session, v6));

    // Link loss: the kernel drops lane A's unicast defaults on its own.
    run_ip(["link", "set", &primary, "down"]);
    let failover = helper.ok(json!({"method": "refresh_runtime", "now_ms": 1_000}));
    assert_eq!(failover["phase"], "Armed", "{failover}");
    assert_effective(&failover, "lane-c", None);

    let backup = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    // Exchanges completed so far: the proof below is that the backup flows
    // keep exchanging after failback, independent of how fast the VM is.
    let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let traffic = {
        let running = std::sync::Arc::clone(&backup);
        let completed = std::sync::Arc::clone(&progress);
        std::thread::spawn(move || {
            let mut stream = tcp_connect(SHARED_TARGET).expect("backup session");
            let datagrams = UdpSocket::bind("0.0.0.0:0").unwrap();
            datagrams
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            datagrams
                .connect(format!("{SHARED_TARGET}:{ECHO_PORT}"))
                .unwrap();
            let (mut exchanges, mut lost) = (0, 0);
            let mut buffer = [0; 128];
            while running.load(std::sync::atomic::Ordering::SeqCst) {
                let payload = format!("tick-{exchanges}");
                assert_echo(&tcp_exchange(&mut stream, &payload), 2, &payload);
                datagrams.send(payload.as_bytes()).unwrap();
                match datagrams.recv(&mut buffer) {
                    Ok(length) => {
                        assert_echo(std::str::from_utf8(&buffer[..length]).unwrap(), 2, &payload)
                    }
                    Err(_) => lost += 1,
                }
                exchanges += 1;
                completed.store(exchanges, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
            }
            (exchanges, lost)
        })
    };
    std::thread::sleep(Duration::from_millis(300));

    // The link returns; failback waits for five seconds of readiness.
    run_ip(["link", "set", &primary, "up"]);
    wait_for_connected_route(&primary);
    restore_lane_a_defaults();
    let waiting = helper.ok(json!({"method": "refresh_runtime", "now_ms": 2_000}));
    // IPv4 still has a working backup and waits; IPv6 had no exit at all and
    // returns at once (its global gateway re-enters a terminal-only table).
    assert_effective(&waiting, "lane-c", Some("lane-a"));
    assert_eq!(waiting["failback_in_ms"], 5_000);
    std::thread::sleep(Duration::from_millis(300));
    let failback = helper.ok(json!({"method": "refresh_runtime", "now_ms": 7_100}));
    assert_effective(&failback, "lane-a", Some("lane-a"));
    let at_failback = progress.load(std::sync::atomic::Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(10);
    while progress.load(std::sync::atomic::Ordering::SeqCst) < at_failback + 5
        && !traffic.is_finished()
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    backup.store(false, std::sync::atomic::Ordering::SeqCst);
    let (exchanges, lost) = traffic.join().expect("backup TCP and UDP kept working");
    eprintln!(
        "evidence: backup flows {exchanges} exchanges ({at_failback} before failback), {lost} UDP replies lost"
    );
    assert!(at_failback > 0, "backup flows exchanged before failback");
    assert!(
        exchanges >= at_failback + 5,
        "backup flows kept exchanging after failback: {exchanges} total, {at_failback} before"
    );

    let mut fresh = tcp_connect(SHARED_TARGET).expect("new flow after failback");
    assert_echo(&tcp_exchange(&mut fresh, "fresh"), 0, "fresh");
    let mut v6 = tcp_connect(V6_A_TARGET).expect("IPv6 global gateway restored");
    assert_eq!(
        tcp_exchange(&mut v6, "v6-back"),
        "peer=2001:db8:1::2 v6-back"
    );
    drop((fresh, v6));
    routing.cleanup();
}

/// After one activation only kernel-evidence refreshes run (no further
/// activation or authorization): carrier, address and main-default-route loss
/// each fail over independently and fail back after five seconds; all links
/// down blocks without escape; a restarted helper resumes the exact state.
fn loss_through_refresh(names: &RunNames, routing: &mut Routing) {
    let Routing::Helper { helper, _state } = routing else {
        panic!("loss through refresh needs the real helper");
    };
    let primary = names.client_interfaces[0].clone();
    let backup = names.client_interfaces[2].clone();
    let lane_a_default = || {
        run_ip([
            "route",
            "replace",
            "default",
            "via",
            "10.10.1.1",
            "dev",
            &primary,
            "metric",
            "100",
        ]);
    };
    lane_a_default();
    run_ip([
        "route",
        "replace",
        "default",
        "via",
        "10.10.3.1",
        "dev",
        &backup,
        "metric",
        "200",
    ]);
    let foreign_before = install_foreign_sentinels();
    let armed = helper.ok(json!({
        "method": "activate_runtime",
        "activation_json": serde_json::to_string(&runtime_activation(names)).unwrap(),
        "uid": 0,
        "now_ms": 0,
    }));
    assert_effective(&armed["status"], "lane-a", None);
    let mut clock = 0;
    let mut refresh = |helper: &mut HelperProcess, advance: u64| {
        clock += advance;
        let status = helper.ok(json!({"method": "refresh_runtime", "now_ms": clock}));
        assert_eq!(status["phase"], "Armed", "{status}");
        status
    };
    let exit_source = |lane: usize| {
        let mut stream = tcp_connect(SHARED_TARGET).expect("a new flow has an exit");
        assert_echo(&tcp_exchange(&mut stream, "probe"), lane, "probe");
    };
    let fail_back =
        |helper: &mut HelperProcess, refresh: &mut dyn FnMut(&mut HelperProcess, u64) -> Value| {
            assert_effective(&refresh(helper, 1_000), "lane-c", None);
            exit_source(2);
            assert_effective(&refresh(helper, 5_100), "lane-a", None);
            exit_source(0);
        };
    exit_source(0);

    // Carrier loss: the peer goes away; the kernel keeps linkdown routes.
    run_ip([
        "-n",
        &names.gateways[0],
        "link",
        "set",
        &names.gateway_client_interfaces[0],
        "down",
    ]);
    assert_effective(&refresh(helper, 1_000), "lane-c", None);
    exit_source(2);
    run_ip([
        "-n",
        &names.gateways[0],
        "link",
        "set",
        &names.gateway_client_interfaces[0],
        "up",
    ]);
    wait_for_connected_route(&primary);
    fail_back(helper, &mut refresh);

    // Address removal deletes every route through the interface.
    run_ip(["addr", "del", "10.10.1.2/24", "dev", &primary]);
    assert_effective(&refresh(helper, 1_000), "lane-c", None);
    exit_source(2);
    run_ip(["addr", "add", "10.10.1.2/24", "dev", &primary]);
    wait_for_connected_route(&primary);
    lane_a_default();
    fail_back(helper, &mut refresh);

    // The OS loses lane A's default route (lease expiry); the owned table
    // route is still there and is retired by the refresh.
    run_ip([
        "route",
        "del",
        "default",
        "via",
        "10.10.1.1",
        "dev",
        &primary,
        "metric",
        "100",
    ]);
    assert_effective(&refresh(helper, 1_000), "lane-c", None);
    exit_source(2);
    lane_a_default();
    fail_back(helper, &mut refresh);

    // A restarted helper resumes exactly and keeps failing over.
    let state = _state.path().to_owned();
    drop(std::mem::replace(helper, HelperProcess::spawn(&state)));
    assert_effective(&refresh(helper, 1_000), "lane-a", None);

    // All links down: nothing leaves through any uplink.
    let escaped = || [1, 2].map(|lane| gateway_counter(names, lane, "escape"));
    let before = escaped();
    for interface in &names.client_interfaces {
        run_ip(["link", "set", interface, "down"]);
    }
    let down = refresh(helper, 1_000);
    assert_effective(&down, "", None);
    assert!(tcp_connect(SHARED_TARGET).is_err());
    assert_eq!(
        escaped(),
        before,
        "no packet escaped while all links were down"
    );
    for interface in &names.client_interfaces {
        run_ip(["link", "set", interface, "up"]);
        wait_for_connected_route(interface);
    }
    lane_a_default();
    run_ip([
        "route",
        "replace",
        "default",
        "via",
        "10.10.3.1",
        "dev",
        &backup,
        "metric",
        "200",
    ]);
    // Links coming back up do not restore routes the kernel deleted; put
    // back exactly what this test's own link changes removed.
    run_ip([
        "route",
        "replace",
        "default",
        "via",
        "10.10.2.1",
        "dev",
        &names.client_interfaces[1],
    ]);
    for route in planned_client_main_routes(names) {
        run_ip([
            "route",
            "replace",
            route.prefix,
            "via",
            route.gateway,
            "dev",
            &route.interface,
        ]);
    }
    run_ip([
        "route",
        "replace",
        "default",
        "via",
        "10.10.2.1",
        "table",
        "100",
    ]);
    // No exit to fail back from: the primary returns at once.
    assert_effective(&refresh(helper, 1_000), "lane-a", None);
    exit_source(0);

    routing.cleanup();
    assert_eq!(
        foreign_sentinels(),
        foreign_before,
        "foreign rules, routes, tables and tunables are unchanged"
    );
}

/// `v4 == ""` expects IPv4 to be blocked.
fn assert_effective(status: &Value, v4: &str, v6: Option<&str>) {
    let effective = &status["decision"]["effective"];
    let expected_v4 = if v4.is_empty() {
        json!("Blocked")
    } else {
        json!({"Lane": v4})
    };
    assert_eq!(effective["v4"], expected_v4, "{status}");
    match v6 {
        Some(lane) => assert_eq!(effective["v6"], json!({"Lane": lane}), "{status}"),
        None => assert_eq!(effective["v6"], json!("Blocked"), "{status}"),
    }
}

fn runtime_activation(names: &RunNames) -> routelane::backend::RuntimeActivation {
    use routelane::domain::{
        InterfaceSelector, Lane, LaneFailurePolicy, PrimarySelection, Profile, RoutingPolicy,
    };
    routelane::backend::RuntimeActivation {
        protocol_version: routelane::backend::RUNTIME_PROTOCOL_VERSION,
        profile: Profile {
            id: ProfileId::from("runtime"),
            name: "Runtime".into(),
            enabled: true,
            rules: vec![],
            routing: RoutingPolicy {
                unmatched_traffic: routelane::domain::UnmatchedTraffic::UsePrimary,
                primary: PrimarySelection::Lane(LANE_IDS[0].into()),
                backups: vec![LANE_IDS[2].into()],
                automatic_failover: true,
            },
            lanes: (0..3)
                .map(|index| Lane {
                    id: LANE_IDS[index].into(),
                    name: LANE_IDS[index].to_owned(),
                    interface: InterfaceSelector::from(names.client_interfaces[index].as_str()),
                    fallback_lane: None,
                    failure_policy: LaneFailurePolicy::Block,
                })
                .collect(),
        },
        resolutions: routelane::backend::ResolutionSnapshot::empty(),
        expected_generation: 0,
    }
}

fn reverse_path_filtering(names: &RunNames, routing: &mut Routing) {
    for (generation, mode) in [(1, 2u8), (2, 1u8)] {
        set_client_rp_filter(names, mode);
        routing.apply(&scenario_plan(generation, names, 0, &[], vec![]));
        let mut stream = tcp_connect(PRIMARY_TARGET).expect("reply on the correct lane");
        assert_echo(&tcp_exchange(&mut stream, "rpf"), 0, "rpf");
        let reply = udp_exchange(PRIMARY_TARGET, "rpf-udp").expect("UDP reply on the lane");
        assert_echo(&reply, 0, "rpf-udp");
        drop(stream);

        // An unsolicited packet whose source routes via another uplink.
        let listener = UdpSocket::bind("10.10.1.2:7100").unwrap();
        listener
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        send_from_namespace(
            &names.gateways[0],
            &format!("{RULE_TARGET}:7101>10.10.1.2:7100"),
        );
        let mut buffer = [0; 64];
        let received = listener.recv_from(&mut buffer).is_ok();
        assert_eq!(
            received,
            mode == 2,
            "rp_filter={mode}: a wrong-path source is accepted only in loose mode"
        );
        routing.cleanup();
        for interface in names.client_interfaces.iter().map(String::as_str) {
            assert_eq!(
                sysctl(&format!("net.ipv4.conf.{interface}.rp_filter")),
                mode
            );
            assert_eq!(
                sysctl(&format!("net.ipv4.conf.{interface}.src_valid_mark")),
                0
            );
        }
        assert_eq!(sysctl("net.ipv4.conf.all.rp_filter"), mode);
    }
}

fn local_controls(names: &RunNames, routing: &mut Routing) {
    let _resolver = LoopbackResolver::start();
    let foreign_before = install_foreign_sentinels();
    routing.apply(&scenario_plan(1, names, 0, &[], vec![vinn_rule()]));

    // DHCP broadcast on the primary link and its unicast answer.
    let dhcp = UdpSocket::bind("0.0.0.0:68").unwrap();
    dhcp.set_broadcast(true).unwrap();
    bind_to_device(&dhcp, &names.client_interfaces[0]);
    dhcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    dhcp.send_to(b"dhcp-discover", "255.255.255.255:67")
        .unwrap();
    let mut buffer = [0; 128];
    let (length, _) = dhcp.recv_from(&mut buffer).expect("DHCP answer");
    assert_echo(
        std::str::from_utf8(&buffer[..length]).unwrap(),
        0,
        "dhcp-discover",
    );
    drop(dhcp);

    // A resolver on a connected LAN and the local stub stay reachable.
    let reply = udp_exchange_port("10.10.2.1", 53, "dns-query").expect("LAN resolver");
    assert_echo(&reply, 1, "dns-query");
    let mut lan =
        TcpStream::connect_timeout(&"10.10.2.1:53".parse().unwrap(), Duration::from_secs(3))
            .expect("LAN TCP service");
    lan.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    assert_echo(&tcp_exchange(&mut lan, "lan-tcp"), 1, "lan-tcp");
    let stub = udp_exchange_port("127.0.0.53", 53, "stub-query").expect("local DNS stub");
    assert_eq!(stub, "peer=127.0.0.1 stub-query");

    routing.cleanup();
    assert_eq!(
        foreign_sentinels(),
        foreign_before,
        "foreign rules, routes, tables and tunables survive RouteLane cleanup unchanged"
    );
}

fn loss_signals(names: &RunNames, routing: &mut Routing) {
    let primary = &names.client_interfaces[0];
    // Packets for the targets that arrive at the other gateways escaped.
    let escaped = || [1, 2].map(|lane| gateway_counter(names, lane, "escape"));
    routing.apply(&scenario_plan(1, names, 0, &[], vec![]));

    // Address removal: the kernel drops lane A's unicast default.
    run_ip(["addr", "del", "10.10.1.2/24", "dev", primary]);
    assert_fails_closed(PRIMARY_TARGET, &escaped);
    run_ip(["addr", "add", "10.10.1.2/24", "dev", primary]);
    routing.cleanup();

    // Default-route loss inside the owned table.
    routing.apply(&scenario_plan(2, names, 0, &[], vec![]));
    run_ip(["route", "del", "default", "table", "20000", "metric", "0"]);
    assert_fails_closed(PRIMARY_TARGET, &escaped);
    routing.cleanup();

    // All uplinks down: nothing may leave through any interface.
    routing.apply(&scenario_plan(3, names, 0, &[], vec![vinn_rule()]));
    for interface in &names.client_interfaces {
        run_ip(["link", "set", interface, "down"]);
    }
    for target in [PRIMARY_TARGET, RULE_TARGET] {
        assert!(tcp_connect(target).is_err(), "{target} fails closed");
    }
    for interface in &names.client_interfaces {
        run_ip(["link", "set", interface, "up"]);
        wait_for_connected_route(interface);
    }
    routing.cleanup();
}

/// Locally generated traffic is rerouted after nftables marks it; a terminal
/// route makes that reroute fail, so the kernel drops the packet and the
/// application sees a timeout rather than an immediate error.
fn assert_fails_closed(target: &str, escaped: &impl Fn() -> [u64; 2]) {
    let before = escaped();
    assert!(tcp_connect(target).is_err(), "{target} must fail closed");
    assert_eq!(
        escaped(),
        before,
        "no packet escaped through another uplink"
    );
}

/// A plan as the runtime would compile it: lane A is the preferred primary,
/// `effective` is the decided IPv4 exit and IPv6 has no approved lane.
pub(super) fn scenario_plan(
    generation: u64,
    names: &RunNames,
    effective: usize,
    unavailable: &[usize],
    rules: Vec<routelane::backend::CompiledRule>,
) -> KernelPlan {
    let topology = capture_system_topology().expect("capture client namespace topology");
    let mut lanes = (0..3)
        .map(|index| {
            lane(
                LANE_IDS[index],
                &names.client_interfaces[index],
                LANE_GATEWAYS[index],
                index as u32,
                &topology,
            )
        })
        .collect::<Vec<_>>();
    for &index in unavailable {
        lanes[index].availability = LaneAvailability::Unavailable;
    }
    KernelPlan {
        unmatched_traffic: routelane::domain::UnmatchedTraffic::UsePrimary,
        primary: PrimaryDecision {
            preferred: FamilyPair {
                v4: Some(LANE_IDS[0].into()),
                v6: None,
            },
            effective: FamilyPair {
                v4: FamilyExit::Lane(LANE_IDS[effective].into()),
                v6: FamilyExit::Blocked,
            },
        },
        local_networks: topology.connected_networks(),
        local_addresses: topology.assigned_addresses(),
        generation,
        profile_id: ProfileId::from("primary-failover"),
        lanes,
        rules,
    }
}

pub(super) fn vinn_rule() -> routelane::backend::CompiledRule {
    rule(
        "vinn-site",
        10,
        &format!("{RULE_TARGET}/32"),
        CompiledAction::Mark(MARK_BASE + 2),
    )
}

pub(super) fn assert_echo(reply: &str, lane: usize, payload: &str) {
    let (observed_source, reply_payload) = reply
        .strip_prefix("peer=")
        .and_then(|rest| rest.split_once(' '))
        .unwrap_or_else(|| panic!("malformed endpoint reply {reply:?}"));
    assert_eq!(
        observed_source, LANE_SOURCES[lane],
        "remote observed source"
    );
    assert_eq!(reply_payload, payload, "reply reached the original socket");
}

pub(super) fn tcp_connect(target: &str) -> std::io::Result<TcpStream> {
    let address: SocketAddr = if target.contains(':') {
        format!("[{target}]:{ECHO_PORT}")
    } else {
        format!("{target}:{ECHO_PORT}")
    }
    .parse()
    .unwrap();
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(3))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    Ok(stream)
}

pub(super) fn tcp_exchange(stream: &mut TcpStream, payload: &str) -> String {
    writeln!(stream, "{payload}").expect("send over TCP");
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).expect("TCP reply");
    line.trim_end().to_owned()
}

pub(super) fn udp_exchange(target: &str, payload: &str) -> Option<String> {
    udp_exchange_port(target, ECHO_PORT, payload)
}

fn udp_exchange_port(target: &str, port: u16, payload: &str) -> Option<String> {
    let (bind, address) = if target.contains(':') {
        ("[::]:0", format!("[{target}]:{port}"))
    } else {
        ("0.0.0.0:0", format!("{target}:{port}"))
    };
    let socket = UdpSocket::bind(bind).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket.connect(address).unwrap();
    socket.send(payload.as_bytes()).ok()?;
    let mut buffer = [0; 256];
    let length = socket.recv(&mut buffer).ok()?;
    Some(String::from_utf8_lossy(&buffer[..length]).into_owned())
}

fn bind_to_device(socket: &UdpSocket, interface: &str) {
    let result = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            interface.as_ptr().cast(),
            interface.len() as libc::socklen_t,
        )
    };
    assert_eq!(result, 0, "bind DHCP socket to {interface}");
}

fn wait_for_connected_route(interface: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = checked_output(
            Command::new("/usr/sbin/ip").args(["-j", "-4", "route", "show", "dev", interface]),
            "read connected routes",
        );
        let routes: Value = serde_json::from_slice(&output.stdout).unwrap();
        let link = checked_output(
            Command::new("/usr/sbin/ip").args(["-j", "link", "show", "dev", interface]),
            "read link state",
        );
        let link: Value = serde_json::from_slice(&link.stdout).unwrap();
        if link[0]["operstate"] == "UP" && !routes.as_array().unwrap().is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{interface} did not come back up"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn set_client_rp_filter(names: &RunNames, mode: u8) {
    for interface in
        std::iter::once("all").chain(names.client_interfaces.iter().map(String::as_str))
    {
        checked_output(
            Command::new("/usr/sbin/sysctl").args([
                "-q",
                "-w",
                &format!("net.ipv4.conf.{interface}.rp_filter={mode}"),
                &format!("net.ipv4.conf.{interface}.src_valid_mark=0"),
            ]),
            "configure run-owned client reverse-path filtering",
        );
    }
}

fn sysctl(key: &str) -> u8 {
    let path = format!("/proc/sys/{}", key.replace('.', "/"));
    std::fs::read_to_string(path)
        .expect("read client namespace tunable")
        .trim()
        .parse()
        .unwrap()
}

fn send_from_namespace(namespace: &str, spec: &str) {
    checked_output(
        Command::new("/usr/sbin/ip")
            .args(["netns", "exec", namespace])
            .arg(env::current_exe().unwrap())
            .args([
                "--exact",
                "primary_failover::udp_send_worker",
                "--test-threads=1",
            ])
            .env(SEND_ENV, spec),
        "send a datagram from a gateway namespace",
    );
}

#[test]
fn udp_send_worker() {
    let Ok(spec) = env::var(SEND_ENV) else {
        return;
    };
    let (source, destination) = spec.split_once('>').expect("source>destination");
    let socket = UdpSocket::bind(source).expect("bind spoof source in gateway namespace");
    socket.send_to(b"wrong-path", destination).unwrap();
}

/// Resources RouteLane does not own, inside the run-owned client namespace.
pub(super) fn install_foreign_sentinels() -> Value {
    let mut nft = Command::new("/usr/sbin/nft")
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    nft.stdin
        .take()
        .unwrap()
        .write_all(
            b"add table ip foreign_sentinel\n\
              add chain ip foreign_sentinel keep { type filter hook output priority 10; policy accept; }\n\
              add rule ip foreign_sentinel keep meta mark 0x1 counter accept\n",
        )
        .unwrap();
    assert!(nft.wait().unwrap().success());
    run_ip([
        "rule", "add", "priority", "9000", "fwmark", "0x1", "lookup", "100",
    ]);
    run_ip([
        "route",
        "add",
        "default",
        "via",
        "10.10.2.1",
        "table",
        "100",
    ]);
    foreign_sentinels()
}

pub(super) fn foreign_sentinels() -> Value {
    let read = |args: &[&str]| -> Value {
        let output = checked_output(Command::new(args[0]).args(&args[1..]), "read sentinel");
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let mut nft = read(&[
        "/usr/sbin/nft",
        "-j",
        "list",
        "table",
        "ip",
        "foreign_sentinel",
    ]);
    strip_handles(&mut nft);
    let tunables = [
        "net.ipv4.conf.all.rp_filter",
        "net.ipv4.conf.default.rp_filter",
        "net.ipv4.conf.all.src_valid_mark",
        "net.ipv4.conf.default.src_valid_mark",
        "net.ipv4.ip_forward",
    ]
    .map(|key| json!([key, sysctl(key)]));
    json!({
        "nft": nft,
        "rules4": read(&["/usr/sbin/ip", "-j", "-4", "rule", "show"]),
        "rules6": read(&["/usr/sbin/ip", "-j", "-6", "rule", "show"]),
        "main4": read(&["/usr/sbin/ip", "-j", "-4", "route", "show", "table", "main"]),
        "table100": read(&["/usr/sbin/ip", "-j", "-4", "route", "show", "table", "100"]),
        "tunables": tunables,
    })
}

fn strip_handles(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("handle");
            object.remove("metainfo");
            for nested in object.values_mut() {
                strip_handles(nested);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(strip_handles),
        _ => {}
    }
}

/// Loopback DNS stub, as systemd-resolved provides on 127.0.0.53.
struct LoopbackResolver;

impl LoopbackResolver {
    fn start() -> Self {
        let socket = UdpSocket::bind("127.0.0.53:53").expect("bind loopback stub");
        std::thread::spawn(move || serve_udp(socket));
        Self
    }
}

fn add_sink_target(names: &RunNames, lane: usize, target: &str) {
    run_ip([
        "-n",
        &names.sinks[lane],
        "addr",
        "add",
        &format!("{target}/32"),
        "dev",
        "lo",
    ]);
    run_ip([
        "-n",
        &names.gateways[lane],
        "route",
        "add",
        &format!("{target}/32"),
        "via",
        &format!("10.20.{}.2", lane + 1),
        "dev",
        &names.gateway_sink_interfaces[lane],
    ]);
}

/// Dual-stack lane B (the OS default) plus a forwarded-packet counter.
fn provision_lane_b_ipv6(names: &RunNames) {
    let gateway = &names.gateways[1];
    let sink = &names.sinks[1];
    let address = |namespace: &str, address: &str, interface: &str| {
        run_ip([
            "-n", namespace, "-6", "addr", "add", address, "dev", interface, "nodad",
        ]);
    };
    address(
        &names.client,
        "2001:db8:2::2/64",
        &names.client_interfaces[1],
    );
    address(
        gateway,
        "2001:db8:2::1/64",
        &names.gateway_client_interfaces[1],
    );
    address(
        gateway,
        "2001:db8:22::1/64",
        &names.gateway_sink_interfaces[1],
    );
    address(sink, "2001:db8:22::2/64", &names.sink_interfaces[1]);
    run_ip([
        "-n",
        sink,
        "-6",
        "addr",
        "add",
        &format!("{V6_TARGET}/128"),
        "dev",
        "lo",
    ]);
    checked_output(
        Command::new("/usr/sbin/ip").args([
            "netns",
            "exec",
            gateway,
            "/usr/sbin/sysctl",
            "-q",
            "-w",
            "net.ipv6.conf.all.forwarding=1",
        ]),
        "enable IPv6 forwarding in gateway B",
    );
    run_ip([
        "-n",
        gateway,
        "-6",
        "route",
        "add",
        &format!("{V6_TARGET}/128"),
        "via",
        "2001:db8:22::2",
    ]);
    run_ip([
        "-n",
        sink,
        "-6",
        "route",
        "add",
        "2001:db8:2::/64",
        "via",
        "2001:db8:22::1",
    ]);
    run_ip([
        "-n",
        &names.client,
        "-6",
        "route",
        "add",
        "default",
        "via",
        "2001:db8:2::1",
        "dev",
        &names.client_interfaces[1],
    ]);
    install_observer(gateway, &format!("ip6 daddr {V6_TARGET}"), "v6_target");
}

/// Dual-stack lane A with a global (not link-local) IPv6 gateway.
fn provision_lane_a_ipv6(names: &RunNames) {
    let gateway = &names.gateways[0];
    let sink = &names.sinks[0];
    let address = |namespace: &str, address: &str, interface: &str| {
        run_ip([
            "-n", namespace, "-6", "addr", "add", address, "dev", interface, "nodad",
        ]);
    };
    address(
        &names.client,
        "2001:db8:1::2/64",
        &names.client_interfaces[0],
    );
    address(
        gateway,
        "2001:db8:1::1/64",
        &names.gateway_client_interfaces[0],
    );
    address(
        gateway,
        "2001:db8:11::1/64",
        &names.gateway_sink_interfaces[0],
    );
    address(sink, "2001:db8:11::2/64", &names.sink_interfaces[0]);
    run_ip([
        "-n",
        sink,
        "-6",
        "addr",
        "add",
        &format!("{V6_A_TARGET}/128"),
        "dev",
        "lo",
    ]);
    checked_output(
        Command::new("/usr/sbin/ip").args([
            "netns",
            "exec",
            gateway,
            "/usr/sbin/sysctl",
            "-q",
            "-w",
            "net.ipv6.conf.all.forwarding=1",
        ]),
        "enable IPv6 forwarding in gateway A",
    );
    run_ip([
        "-n",
        gateway,
        "-6",
        "route",
        "add",
        &format!("{V6_A_TARGET}/128"),
        "via",
        "2001:db8:11::2",
    ]);
    run_ip([
        "-n",
        sink,
        "-6",
        "route",
        "add",
        "2001:db8:1::/64",
        "via",
        "2001:db8:11::1",
    ]);
}

/// Counts matching packets as they arrive at a run-owned gateway.
fn install_observer(namespace: &str, matches: &str, comment: &str) {
    let mut nft = Command::new("/usr/sbin/ip")
        .args(["netns", "exec", namespace, "/usr/sbin/nft", "-f", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    nft.stdin
        .take()
        .unwrap()
        .write_all(
            format!(
                "add table inet routelane_test_observer\n\
                 add chain inet routelane_test_observer observe {{ type filter hook prerouting priority -300; policy accept; }}\n\
                 add rule inet routelane_test_observer observe {matches} counter comment \"{comment}\"\n"
            )
            .as_bytes(),
        )
        .unwrap();
    assert!(nft.wait().unwrap().success());
}

fn gateway_counter(names: &RunNames, lane: usize, comment: &str) -> u64 {
    let output = checked_output(
        Command::new("/usr/sbin/ip").args([
            "netns",
            "exec",
            &names.gateways[lane],
            "/usr/sbin/nft",
            "-j",
            "list",
            "table",
            "inet",
            "routelane_test_observer",
        ]),
        "read gateway observer counter",
    );
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    document["nftables"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|object| object["rule"]["comment"] == comment)
        .find_map(|object| {
            object["rule"]["expr"]
                .as_array()?
                .iter()
                .find_map(|expression| expression["counter"]["packets"].as_u64())
        })
        .expect("observer counter rule")
}

/// TCP and UDP echo endpoint inside a run-owned namespace. Bind the exact
/// service address: an unconnected UDP reply from a wildcard socket would use
/// the outgoing interface address and never match the client's flow.
pub(super) struct EchoServer(Child);

impl EchoServer {
    pub(super) fn spawn(namespace: &str, binds: &[&str]) -> Self {
        let mut child = Command::new("/usr/sbin/ip")
            .args(["netns", "exec", namespace])
            .arg(env::current_exe().unwrap())
            .args([
                "--exact",
                "primary_failover::echo_server_worker",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(SERVER_ENV, binds.join(","))
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn echo endpoint");
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        let mut ready = false;
        while stdout.read_line(&mut line).unwrap() > 0 {
            if line.contains("ECHO-READY") {
                ready = true;
                break;
            }
            line.clear();
        }
        if !ready {
            let _ = child.kill();
            let _ = child.wait();
            panic!("echo endpoint in {namespace} exited before it was ready");
        }
        Self(child)
    }
}

impl Drop for EchoServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn echo_server_worker() {
    let Ok(binds) = env::var(SERVER_ENV) else {
        return;
    };
    for bind in binds.split(',') {
        let address: SocketAddr = bind.parse().unwrap();
        let tcp = TcpListener::bind(address).expect("bind TCP echo");
        std::thread::spawn(move || {
            for stream in tcp.incoming().flatten() {
                std::thread::spawn(move || serve_tcp(stream));
            }
        });
        let udp = UdpSocket::bind(address).expect("bind UDP echo");
        std::thread::spawn(move || serve_udp(udp));
    }
    println!("ECHO-READY");
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::park();
    }
}

fn serve_tcp(stream: TcpStream) {
    let Ok(peer) = stream.peer_addr() else {
        return;
    };
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        let reply = format!("peer={} {}\n", peer.ip(), line.trim_end());
        if writer.write_all(reply.as_bytes()).is_err() {
            return;
        }
        line.clear();
    }
}

fn serve_udp(socket: UdpSocket) {
    let mut buffer = [0; 256];
    while let Ok((length, peer)) = socket.recv_from(&mut buffer) {
        let reply = format!(
            "peer={} {}",
            peer.ip(),
            String::from_utf8_lossy(&buffer[..length])
        );
        let _ = socket.send_to(reply.as_bytes(), peer);
    }
}

/// The real helper, or the schema-1 mark-only semantics for baseline RED.
pub(super) enum Routing {
    Helper {
        helper: HelperProcess,
        _state: tempfile::TempDir,
    },
    Legacy {
        active: Option<KernelPlan>,
    },
}

impl Routing {
    /// The real helper process and its journal directory.
    pub(super) fn helper(&mut self) -> (&mut HelperProcess, &std::path::Path) {
        match self {
            Self::Helper { helper, _state } => (helper, _state.path()),
            Self::Legacy { .. } => panic!("the schema-1 baseline has no helper"),
        }
    }

    /// A fresh helper process on the same journal, as after a crash.
    pub(super) fn restart_helper(&mut self) {
        match self {
            Self::Helper { helper, _state } => *helper = HelperProcess::spawn(_state.path()),
            Self::Legacy { .. } => panic!("the schema-1 baseline has no helper"),
        }
    }

    pub(super) fn start(names: &RunNames) -> Self {
        if env::var_os(LEGACY_ENV).is_some() {
            return Self::Legacy { active: None };
        }
        let state = tempfile::Builder::new()
            .prefix(&format!("routelane-test-{}-state-", names.token))
            .tempdir_in("/tmp")
            .expect("create isolated helper journal directory");
        std::fs::set_permissions(
            state.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        Self::Helper {
            helper: HelperProcess::spawn(state.path()),
            _state: state,
        }
    }

    pub(super) fn apply(&mut self, plan: &KernelPlan) {
        match self {
            Self::Helper { helper, .. } => {
                let applied = helper.ok(json!({
                    "method": "apply",
                    "plan_json": serde_json::to_string(plan).unwrap(),
                }));
                assert_eq!(applied["generation"], plan.generation);
            }
            Self::Legacy { active } => {
                if active.is_some() {
                    legacy_remove(active.take().as_ref().unwrap());
                }
                legacy_install(plan);
                *active = Some(plan.clone());
            }
        }
    }

    pub(super) fn cleanup(&mut self) {
        match self {
            Self::Helper { helper, .. } => {
                assert_eq!(helper.ok(json!({"method": "cleanup"}))["cleaned"], true);
            }
            Self::Legacy { active } => {
                if let Some(plan) = active.take() {
                    legacy_remove(&plan);
                }
            }
        }
        assert_managed_resources_absent();
    }
}

/// Schema-1 semantics: explicit rules only set a mark; unmatched traffic
/// follows the OS default and nothing corrects the source address.
fn legacy_install(plan: &KernelPlan) {
    let mut batch = String::from(
        "add table inet routelane\n\
         add chain inet routelane output { type route hook output priority mangle; policy accept; }\n",
    );
    for rule in &plan.rules {
        let (CompiledTarget::Network(network), CompiledAction::Mark(mark)) =
            (&rule.target, &rule.action)
        else {
            continue;
        };
        batch.push_str(&format!(
            "add rule inet routelane output ip daddr {network} meta mark set {mark:#010x} counter return\n"
        ));
    }
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
    assert!(nft.wait().unwrap().success());
    for lane in plan
        .lanes
        .iter()
        .filter(|lane| lane.availability == LaneAvailability::Available)
    {
        let Some(RouteMode::Via(gateway)) = lane.route_v4 else {
            continue;
        };
        run_ip([
            "route",
            "replace",
            "default",
            "via",
            &gateway.to_string(),
            "dev",
            &lane.interface,
            "table",
            &lane.table.to_string(),
        ]);
        run_ip([
            "rule",
            "add",
            "fwmark",
            &format!("{:#010x}", lane.mark),
            "lookup",
            &lane.table.to_string(),
            "priority",
            &lane.rule_priority.to_string(),
        ]);
    }
}

fn legacy_remove(plan: &KernelPlan) {
    let _ = command_output(Command::new("/usr/sbin/nft").args([
        "delete",
        "table",
        "inet",
        "routelane",
    ]));
    for lane in &plan.lanes {
        let _ = command_output(Command::new("/usr/sbin/ip").args([
            "rule",
            "del",
            "priority",
            &lane.rule_priority.to_string(),
        ]));
        let _ = command_output(Command::new("/usr/sbin/ip").args([
            "route",
            "flush",
            "table",
            &lane.table.to_string(),
        ]));
    }
}
