use crate::models::{DaemonToUi, UiRequest};
use routelane::config::{ConfigV3, PersistenceRequest};
use routelane::domain::{HealthState, Profile, Rule, RuleSource};
use routelane::ipc::{
    DaemonEvent, DaemonRequest, DaemonResponse, DaemonResult, DaemonStatus, IpcError,
    IPC_PROTOCOL_VERSION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Apply,
    SetRuleEnabled,
    Deactivate,
    Launch,
    Adopt,
    Recover,
    SaveOnly,
    ChangeLanguage,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStage {
    Saving,
    AwaitingReply,
    AwaitingSnapshot,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveState {
    Clean,
    Saving,
    SavedInactive,
    Applying,
    Applied,
    SavedUnapplied,
    Failed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiGateError {
    Busy,
    StateUnknown,
    ReadOnlyConfig,
    UnappliedProfile,
    DisabledRule,
}
#[derive(Debug)]
pub enum AfterSave {
    None,
    Request(DaemonRequest),
    QuitApplication,
}
#[derive(Debug)]
pub enum UiEffect {
    Persist(PersistenceRequest),
    Send(UiRequest),
    RefreshSnapshot,
    QuitApplication,
}

#[derive(Debug)]
struct Pending {
    kind: OperationKind,
    stage: OperationStage,
    request_id: Option<String>,
    save_generation: Option<u64>,
    candidate: Option<Profile>,
    follow_up: AfterSave,
    expected_generation: Option<u64>,
}

#[derive(Debug)]
pub struct UiSession {
    epoch: u64,
    known: bool,
    status: Option<DaemonStatus>,
    accepted_revision: Option<(u64, u64)>,
    saved_profile: Profile,
    applied_revision: Option<(u64, Profile)>,
    writable: bool,
    persistence_generation: u64,
    pending: Option<Pending>,
    error: Option<IpcError>,
    config_error: Option<IpcError>,
    save_state: SaveState,
    profiles: Vec<Profile>,
    result: Option<DaemonResult>,
}

impl UiSession {
    pub fn new(profile: Profile, config_writable: bool) -> Self {
        Self {
            epoch: 0,
            known: false,
            status: None,
            accepted_revision: None,
            saved_profile: profile,
            applied_revision: None,
            writable: config_writable,
            persistence_generation: 0,
            pending: None,
            error: None,
            config_error: None,
            save_state: SaveState::Clean,
            profiles: vec![],
            result: None,
        }
    }

    pub fn snapshot_known(&self) -> bool {
        self.known
    }
    pub fn confirmed_status(&self) -> Option<&DaemonStatus> {
        self.status.as_ref()
    }
    pub fn config_writable(&self) -> bool {
        self.writable
    }
    pub fn last_error(&self) -> Option<&IpcError> {
        self.error.as_ref().or(self.config_error.as_ref())
    }
    pub fn save_state(&self) -> SaveState {
        self.save_state
    }
    pub fn daemon_profiles(&self) -> &[Profile] {
        &self.profiles
    }
    pub fn last_result(&self) -> Option<&DaemonResult> {
        self.result.as_ref()
    }
    pub fn accepts_reply(&self, epoch: u64, id: &str) -> bool {
        epoch == self.epoch
            && self.pending.as_ref().is_some_and(|p| {
                p.stage == OperationStage::AwaitingReply && p.request_id.as_deref() == Some(id)
            })
    }
    pub fn pending_candidate(&self) -> Option<&Profile> {
        self.pending.as_ref().and_then(|p| p.candidate.as_ref())
    }
    pub fn connection_epoch(&self) -> u64 {
        self.epoch
    }
    pub fn configuration_failed(&mut self, error: String) {
        self.writable = false;
        self.save_state = SaveState::Failed;
        self.config_error = Some(IpcError::new("UI_CONFIG_FAILED", error));
    }
    pub fn operation(&self) -> Option<(OperationKind, OperationStage)> {
        self.pending.as_ref().map(|p| (p.kind, p.stage))
    }
    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
            || (self.known
                && self
                    .status
                    .as_ref()
                    .is_some_and(|s| s.health.state == HealthState::Applying))
    }
    pub fn needs_reapply(&self, profile: &Profile) -> bool {
        !self.known
            || !self
                .applied_revision
                .as_ref()
                .is_some_and(|(generation, applied)| {
                    self.status.as_ref().is_some_and(|status| {
                        status.generation == *generation
                            && status.applied_profile_id.as_ref() == Some(&profile.id)
                    }) && same_profile(applied, profile)
                        && same_profile(&self.saved_profile, profile)
                })
    }
    pub fn application_actions_enabled(&self, profile: &Profile, rule: &Rule) -> bool {
        !self.is_busy()
            && !self.needs_reapply(profile)
            && rule.enabled
            && matches!(rule.source, RuleSource::Application(_))
            && profile.rules.iter().any(|candidate| candidate == rule)
    }

    pub fn begin_save(
        &mut self,
        config: ConfigV3,
        profile: Profile,
        follow_up: AfterSave,
    ) -> Result<Vec<UiEffect>, UiGateError> {
        if self.is_busy() {
            return Err(UiGateError::Busy);
        }
        if !self.writable {
            return Err(UiGateError::ReadOnlyConfig);
        }
        if matches!(follow_up, AfterSave::Request(_)) && !self.known {
            return Err(UiGateError::StateUnknown);
        }
        self.persistence_generation = self
            .persistence_generation
            .checked_add(1)
            .expect("UI persistence generation exhausted");
        self.pending = Some(Pending {
            kind: if matches!(follow_up, AfterSave::QuitApplication) {
                OperationKind::ChangeLanguage
            } else {
                OperationKind::SaveOnly
            },
            stage: OperationStage::Saving,
            request_id: None,
            save_generation: Some(self.persistence_generation),
            candidate: Some(profile),
            follow_up,
            expected_generation: None,
        });
        self.save_state = SaveState::Saving;
        self.error = None;
        Ok(vec![UiEffect::Persist(PersistenceRequest::new(
            self.persistence_generation,
            config,
        ))])
    }

    pub fn begin_request(
        &mut self,
        request: DaemonRequest,
        submitted_profile: Option<Profile>,
    ) -> Result<Vec<UiEffect>, UiGateError> {
        if matches!(
            request,
            DaemonRequest::GetStatus | DaemonRequest::ListProfiles | DaemonRequest::Subscribe
        ) {
            return Ok(vec![UiEffect::Send(UiRequest::new(request))]);
        }
        if self.is_busy() {
            return Err(UiGateError::Busy);
        }
        let recovery = matches!(request, DaemonRequest::RecoverOrphanedResources)
            && self
                .error
                .as_ref()
                .is_some_and(|error| error.code == "DAEMON_ORPHANED_RESOURCES");
        if !self.known && !recovery {
            return Err(UiGateError::StateUnknown);
        }
        let kind = match &request {
            DaemonRequest::ApplyProfile(_) => OperationKind::Apply,
            DaemonRequest::SetRuleEnabled { .. } => OperationKind::SetRuleEnabled,
            DaemonRequest::Deactivate => {
                if self
                    .status
                    .as_ref()
                    .is_none_or(|status| status.applied_profile_id.is_none())
                {
                    return Err(UiGateError::UnappliedProfile);
                }
                OperationKind::Deactivate
            }
            DaemonRequest::LaunchApplication {
                profile_id,
                rule_id,
            }
            | DaemonRequest::AdoptApplicationProcesses {
                profile_id,
                rule_id,
            } => {
                let profile = submitted_profile.as_ref().unwrap_or(&self.saved_profile);
                let rule = profile
                    .rules
                    .iter()
                    .find(|rule| &rule.id == rule_id)
                    .ok_or(UiGateError::DisabledRule)?;
                if &profile.id != profile_id || !self.application_actions_enabled(profile, rule) {
                    return Err(UiGateError::UnappliedProfile);
                }
                if matches!(request, DaemonRequest::LaunchApplication { .. }) {
                    OperationKind::Launch
                } else {
                    OperationKind::Adopt
                }
            }
            DaemonRequest::RecoverOrphanedResources => {
                if !recovery {
                    return Err(UiGateError::UnappliedProfile);
                }
                OperationKind::Recover
            }
            _ => unreachable!(),
        };
        if matches!(kind, OperationKind::Apply | OperationKind::SetRuleEnabled) && !self.writable {
            return Err(UiGateError::ReadOnlyConfig);
        }
        let candidate = match &request {
            DaemonRequest::ApplyProfile(profile) => Some(profile.clone()),
            _ => submitted_profile,
        };
        let tracked = UiRequest::new(request);
        self.pending = Some(Pending {
            kind,
            stage: OperationStage::AwaitingReply,
            request_id: Some(tracked.request_id.clone()),
            save_generation: None,
            candidate,
            follow_up: AfterSave::None,
            expected_generation: None,
        });
        self.error = None;
        self.result = None;
        if matches!(kind, OperationKind::Apply | OperationKind::SetRuleEnabled) {
            self.save_state = SaveState::Applying;
        }
        Ok(vec![UiEffect::Send(tracked)])
    }

    pub fn handle_persistence(
        &mut self,
        generation: u64,
        result: Result<(), String>,
    ) -> Vec<UiEffect> {
        if !self.pending.as_ref().is_some_and(|p| {
            p.stage == OperationStage::Saving && p.save_generation == Some(generation)
        }) {
            return vec![];
        }
        let pending = self.pending.take().expect("matched persistence");
        if let Err(error) = result {
            self.configuration_failed(error);
            return vec![];
        }
        if let Some(profile) = pending.candidate {
            self.saved_profile = profile;
        }
        self.save_state = if self
            .status
            .as_ref()
            .is_some_and(|status| status.applied_profile_id.is_some())
        {
            SaveState::SavedUnapplied
        } else {
            SaveState::SavedInactive
        };
        match pending.follow_up {
            AfterSave::None => vec![],
            AfterSave::QuitApplication => vec![UiEffect::QuitApplication],
            AfterSave::Request(request) => {
                match self.begin_request(request, Some(self.saved_profile.clone())) {
                    Ok(effects) => effects,
                    Err(_) => {
                        self.save_state = SaveState::SavedUnapplied;
                        vec![UiEffect::RefreshSnapshot]
                    }
                }
            }
        }
    }

    pub fn handle_daemon(&mut self, message: DaemonToUi) -> Vec<UiEffect> {
        match message {
            DaemonToUi::ConnectionEstablished { epoch } => {
                if epoch <= self.epoch {
                    return vec![];
                }
                self.epoch = epoch;
                self.known = false;
                self.accepted_revision = None;
                self.applied_revision = None;
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.stage != OperationStage::Saving)
                {
                    self.pending = None;
                    self.save_state = SaveState::SavedUnapplied;
                }
            }
            DaemonToUi::Snapshot {
                epoch,
                status,
                profiles,
            } if epoch == self.epoch => {
                if self.pending.as_ref().is_some_and(|pending| {
                    pending.stage == OperationStage::AwaitingSnapshot
                        && pending
                            .expected_generation
                            .is_some_and(|expected| status.generation < expected)
                }) {
                    return vec![];
                }
                if !self.accept_status(status) {
                    return vec![];
                }
                self.profiles = profiles;
                self.known = true;
                let current = self.status.as_ref().expect("accepted status");
                if self
                    .applied_revision
                    .as_ref()
                    .is_some_and(|(generation, profile)| {
                        *generation != current.generation
                            || current.applied_profile_id.as_ref() != Some(&profile.id)
                    })
                {
                    self.applied_revision = None;
                }
                if self.pending.as_ref().is_some_and(|p| {
                    p.stage == OperationStage::AwaitingSnapshot
                        && p.expected_generation == Some(current.generation)
                        && current.applied_profile_id.as_ref()
                            == p.candidate.as_ref().map(|p| &p.id)
                }) {
                    self.pending = None;
                    self.save_state = SaveState::Applied;
                } else if self
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.stage == OperationStage::AwaitingSnapshot)
                {
                    self.pending = None;
                    self.applied_revision = None;
                    self.save_state = SaveState::SavedUnapplied;
                }
            }
            DaemonToUi::Response { epoch, response } if epoch == self.epoch => {
                return self.handle_response(response)
            }
            DaemonToUi::Event { epoch, event } if epoch == self.epoch => {
                let (generation, sequence, health) = match event {
                    DaemonEvent::StateChanged {
                        generation,
                        sequence,
                        health,
                    } => (generation, sequence, health),
                    DaemonEvent::OperationFailed {
                        generation,
                        sequence,
                        health,
                        ..
                    } => (generation, sequence, health),
                };
                if let Some(mut status) = self.status.clone() {
                    status.generation = generation;
                    status.event_sequence = sequence;
                    status.health = health;
                    self.accept_status(status);
                }
                return vec![UiEffect::RefreshSnapshot];
            }
            DaemonToUi::DaemonUnavailable {
                epoch,
                request_id,
                error,
            } if epoch == self.epoch => {
                if request_id.as_ref().is_some_and(|id| {
                    self.pending
                        .as_ref()
                        .is_some_and(|p| p.request_id.as_ref() != Some(id))
                }) {
                    return vec![];
                }
                self.known = false;
                self.applied_revision = None;
                self.error = Some(error);
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.stage != OperationStage::Saving)
                {
                    self.pending = None;
                    self.save_state = SaveState::SavedUnapplied;
                }
                return vec![UiEffect::RefreshSnapshot];
            }
            _ => {}
        }
        vec![]
    }

    fn accept_status(&mut self, status: DaemonStatus) -> bool {
        if self
            .accepted_revision
            .is_some_and(|(generation, sequence)| {
                status.generation < generation || status.event_sequence < sequence
            })
        {
            return false;
        }
        self.accepted_revision = Some((status.generation, status.event_sequence));
        self.status = Some(status);
        true
    }

    fn handle_response(&mut self, response: DaemonResponse) -> Vec<UiEffect> {
        let matches_pending = self.pending.as_ref().is_some_and(|p| {
            p.stage == OperationStage::AwaitingReply
                && p.request_id.as_ref() == Some(&response.request_id)
        });
        if !matches_pending {
            if let Some(DaemonResult::Status(status)) = response.result {
                self.accept_status(status);
            }
            return vec![];
        }
        let mut pending = self.pending.take().expect("matched request");
        if response.protocol_version != IPC_PROTOCOL_VERSION {
            self.error = Some(IpcError::new(
                "IPC_INVALID_RESPONSE",
                "unsupported response version",
            ));
            self.known = false;
            return vec![UiEffect::RefreshSnapshot];
        }
        if let Some(error) = response.error {
            self.error = Some(error);
            self.applied_revision = None;
            self.save_state = SaveState::SavedUnapplied;
            self.known = false;
            return vec![UiEffect::RefreshSnapshot];
        }
        match response.result {
            Some(DaemonResult::Applied(result))
                if matches!(
                    pending.kind,
                    OperationKind::Apply | OperationKind::SetRuleEnabled
                ) =>
            {
                if let Some(profile) = &pending.candidate {
                    self.applied_revision = Some((result.generation, profile.clone()));
                }
                pending.stage = OperationStage::AwaitingSnapshot;
                pending.expected_generation = Some(result.generation);
                self.pending = Some(pending);
                self.save_state = SaveState::Applying;
                vec![UiEffect::RefreshSnapshot]
            }
            Some(DaemonResult::Deactivated) if pending.kind == OperationKind::Deactivate => {
                if let Some(status) = &mut self.status {
                    status.applied_profile_id = None;
                    status.health = routelane::health::HealthReport::inactive();
                    status.last_error = None;
                }
                self.applied_revision = None;
                self.save_state = SaveState::SavedInactive;
                self.result = Some(DaemonResult::Deactivated);
                vec![UiEffect::RefreshSnapshot]
            }
            Some(result @ DaemonResult::ApplicationLaunched(_))
                if pending.kind == OperationKind::Launch =>
            {
                self.result = Some(result);
                vec![]
            }
            Some(result @ DaemonResult::ApplicationProcessesAdopted(_))
                if pending.kind == OperationKind::Adopt =>
            {
                self.result = Some(result);
                vec![]
            }
            Some(result @ DaemonResult::OrphanedResourcesRecovered(_))
                if pending.kind == OperationKind::Recover =>
            {
                self.result = Some(result);
                self.known = false;
                self.applied_revision = None;
                vec![UiEffect::RefreshSnapshot]
            }
            _ => {
                self.error = Some(IpcError::new(
                    "IPC_INVALID_RESPONSE",
                    "request returned an unexpected response",
                ));
                self.known = false;
                self.applied_revision = None;
                vec![UiEffect::RefreshSnapshot]
            }
        }
    }
}

