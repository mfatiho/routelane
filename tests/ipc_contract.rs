use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use routelane::backend::{
    ApplyResponse, CleanupResponse, InspectResponse, KernelPlan, KernelSnapshot,
    OrphanRecoveryResponse, ResolutionSnapshot, RestoreResponse, SnapshotInput, TopologySnapshot,
};
use routelane::daemon::{
    AtomicJsonConfigRepository, ConfigRepository, DaemonError, DaemonService,
    PrivilegedHelperClient, ResolutionProvider, TopologyProvider,
};
use routelane::domain::{
    HealthState, Profile, ProfileId, Rule, RuleAction, RuleId, RuleSource, RuleTarget,
};
use routelane::health::DiagnosticCode;
use routelane::ipc::{
    read_frame, write_frame, DaemonClient, DaemonEnvelope, DaemonEvent, DaemonRequest,
    DaemonResult, IpcError, UnixSocketServer, IPC_PROTOCOL_VERSION, MAX_FRAME_BYTES,
};
use tempfile::TempDir;
use tokio::io::{duplex, AsyncWriteExt};
use tokio::sync::Notify;

#[test]
fn application_requests_serialize_only_profile_and_rule_identifiers() {
    let request = DaemonRequest::LaunchApplication {
        profile_id: ProfileId::from("default"),
        rule_id: RuleId::from("browser"),
    };

    let json = serde_json::to_value(request).unwrap();
    assert_eq!(json["method"], "LaunchApplication");
    assert_eq!(json["parameters"]["profile_id"], "default");
    assert_eq!(json["parameters"]["rule_id"], "browser");
}

#[derive(Clone, Default)]
struct FakeTopology;

#[async_trait]
impl TopologyProvider for FakeTopology {
    async fn snapshot(&self, _profile: &Profile) -> Result<TopologySnapshot, DaemonError> {
        Ok(TopologySnapshot::empty())
    }
}

#[derive(Clone, Default)]
struct FakeResolution;

#[async_trait]
impl ResolutionProvider for FakeResolution {
    async fn snapshot(&self, _profile: &Profile) -> Result<ResolutionSnapshot, DaemonError> {
        Ok(ResolutionSnapshot::empty())
    }
}

#[derive(Clone, Default)]
struct FakeHelper {
    behavior: Arc<Mutex<HelperBehavior>>,
    applied: Arc<Mutex<Vec<KernelPlan>>>,
    live: Arc<Mutex<Option<KernelPlan>>>,
    restore_fails: Arc<Mutex<bool>>,
    cleanup_calls: Arc<Mutex<usize>>,
    inspect_calls: Arc<Mutex<usize>>,
    next_inspect_gate: Arc<Mutex<Option<InspectGate>>>,
    orphaned: Arc<Mutex<bool>>,
    recovery_calls: Arc<Mutex<usize>>,
}

#[derive(Clone)]
struct InspectGate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[derive(Clone, Default)]
enum HelperBehavior {
    #[default]
    Success,
    Fail,
    Mismatch,
    InspectFail,
    AuthorizationDenied,
    Drift,
}

impl FakeHelper {
    fn fail(&self) {
        *self.behavior.lock().unwrap() = HelperBehavior::Fail;
    }

    fn mismatch(&self) {
        *self.behavior.lock().unwrap() = HelperBehavior::Mismatch;
    }

    fn succeed(&self) {
        *self.behavior.lock().unwrap() = HelperBehavior::Success;
    }

    fn fail_inspect(&self) {
        *self.behavior.lock().unwrap() = HelperBehavior::InspectFail;
    }

    fn deny_authorization(&self) {
        *self.behavior.lock().unwrap() = HelperBehavior::AuthorizationDenied;
    }

    fn drift(&self) {
        *self.behavior.lock().unwrap() = HelperBehavior::Drift;
    }

    fn live_plan(&self) -> Option<KernelPlan> {
        self.live.lock().unwrap().clone()
    }

    fn fail_restore(&self) {
        *self.restore_fails.lock().unwrap() = true;
    }

    fn seed_live_plan(&self, plan: KernelPlan) {
        *self.live.lock().unwrap() = Some(plan);
    }

    fn pause_next_inspect(&self) -> (Arc<Notify>, Arc<Notify>) {
        let gate = InspectGate {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        };
        *self.next_inspect_gate.lock().unwrap() = Some(gate.clone());
        (gate.entered, gate.release)
    }

    fn seed_orphaned_resources(&self) {
        *self.orphaned.lock().unwrap() = true;
    }

    fn recovery_calls(&self) -> usize {
        *self.recovery_calls.lock().unwrap()
    }
}

