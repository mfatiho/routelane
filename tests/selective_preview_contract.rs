//! Read-only routing preview: which enabled explicit rules reach a network
//! the helper observes as directly connected (a lab bridge, the LAN), and
//! which rules match all traffic. The preview never saves, arms or edits.

#[path = "support/primary.rs"]
mod primary_fixtures;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ipnet::IpNet;
use routelane::backend::{
    compile_profile, policy_hash, routing_preview, ApplyResponse, CleanupResponse, InspectResponse,
    KernelPlan, OrphanRecoveryResponse, ResolutionSnapshot, RestoreResponse, RoutingPreview,
    RoutingWarning, RuntimeActivation, RuntimeApplyResponse, TopologySnapshot,
    RUNTIME_PROTOCOL_VERSION,
};
use routelane::daemon::{
    ConfigRepository, DaemonError, DaemonService, PrivilegedHelperClient, ResolutionProvider,
    TopologyProvider,
};
use routelane::domain::{
    ApplicationSelector, LaneId, Profile, Rule, RuleAction, RuleId, RuleSource, RuleTarget,
};
use routelane::ipc::{DaemonEnvelope, DaemonRequest, DaemonResult};

fn net(text: &str) -> IpNet {
    text.parse().unwrap()
}

fn rule(id: &str, target: RuleTarget, action: RuleAction) -> Rule {
    Rule {
        id: RuleId::from(id),
        enabled: true,
        priority: 10,
        source: RuleSource::Any,
        target,
        action,
    }
}

fn route(lane: &str) -> RuleAction {
    RuleAction::Route(LaneId::from(lane))
}

fn application_rule() -> Rule {
    Rule {
        id: RuleId::from("browser"),
        enabled: true,
        priority: 5,
        source: RuleSource::Application(ApplicationSelector {
            desktop_id: None,
            executable_path: "/usr/bin/firefox".into(),
            include_children: true,
            launch: None,
        }),
        target: RuleTarget::Any,
        action: route("vinn"),
    }
}

fn lab_profile(enabled: bool) -> Profile {
    let mut profile = primary_fixtures::profile();
    let mut lab = rule(
        "lab-route",
        RuleTarget::Cidr(net("10.65.201.0/24")),
        route("ethernet"),
    );
    lab.enabled = enabled;
    profile.rules = vec![lab];
    profile
}

const LAB: &str = "10.65.201.0/24";

fn local(rule_id: &str, network: &str) -> RoutingWarning {
    RoutingWarning::LocalNetworkRule {
        rule_id: RuleId::from(rule_id),
        network: net(network),
    }
}