fn same_profile(left: &Profile, right: &Profile) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.enabled = false;
    right.enabled = false;
    left == right
}

#[cfg(test)]
mod tests {
    use super::*;
    use routelane::backend::KernelPlan;
    use routelane::domain::{
        ApplicationSelector, Lane, LaneFailurePolicy, RuleAction, RuleSource, RuleTarget,
    };
    use routelane::health::HealthReport;
    use routelane::ipc::ApplyProfileResult;

    fn profile() -> Profile {
        Profile {
            routing: Default::default(),
            id: "work".into(),
            name: "Work".into(),
            enabled: false,
            lanes: vec![Lane {
                id: "vpn".into(),
                name: "VPN".into(),
                interface: "tun0".into(),
                fallback_lane: None,
                failure_policy: LaneFailurePolicy::Block,
            }],
            rules: vec![Rule {
                id: "browser".into(),
                enabled: true,
                priority: 10,
                source: RuleSource::Application(ApplicationSelector {
                    desktop_id: None,
                    executable_path: "/usr/bin/browser".into(),
                    include_children: true,
                    launch: None,
                }),
                target: RuleTarget::Any,
                action: RuleAction::Route("vpn".into()),
            }],
        }
    }

    fn status(generation: u64, active: bool) -> DaemonStatus {
        DaemonStatus {
            generation,
            event_sequence: generation,
            health: if active {
                HealthReport::from_parts(HealthState::Healthy, Default::default(), vec![])
            } else {
                HealthReport::inactive()
            },
            desired_profile_id: Some("work".into()),
            applied_profile_id: active.then(|| "work".into()),
            last_error: None,
        }
    }

