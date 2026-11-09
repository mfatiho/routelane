//! Public runtime-authority contracts. Behaviour that needs the private kernel
//! and journal fakes lives in the `backend::privileged` unit harness.

#[path = "support/primary.rs"]
mod fixtures;

use routelane::backend::{
    policy_hash, restrict_to_grant, InspectResponse, InterfaceIdentity, PlanHash,
    ResolutionSnapshot, RuntimeActivation, RuntimeGrant, RuntimePhase, RuntimeStatus,
    RUNTIME_PROTOCOL_VERSION,
};
use routelane::domain::{Profile, UnmatchedTraffic};

fn grant_for(topology: &routelane::backend::TopologySnapshot) -> RuntimeGrant {
    grant_with(&fixtures::profile(), topology)
}

fn grant_with(profile: &Profile, topology: &routelane::backend::TopologySnapshot) -> RuntimeGrant {
    RuntimeGrant {
        owner_uid: 1000,
        boot_id: "boot".into(),
        network_namespace: 1,
        policy_hash: policy_hash(profile, &ResolutionSnapshot::empty()),
        interfaces: profile
            .lanes
            .iter()
            .map(|lane| {
                let identity = topology
                    .interface(lane.interface.as_str())
                    .and_then(|entry| entry.identity.clone())
                    .unwrap();
                (lane.id.clone(), identity)
            })
            .collect(),
    }
}

#[test]
fn policy_hash_covers_routing_rules_and_resolutions() {
    let profile = fixtures::profile();
    let base = policy_hash(&profile, &ResolutionSnapshot::empty());
    let mut wider = profile.clone();
    wider.routing.backups.clear();
    assert_ne!(policy_hash(&wider, &ResolutionSnapshot::empty()), base);
    let resolved =
        ResolutionSnapshot::from_rule("site".into(), vec!["198.51.100.0/24".parse().unwrap()]);
    assert_ne!(policy_hash(&profile, &resolved), base);
    assert_eq!(policy_hash(&profile, &ResolutionSnapshot::empty()), base);
}

#[test]
fn grant_filter_treats_a_reused_interface_name_as_absent() {
    let grant = grant_for(&fixtures::topology(true, true));
    let replaced = fixtures::topology_case(true, true, true, true, "usb0", 9);
    let filtered = restrict_to_grant(&replaced, &grant);
    assert!(
        filtered.interface("usb0").is_none(),
        "new device behind an old name"
    );
    assert!(filtered.interface("eth0").is_some());
    assert!(filtered.interface("third0").is_none(), "never granted");
}

#[test]
fn activation_schema_is_strict_and_versioned() {
    let activation = RuntimeActivation {
        protocol_version: RUNTIME_PROTOCOL_VERSION,
        profile: fixtures::profile(),
        resolutions: ResolutionSnapshot::empty(),
        expected_generation: 0,
    };
    assert_eq!(RUNTIME_PROTOCOL_VERSION, 3);
    let mut value = serde_json::to_value(&activation).unwrap();
    value["owner_uid"] = 0.into();
    assert!(
        serde_json::from_value::<RuntimeActivation>(value).is_err(),
        "a client cannot smuggle grant fields into an activation"
    );
}

#[test]
fn helper1_inspect_responses_decode_as_inactive_runtime() {
    let mut helper1 = serde_json::to_value(InspectResponse::empty()).unwrap();
    helper1.as_object_mut().unwrap().remove("runtime");
    let decoded: InspectResponse = serde_json::from_value(helper1).unwrap();
    assert_eq!(decoded.runtime, RuntimeStatus::inactive(0));
    assert_eq!(decoded.runtime.phase, RuntimePhase::Inactive);
}

#[test]
fn grant_identity_is_the_full_kernel_identity() {
    let grant = grant_for(&fixtures::topology(true, true));
    let usb: &InterfaceIdentity = &grant.interfaces[&"vinn".into()];
    assert_eq!((usb.ifindex, usb.name.as_str()), (3, "usb0"));
    let _: PlanHash = grant.policy_hash;
}

#[test]
fn mode_change_requires_a_new_policy_grant() {
    let topology = fixtures::topology(true, true);
    let mut selective = fixtures::profile();
    selective.routing.unmatched_traffic = UnmatchedTraffic::PreserveSystem;
    let mut primary = fixtures::profile();
    primary.routing.unmatched_traffic = UnmatchedTraffic::UsePrimary;
    let selective_grant = grant_with(&selective, &topology);
    let primary_grant = grant_with(&primary, &topology);
    assert_ne!(selective_grant.policy_hash, primary_grant.policy_hash);

    // Neither grant lets a new device behind an old name acquire authority.
    let replaced = fixtures::topology_case(true, true, true, true, "usb0", 9);
    for grant in [&selective_grant, &primary_grant] {
        assert!(restrict_to_grant(&replaced, grant)
            .interface("usb0")
            .is_none());
    }

    // Only a verified modern runtime reports a mode; an inactive or older
    // status never claims one.
    assert_eq!(RuntimeStatus::inactive(0).unmatched_traffic, None);
    let mut older = serde_json::to_value(RuntimeStatus::inactive(3)).unwrap();
    older.as_object_mut().unwrap().remove("unmatched_traffic");
    let decoded: RuntimeStatus = serde_json::from_value(older).unwrap();
    assert_eq!(decoded.unmatched_traffic, None);
}