#[test]
fn local_cidr_overlap_is_warned_without_becoming_an_exit() {
    let mut profile = primary_fixtures::profile();
    profile.rules = vec![
        rule("lab-route", RuleTarget::Cidr(net(LAB)), route("ethernet")),
        rule(
            "lab-host",
            RuleTarget::Ip("10.65.201.60".parse().unwrap()),
            RuleAction::Direct,
        ),
        rule(
            "wide-block",
            RuleTarget::Cidr(net("10.0.0.0/8")),
            RuleAction::Block,
        ),
        rule(
            "lab-half",
            RuleTarget::Cidr(net("10.65.201.128/25")),
            route("vinn"),
        ),
        rule(
            "lab-v6",
            RuleTarget::Cidr(net("fd00:65::/64")),
            route("vinn"),
        ),
        rule(
            "lab-name",
            RuleTarget::Domain("lab.example".into()),
            route("vinn"),
        ),
        rule(
            "public",
            RuleTarget::Cidr(net("198.51.100.0/24")),
            route("vinn"),
        ),
        rule("everything", RuleTarget::Any, route("vinn")),
        application_rule(),
    ];
    let resolutions =
        ResolutionSnapshot::from_rule(RuleId::from("lab-name"), vec![net("10.65.201.7/32")]);
    let before = profile.clone();
    let observed = [
        net("10.65.201.1/24"),
        net("fd00:65::1/64"),
        net("192.168.122.0/24"),
    ];

    let preview = routing_preview(&profile, &resolutions, &observed);

    assert!(preview
        .warnings
        .contains(&RoutingWarning::LocalNetworkRule {
            rule_id: RuleId::from("lab-route"),
            network: "10.65.201.0/24".parse().unwrap(),
        }));
    let mut expected = vec![
        local("lab-route", LAB),
        local("lab-host", LAB),
        local("wide-block", LAB),
        local("lab-half", LAB),
        local("lab-v6", "fd00:65::/64"),
        local("lab-name", LAB),
        RoutingWarning::AllTrafficRule {
            rule_id: RuleId::from("everything"),
        },
    ];
    expected.sort();
    assert_eq!(preview.warnings, expected);
    assert_eq!(preview.policy_hash, policy_hash(&profile, &resolutions));
    // No rule edit, no extra lane: the plan still has only the profile's
    // own exits.
    assert_eq!(profile, before);
    let plan = compile_profile(
        &profile,
        &primary_fixtures::topology(true, true),
        &resolutions,
        1,
    )
    .unwrap();
    let mut exits = plan
        .lanes
        .iter()
        .map(|lane| lane.interface.as_str())
        .collect::<Vec<_>>();
    exits.sort();
    assert_eq!(exits, ["eth0", "usb0"]);
    // Without facts about the lab, nothing about it is claimed.
    assert!(routing_preview(&profile, &resolutions, &[])
        .warnings
        .iter()
        .all(|warning| matches!(warning, RoutingWarning::AllTrafficRule { .. })));
}

#[test]
fn disabled_lab_rule_is_not_warned_or_deleted() {
    let profile = lab_profile(false);
    let before = profile.clone();
    let preview = routing_preview(&profile, &ResolutionSnapshot::empty(), &[net(LAB)]);
    assert!(preview.warnings.is_empty());
    assert_eq!(profile, before);
    assert!(!profile.rules[0].enabled);
}

#[derive(Default)]
struct HelperState {
    observed: Option<Vec<IpNet>>,
    inspections: usize,
}

/// Only the read-only surface exists; every mutation panics.
#[derive(Clone, Default)]
struct PreviewHelper(Arc<Mutex<HelperState>>);

impl PreviewHelper {
    fn observe(&self, networks: Option<Vec<IpNet>>) {
        self.0.lock().unwrap().observed = networks;
    }
}

#[async_trait]
impl PrivilegedHelperClient for PreviewHelper {
    async fn runtime_protocol_version(&self) -> Result<u32, DaemonError> {
        Ok(RUNTIME_PROTOCOL_VERSION)
    }
    async fn inspect(&self) -> Result<InspectResponse, DaemonError> {
        let mut state = self.0.lock().unwrap();
        state.inspections += 1;
        let mut response = InspectResponse::empty();
        response.observed_connected_networks = state.observed.clone();
        Ok(response)
    }
    async fn apply(&self, _: &KernelPlan) -> Result<ApplyResponse, DaemonError> {
        panic!("preview must not apply")
    }
    async fn restore(
        &self,
        _: &KernelPlan,
        _: Option<&KernelPlan>,
    ) -> Result<RestoreResponse, DaemonError> {
        panic!("preview must not restore")
    }
    async fn cleanup(&self) -> Result<CleanupResponse, DaemonError> {
        panic!("preview must not clean up")
    }
    async fn recover_orphaned_resources(&self) -> Result<OrphanRecoveryResponse, DaemonError> {
        panic!("preview must not recover orphans")
    }
    async fn prepare_application_cgroup(
        &self,
        _: &RuleId,
    ) -> Result<std::path::PathBuf, DaemonError> {
        panic!("preview must not prepare cgroups")
    }
    async fn activate_runtime(
        &self,
        _: &RuntimeActivation,
    ) -> Result<RuntimeApplyResponse, DaemonError> {
        panic!("preview must not arm a runtime")
    }
}