    fn snapshot(session: &mut UiSession, generation: u64, active: bool) {
        session.handle_daemon(DaemonToUi::Snapshot {
            epoch: 1,
            status: status(generation, active),
            profiles: vec![profile()],
        });
    }

    fn session(writable: bool, active: bool) -> UiSession {
        let mut session = UiSession::new(profile(), writable);
        session.handle_daemon(DaemonToUi::ConnectionEstablished { epoch: 1 });
        snapshot(&mut session, 7, active);
        session
    }

    fn sent(effects: Vec<UiEffect>) -> UiRequest {
        match effects.into_iter().next().unwrap() {
            UiEffect::Send(request) => request,
            other => panic!("expected send, got {other:?}"),
        }
    }

    fn applied_response(id: &str, generation: u64) -> DaemonResponse {
        let hash = KernelPlan {
            generation,
            profile_id: "work".into(),
            lanes: vec![],
            rules: vec![],
        }
        .content_hash();
        DaemonResponse::success(
            id,
            DaemonResult::Applied(ApplyProfileResult {
                generation,
                plan_hash: hash,
            }),
        )
    }

    fn applied_session() -> UiSession {
        let mut session = session(true, false);
        let request = sent(
            session
                .begin_request(DaemonRequest::ApplyProfile(profile()), Some(profile()))
                .unwrap(),
        );
        session.handle_daemon(DaemonToUi::Response {
            epoch: 1,
            response: applied_response(&request.request_id, 8),
        });
        snapshot(&mut session, 8, true);
        session
    }

