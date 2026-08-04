use std::cmp::Ordering;
use std::net::IpAddr;
use std::path::PathBuf;

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

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

string_id!(RuleId);
string_id!(ServicePackId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationSelector {
    pub desktop_id: Option<String>,
    pub executable_path: PathBuf,
    pub include_children: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleSource {
    Any,
    Application(ApplicationSelector),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleTarget {
    Any,
    Domain(String),
    WildcardDomain(String),
    Ip(IpAddr),
    Cidr(IpNet),
    ServicePack(ServicePackId),
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

    #[test]
    fn rule_id_exposes_its_stable_value() {
        assert_eq!(RuleId::from("browser").as_str(), "browser");
    }

    #[test]
    fn serializes_application_source_and_profile_targets() {
        let selector = ApplicationSelector {
            desktop_id: Some("org.example.App".to_owned()),
            executable_path: "/usr/bin/example".into(),
            include_children: true,
        };
        let source = RuleSource::Application(selector.clone());

        assert_eq!(
            serde_json::to_value(&source).unwrap(),
            serde_json::json!({
                "Application": {
                    "desktop_id": "org.example.App",
                    "executable_path": "/usr/bin/example",
                    "include_children": true
                }
            })
        );
        assert_eq!(
            serde_json::to_value(RuleTarget::Any).unwrap(),
            serde_json::json!("Any")
        );
        assert_eq!(
            serde_json::to_value(RuleTarget::WildcardDomain("*.example.com".to_owned())).unwrap(),
            serde_json::json!({ "WildcardDomain": "*.example.com" })
        );
        assert_eq!(
            serde_json::to_value(RuleTarget::ServicePack(ServicePackId::from("streaming")))
                .unwrap(),
            serde_json::json!({ "ServicePack": "streaming" })
        );
        assert_eq!(source, RuleSource::Application(selector));
    }
}
