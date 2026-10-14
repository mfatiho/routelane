use routelane::backend::{parse_topology, TopologyInput, TopologySnapshot};
use routelane::domain::{
    InterfaceSelector, Lane, LaneFailurePolicy, LaneId, PrimarySelection, Profile, RoutingPolicy,
};
use serde_json::json;

pub fn profile() -> Profile {
    Profile {
        id: "office".into(),
        name: "Office".into(),
        enabled: true,
        rules: vec![],
        routing: RoutingPolicy {
            primary: PrimarySelection::Lane("ethernet".into()),
            backups: vec!["vinn".into()],
            automatic_failover: true,
        },
        lanes: [("ethernet", "eth0"), ("vinn", "usb0")]
            .into_iter()
            .map(|(id, interface)| Lane {
                id: LaneId::from(id),
                name: id.to_owned(),
                interface: InterfaceSelector::from(interface),
                fallback_lane: None,
                failure_policy: LaneFailurePolicy::Block,
            })
            .collect(),
    }
}

pub fn topology(ethernet: bool, vinn: bool) -> TopologySnapshot {
    topology_case(ethernet, vinn, true, true, "usb0", 3)
}

pub fn topology_case(
    ethernet: bool,
    vinn: bool,
    eth6: bool,
    vinn6: bool,
    default: &str,
    usb_index: u32,
) -> TopologySnapshot {
    let links = json!([
        {"ifindex":2,"ifname":"eth0","flags":if ethernet {vec!["UP","LOWER_UP"]} else {vec!["UP"]},"operstate":if ethernet {"UP"} else {"DOWN"}},
        {"ifindex":usb_index,"ifname":"usb0","flags":if vinn {vec!["UP","LOWER_UP"]} else {vec!["UP"]},"operstate":if vinn {"UP"} else {"DOWN"}},
        {"ifindex":4,"ifname":"third0","flags":["UP","LOWER_UP"],"operstate":"UP"}
    ]).to_string();
    let addresses = json!([
        {"ifindex":2,"ifname":"eth0","addr_info":[{"family":"inet","local":"10.0.0.2","scope":"global"},{"family":"inet6","local":"2001:db8:1::2","scope":"global"}]},
        {"ifindex":usb_index,"ifname":"usb0","addr_info":[{"family":"inet","local":"192.168.0.2","scope":"global"},{"family":"inet6","local":"2001:db8:2::2","scope":"global"}]},
        {"ifindex":4,"ifname":"third0","addr_info":[{"family":"inet","local":"172.16.0.2","scope":"global"},{"family":"inet6","local":"2001:db8:3::2","scope":"global"}]}
    ]).to_string();
    let mut v4 = vec![];
    let mut v6 = vec![];
    for (name, gateway, gw6, has6) in [
        ("eth0", "10.0.0.1", "2001:db8:1::1", eth6),
        ("usb0", "192.168.0.1", "2001:db8:2::1", vinn6),
        ("third0", "172.16.0.1", "2001:db8:3::1", true),
    ] {
        let metric = if name == default {
            1
        } else if name == "eth0" {
            100
        } else {
            200
        };
        v4.push(json!({"dst":"default","dev":name,"gateway":gateway,"metric":metric}));
        if has6 {
            v6.push(json!({"dst":"default","dev":name,"gateway":gw6,"metric":metric}));
        }
    }
    parse_topology(TopologyInput {
        links: links.as_bytes(),
        addresses: addresses.as_bytes(),
        routes_v4: serde_json::to_string(&v4).unwrap().as_bytes(),
        routes_v6: serde_json::to_string(&v6).unwrap().as_bytes(),
        rules_v4: b"[]",
        rules_v6: b"[]",
    })
    .unwrap()
}