    #[test]
    fn initial_state_does_not_claim_confirmed_routing() {
        let mut s = UiSession::new(profile(), false);
        assert!(!s.snapshot_known());
        assert!(s.confirmed_status().is_none());
        assert!(s.needs_reapply(&profile()));
        assert_eq!(
            s.begin_request(DaemonRequest::Deactivate, None)
                .unwrap_err(),
            UiGateError::StateUnknown
        );
    }

    #[test]
    fn failed_health_without_applied_plan_is_not_active() {
        let mut s = session(true, false);
        let mut failed = status(7, false);
        failed.health.state = HealthState::Failed;
        s.handle_daemon(DaemonToUi::Snapshot {
            epoch: 1,
            status: failed,
            profiles: vec![],
        });
        assert!(s.confirmed_status().unwrap().applied_profile_id.is_none());
        assert_eq!(
            s.begin_request(DaemonRequest::Deactivate, None)
                .unwrap_err(),
            UiGateError::UnappliedProfile
        );
    }

    #[test]
    fn readonly_config_preserves_deactivate() {
        let mut s = session(false, true);
        assert!(s
            .begin_request(DaemonRequest::ApplyProfile(profile()), Some(profile()))
            .is_err());
        assert!(matches!(
            sent(s.begin_request(DaemonRequest::Deactivate, None).unwrap()).request,
            DaemonRequest::Deactivate
        ));
    }

