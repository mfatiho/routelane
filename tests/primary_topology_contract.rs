use routelane::backend::{parse_topology, IpFamily, RouteMode, TopologyInput, TopologySnapshot};
use serde_json::{json, Value};

fn links() -> Value {
    json!([
        {"ifindex":2,"ifname":"eth0","link_type":"ether","address":"02:00:00:00:00:01","flags":["UP","LOWER_UP"],"operstate":"UP"},
        {"ifindex":3,"ifname":"usb0","link_type":"ether","address":"02:00:00:00:00:02","flags":["UP","LOWER_UP"],"operstate":"UP"}
    ])
}

fn addresses() -> Value {
    json!([
        {"ifindex":2,"ifname":"eth0","addr_info":[
            {"family":"inet","local":"10.0.0.2","prefixlen":24,"scope":"global"},
            {"family":"inet6","local":"2001:db8:1::2","prefixlen":64,"scope":"global"}
        ]},
        {"ifindex":3,"ifname":"usb0","addr_info":[
            {"family":"inet","local":"192.168.0.2","prefixlen":24,"scope":"global"},
            {"family":"inet6","local":"2001:db8:2::2","prefixlen":64,"scope":"global"}
        ]}
    ])
}

fn routes4() -> Value {
    json!([
        {"dst":"default","table":20000,"dev":"eth0","gateway":"10.0.0.254","metric":0},
        {"dst":"default","table":254,"dev":"eth0","gateway":"10.0.0.1","metric":102},
        {"dst":"default","table":254,"dev":"usb0","gateway":"192.168.0.1","metric":101},
        {"dst":"10.0.0.0/24","table":254,"dev":"eth0","scope":"link","protocol":"kernel"},
        {"dst":"192.168.0.0/24","table":254,"dev":"usb0","scope":"link","protocol":"kernel"}
    ])
}

fn routes6() -> Value {
    json!([
        {"dst":"default","dev":"eth0","gateway":"fe80::1","metric":100},
        {"dst":"default","dev":"usb0","gateway":"fe80::2","metric":200}
    ])
}

fn rules() -> Value {
    json!([
        {"priority":0,"table":255,"src":"all"},
        {"priority":10000,"table":20000,"fwmark":"0x524c0001","src":"all"},
        {"priority":32766,"table":254,"src":"all"},
        {"priority":32767,"table":253,"src":"all"}
    ])
}

fn snapshot(l: Value, a: Value, v4: Value, v6: Value, r: Value) -> TopologySnapshot {
    parse_topology(TopologyInput {
        links: l.to_string().as_bytes(),
        addresses: a.to_string().as_bytes(),
        routes_v4: v4.to_string().as_bytes(),
        routes_v6: v6.to_string().as_bytes(),
        rules_v4: r.to_string().as_bytes(),
        rules_v6: rules().to_string().as_bytes(),
    })
    .unwrap()
}

#[test]
fn owned_defaults_never_become_system_default() {
    let s = snapshot(links(), addresses(), routes4(), routes6(), rules());
    assert_eq!(s.system_default(IpFamily::V4).unwrap(), Some("usb0"));
    assert_eq!(
        s.interface("eth0").unwrap().route_v4,
        Some(RouteMode::Via("10.0.0.1".parse().unwrap()))
    );
    assert_eq!(
        s.interface("eth0").unwrap().source_v4,
        vec!["10.0.0.2".parse::<std::net::Ipv4Addr>().unwrap()]
    );
    assert!(s
        .interface("eth0")
        .unwrap()
        .connected_networks
        .contains(&"10.0.0.0/24".parse().unwrap()));
}

#[test]
fn metric_and_family_select_os_default() {
    let mut reversed = routes4();
    reversed.as_array_mut().unwrap().reverse();
    let s = snapshot(links(), addresses(), reversed, routes6(), rules());
    assert_eq!(s.system_default(IpFamily::V4).unwrap(), Some("usb0"));
    assert_eq!(s.system_default(IpFamily::V6).unwrap(), Some("eth0"));
}

#[test]
fn carrier_route_or_address_loss_is_unavailable() {
    let mut down = links();
    down[0]["flags"] = json!(["UP"]);
    down[0]["operstate"] = json!("DOWN");
    let s = snapshot(down, addresses(), routes4(), routes6(), rules());
    assert!(!s.interface("eth0").unwrap().ready(IpFamily::V4));
    let s = snapshot(links(), json!([]), routes4(), routes6(), rules());
    assert!(!s.interface("eth0").unwrap().ready(IpFamily::V4));
    let s = snapshot(links(), addresses(), json!([]), routes6(), rules());
    assert!(!s.interface("eth0").unwrap().ready(IpFamily::V4));
    assert!(s.interface("eth0").unwrap().ready(IpFamily::V6));
}

