use std::collections::BTreeMap;
use std::net::IpAddr;

use ipnet::IpNet;

use crate::domain::{
    validate_profile, DomainError, Lane, LaneFailurePolicy, LaneId, Profile, Rule, RuleAction,
    RuleId, RuleSource, RuleTarget,
};

use super::plan::{
    CompiledAction, CompiledRule, CompiledTarget, KernelPlan, LaneAvailability, LanePlan,
    ResolutionSnapshot, TopologySnapshot, MARK_BASE, RULE_PRIORITY_BASE, TABLE_BASE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Application,
    WildcardDomain,
    ServicePack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    Domain(DomainError),
    ProfileInactive,
    DomainResolutionUnavailable(RuleId),
    UnsupportedCapability {
        rule_id: RuleId,
        capability: Capability,
    },
}

impl From<DomainError> for PlanError {
    fn from(error: DomainError) -> Self {
        Self::Domain(error)
    }
}

pub fn compile_profile(
    profile: &Profile,
    topology: &TopologySnapshot,
    resolutions: &ResolutionSnapshot,
    generation: u64,
) -> Result<KernelPlan, PlanError> {
    if !profile.enabled {
        return Err(PlanError::ProfileInactive);
    }
    validate_profile(profile)?;

    let mut lanes = profile.lanes.iter().collect::<Vec<_>>();
    lanes.sort_by(|left, right| left.id.cmp(&right.id));

    let lane_plans = lanes
        .iter()
        .enumerate()
        .map(|(slot, lane)| compile_lane(lane, topology, slot as u32))
        .collect::<Vec<_>>();
    let profile_lanes_by_id = profile
        .lanes
        .iter()
        .map(|lane| (lane.id.clone(), lane))
        .collect::<BTreeMap<_, _>>();
    let lanes_by_id = lane_plans
        .iter()
        .map(|lane| (lane.id.clone(), lane))
        .collect::<BTreeMap<_, _>>();

    let mut rules = profile
        .rules
        .iter()
        .filter(|rule| rule.enabled)
        .collect::<Vec<_>>();
    rules.sort_by(|left, right| left.execution_cmp(right));

    let mut compiled_rules = Vec::new();
    for rule in rules {
        compile_rule(
            rule,
            resolutions,
            &profile_lanes_by_id,
            &lanes_by_id,
            &mut compiled_rules,
        )?;
    }

    Ok(KernelPlan {
        generation,
        profile_id: profile.id.clone(),
        lanes: lane_plans,
        rules: compiled_rules,
    })
}

fn compile_lane(lane: &Lane, topology: &TopologySnapshot, slot: u32) -> LanePlan {
    let interface = lane.interface.as_str();
    let topology_entry = topology.interface(interface);

    LanePlan {
        id: lane.id.clone(),
        interface: interface.to_owned(),
        route_v4: topology_entry.and_then(|entry| entry.route_v4),
        route_v6: topology_entry.and_then(|entry| entry.route_v6),
        availability: if topology_entry
            .is_some_and(|entry| entry.route_v4.is_some() || entry.route_v6.is_some())
        {
            LaneAvailability::Available
        } else {
            LaneAvailability::Unavailable
        },
        table: TABLE_BASE + slot,
        mark: MARK_BASE + slot,
        rule_priority: RULE_PRIORITY_BASE + slot,
        failure_policy: lane.failure_policy.clone(),
    }
}

fn compile_rule(
    rule: &Rule,
    resolutions: &ResolutionSnapshot,
    profile_lanes: &BTreeMap<LaneId, &Lane>,
    lane_plans: &BTreeMap<LaneId, &LanePlan>,
    compiled_rules: &mut Vec<CompiledRule>,
) -> Result<(), PlanError> {
    let targets = match &rule.source {
        RuleSource::Application(_) => vec![CompiledTarget::Cgroup(format!(
            "routelane.apps/{}",
            rule.id.as_str()
        ))],
        RuleSource::Any => match &rule.target {
            RuleTarget::Any => vec![CompiledTarget::Any],
            RuleTarget::Ip(address) => vec![CompiledTarget::Network(host_network(*address))],
            RuleTarget::Cidr(network) => vec![CompiledTarget::Network(*network)],
            RuleTarget::Domain(_) => {
                let mut addresses = resolutions
                    .addresses(&rule.id)
                    .ok_or_else(|| PlanError::DomainResolutionUnavailable(rule.id.clone()))?
                    .to_vec();
                addresses.sort();
                addresses.dedup();
                addresses.into_iter().map(CompiledTarget::Network).collect()
            }
            RuleTarget::WildcardDomain(_) => {
                return Err(PlanError::UnsupportedCapability {
                    rule_id: rule.id.clone(),
                    capability: Capability::WildcardDomain,
                });
            }
            RuleTarget::ServicePack(_) => {
                return Err(PlanError::UnsupportedCapability {
                    rule_id: rule.id.clone(),
                    capability: Capability::ServicePack,
                });
            }
        },
    };
    let action = match compile_action(&rule.action, profile_lanes, lane_plans) {
        Some(action) => action,
        None => return Ok(()),
    };

    compiled_rules.extend(targets.into_iter().map(|target| CompiledRule {
        id: rule.id.clone(),
        priority: rule.priority,
        target,
        action: action.clone(),
    }));
    Ok(())
}

fn compile_action(
    action: &RuleAction,
    profile_lanes: &BTreeMap<LaneId, &Lane>,
    lane_plans: &BTreeMap<LaneId, &LanePlan>,
) -> Option<CompiledAction> {
    match action {
        RuleAction::Route(lane_id) => compile_route_action(lane_id, profile_lanes, lane_plans),
        RuleAction::Direct => Some(CompiledAction::Direct),
        RuleAction::Block => Some(CompiledAction::Block),
    }
}

fn compile_route_action(
    original_lane_id: &LaneId,
    profile_lanes: &BTreeMap<LaneId, &Lane>,
    lane_plans: &BTreeMap<LaneId, &LanePlan>,
) -> Option<CompiledAction> {
    let original_lane = profile_lanes
        .get(original_lane_id)
        .expect("profile validation guarantees route lanes exist");
    let original_plan = lane_plans
        .get(original_lane_id)
        .expect("every profile lane has a lane plan");

    if original_plan.availability == LaneAvailability::Available {
        return Some(CompiledAction::Mark(original_plan.mark));
    }

    let mut fallback_id = original_lane.fallback_lane.as_ref();
    while let Some(lane_id) = fallback_id {
        let fallback_plan = lane_plans
            .get(lane_id)
            .expect("profile validation guarantees fallback lanes exist");
        if fallback_plan.availability == LaneAvailability::Available {
            return Some(CompiledAction::Mark(fallback_plan.mark));
        }
        fallback_id = profile_lanes
            .get(lane_id)
            .expect("profile validation guarantees fallback lanes exist")
            .fallback_lane
            .as_ref();
    }

    match original_lane.failure_policy {
        LaneFailurePolicy::Block => Some(CompiledAction::Block),
        LaneFailurePolicy::Direct => Some(CompiledAction::Direct),
        LaneFailurePolicy::Inactive => None,
    }
}

fn host_network(address: IpAddr) -> IpNet {
    IpNet::from(address)
}