    #[test]
    fn configuration_error_survives_cleanup_and_health_refresh() {
        let mut s = session(true, true);
        s.configuration_failed("permission denied".into());
        let request = sent(s.begin_request(DaemonRequest::Deactivate, None).unwrap());
        assert_eq!(s.last_error().unwrap().code, "UI_CONFIG_FAILED");
        s.handle_daemon(DaemonToUi::Response {
            epoch: 1,
            response: DaemonResponse::success(request.request_id, DaemonResult::Deactivated),
        });
        snapshot(&mut s, 7, false);
        assert!(!s.config_writable());
        assert_eq!(s.last_error().unwrap().code, "UI_CONFIG_FAILED");
    }

    #[test]
    fn language_quits_only_after_its_own_successful_persistence() {
        let mut s = session(true, false);
        let effects = s
            .begin_save(ConfigV3::default(), profile(), AfterSave::QuitApplication)
            .unwrap();
        let generation = match &effects[0] {
            UiEffect::Persist(r) => r.generation,
            _ => unreachable!(),
        };
        assert!(s.handle_persistence(generation + 1, Ok(())).is_empty());
        assert!(s.is_busy());
        assert!(matches!(
            s.handle_persistence(generation, Ok(())).as_slice(),
            [UiEffect::QuitApplication]
        ));
        let mut s = session(true, false);
        s.begin_save(ConfigV3::default(), profile(), AfterSave::QuitApplication)
            .unwrap();
        assert!(s.handle_persistence(1, Err("failed".into())).is_empty());
    }

