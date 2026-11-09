//! Read-only activation preview: which enabled explicit rules reach a
//! network the helper observed as directly connected, and which rules match
//! all traffic. It edits nothing and grants nothing; a bridge it mentions is
//! never an exit.

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use super::{policy_hash, PlanHash, ResolutionSnapshot};
use crate::domain::{Profile, RuleId, RuleSource, RuleTarget};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum RoutingWarning {
    /// An explicit rule (Route, Direct or Block) covers part of a connected
    /// network, so local or lab access there follows that rule.
    LocalNetworkRule { rule_id: RuleId, network: IpNet },
    /// A rule with source Any and target Any matches every new connection.
    AllTrafficRule { rule_id: RuleId },
}

/// Valid only for the exact profile and resolution snapshot its
/// `policy_hash` identifies; a changed draft needs a new preview. It is not
/// evidence that any network is reachable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingPreview {
    pub policy_hash: PlanHash,
    pub warnings: Vec<RoutingWarning>,
}

pub fn routing_preview(
    profile: &Profile,
    resolutions: &ResolutionSnapshot,
    observed_networks: &[IpNet],
) -> RoutingPreview {
    let mut observed = observed_networks
        .iter()
        .map(IpNet::trunc)
        .collect::<Vec<_>>();
    observed.sort();
    observed.dedup();

    let mut warnings = Vec::new();
    // Application rules are scoped to the selected process tree, not to a
    // destination, so they are neither local-network nor all-traffic rules.
    for rule in profile
        .rules
        .iter()
        .filter(|rule| rule.enabled && rule.source == RuleSource::Any)
    {
        let targets = match &rule.target {
            RuleTarget::Any => {
                warnings.push(RoutingWarning::AllTrafficRule {
                    rule_id: rule.id.clone(),
                });
                continue;
            }
            RuleTarget::Ip(address) => vec![IpNet::from(*address)],
            RuleTarget::Cidr(network) => vec![network.trunc()],
            // Unresolved names are rejected by the planner at activation.
            RuleTarget::Domain(_) => resolutions
                .addresses(&rule.id)
                .unwrap_or_default()
                .iter()
                .map(IpNet::trunc)
                .collect(),
            RuleTarget::WildcardDomain(_) | RuleTarget::ServicePack(_) => vec![],
        };
        for network in &observed {
            if targets
                .iter()
                .any(|target| target.contains(network) || network.contains(target))
            {
                warnings.push(RoutingWarning::LocalNetworkRule {
                    rule_id: rule.id.clone(),
                    network: *network,
                });
            }
        }
    }
    warnings.sort();
    warnings.dedup();
    RoutingPreview {
        policy_hash: policy_hash(profile, resolutions),
        warnings,
    }
}