#[test]
fn tentative_ipv6_is_not_ready() {
    let mut a = addresses();
    a[0]["addr_info"][1]["flags"] = json!(["tentative"]);
    let s = snapshot(links(), a, routes4(), routes6(), rules());
    assert!(!s.interface("eth0").unwrap().ready(IpFamily::V6));
    assert!(s.interface("eth0").unwrap().ready(IpFamily::V4));
    assert_eq!(s.system_default(IpFamily::V6).unwrap(), Some("usb0"));
}

#[test]
fn same_name_new_ifindex_is_not_same_identity() {
    let before = snapshot(links(), addresses(), routes4(), routes6(), rules());
    let mut l = links();
    let mut a = addresses();
    l[1]["ifindex"] = json!(9);
    a[1]["ifindex"] = json!(9);
    let after = snapshot(l, a, routes4(), routes6(), rules());
    assert_ne!(
        before.interface("usb0").unwrap().identity,
        after.interface("usb0").unwrap().identity
    );
}

#[test]
fn ambiguous_metric_or_foreign_policy_is_not_arbitrarily_chosen() {
    let mut v4 = routes4();
    v4[1]["metric"] = json!(101);
    let s = snapshot(links(), addresses(), v4, routes6(), rules());
    assert!(s.system_default(IpFamily::V4).is_err());
    assert!(s.interface("eth0").unwrap().ready(IpFamily::V4));
    let mut r = rules();
    r.as_array_mut()
        .unwrap()
        .push(json!({"priority":1,"table":100,"src":"all"}));
    let s = snapshot(links(), addresses(), routes4(), routes6(), r);
    assert!(s.system_default(IpFamily::V4).is_err());
    assert!(!s.routing_conflicts().is_empty());
}

#[test]
fn terminal_default_is_not_silently_ignored() {
    let mut routes = routes4();
    routes
        .as_array_mut()
        .unwrap()
        .push(json!({"dst":"default","table":254,"type":"unreachable","metric":0}));
    let s = snapshot(links(), addresses(), routes, routes6(), rules());
    assert!(s.system_default(IpFamily::V4).is_err());
    assert!(!s.routing_conflicts().is_empty());
}

#[test]
fn malformed_json_and_multipath_do_not_manufacture_a_default() {
    assert!(parse_topology(TopologyInput {
        links: b"{",
        addresses: b"[]",
        routes_v4: b"[]",
        routes_v6: b"[]",
        rules_v4: b"[]",
        rules_v6: b"[]"
    })
    .is_err());
    let multipath = json!([{"dst":"default","table":254,"nexthops":[{"dev":"eth0","gateway":"10.0.0.1"},{"dev":"usb0","gateway":"192.168.0.1"}]}]);
    let s = snapshot(links(), addresses(), multipath, routes6(), rules());
    assert!(s.system_default(IpFamily::V4).is_err());
}

#[test]
fn gatewayless_point_to_point_route_requires_a_usable_address() {
    let mut l = links();
    l[0]["flags"] = json!(["UP", "POINTOPOINT"]);
    l[0]["operstate"] = json!("UNKNOWN");
    let s = snapshot(
        l,
        addresses(),
        json!([{"dst":"default","dev":"eth0","table":"main"}]),
        json!([]),
        rules(),
    );
    assert_eq!(s.system_default(IpFamily::V4).unwrap(), Some("eth0"));
    assert_eq!(
        s.interface("eth0").unwrap().route_v4,
        Some(RouteMode::Device)
    );
}

#[test]
fn stale_owned_route_does_not_make_a_down_interface_reachable() {
    use routelane::backend::{KernelSnapshot, SnapshotInput};
    let mut l = links();
    l[0]["flags"] = json!(["UP"]);
    l[0]["operstate"] = json!("DOWN");
    let topology = snapshot(l, addresses(), routes4(), routes6(), rules());
    let routes = routes4().to_string();
    let kernel = KernelSnapshot::parse(SnapshotInput {
        ipv4_routes: routes.as_bytes(),
        ipv6_routes: b"[]",
        ipv4_rules: b"[]",
        ipv6_rules: b"[]",
        nft: br#"{"nftables":[]}"#,
    })
    .unwrap()
    .with_topology(&topology);
    assert!(!kernel.route_reachable("eth0", IpFamily::V4, Some("10.0.0.254".parse().unwrap())));
}

#[tokio::test]
#[ignore = "reads the host kernel through installed iproute2; run explicitly"]
async fn live_capture_uses_bounded_observed_topology() {
    use routelane::daemon::{SystemTopologyProvider, TopologyProvider};
    use routelane::domain::Profile;
    let profile = Profile {
        id: "capture".into(),
        name: "Capture".into(),
        enabled: false,
        lanes: vec![],
        rules: vec![],
        routing: Default::default(),
    };
    let s = SystemTopologyProvider.snapshot(&profile).await.unwrap();
    assert!(s.is_observed());
    assert!(s.interface("lo").unwrap().identity.is_some());
    for family in [IpFamily::V4, IpFamily::V6] {
        if let Ok(Some(name)) = s.system_default(family) {
            assert!(s.interface(name).unwrap().ready(family));
        }
    }
}