    #[test]
    fn status_does_not_complete_apply() {
        let mut s = session(true, false);
        s.begin_request(DaemonRequest::ApplyProfile(profile()), Some(profile()))
            .unwrap();
        snapshot(&mut s, 8, true);
        assert!(s.is_busy());
        assert_eq!(
            s.begin_request(DaemonRequest::Deactivate, None)
                .unwrap_err(),
            UiGateError::Busy
        );
    }

    #[test]
    fn foreign_request_or_epoch_cannot_finish_operation() {
        let mut s = session(true, false);
        let request = sent(
            s.begin_request(DaemonRequest::ApplyProfile(profile()), Some(profile()))
                .unwrap(),
        );
        s.handle_daemon(DaemonToUi::Response {
            epoch: 1,
            response: applied_response("other", 8),
        });
        s.handle_daemon(DaemonToUi::Response {
            epoch: 0,
            response: applied_response(&request.request_id, 8),
        });
        assert!(s.is_busy());
        s.handle_daemon(DaemonToUi::Response {
            epoch: 1,
            response: applied_response(&request.request_id, 8),
        });
        assert!(s.is_busy(), "must await the matching confirmed generation");
        snapshot(&mut s, 8, true);
        assert!(!s.is_busy());
        assert!(s.application_actions_enabled(&profile(), &profile().rules[0]));
    }

    #[test]
    fn same_profile_id_after_reconnect_does_not_prove_revision() {
        let mut s = applied_session();
        s.handle_daemon(DaemonToUi::ConnectionEstablished { epoch: 2 });
        s.handle_daemon(DaemonToUi::Snapshot {
            epoch: 2,
            status: status(8, true),
            profiles: vec![profile()],
        });
        assert!(s.needs_reapply(&profile()));
        assert!(!s.application_actions_enabled(&profile(), &profile().rules[0]));
        assert!(s.begin_request(DaemonRequest::Deactivate, None).is_ok());
    }

    #[test]
    fn persistence_result_can_start_only_its_own_followup() {
        let mut s = session(true, false);
        let config = ConfigV3 {
            profiles: vec![profile()],
            ..Default::default()
        };
        let effects = s
            .begin_save(
                config,
                profile(),
                AfterSave::Request(DaemonRequest::ApplyProfile(profile())),
            )
            .unwrap();
        let generation = match &effects[0] {
            UiEffect::Persist(r) => r.generation,
            _ => panic!("expected persistence"),
        };
        assert!(s.handle_persistence(generation + 1, Ok(())).is_empty());
        assert!(s.is_busy());
        assert!(matches!(
            sent(s.handle_persistence(generation, Ok(()))).request,
            DaemonRequest::ApplyProfile(_)
        ));
        assert!(s.is_busy());
        assert!(s.handle_persistence(generation, Ok(())).is_empty());
    }