/// The daemon's own (user-side) topology must not stand in for the helper's
/// root observation.
#[derive(Clone, Default)]
struct UserTopology(Arc<Mutex<usize>>);

#[async_trait]
impl TopologyProvider for UserTopology {
    async fn snapshot(&self, _: &Profile) -> Result<TopologySnapshot, DaemonError> {
        *self.0.lock().unwrap() += 1;
        Ok(TopologySnapshot::empty())
    }
}

#[derive(Clone)]
struct Resolutions;

#[async_trait]
impl ResolutionProvider for Resolutions {
    async fn snapshot(&self, _: &Profile) -> Result<ResolutionSnapshot, DaemonError> {
        Ok(ResolutionSnapshot::empty())
    }
}

#[derive(Clone, Default)]
struct CountingConfig(Arc<Mutex<usize>>);

impl ConfigRepository for CountingConfig {
    fn load(&self) -> Result<Option<Profile>, DaemonError> {
        Ok(None)
    }
    fn save(&self, _: Option<&Profile>) -> Result<(), DaemonError> {
        *self.0.lock().unwrap() += 1;
        Ok(())
    }
}

type Service = DaemonService<UserTopology, Resolutions, PreviewHelper, CountingConfig>;

fn service(helper: &PreviewHelper, topology: &UserTopology, config: &CountingConfig) -> Service {
    DaemonService::new(
        topology.clone(),
        Resolutions,
        helper.clone(),
        config.clone(),
    )
    .unwrap()
}

async fn preview(service: &Service, profile: &Profile) -> Result<RoutingPreview, String> {
    let response = service
        .handle(DaemonEnvelope::new(
            "preview",
            DaemonRequest::PreviewProfile(profile.clone()),
        ))
        .await;
    match (response.result, response.error) {
        (Some(DaemonResult::Preview(preview)), None) => Ok(preview),
        (None, Some(error)) => Err(error.code),
        other => panic!("unexpected preview response {other:?}"),
    }
}

#[tokio::test]
async fn preview_uses_fresh_root_networks() {
    let helper = PreviewHelper::default();
    let topology = UserTopology::default();
    let service = service(&helper, &topology, &CountingConfig::default());
    let profile = lab_profile(true);

    helper.observe(Some(vec![net(LAB)]));
    let first = preview(&service, &profile).await.unwrap();
    assert_eq!(first.warnings, vec![local("lab-route", LAB)]);

    // Each preview asks the helper again; nothing is cached.
    helper.observe(Some(vec![net("192.168.122.0/24")]));
    let second = preview(&service, &profile).await.unwrap();
    assert!(second.warnings.is_empty());

    // Unknown topology is never presented as a clean preview.
    helper.observe(None);
    assert_eq!(
        preview(&service, &profile).await.unwrap_err(),
        "DAEMON_PREVIEW_UNAVAILABLE"
    );
    assert_eq!(helper.0.lock().unwrap().inspections, 3);
    assert_eq!(*topology.0.lock().unwrap(), 0, "user topology is not used");
}

#[tokio::test]
async fn preview_never_saves_or_activates() {
    let helper = PreviewHelper::default();
    helper.observe(Some(vec![net(LAB)]));
    let config = CountingConfig::default();
    let service = service(&helper, &UserTopology::default(), &config);
    let mut profile = lab_profile(true);
    profile.rules.push(application_rule());
    let profile_before_preview = profile.clone();
    let state_before = service.state().await;

    let result = preview(&service, &profile).await.unwrap();

    let profile_after_preview = profile.clone();
    assert_eq!(profile_after_preview, profile_before_preview);
    assert_eq!(
        result.policy_hash,
        policy_hash(&profile, &ResolutionSnapshot::empty())
    );
    assert_eq!(*config.0.lock().unwrap(), 0);
    let state = service.state().await;
    assert!(state.desired.is_none());
    assert!(state.applied.is_none());
    assert!(state.last_error.is_none());
    assert_eq!(state.event_sequence, state_before.event_sequence);
    assert_eq!(state.health, state_before.health);
    assert_eq!(state.primary, state_before.primary);
}