#[async_trait]
impl PrivilegedHelperClient for FakeHelper {
    async fn apply(&self, plan: &KernelPlan) -> Result<ApplyResponse, DaemonError> {
        self.applied.lock().unwrap().push(plan.clone());
        let behavior = self.behavior.lock().unwrap().clone();
        match behavior {
            HelperBehavior::Fail => Err(DaemonError::helper("injected apply failure")),
            HelperBehavior::AuthorizationDenied => {
                Err(DaemonError::authorization("injected authorization denial"))
            }
            behavior => {
                if let Some(current) = self.live.lock().unwrap().as_ref() {
                    if plan.generation < current.generation
                        || (plan.generation == current.generation
                            && plan.content_hash() != current.content_hash())
                    {
                        return Err(DaemonError::helper("injected helper generation conflict"));
                    }
                }
                *self.live.lock().unwrap() = Some(plan.clone());
                if matches!(behavior, HelperBehavior::Mismatch) {
                    let mut mismatched = plan.clone();
                    mismatched.generation += 1;
                    Ok(ApplyResponse::from_plan(&mismatched, vec![]))
                } else {
                    Ok(ApplyResponse::from_plan(plan, vec![]))
                }
            }
        }
    }

    async fn restore(
        &self,
        failed_candidate: &KernelPlan,
        previous: Option<&KernelPlan>,
    ) -> Result<RestoreResponse, DaemonError> {
        if *self.restore_fails.lock().unwrap() {
            return Err(DaemonError::helper("injected restore failure"));
        }
        let mut live = self.live.lock().unwrap();
        let Some(current) = live.as_ref() else {
            return Err(DaemonError::helper("no live candidate to restore"));
        };
        if current.generation != failed_candidate.generation
            || current.content_hash() != failed_candidate.content_hash()
        {
            return Err(DaemonError::helper(
                "live plan does not match failed candidate",
            ));
        }
        *live = previous.cloned();
        Ok(RestoreResponse::from_plan(previous))
    }

    async fn inspect(&self) -> Result<InspectResponse, DaemonError> {
        *self.inspect_calls.lock().unwrap() += 1;
        match *self.behavior.lock().unwrap() {
            HelperBehavior::InspectFail => {
                return Err(DaemonError::helper("injected inspect failure"));
            }
            HelperBehavior::AuthorizationDenied => {
                return Err(DaemonError::authorization("injected authorization denial"));
            }
            _ => {}
        }
        let plan = self.live.lock().unwrap().clone();
        let drift = matches!(*self.behavior.lock().unwrap(), HelperBehavior::Drift);
        let orphaned = *self.orphaned.lock().unwrap();
        let nft = if (plan.is_some() && !drift) || orphaned {
            br#"{"nftables":[{"table":{"family":"inet","name":"routelane"}}]}"#.as_slice()
        } else {
            br#"{"nftables":[]}"#.as_slice()
        };
        let snapshot = KernelSnapshot::parse(SnapshotInput {
            ipv4_routes: b"[]",
            ipv6_routes: b"[]",
            ipv4_rules: b"[]",
            ipv6_rules: b"[]",
            nft,
        })
        .unwrap();
        let snapshot = match &plan {
            Some(plan) => snapshot.with_nft_ownership(plan.generation, plan.content_hash()),
            None => snapshot,
        };
        let response = InspectResponse {
            generation: plan.as_ref().map(|plan| plan.generation),
            plan_hash: plan.as_ref().map(KernelPlan::content_hash),
            applied_plan: plan.clone(),
            routelane_table_present: plan.is_some() || orphaned,
            snapshot,
            diagnostics: vec![],
        };
        let gate = self.next_inspect_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        Ok(response)
    }

    async fn cleanup(&self) -> Result<CleanupResponse, DaemonError> {
        *self.cleanup_calls.lock().unwrap() += 1;
        match self.behavior.lock().unwrap().clone() {
            HelperBehavior::Fail => Err(DaemonError::helper("injected cleanup failure")),
            HelperBehavior::AuthorizationDenied => {
                Err(DaemonError::authorization("injected authorization denial"))
            }
            _ => {
                let cleaned = self.live.lock().unwrap().take().is_some();
                Ok(CleanupResponse {
                    cleaned,
                    diagnostics: vec![],
                })
            }
        }
    }

    async fn recover_orphaned_resources(&self) -> Result<OrphanRecoveryResponse, DaemonError> {
        *self.recovery_calls.lock().unwrap() += 1;
        match self.behavior.lock().unwrap().clone() {
            HelperBehavior::Fail => {
                return Err(DaemonError::helper("injected orphan recovery failure"));
            }
            HelperBehavior::AuthorizationDenied => {
                return Err(DaemonError::authorization(
                    "injected orphan recovery authorization denial",
                ));
            }
            _ => {}
        }
        let cleaned = std::mem::take(&mut *self.orphaned.lock().unwrap());
        Ok(OrphanRecoveryResponse {
            cleaned,
            diagnostics: vec![],
        })
    }
}