    #[test]
    fn save_failure_does_not_apply_or_disable_safe_cleanup() {
        let mut s = session(true, true);
        let effects = s
            .begin_save(
                ConfigV3::default(),
                profile(),
                AfterSave::Request(DaemonRequest::ApplyProfile(profile())),
            )
            .unwrap();
        let generation = match &effects[0] {
            UiEffect::Persist(r) => r.generation,
            _ => unreachable!(),
        };
        assert!(s
            .handle_persistence(generation, Err("permission denied".into()))
            .is_empty());
        assert!(!s.config_writable());
        assert!(s.begin_request(DaemonRequest::Deactivate, None).is_ok());
    }

    #[test]
    fn deactivated_generation_is_not_required_to_increase() {
        let mut s = session(true, true);
        let request = sent(s.begin_request(DaemonRequest::Deactivate, None).unwrap());
        s.handle_daemon(DaemonToUi::Response {
            epoch: 1,
            response: DaemonResponse::success(request.request_id, DaemonResult::Deactivated),
        });
        assert!(!s.is_busy());
        assert!(s.confirmed_status().unwrap().applied_profile_id.is_none());
    }

    #[test]
    fn new_external_generation_invalidates_applied_revision() {
        let mut s = applied_session();
        snapshot(&mut s, 9, true);
        assert!(s.needs_reapply(&profile()));
    }

    #[test]
    fn timeout_does_not_replay_mutation() {
        let mut s = session(true, false);
        let request = sent(
            s.begin_request(DaemonRequest::ApplyProfile(profile()), Some(profile()))
                .unwrap(),
        );
        let effects = s.handle_daemon(DaemonToUi::DaemonUnavailable {
            epoch: 1,
            request_id: Some(request.request_id),
            error: IpcError::new("IPC_TIMEOUT", "timeout"),
        });
        assert!(!s.snapshot_known());
        assert!(effects
            .iter()
            .all(|effect| !matches!(effect, UiEffect::Send(_))));
        snapshot(&mut s, 8, true);
        assert!(s.needs_reapply(&profile()));
    }

    #[test]
    fn disabled_rule_never_enables_application_actions() {
        let s = applied_session();
        let mut candidate = profile();
        candidate.rules[0].enabled = false;
        assert!(!s.application_actions_enabled(&candidate, &candidate.rules[0]));
    }

    #[test]
    fn same_profile_id_but_unapplied_edit_blocks_launch() {
        let s = applied_session();
        let mut candidate = profile();
        candidate.rules[0].priority = 20;
        assert!(!s.application_actions_enabled(&candidate, &candidate.rules[0]));
    }

    #[test]
    fn snapshot_before_acknowledged_generation_does_not_discard_revision() {
        let mut s = session(true, false);
        let request = sent(
            s.begin_request(DaemonRequest::ApplyProfile(profile()), Some(profile()))
                .unwrap(),
        );
        s.handle_daemon(DaemonToUi::Response {
            epoch: 1,
            response: applied_response(&request.request_id, 8),
        });
        snapshot(&mut s, 7, false);
        assert!(s.is_busy());
        snapshot(&mut s, 8, true);
        assert!(s.application_actions_enabled(&profile(), &profile().rules[0]));
    }

    #[test]
    fn newer_external_snapshot_finishes_verification_without_claiming_our_revision() {
        let mut s = session(true, false);
        let request = sent(
            s.begin_request(DaemonRequest::ApplyProfile(profile()), Some(profile()))
                .unwrap(),
        );
        s.handle_daemon(DaemonToUi::Response {
            epoch: 1,
            response: applied_response(&request.request_id, 8),
        });
        snapshot(&mut s, 9, true);
        assert!(
            !s.is_busy(),
            "a superseded apply must not leave the UI locked forever"
        );
        assert!(s.needs_reapply(&profile()));
        assert_eq!(s.save_state(), SaveState::SavedUnapplied);
        assert!(s.begin_request(DaemonRequest::Deactivate, None).is_ok());
    }
}
