use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use routelane::backend::{
    ApplyResponse, CleanupResponse, InspectResponse, KernelPlan, KernelSnapshot,
    ResolutionSnapshot, SnapshotInput, TopologySnapshot,
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
    cleanup_calls: Arc<Mutex<usize>>,
    inspect_calls: Arc<Mutex<usize>>,
}

#[derive(Clone, Default)]
enum HelperBehavior {
    #[default]
    Success,
    Fail,
    Mismatch,
    InspectFail,
    AuthorizationDenied,
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
}

#[async_trait]
impl PrivilegedHelperClient for FakeHelper {
    async fn apply(&self, plan: &KernelPlan) -> Result<ApplyResponse, DaemonError> {
        self.applied.lock().unwrap().push(plan.clone());
        match self.behavior.lock().unwrap().clone() {
            HelperBehavior::Success => Ok(ApplyResponse::from_plan(plan, vec![])),
            HelperBehavior::Fail => Err(DaemonError::helper("injected apply failure")),
            HelperBehavior::Mismatch => {
                let mut mismatched = plan.clone();
                mismatched.generation += 1;
                Ok(ApplyResponse::from_plan(&mismatched, vec![]))
            }
            HelperBehavior::InspectFail => Ok(ApplyResponse::from_plan(plan, vec![])),
            HelperBehavior::AuthorizationDenied => {
                Err(DaemonError::authorization("injected authorization denial"))
            }
        }
    }

    async fn inspect(&self) -> Result<InspectResponse, DaemonError> {
        *self.inspect_calls.lock().unwrap() += 1;
        if matches!(*self.behavior.lock().unwrap(), HelperBehavior::InspectFail) {
            return Err(DaemonError::helper("injected inspect failure"));
        }
        let plan = self.applied.lock().unwrap().last().cloned();
        let nft = if plan.is_some() {
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
        Ok(InspectResponse {
            generation: plan.as_ref().map(|plan| plan.generation),
            plan_hash: plan.as_ref().map(KernelPlan::content_hash),
            routelane_table_present: plan.is_some(),
            snapshot,
            diagnostics: vec![],
        })
    }

    async fn cleanup(&self) -> Result<CleanupResponse, DaemonError> {
        *self.cleanup_calls.lock().unwrap() += 1;
        match self.behavior.lock().unwrap().clone() {
            HelperBehavior::Fail => Err(DaemonError::helper("injected cleanup failure")),
            HelperBehavior::AuthorizationDenied => {
                Err(DaemonError::authorization("injected authorization denial"))
            }
            _ => Ok(CleanupResponse {
                cleaned: true,
                diagnostics: vec![],
            }),
        }
    }
}

#[derive(Clone, Default)]
struct MemoryConfig {
    desired: Arc<Mutex<Option<Profile>>>,
    saves: Arc<Mutex<Vec<Option<Profile>>>>,
}

impl ConfigRepository for MemoryConfig {
    fn load(&self) -> Result<Option<Profile>, DaemonError> {
        Ok(self.desired.lock().unwrap().clone())
    }

    fn save(&self, desired: Option<&Profile>) -> Result<(), DaemonError> {
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
    assert_eq!(
        state.health.diagnostics[0].code,
        DiagnosticCode::PolicyApplyRolledBack
    );
    assert_eq!(*config.desired.lock().unwrap(), Some(changed));
    assert!(matches!(
        event,
        DaemonEvent::OperationFailed {
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
    assert!(config.saves.lock().unwrap().is_empty());
    assert!(matches!(
        events.recv().await.unwrap(),
        DaemonEvent::OperationFailed {
            generation: 0,
            health,
            ..
        } if health.state == HealthState::Failed
    ));
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
async fn inspect_failure_keeps_previous_applied_generation_and_reports_rollback() {
    let (service, helper, _) = service();
    service.apply_profile(fixture_profile()).await.unwrap();
    let mut changed = fixture_profile();
    changed.name = "Changed".to_owned();
    helper.fail_inspect();

    let error = service.apply_profile(changed).await.unwrap_err();

    assert_eq!(error.code(), "DAEMON_HEALTH_INSPECTION_FAILED");
    let state = service.state().await;
    assert_eq!(state.generation, 1);
    assert_eq!(state.applied.unwrap().generation, 1);
    assert_eq!(state.health.state, HealthState::Degraded);
    assert_eq!(
        state.health.diagnostics[0].code,
        DiagnosticCode::PolicyApplyRolledBack
    );
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