#[derive(Clone, Default)]
struct MemoryConfig {
    desired: Arc<Mutex<Option<Profile>>>,
    saves: Arc<Mutex<Vec<Option<Profile>>>>,
    fail_saves: Arc<Mutex<bool>>,
}

impl ConfigRepository for MemoryConfig {
    fn load(&self) -> Result<Option<Profile>, DaemonError> {
        Ok(self.desired.lock().unwrap().clone())
    }

    fn save(&self, desired: Option<&Profile>) -> Result<(), DaemonError> {
        if *self.fail_saves.lock().unwrap() {
            return Err(DaemonError::new(
                "DAEMON_CONFIG_FAILED",
                "injected config failure",
            ));
        }
        let desired = desired.cloned();
        *self.desired.lock().unwrap() = desired.clone();
        self.saves.lock().unwrap().push(desired);
        Ok(())
    }
}

type TestService = DaemonService<FakeTopology, FakeResolution, FakeHelper, MemoryConfig>;

fn fixture_profile() -> Profile {
    Profile {
        id: ProfileId::from("default"),
        name: "Default".to_owned(),
        enabled: true,
        lanes: vec![],
        rules: vec![Rule {
            id: RuleId::from("deny-example"),
            enabled: true,
            priority: 10,
            source: RuleSource::Any,
            target: RuleTarget::Any,
            action: RuleAction::Block,
        }],
    }
}

fn service() -> (Arc<TestService>, FakeHelper, MemoryConfig) {
    let helper = FakeHelper::default();
    let config = MemoryConfig::default();
    let service = Arc::new(
        DaemonService::new(FakeTopology, FakeResolution, helper.clone(), config.clone()).unwrap(),
    );
    (service, helper, config)
}

fn restarted_service(helper: FakeHelper, config: MemoryConfig) -> Arc<TestService> {
    Arc::new(DaemonService::new(FakeTopology, FakeResolution, helper, config).unwrap())
}

fn applied_fixture_plan(generation: u64) -> KernelPlan {
    KernelPlan {
        generation,
        profile_id: ProfileId::from("default"),
        lanes: vec![],
        rules: vec![],
    }
}

