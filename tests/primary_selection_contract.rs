use std::time::Duration;

use routelane::backend::{
    parse_topology, FamilyExit, PrimarySelector, TopologyInput, TopologySnapshot,
};
use routelane::domain::{
    InterfaceSelector, Lane, LaneFailurePolicy, LaneId, PrimarySelection, Profile, RoutingPolicy,
};
use serde_json::json;

fn profile() -> Profile {
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

fn topology(ethernet: bool, vinn: bool) -> TopologySnapshot {
    topology_case(ethernet, vinn, true, true, "usb0", 3)
}

fn topology_case(
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

fn lane(id: &str) -> FamilyExit {
    FamilyExit::Lane(id.into())
}
fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

#[test]
fn primary_loss_uses_approved_backup_without_changing_preference() {
    let p = profile();
    let mut selector = PrimarySelector::new(&p, &topology(true, true));
    assert_eq!(
        selector
            .update(&p.routing, &topology(true, true), ms(0))
            .effective
            .v4,
        lane("ethernet")
    );
    let down = selector.update(&p.routing, &topology(false, true), ms(10));
    assert_eq!(down.effective.v4, lane("vinn"));
    assert_eq!(down.preferred.v4, Some("ethernet".into()));
    assert_eq!(p.routing.primary, PrimarySelection::Lane("ethernet".into()));
    assert_eq!(
        selector.update(&p.routing, &topology(false, true), ms(11)),
        down
    );
}

#[test]
fn failback_requires_5000_ms_continuous_readiness() {
    let p = profile();
    let mut selector = PrimarySelector::new(&p, &topology(true, true));
    selector.update(&p.routing, &topology(false, true), ms(0));
    assert_eq!(
        selector
            .update(&p.routing, &topology(true, true), ms(20))
            .effective
            .v4,
        lane("vinn")
    );
    assert_eq!(selector.next_deadline(), Some(ms(5020)));
    assert_eq!(
        selector
            .update(&p.routing, &topology(true, true), ms(5019))
            .effective
            .v4,
        lane("vinn")
    );
    assert_eq!(
        selector
            .update(&p.routing, &topology(true, true), ms(5020))
            .effective
            .v4,
        lane("ethernet")
    );
    assert_eq!(selector.next_deadline(), None);
}

#[test]
fn a_short_flap_resets_the_failback_deadline() {
    let p = profile();
    let mut selector = PrimarySelector::new(&p, &topology(true, true));
    selector.update(&p.routing, &topology(false, true), ms(0));
    selector.update(&p.routing, &topology(true, true), ms(20));
    selector.update(&p.routing, &topology(false, true), ms(4000));
    selector.update(&p.routing, &topology(true, true), ms(5000));
    assert_eq!(selector.next_deadline(), Some(ms(10000)));
    assert_eq!(
        selector
            .update(&p.routing, &topology(true, true), ms(9999))
            .effective
            .v4,
        lane("vinn")
    );
    assert_eq!(
        selector
            .update(&p.routing, &topology(true, true), ms(10000))
            .effective
            .v4,
        lane("ethernet")
    );
}

#[test]
fn v6_loss_does_not_follow_v4() {
    let p = profile();
    let mut selector = PrimarySelector::new(&p, &topology(true, true));
    let d = selector.update(
        &p.routing,
        &topology_case(true, true, false, true, "usb0", 3),
        ms(0),
    );
    assert_eq!(d.effective.v4, lane("ethernet"));
    assert_eq!(d.effective.v6, lane("vinn"));
    let d = selector.update(
        &p.routing,
        &topology_case(true, true, false, false, "usb0", 3),
        ms(1),
    );
    assert_eq!(d.effective.v4, lane("ethernet"));
    assert_eq!(d.effective.v6, FamilyExit::Blocked);
}

#[test]
fn no_exit_or_disabled_failover_blocks() {
    let mut p = profile();
    let mut selector = PrimarySelector::new(&p, &topology(true, true));
    assert_eq!(
        selector
            .update(&p.routing, &topology(false, false), ms(0))
            .effective
            .v4,
        FamilyExit::Blocked
    );
    p.routing.automatic_failover = false;
    assert_eq!(
        selector
            .update(&p.routing, &topology(false, true), ms(1))
            .effective
            .v4,
        FamilyExit::Blocked
    );
}

#[test]
fn system_default_outside_grant_is_not_adopted() {
    let mut p = profile();
    p.routing.primary = PrimarySelection::FollowSystemDefault;
    let mut selector = PrimarySelector::new(&p, &topology(true, true));
    let d = selector.update(
        &p.routing,
        &topology_case(true, true, true, true, "third0", 3),
        ms(0),
    );
    assert_eq!(d.preferred.v4, None);
    assert_eq!(d.effective.v4, lane("vinn"));
}

#[test]
fn interface_recreation_cannot_inherit_an_authorized_identity() {
    let p = profile();
    let mut selector = PrimarySelector::new(&p, &topology(true, true));
    let d = selector.update(
        &p.routing,
        &topology_case(false, true, true, true, "usb0", 9),
        ms(0),
    );
    assert_eq!(d.effective.v4, FamilyExit::Blocked);
    assert_eq!(d.effective.v6, FamilyExit::Blocked);
}

#[test]
fn no_working_exit_does_not_delay_recovery_of_primary() {
    let p = profile();
    let mut selector = PrimarySelector::new(&p, &topology(true, true));
    selector.update(&p.routing, &topology(false, false), ms(0));
    assert_eq!(
        selector
            .update(&p.routing, &topology(true, false), ms(1))
            .effective
            .v4,
        lane("ethernet")
    );
    assert_eq!(selector.next_deadline(), None);
}

#[test]
fn default_lane_id_is_not_a_role() {
    let mut p = profile();
    p.lanes[1].id = "default".into();
    p.routing = RoutingPolicy {
        primary: PrimarySelection::FollowSystemDefault,
        backups: vec!["default".into()],
        automatic_failover: true,
    };
    let t = topology_case(true, true, true, true, "eth0", 3);
    let mut selector = PrimarySelector::new(&p, &t);
    let d = selector.update(&p.routing, &t, ms(0));
    assert_eq!(d.preferred.v4, Some("ethernet".into()));
    assert_eq!(d.effective.v4, lane("ethernet"));
}

#[test]
fn two_lane_aliases_for_system_default_require_explicit_selection() {
    let mut p = profile();
    p.lanes[1].interface = "eth0".into();
    p.routing.primary = PrimarySelection::FollowSystemDefault;
    let t = topology_case(true, true, true, true, "eth0", 3);
    let mut selector = PrimarySelector::new(&p, &t);
    assert_eq!(
        selector.update(&p.routing, &t, ms(0)).effective.v4,
        FamilyExit::Blocked
    );
}

#[test]
fn resumed_backup_restarts_stabilization_without_immediate_failback() {
    let p = profile();
    let ready = topology(true, true);
    let mut previous = PrimarySelector::new(&p, &ready);
    let decision = previous.update(&p.routing, &topology(false, true), ms(10));
    let mut resumed = PrimarySelector::resume(&p, &ready, &decision);
    assert_eq!(
        resumed.update(&p.routing, &ready, ms(10000)).effective.v4,
        lane("vinn")
    );
    assert_eq!(resumed.next_deadline(), Some(ms(15000)));
    assert_eq!(
        resumed.update(&p.routing, &ready, ms(14999)).effective.v4,
        lane("vinn")
    );
    assert_eq!(
        resumed.update(&p.routing, &ready, ms(15000)).effective.v4,
        lane("ethernet")
    );
}
