use std::cmp::Ordering;
use std::net::IpAddr;

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use super::LaneId;

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

string_id!(RuleId);
string_id!(DynamicSetId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleSource {
    Any,
    Cidr(IpNet),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleTarget {
    Ip(IpAddr),
    Cidr(IpNet),
    Domain(String),
    DynamicSet(DynamicSetId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleAction {
    Route(LaneId),
    Direct,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: RuleId,
    pub enabled: bool,
    pub priority: u32,
    pub source: RuleSource,
    pub target: RuleTarget,
    pub action: RuleAction,
}

impl Rule {
    pub fn execution_cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| self.id.cmp(&other.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, priority: u32) -> Rule {
        Rule {
            id: RuleId::from(id),
            enabled: true,
            priority,
            source: RuleSource::Any,
            target: RuleTarget::Ip("203.0.113.7".parse().unwrap()),
            action: RuleAction::Direct,
        }
    }

    #[test]
    fn execution_order_uses_priority_then_rule_id() {
        let mut rules = vec![rule("z", 10), rule("b", 5), rule("a", 5)];

        rules.sort_by(Rule::execution_cmp);

        assert_eq!(
            rules.into_iter().map(|rule| rule.id).collect::<Vec<_>>(),
            vec![RuleId::from("a"), RuleId::from("b"), RuleId::from("z")]
        );
    }
}
