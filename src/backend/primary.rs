use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{InterfaceIdentity, IpFamily, TopologySnapshot};
use crate::domain::{LaneId, PrimarySelection, Profile, RoutingPolicy};

const FAILBACK_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FamilyPair<T> {
    pub v4: T,
    pub v6: T,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FamilyExit {
    Lane(LaneId),
    #[default]
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PrimaryDecision {
    pub preferred: FamilyPair<Option<LaneId>>,
    pub effective: FamilyPair<FamilyExit>,
}

#[derive(Default)]
struct FamilyState {
    preferred: Option<LaneId>,
    effective: FamilyExit,
    ready_since: Option<Duration>,
}

/// Pure, monotonic-time selector. Bindings are frozen at authorized activation;
/// neither a new OS-default interface nor interface-name reuse expands them.
pub struct PrimarySelector {
    bindings: BTreeMap<LaneId, InterfaceIdentity>,
    states: [FamilyState; 2],
    policy: Option<RoutingPolicy>,
}

impl PrimarySelector {
    /// Only call after the privileged runtime validates the stored grant and plan.
    pub fn resume(
        profile: &Profile,
        topology: &TopologySnapshot,
        previous: &PrimaryDecision,
    ) -> Self {
        let mut selector = Self::new(profile, topology);
        selector.policy = Some(profile.routing.clone());
        selector.states = [
            FamilyState {
                preferred: previous.preferred.v4.clone(),
                effective: previous.effective.v4.clone(),
                ready_since: None,
            },
            FamilyState {
                preferred: previous.preferred.v6.clone(),
                effective: previous.effective.v6.clone(),
                ready_since: None,
            },
        ];
        selector
    }
    pub fn new(profile: &Profile, topology: &TopologySnapshot) -> Self {
        let bindings = profile
            .lanes
            .iter()
            .filter_map(|lane| {
                let identity = topology
                    .interface(lane.interface.as_str())?
                    .identity
                    .clone()?;
                Some((lane.id.clone(), identity))
            })
            .collect();
        Self {
            bindings,
            states: Default::default(),
            policy: None,
        }
    }

    pub fn update(
        &mut self,
        policy: &RoutingPolicy,
        topology: &TopologySnapshot,
        now: Duration,
    ) -> PrimaryDecision {
        if self.policy.as_ref() != Some(policy) {
            self.states = Default::default();
            self.policy = Some(policy.clone());
        }
        let bindings = &self.bindings;
        for (index, family) in [IpFamily::V4, IpFamily::V6].into_iter().enumerate() {
            let state = &mut self.states[index];
            let (preferred, ambiguous) = match &policy.primary {
                PrimarySelection::Lane(id) => (Some(id.clone()), false),
                PrimarySelection::FollowSystemDefault => match topology.system_default(family) {
                    Ok(Some(name)) => {
                        let mut matches = bindings
                            .iter()
                            .filter(|(_, identity)| identity.name == name);
                        let first = matches.next().map(|(id, _)| id.clone());
                        if matches.next().is_some() {
                            (None, true)
                        } else {
                            (first, false)
                        }
                    }
                    Ok(None) => (None, false),
                    Err(_) => (None, true),
                },
            };
            if state.preferred != preferred {
                state.ready_since = None;
            }
            state.preferred = preferred;
            let ready = |id: &LaneId| {
                topology.is_observed()
                    && bindings.get(id).is_some_and(|identity| {
                        topology.interface(&identity.name).is_some_and(|entry| {
                            entry.identity.as_ref() == Some(identity) && entry.ready(family)
                        })
                    })
            };
            if ambiguous {
                state.effective = FamilyExit::Blocked;
                state.ready_since = None;
                continue;
            }
            let old_ready = match &state.effective {
                FamilyExit::Lane(id) => {
                    ready(id) && (policy.automatic_failover || state.preferred.as_ref() == Some(id))
                }
                FamilyExit::Blocked => false,
            };
            if !old_ready {
                // No viable existing exit: never wait five seconds to restore connectivity.
                let chosen = state.preferred.as_ref().filter(|id| ready(id)).or_else(|| {
                    policy
                        .automatic_failover
                        .then(|| {
                            policy
                                .backups
                                .iter()
                                .find(|id| state.preferred.as_ref() != Some(id) && ready(id))
                        })
                        .flatten()
                });
                state.effective = chosen.cloned().map(FamilyExit::Lane).unwrap_or_default();
                state.ready_since = None;
                continue;
            }
            let returning = state
                .preferred
                .as_ref()
                .filter(|id| ready(id) && state.effective != FamilyExit::Lane((*id).clone()));
            if let Some(primary) = returning {
                let started = state.ready_since.get_or_insert(now);
                match now.checked_sub(*started) {
                    Some(elapsed) if elapsed >= FAILBACK_DELAY => {
                        state.effective = FamilyExit::Lane(primary.clone());
                        state.ready_since = None;
                    }
                    None => *started = now,
                    _ => {}
                }
            } else {
                state.ready_since = None;
            }
        }
        PrimaryDecision {
            preferred: FamilyPair {
                v4: self.states[0].preferred.clone(),
                v6: self.states[1].preferred.clone(),
            },
            effective: FamilyPair {
                v4: self.states[0].effective.clone(),
                v6: self.states[1].effective.clone(),
            },
        }
    }

    pub fn next_deadline(&self) -> Option<Duration> {
        self.states
            .iter()
            .filter_map(|state| {
                state
                    .ready_since
                    .and_then(|since| since.checked_add(FAILBACK_DELAY))
            })
            .min()
    }
}
