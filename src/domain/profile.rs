use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{Rule, RuleAction, RuleId};

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

string_id!(ProfileId);
string_id!(LaneId);
string_id!(InterfaceSelector);

impl LaneId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl InterfaceSelector {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaneFailurePolicy {
    Block,
    Direct,
    Inactive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    pub id: LaneId,
    #[serde(default)]
    pub name: String,
    pub interface: InterfaceSelector,
    pub fallback_lane: Option<LaneId>,
    pub failure_policy: LaneFailurePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,
    pub enabled: bool,
    pub lanes: Vec<Lane>,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    TooManyLanes { actual: usize, maximum: usize },
    TooManyRules { actual: usize, maximum: usize },
    DuplicateLaneId(LaneId),
    DuplicateRuleId(RuleId),
    UnknownLane(LaneId),
    SelfReferentialFallback(LaneId),
    FallbackCycle(LaneId),
}

pub fn validate_profile(profile: &Profile) -> Result<(), DomainError> {
    if profile.lanes.len() > 100 {
        return Err(DomainError::TooManyLanes {
            actual: profile.lanes.len(),
            maximum: 100,
        });
    }

    if profile.rules.len() > 10_000 {
        return Err(DomainError::TooManyRules {
            actual: profile.rules.len(),
            maximum: 10_000,
        });
    }

    let mut lane_ids = BTreeSet::new();
    for lane in &profile.lanes {
        if !lane_ids.insert(lane.id.clone()) {
            return Err(DomainError::DuplicateLaneId(lane.id.clone()));
        }
    }

    let mut rule_ids = BTreeSet::new();
    for rule in &profile.rules {
        if !rule_ids.insert(rule.id.clone()) {
            return Err(DomainError::DuplicateRuleId(rule.id.clone()));
        }
    }

    for rule in &profile.rules {
        if let RuleAction::Route(lane_id) = &rule.action {
            if !lane_ids.contains(lane_id) {
                return Err(DomainError::UnknownLane(lane_id.clone()));
            }
        }
    }

    for lane in &profile.lanes {
        if let Some(fallback_lane) = &lane.fallback_lane {
            if !lane_ids.contains(fallback_lane) {
                return Err(DomainError::UnknownLane(fallback_lane.clone()));
            }
            if fallback_lane == &lane.id {
                return Err(DomainError::SelfReferentialFallback(lane.id.clone()));
            }
        }
    }

    for lane in &profile.lanes {
        let mut visited = BTreeSet::new();
        let mut current = &lane.id;
        while let Some(next) = profile
            .lanes
            .iter()
            .find(|candidate| candidate.id == *current)
            .and_then(|candidate| candidate.fallback_lane.as_ref())
        {
            if !visited.insert(current.clone()) || visited.contains(next) {
                return Err(DomainError::FallbackCycle(lane.id.clone()));
            }
            current = next;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        validate_profile, DomainError, InterfaceSelector, Lane, LaneFailurePolicy, LaneId, Profile,
        ProfileId,
    };
    use crate::domain::{Rule, RuleAction, RuleId, RuleSource, RuleTarget};

    fn fixture_lane(id: &str, interface: &str) -> Lane {
        Lane {
            id: LaneId::from(id),
            name: id.to_owned(),
            interface: InterfaceSelector::from(interface),
            fallback_lane: None,
            failure_policy: LaneFailurePolicy::Inactive,
        }
    }

    fn fixture_rule(id: &str, priority: u32, target: RuleTarget, action: RuleAction) -> Rule {
        Rule {
            id: RuleId::from(id),
            enabled: true,
            priority,
            source: RuleSource::Any,
            target,
            action,
        }
    }

    fn fixture_profile(lanes: Vec<Lane>, rules: Vec<Rule>) -> Profile {
        Profile {
            id: ProfileId::from("default"),
            name: "Default".to_owned(),
            enabled: true,
            lanes,
            rules,
        }
    }

    #[test]
    fn profile_exposes_enabled_state_and_transparent_interface_selector() {
        let profile = fixture_profile(vec![fixture_lane("work", "wg0")], vec![]);

        assert!(profile.enabled);
        assert_eq!(profile.lanes[0].interface.as_str(), "wg0");
        assert_eq!(
            serde_json::to_string(&profile.lanes[0].interface).unwrap(),
            "\"wg0\""
        );
    }

    #[test]
    fn rejects_route_rule_that_references_missing_lane() {
        let profile = fixture_profile(
            vec![fixture_lane("work", "wg0")],
            vec![fixture_rule(
                "rule-a",
                10,
                RuleTarget::Cidr("203.0.113.0/24".parse().unwrap()),
                RuleAction::Route(LaneId::from("missing")),
            )],
        );

        assert_eq!(
            validate_profile(&profile),
            Err(DomainError::UnknownLane(LaneId::from("missing")))
        );
    }

    #[test]
    fn rejects_more_than_one_hundred_lanes() {
        let lanes = (0..101)
            .map(|n| fixture_lane(&format!("lane-{n}"), &format!("tun{n}")))
            .collect();
        let profile = fixture_profile(lanes, vec![]);

        assert_eq!(
            validate_profile(&profile),
            Err(DomainError::TooManyLanes {
                actual: 101,
                maximum: 100
            })
        );
    }

    #[test]
    fn rejects_duplicate_rule_ids() {
        let lane = fixture_lane("work", "wg0");
        let rule = fixture_rule(
            "duplicate",
            10,
            RuleTarget::Ip("203.0.113.7".parse().unwrap()),
            RuleAction::Direct,
        );
        let profile = fixture_profile(vec![lane], vec![rule.clone(), rule]);

        assert_eq!(
            validate_profile(&profile),
            Err(DomainError::DuplicateRuleId(RuleId::from("duplicate")))
        );
    }
}