#[tokio::test]
async fn rejects_unsupported_protocol_version_without_mutating_state() {
    let (service, _, config) = service();
    let response = service
        .handle(DaemonEnvelope {
            protocol_version: 999,
            request_id: "request-1".into(),
            request: DaemonRequest::GetStatus,
        })
        .await;

    assert_eq!(
        response.error.unwrap().code,
        "IPC_UNSUPPORTED_PROTOCOL_VERSION"
    );
    assert_eq!(service.state().await.generation, 0);
    assert!(config.saves.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rejects_empty_request_id_without_mutating_state() {
    let (service, _, config) = service();
    let response = service
        .handle(DaemonEnvelope {
            protocol_version: IPC_PROTOCOL_VERSION,
            request_id: String::new(),
            request: DaemonRequest::ApplyProfile(fixture_profile()),
        })
        .await;

    assert_eq!(response.error.unwrap().code, "IPC_INVALID_REQUEST_ID");
    assert_eq!(service.state().await.generation, 0);
    assert!(config.saves.lock().unwrap().is_empty());
}

#[tokio::test]
async fn apply_profile_returns_generation_and_publishes_event() {
    let (service, _, _) = service();
    let mut events = service.subscribe();

    let response = service.apply_profile(fixture_profile()).await.unwrap();
    let event = events.recv().await.unwrap();

    assert_eq!(response.generation, 1);
    assert!(matches!(
        event,
        DaemonEvent::StateChanged {
            sequence: 1,
            generation: 1,
            health
        } if health.state == HealthState::Healthy
    ));
    let state = service.state().await;
    assert_eq!(state.generation, 1);
    assert_eq!(state.applied.unwrap().generation, 1);
    assert_eq!(state.health.state, HealthState::Healthy);
}

#[tokio::test]
async fn failed_apply_keeps_persisted_desired_and_last_successful_generation() {
    let (service, helper, config) = service();
    service.apply_profile(fixture_profile()).await.unwrap();
    let mut changed = fixture_profile();
    changed.name = "Changed".to_owned();
    helper.fail();
    let mut events = service.subscribe();

    let error = service.apply_profile(changed.clone()).await.unwrap_err();
    let event = events.recv().await.unwrap();

    assert_eq!(error.code(), "DAEMON_HELPER_FAILED");
    let state = service.state().await;
    assert_eq!(state.desired, Some(changed.clone()));
    assert_eq!(state.generation, 1);
    assert_eq!(state.applied.unwrap().generation, 1);
    assert_eq!(state.applied_profile.unwrap().name, "Default");
    assert_eq!(state.health.state, HealthState::Degraded);
    assert!(state.health.diagnostics.is_empty());
    assert_eq!(*config.desired.lock().unwrap(), Some(changed));
    assert!(matches!(
        event,
        DaemonEvent::OperationFailed {
            sequence: 2,
            generation: 1,
            health,
            error
        } if health.state == HealthState::Degraded
            && error == IpcError::new("DAEMON_HELPER_FAILED", "injected apply failure")
    ));
}

#[tokio::test]
async fn mismatched_helper_response_is_not_committed_as_applied() {
    let (service, helper, _) = service();
    helper.mismatch();

    let error = service.apply_profile(fixture_profile()).await.unwrap_err();

    assert_eq!(error.code(), "DAEMON_HELPER_RESPONSE_MISMATCH");
    let state = service.state().await;
    assert_eq!(state.generation, 0);
    assert!(state.applied.is_none());
    assert_eq!(state.health.state, HealthState::Failed);
    assert_eq!(
        state.health.diagnostics[0].code,
        DiagnosticCode::PolicyApplyRolledBack
    );
    assert!(helper.live_plan().is_none());
}

#[tokio::test]
async fn invalid_profile_emits_failure_without_persisting_or_advancing_generation() {
    let (service, _, config) = service();
    let mut invalid = fixture_profile();
    invalid.rules.push(invalid.rules[0].clone());
    let mut events = service.subscribe();

    let error = service.apply_profile(invalid).await.unwrap_err();

    assert_eq!(error.code(), "DAEMON_PROFILE_INVALID");
    let state = service.state().await;
    assert_eq!(state.generation, 0);
    assert!(state.desired.is_none());
    assert!(state.applied.is_none());
    assert_eq!(state.health.state, HealthState::Failed);
    assert!(state.health.diagnostics.is_empty());
    assert!(config.saves.lock().unwrap().is_empty());
    assert!(matches!(
        events.recv().await.unwrap(),
        DaemonEvent::OperationFailed {
            sequence: 1,
            generation: 0,
            health,
            ..
        } if health.state == HealthState::Failed
    ));
}

#[tokio::test]
async fn config_failure_without_kernel_mutation_has_no_rollback_diagnostic() {
    let (service, helper, config) = service();
    *config.fail_saves.lock().unwrap() = true;

    let error = service.apply_profile(fixture_profile()).await.unwrap_err();

    assert_eq!(error.code(), "DAEMON_CONFIG_FAILED");
    assert!(helper.live_plan().is_none());
    let state = service.state().await;
    assert_eq!(state.health.state, HealthState::Failed);
    assert!(state.health.diagnostics.is_empty());
}

#[tokio::test]
async fn set_rule_enabled_preserves_block_rule_and_runs_normal_apply_path() {
    let (service, helper, config) = service();
    service.apply_profile(fixture_profile()).await.unwrap();

    let response = service
        .set_rule_enabled(
            ProfileId::from("default"),
            RuleId::from("deny-example"),
            false,
        )
        .await
        .unwrap();

    assert_eq!(response.generation, 2);
    let desired = config.desired.lock().unwrap().clone().unwrap();
    assert_eq!(desired.rules.len(), 1);
    assert!(!desired.rules[0].enabled);
    assert_eq!(desired.rules[0].action, RuleAction::Block);
    assert_eq!(helper.applied.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn deactivate_only_clears_applied_state_after_cleanup_success() {
    let (service, helper, _) = service();
    service.apply_profile(fixture_profile()).await.unwrap();
    helper.fail();

    service.deactivate().await.unwrap_err();
    assert!(service.state().await.applied.is_some());
    assert_eq!(service.state().await.health.state, HealthState::Degraded);

    helper.succeed();
    service.deactivate().await.unwrap();
    let state = service.state().await;
    assert!(state.applied.is_none());
    assert_eq!(state.generation, 1);
    assert_eq!(state.health.state, HealthState::Inactive);
}

#[tokio::test]
async fn status_and_list_profiles_are_read_only() {
    let (service, _, config) = service();
    service.apply_profile(fixture_profile()).await.unwrap();
    let saves_before = config.saves.lock().unwrap().len();

    let status = service
        .handle(DaemonEnvelope::new("status", DaemonRequest::GetStatus))
        .await;
    let profiles = service
        .handle(DaemonEnvelope::new("profiles", DaemonRequest::ListProfiles))
        .await;

    assert!(matches!(status.result, Some(DaemonResult::Status(_))));
    assert!(matches!(
        profiles.result,
        Some(DaemonResult::Profiles(ref values)) if values.len() == 1
    ));
    assert_eq!(service.state().await.generation, 1);
    assert_eq!(config.saves.lock().unwrap().len(), saves_before);
}

#[tokio::test]
async fn restart_without_helper_state_reports_inactive() {
    let (service, helper, _) = service();

    let response = service
        .handle(DaemonEnvelope::new(
            "fresh-status",
            DaemonRequest::GetStatus,
        ))
        .await;
    let Some(DaemonResult::Status(status)) = response.result else {
        panic!("expected fresh status");
    };

    assert_eq!(status.generation, 0);
    assert_eq!(status.health.state, HealthState::Inactive);
    assert!(status.applied_profile_id.is_none());
    assert_eq!(*helper.inspect_calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn restart_reconciles_applied_generation_before_status_and_next_apply() {
    let helper = FakeHelper::default();
    helper.seed_live_plan(applied_fixture_plan(7));
    let config = MemoryConfig::default();
    let service = restarted_service(helper.clone(), config);

    let status = service
        .handle(DaemonEnvelope::new(
            "restart-status",
            DaemonRequest::GetStatus,
        ))
        .await;
    let Some(DaemonResult::Status(status)) = status.result else {
        panic!("expected status after restart");
    };
    assert_eq!(status.generation, 7);
    assert_eq!(status.applied_profile_id, Some(ProfileId::from("default")));
    assert_eq!(status.health.state, HealthState::Healthy);

    let repeated = service
        .handle(DaemonEnvelope::new(
            "restart-status-again",
            DaemonRequest::GetStatus,
        ))
        .await;
    assert!(matches!(
        repeated.result,
        Some(DaemonResult::Status(ref status))
            if status.generation == 7
                && status.applied_profile_id == Some(ProfileId::from("default"))
                && status.health.state == HealthState::Healthy
    ));

    let mut changed = fixture_profile();
    changed.name = "After restart".to_owned();
    let applied = service.apply_profile(changed).await.unwrap();
    assert_eq!(applied.generation, 8);
    assert_eq!(helper.live_plan().unwrap().generation, 8);
}

#[tokio::test]
async fn restart_inspect_failures_are_explicit_and_block_stale_mutation() {
    for (authorize, expected_state, expected_code) in [
        (false, HealthState::Failed, "DAEMON_RECONCILIATION_FAILED"),
        (
            true,
            HealthState::NeedsAuthorization,
            "DAEMON_NEEDS_AUTHORIZATION",
        ),
    ] {
        let helper = FakeHelper::default();
        helper.seed_live_plan(applied_fixture_plan(7));
        if authorize {
            helper.deny_authorization();
        } else {
            helper.fail_inspect();
        }
        let service = restarted_service(helper.clone(), MemoryConfig::default());

        let status = service
            .handle(DaemonEnvelope::new(
                "restart-error",
                DaemonRequest::GetStatus,
            ))
            .await;
        let Some(DaemonResult::Status(status)) = status.result else {
            panic!("reconciliation failure must still produce explicit status");
        };
        assert_eq!(status.health.state, expected_state);

        let error = service.apply_profile(fixture_profile()).await.unwrap_err();
        assert_eq!(error.code(), expected_code);
        assert_eq!(helper.live_plan().unwrap().generation, 7);
    }
}

#[tokio::test]
async fn restart_reconciliation_retries_after_helper_authorization_recovers() {
    let helper = FakeHelper::default();
    helper.seed_live_plan(applied_fixture_plan(7));
    helper.deny_authorization();
    let service = restarted_service(helper.clone(), MemoryConfig::default());

    let denied = service
        .handle(DaemonEnvelope::new("denied", DaemonRequest::GetStatus))
        .await;
    assert!(matches!(
        denied.result,
        Some(DaemonResult::Status(ref status))
            if status.health.state == HealthState::NeedsAuthorization
    ));

    helper.succeed();
    let recovered = service
        .handle(DaemonEnvelope::new("recovered", DaemonRequest::GetStatus))
        .await;
    assert!(matches!(
        recovered.result,
        Some(DaemonResult::Status(ref status))
            if status.generation == 7 && status.health.state == HealthState::Healthy
    ));
    assert_eq!(*helper.inspect_calls.lock().unwrap(), 2);
}

#[tokio::test]
async fn restart_live_drift_is_failed_without_claiming_inactive_or_mutating() {
    let helper = FakeHelper::default();
    helper.seed_live_plan(applied_fixture_plan(7));
    helper.drift();
    let service = restarted_service(helper.clone(), MemoryConfig::default());

    let status = service
        .handle(DaemonEnvelope::new("drift", DaemonRequest::GetStatus))
        .await;
    let Some(DaemonResult::Status(status)) = status.result else {
        panic!("drift must produce explicit status");
    };
    assert_eq!(status.generation, 7);
    assert_eq!(status.applied_profile_id, Some(ProfileId::from("default")));
    assert_eq!(status.health.state, HealthState::Failed);
    assert_eq!(
        status.health.diagnostics[0].code,
        DiagnosticCode::RoutingResourceConflict
    );

    let error = service.apply_profile(fixture_profile()).await.unwrap_err();
    assert_eq!(error.code(), "DAEMON_RECONCILIATION_FAILED");
    assert_eq!(helper.live_plan().unwrap().generation, 7);
}

#[tokio::test]
async fn orphan_recovery_resets_failed_reconciliation_and_allows_apply() {
    let helper = FakeHelper::default();
    helper.seed_orphaned_resources();
    let service = restarted_service(helper.clone(), MemoryConfig::default());

    let first = service.apply_profile(fixture_profile()).await.unwrap_err();
    assert_eq!(first.code(), "DAEMON_ORPHANED_RESOURCES");

    let recovered = service.recover_orphaned_resources().await.unwrap();
    assert!(recovered.cleaned);
    assert_eq!(helper.recovery_calls(), 1);

    service.apply_profile(fixture_profile()).await.unwrap();
}

#[tokio::test]
async fn failed_orphan_recovery_keeps_reconciliation_blocked() {
    let helper = FakeHelper::default();
    helper.seed_orphaned_resources();
    let service = restarted_service(helper.clone(), MemoryConfig::default());
    assert_eq!(
        service
            .apply_profile(fixture_profile())
            .await
            .unwrap_err()
            .code(),
        "DAEMON_ORPHANED_RESOURCES"
    );
    helper.fail();

    assert!(service.recover_orphaned_resources().await.is_err());
    assert_eq!(helper.recovery_calls(), 1);
    assert_eq!(
        service
            .apply_profile(fixture_profile())
            .await
            .unwrap_err()
            .code(),
        "DAEMON_ORPHANED_RESOURCES"
    );
}

#[tokio::test]
async fn concurrent_first_requests_share_one_restart_reconciliation() {
    let helper = FakeHelper::default();
    helper.seed_live_plan(applied_fixture_plan(7));
    let service = restarted_service(helper.clone(), MemoryConfig::default());
    let (inspect_entered, release_inspect) = helper.pause_next_inspect();

    let left_service = service.clone();
    let left = tokio::spawn(async move {
        left_service
            .handle(DaemonEnvelope::new("left", DaemonRequest::GetStatus))
            .await
    });
    inspect_entered.notified().await;
    let right_service = service.clone();
    let right = tokio::spawn(async move {
        right_service
            .handle(DaemonEnvelope::new("right", DaemonRequest::GetStatus))
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    release_inspect.notify_one();
    let (left, right) = (left.await.unwrap(), right.await.unwrap());

    assert!(matches!(left.result, Some(DaemonResult::Status(_))));
    assert!(matches!(right.result, Some(DaemonResult::Status(_))));
    assert_eq!(*helper.inspect_calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn framing_rejects_oversize_invalid_utf8_json_and_truncated_payloads() {
    async fn read_raw(bytes: &[u8]) -> IpcError {
        let (mut sender, mut receiver) = duplex(bytes.len().max(8));
        sender.write_all(bytes).await.unwrap();
        drop(sender);
        read_frame::<_, DaemonEnvelope>(&mut receiver)
            .await
            .unwrap_err()
    }

    let oversize = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes();
    assert_eq!(read_raw(&oversize).await.code, "IPC_FRAME_TOO_LARGE");

    let mut invalid_utf8 = 1u32.to_be_bytes().to_vec();
    invalid_utf8.push(0xff);
    assert_eq!(read_raw(&invalid_utf8).await.code, "IPC_INVALID_UTF8");

    let mut invalid_json = 1u32.to_be_bytes().to_vec();
    invalid_json.push(b'{');
    assert_eq!(read_raw(&invalid_json).await.code, "IPC_INVALID_JSON");

    let mut truncated = 4u32.to_be_bytes().to_vec();
    truncated.extend_from_slice(b"{}");
    assert_eq!(read_raw(&truncated).await.code, "IPC_UNEXPECTED_EOF");
}

#[tokio::test]
async fn unix_socket_round_trip_uses_secure_permissions() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("runtime/routelane/routelane.sock");
    let (service, _, _) = service();
    let server = UnixSocketServer::bind_at(path.clone(), service.clone())
        .await
        .unwrap();
    let task = tokio::spawn(server.run());

    let client = DaemonClient::new(path.clone());
    let mut events = client.subscribe("events").await.unwrap();
    let response = client
        .send(DaemonEnvelope::new("status", DaemonRequest::GetStatus))
        .await
        .unwrap();
    service.apply_profile(fixture_profile()).await.unwrap();

    assert!(matches!(response.result, Some(DaemonResult::Status(_))));
    assert_eq!(
        events.recv().await.unwrap(),
        DaemonEvent::StateChanged {
            sequence: 1,
            generation: 1,
            health: service.state().await.health,
        }
    );
    assert_eq!(
        fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let socket_metadata = fs::symlink_metadata(&path).unwrap();
    assert!(socket_metadata.file_type().is_socket());
    assert_eq!(socket_metadata.permissions().mode() & 0o777, 0o600);
    task.abort();
}

#[tokio::test]
async fn inspect_failure_restores_helper_state_and_allows_a_different_next_generation() {
    let (service, helper, _) = service();
    service.apply_profile(fixture_profile()).await.unwrap();
    let previous = helper.live_plan().unwrap();
    let mut changed = fixture_profile();
    changed.name = "Changed".to_owned();
    helper.fail_inspect();

    let error = service.apply_profile(changed).await.unwrap_err();

    assert_eq!(error.code(), "DAEMON_HEALTH_INSPECTION_FAILED");
    assert_eq!(helper.live_plan(), Some(previous));
    let state = service.state().await;
    assert_eq!(state.generation, 1);
    assert_eq!(state.applied.unwrap().generation, 1);
    assert_eq!(state.health.state, HealthState::Degraded);
    assert_eq!(
        state.health.diagnostics[0].code,
        DiagnosticCode::PolicyApplyRolledBack
    );

    helper.succeed();
    let mut next = fixture_profile();
    next.name = "Different next generation".to_owned();
    let response = service.apply_profile(next).await.unwrap();
    assert_eq!(response.generation, 2);
    assert_eq!(helper.live_plan().unwrap().generation, 2);
}

#[tokio::test]
async fn failed_kernel_restore_is_failed_without_claiming_policy_rollback() {
    let (service, helper, _) = service();
    service.apply_profile(fixture_profile()).await.unwrap();
    helper.fail_inspect();
    helper.fail_restore();
    let mut changed = fixture_profile();
    changed.name = "Changed".to_owned();

    let error = service.apply_profile(changed).await.unwrap_err();

    assert_eq!(error.code(), "DAEMON_KERNEL_ROLLBACK_FAILED");
    assert_eq!(helper.live_plan().unwrap().generation, 2);
    let state = service.state().await;
    assert_eq!(state.generation, 1);
    assert_eq!(state.applied.unwrap().generation, 1);
    assert_eq!(state.health.state, HealthState::Failed);
    assert!(state
        .health
        .diagnostics
        .iter()
        .all(|diagnostic| diagnostic.code != DiagnosticCode::PolicyApplyRolledBack));
}

#[tokio::test]
async fn authorization_denial_reports_needs_authorization_without_replacing_applied_state() {
    let (service, helper, _) = service();
    service.apply_profile(fixture_profile()).await.unwrap();
    helper.deny_authorization();

    let error = service.apply_profile(fixture_profile()).await.unwrap_err();

    assert_eq!(error.code(), "DAEMON_NEEDS_AUTHORIZATION");
    let state = service.state().await;
    assert_eq!(state.generation, 1);
    assert_eq!(state.applied.unwrap().generation, 1);
    assert_eq!(state.health.state, HealthState::NeedsAuthorization);
}

#[tokio::test]
async fn slow_event_subscriber_does_not_block_mutations() {
    let (service, _, _) = service();
    let _slow_subscriber = service.subscribe();

    for generation in 1..=40 {
        let mut profile = fixture_profile();
        profile.name = format!("Generation {generation}");
        let response = service.apply_profile(profile).await.unwrap();
        assert_eq!(response.generation, generation);
    }
}

#[tokio::test]
async fn event_sequence_orders_a_status_snapshot_against_a_concurrent_publish() {
    let (service, helper, _) = service();
    let mut events = service.subscribe();
    service.apply_profile(fixture_profile()).await.unwrap();
    assert!(matches!(
        events.recv().await.unwrap(),
        DaemonEvent::StateChanged { sequence: 1, .. }
    ));

    let (inspect_entered, release_inspect) = helper.pause_next_inspect();
    let status_service = service.clone();
    let status_task = tokio::spawn(async move {
        status_service
            .handle(DaemonEnvelope::new(
                "concurrent-status",
                DaemonRequest::GetStatus,
            ))
            .await
    });
    inspect_entered.notified().await;

    let mut changed = fixture_profile();
    changed.name = "Concurrent change".to_owned();
    service.apply_profile(changed).await.unwrap();
    let published = events.recv().await.unwrap();
    release_inspect.notify_one();
    let status_response = status_task.await.unwrap();
    let Some(DaemonResult::Status(status)) = status_response.result else {
        panic!("expected a daemon status response");
    };

    assert_eq!(status.generation, 1);
    assert_eq!(status.event_sequence, 1);
    assert!(matches!(
        published,
        DaemonEvent::StateChanged {
            sequence: 2,
            generation: 2,
            ..
        }
    ));
}

#[tokio::test]
async fn protocol_v1_serializes_status_and_event_sequence_fields() {
    let (service, _, _) = service();
    let mut events = service.subscribe();
    service.apply_profile(fixture_profile()).await.unwrap();
    let event = events.recv().await.unwrap();
    let response = service
        .handle(DaemonEnvelope::new("status-json", DaemonRequest::GetStatus))
        .await;

    let response_json = serde_json::to_value(response).unwrap();
    assert_eq!(
        response_json["result"]["value"]["event_sequence"],
        serde_json::json!(1)
    );
    let event_json = serde_json::to_value(event).unwrap();
    assert_eq!(event_json["value"]["sequence"], serde_json::json!(1));
}

#[tokio::test]
async fn second_bind_preserves_active_socket_and_reports_already_running() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("runtime/routelane/routelane.sock");
    let (service, _, _) = service();
    let first = UnixSocketServer::bind_at(path.clone(), service.clone())
        .await
        .unwrap();
    let original_metadata = fs::symlink_metadata(&path).unwrap();

    let error = UnixSocketServer::bind_at(path.clone(), service)
        .await
        .unwrap_err();

    assert_eq!(error.code, "IPC_ALREADY_RUNNING");
    let current_metadata = fs::symlink_metadata(&path).unwrap();
    assert!(current_metadata.file_type().is_socket());
    assert_eq!(current_metadata.dev(), original_metadata.dev());
    assert_eq!(current_metadata.ino(), original_metadata.ino());
    assert_eq!(
        fs::metadata(path.parent().unwrap().join("routelane.lock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    drop(first);
}

#[tokio::test]
async fn active_owned_socket_without_lock_is_detected_by_liveness_probe() {
    let temp = TempDir::new().unwrap();
    let parent = temp.path().join("runtime/routelane");
    fs::create_dir_all(&parent).unwrap();
    let path = parent.join("routelane.sock");
    let active_listener = StdUnixListener::bind(&path).unwrap();
    let (service, _, _) = service();

    let error = UnixSocketServer::bind_at(path.clone(), service)
        .await
        .unwrap_err();

    assert_eq!(error.code, "IPC_ALREADY_RUNNING");
    assert!(fs::symlink_metadata(&path).unwrap().file_type().is_socket());
    drop(active_listener);
}

#[tokio::test]
async fn refused_owned_stale_socket_is_removed_and_rebound() {
    let temp = TempDir::new().unwrap();
    let parent = temp.path().join("runtime/routelane");
    fs::create_dir_all(&parent).unwrap();
    let path = parent.join("routelane.sock");
    let stale_listener = StdUnixListener::bind(&path).unwrap();
    drop(stale_listener);
    let (service, _, _) = service();

    let server = UnixSocketServer::bind_at(path.clone(), service)
        .await
        .unwrap();

    assert!(fs::symlink_metadata(&path).unwrap().file_type().is_socket());
    drop(server);
}

#[tokio::test]
async fn lock_file_symlink_is_rejected_without_touching_target() {
    let temp = TempDir::new().unwrap();
    let parent = temp.path().join("runtime/routelane");
    fs::create_dir_all(&parent).unwrap();
    let target = parent.join("lock-target");
    fs::write(&target, b"keep").unwrap();
    std::os::unix::fs::symlink(&target, parent.join("routelane.lock")).unwrap();
    let path = parent.join("routelane.sock");
    let (service, _, _) = service();

    let error = UnixSocketServer::bind_at(path, service).await.unwrap_err();

    assert_eq!(error.code, "IPC_UNSAFE_LOCK_FILE");
    assert_eq!(fs::read(target).unwrap(), b"keep");
}

#[tokio::test]
async fn socket_binding_never_unlinks_symlink_or_regular_file() {
    let temp = TempDir::new().unwrap();
    let parent = temp.path().join("runtime/routelane");
    fs::create_dir_all(&parent).unwrap();
    let path = parent.join("routelane.sock");
    let target = parent.join("target");
    fs::write(&target, b"keep").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let (service, _, _) = service();

    let error = UnixSocketServer::bind_at(path.clone(), service.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, "IPC_UNSAFE_SOCKET_PATH");
    assert!(fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
    fs::remove_file(&path).unwrap();
    fs::write(&path, b"foreign").unwrap();

    let error = UnixSocketServer::bind_at(path.clone(), service)
        .await
        .unwrap_err();
    assert_eq!(error.code, "IPC_UNSAFE_SOCKET_PATH");
    assert_eq!(fs::read(path).unwrap(), b"foreign");
}

#[test]
fn atomic_json_repository_round_trips_user_owned_profile_with_private_mode() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("config/routelane/desired.json");
    let repository = AtomicJsonConfigRepository::new(path.clone());

    repository.save(Some(&fixture_profile())).unwrap();

    assert_eq!(repository.load().unwrap(), Some(fixture_profile()));
    assert_private_file(&path);
    assert_eq!(
        fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

fn assert_private_file(path: &Path) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_file());
    assert!(!metadata.file_type().is_symlink());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
}

#[tokio::test]
async fn write_frame_and_read_frame_round_trip_json() {
    let (mut sender, mut receiver) = duplex(4096);
    let envelope = DaemonEnvelope::new("round-trip", DaemonRequest::GetStatus);

    write_frame(&mut sender, &envelope).await.unwrap();
    let decoded: DaemonEnvelope = read_frame(&mut receiver).await.unwrap();

    assert_eq!(decoded, envelope);
}
