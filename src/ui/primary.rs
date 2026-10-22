//! Primary connection selection: draft validation, the confirmation gate for
//! a manual primary change, and the Overview's saved-versus-effective model.
//! Pure logic; widgets live in `connections.rs` and `overview.rs`.

use routelane::backend::{FamilyExit, RuntimePhase};
use routelane::domain::{
    validate_profile, LaneId, PrimarySelection, Profile, ProfileId, RoutingPolicy,
};
use routelane::ipc::DaemonStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryDraft {
    pub profile_id: ProfileId,
    pub primary: PrimarySelection,
    pub backups: Vec<LaneId>,
    pub automatic_failover: bool,
}

impl PrimaryDraft {
    pub fn of(profile: &Profile) -> Self {
        Self {
            profile_id: profile.id.clone(),
            primary: profile.routing.primary.clone(),
            backups: profile.routing.backups.clone(),
            automatic_failover: profile.routing.automatic_failover,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimaryEditError {
    /// The editor was opened for another profile than the current one.
    StaleProfile,
    /// A referenced lane no longer exists; never switch to another target.
    UnknownLane(LaneId),
    Invalid(String),
}

pub fn validate_primary_draft(
    profile: &Profile,
    draft: &PrimaryDraft,
) -> Result<Profile, PrimaryEditError> {
    if draft.profile_id != profile.id {
        return Err(PrimaryEditError::StaleProfile);
    }
    let referenced = match &draft.primary {
        PrimarySelection::Lane(id) => Some(id),
        PrimarySelection::FollowSystemDefault => None,
    }
    .into_iter()
    .chain(&draft.backups);
    for id in referenced {
        if !profile.lanes.iter().any(|lane| &lane.id == id) {
            return Err(PrimaryEditError::UnknownLane(id.clone()));
        }
    }
    let mut candidate = profile.clone();
    candidate.routing = RoutingPolicy {
        unmatched_traffic: profile.routing.unmatched_traffic,
        primary: draft.primary.clone(),
        backups: draft.backups.clone(),
        automatic_failover: draft.automatic_failover,
    };
    validate_profile(&candidate)
        .map_err(|error| PrimaryEditError::Invalid(format!("{error:?}")))?;
    Ok(candidate)
}

/// With exactly two lanes the other one is proposed as the backup; otherwise
/// the user orders backups explicitly. Lane IDs and names are never roles.
pub fn proposed_backups(profile: &Profile, primary_lane: Option<&LaneId>) -> Vec<LaneId> {
    match (primary_lane, profile.lanes.as_slice()) {
        (Some(primary), [first, second]) if &first.id == primary => vec![second.id.clone()],
        (Some(primary), [first, second]) if &second.id == primary => vec![first.id.clone()],
        _ => vec![],
    }
}

/// Whether the routing policy references a lane (as primary or backup).
pub fn routing_references(profile: &Profile, lane_id: &LaneId) -> bool {
    profile.routing.primary == PrimarySelection::Lane(lane_id.clone())
        || profile.routing.backups.contains(lane_id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimaryEffect {
    Save(Profile),
}

/// A manual primary change has no effect until it is explicitly confirmed.
/// Cancel, Escape and closing the dialog all take the `cancel` path.
#[derive(Debug, Default)]
pub struct PrimaryChange {
    pending: Option<PrimaryDraft>,
}

impl PrimaryChange {
    pub fn request(&mut self, draft: PrimaryDraft) {
        self.pending = Some(draft);
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn cancel(&mut self) -> Vec<PrimaryEffect> {
        self.pending = None;
        vec![]
    }

    /// Revalidates against the profile as it is now and saves at most once.
    pub fn confirm(&mut self, current: &Profile) -> Result<Vec<PrimaryEffect>, PrimaryEditError> {
        let Some(draft) = self.pending.take() else {
            return Ok(vec![]);
        };
        validate_primary_draft(current, &draft)
            .map(|candidate| vec![PrimaryEffect::Save(candidate)])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryOverview {
    /// The saved preference, which may not be active yet.
    pub preferred: PrimarySelection,
    /// Verified effective exits; `None` when no runtime is armed.
    pub effective: Option<(FamilyExit, FamilyExit)>,
    /// The effective IPv4 exit is not the resolved preferred lane.
    pub backup_in_use: bool,
    pub phase: RuntimePhase,
}

pub fn primary_overview(saved: &Profile, status: Option<&DaemonStatus>) -> PrimaryOverview {
    let runtime = status
        .map(|status| &status.primary)
        .filter(|runtime| runtime.grant.is_some() || runtime.decision.is_some());
    let decision = runtime.and_then(|runtime| runtime.decision.as_ref());
    PrimaryOverview {
        preferred: saved.routing.primary.clone(),
        effective: decision
            .map(|decision| (decision.effective.v4.clone(), decision.effective.v6.clone())),
        backup_in_use: decision.is_some_and(|decision| match &decision.effective.v4 {
            FamilyExit::Lane(id) => decision.preferred.v4.as_ref() != Some(id),
            FamilyExit::Blocked => false,
        }),
        phase: runtime.map_or(RuntimePhase::Inactive, |runtime| runtime.phase),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::connections::{delete_candidate, LaneEditError};
    use routelane::backend::{FamilyPair, PrimaryDecision, RuntimeStatus};
    use routelane::domain::{InterfaceSelector, Lane, LaneFailurePolicy};

    fn profile() -> Profile {
        Profile {
            id: ProfileId::from("office"),
            name: "Office".into(),
            enabled: true,
            rules: vec![],
            routing: RoutingPolicy::default(),
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

    fn ethernet_primary(profile: &Profile) -> PrimaryDraft {
        PrimaryDraft {
            primary: PrimarySelection::Lane("ethernet".into()),
            backups: proposed_backups(profile, Some(&"ethernet".into())),
            ..PrimaryDraft::of(profile)
        }
    }

    #[test]
    fn primary_cancel_escape_close_emit_no_effect() {
        let profile = profile();
        let mut change = PrimaryChange::default();
        change.request(ethernet_primary(&profile));
        assert!(change.is_pending());
        let effects = change.cancel();
        assert!(effects.is_empty());
        assert!(!change.is_pending());
        assert_eq!(
            change.confirm(&profile).unwrap(),
            vec![],
            "nothing left to confirm"
        );
    }

    #[test]
    fn primary_double_confirmation_saves_once() {
        let profile = profile();
        let mut change = PrimaryChange::default();
        change.request(ethernet_primary(&profile));
        let mut persistence_request_count = 0;
        for _ in 0..2 {
            for effect in change.confirm(&profile).unwrap() {
                let PrimaryEffect::Save(saved) = effect;
                assert_eq!(
                    saved.routing.primary,
                    PrimarySelection::Lane("ethernet".into())
                );
                assert_eq!(saved.routing.backups, vec![LaneId::from("vinn")]);
                persistence_request_count += 1;
            }
        }
        assert_eq!(persistence_request_count, 1);
    }

    #[test]
    fn primary_stale_draft_is_revalidated() {
        let opened = profile();
        let mut change = PrimaryChange::default();
        change.request(ethernet_primary(&opened));
        // The lane was deleted (and another one renamed) while the dialog was open.
        let mut current = opened.clone();
        current.lanes.retain(|lane| lane.id.as_str() != "ethernet");
        assert_eq!(
            change.confirm(&current),
            Err(PrimaryEditError::UnknownLane("ethernet".into()))
        );
        let mut other = opened.clone();
        other.id = ProfileId::from("home");
        let mut change = PrimaryChange::default();
        change.request(ethernet_primary(&opened));
        assert_eq!(change.confirm(&other), Err(PrimaryEditError::StaleProfile));
    }

    #[test]
    fn primary_saved_primary_is_not_effective_after_apply_failure() {
        let mut saved = profile();
        saved.routing.primary = PrimarySelection::Lane("ethernet".into());
        saved.routing.backups = vec!["vinn".into()];
        // The daemon still verifies the previous runtime: VINN is the exit.
        let status = DaemonStatus {
            generation: 3,
            event_sequence: 9,
            health: routelane::health::HealthReport::inactive(),
            desired_profile_id: Some(saved.id.clone()),
            applied_profile_id: Some(saved.id.clone()),
            last_error: None,
            primary: RuntimeStatus {
                generation: 3,
                decision: Some(PrimaryDecision {
                    preferred: FamilyPair {
                        v4: Some("vinn".into()),
                        v6: None,
                    },
                    effective: FamilyPair {
                        v4: FamilyExit::Lane("vinn".into()),
                        v6: FamilyExit::Blocked,
                    },
                }),
                phase: RuntimePhase::Armed,
                ..RuntimeStatus::default()
            },
        };
        let overview = primary_overview(&saved, Some(&status));
        assert_eq!(
            overview.preferred,
            PrimarySelection::Lane("ethernet".into())
        );
        assert_eq!(
            overview.effective,
            Some((FamilyExit::Lane("vinn".into()), FamilyExit::Blocked))
        );
        assert!(
            !overview.backup_in_use,
            "VINN is the verified preferred exit"
        );
        assert_eq!(primary_overview(&saved, None).effective, None);
    }

    #[test]
    fn primary_lane_delete_revalidates_primary_and_backup_references() {
        let mut profile = profile();
        profile.routing.primary = PrimarySelection::Lane("ethernet".into());
        profile.routing.backups = vec!["vinn".into()];
        assert!(routing_references(&profile, &"ethernet".into()));
        assert!(routing_references(&profile, &"vinn".into()));
        assert_eq!(
            delete_candidate(&profile, &"vinn".into()),
            Err(LaneEditError::RoutingInUse)
        );
        assert_eq!(
            delete_candidate(&profile, &"ethernet".into()),
            Err(LaneEditError::RoutingInUse)
        );
        // After an explicit revised policy the lane can go.
        let revised = validate_primary_draft(
            &profile,
            &PrimaryDraft {
                backups: vec![],
                ..PrimaryDraft::of(&profile)
            },
        )
        .unwrap();
        assert!(delete_candidate(&revised, &"vinn".into()).is_ok());
    }

    #[test]
    fn primary_proposal_uses_the_other_lane_not_names_or_ids() {
        let mut profile = profile();
        profile.lanes[0].id = "default".into();
        assert_eq!(
            proposed_backups(&profile, Some(&"default".into())),
            vec![LaneId::from("vinn")]
        );
        assert_eq!(
            proposed_backups(&profile, Some(&"vinn".into())),
            vec![LaneId::from("default")]
        );
        assert_eq!(proposed_backups(&profile, None), Vec::<LaneId>::new());
    }
}
