//! Daemon IPC v3: the helper owns runtime authority, generations and the
//! applied routing mode; the daemon only verifies, pairs and reports them.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use routelane::backend::{
    compile_profile, policy_hash, CleanupResponse, FamilyExit, InspectResponse, KernelPlan,
    KernelSnapshot, OrphanRecoveryResponse, ResolutionSnapshot, RestoreResponse, RuntimeActivation,
    RuntimeApplyResponse, RuntimeGrant, RuntimePhase, RuntimeStatus, SnapshotInput,
    TopologySnapshot, RUNTIME_PROTOCOL_VERSION,
};
use routelane::daemon::{
    ConfigRepository, DaemonError, DaemonService, PrivilegedHelperClient, ResolutionProvider,
    TopologyProvider,
};
use routelane::domain::{
    HealthState, Profile, ProfileId, Rule, RuleAction, RuleId, RuleSource, RuleTarget,
    UnmatchedTraffic,
};
use routelane::ipc::{
    DaemonEnvelope, DaemonEvent, DaemonRequest, DaemonResult, DaemonStatus, IPC_PROTOCOL_VERSION,
};

#[derive(Clone, Default)]
struct EmptyTopology;

#[async_trait]
impl TopologyProvider for EmptyTopology {
    async fn snapshot(&self, _profile: &Profile) -> Result<TopologySnapshot, DaemonError> {
        Ok(TopologySnapshot::empty())
    }
}

#[derive(Clone, Default)]
struct NoResolutions;

#[async_trait]
impl ResolutionProvider for NoResolutions {
    async fn snapshot(&self, _profile: &Profile) -> Result<ResolutionSnapshot, DaemonError> {
        Ok(ResolutionSnapshot::empty())
    }
}

#[derive(Default)]
struct HelperState {
    live: Option<KernelPlan>,
    runtime: RuntimeStatus,
    previous: Option<(Option<KernelPlan>, RuntimeStatus)>,
    activations: usize,
    restores: Vec<String>,
    wrong_policy: bool,
    /// Runtime protocol this helper reports; `None` is the current one.
    protocol: Option<u32>,
    protocol_queries: usize,
    cleanups: usize,
    fail_activation: bool,
    /// Reports a mode other than the one its plan was compiled with.
    wrong_mode: bool,
}

/// Helper2 emulation: compiles and owns the plan, issues receipts.
#[derive(Clone, Default)]
struct RuntimeHelper(Arc<Mutex<HelperState>>);

impl RuntimeHelper {
    /// An automatic failover the helper performed on its own.
    fn transition(&self, phase: RuntimePhase) -> RuntimeStatus {
        let mut state = self.0.lock().unwrap();
        let mut plan = state.live.clone().expect("armed plan");
        plan.generation += 1;
        plan.primary.effective.v4 = FamilyExit::Blocked;
        state.runtime.generation = plan.generation;
        state.runtime.decision = Some(plan.primary.clone());
        state.runtime.phase = phase;
        state.runtime.last_error =
            (phase != RuntimePhase::Armed).then(|| "kernel monitor closed".to_owned());
        state.live = Some(plan);
        state.runtime.clone()
    }
}

#[async_trait]
impl PrivilegedHelperClient for RuntimeHelper {
    async fn runtime_protocol_version(&self) -> Result<u32, DaemonError> {
        let mut state = self.0.lock().unwrap();
        state.protocol_queries += 1;
        Ok(state.protocol.unwrap_or(RUNTIME_PROTOCOL_VERSION))
    }

    async fn apply(
        &self,
        _plan: &KernelPlan,
    ) -> Result<routelane::backend::ApplyResponse, DaemonError> {
        panic!("IPC v2 never sends a daemon-compiled plan")
    }

    async fn restore(
        &self,
        _failed: &KernelPlan,
        _previous: Option<&KernelPlan>,
    ) -> Result<RestoreResponse, DaemonError> {
        panic!("IPC v2 restores only through helper-issued receipts")
    }

    async fn activate_runtime(
        &self,
        activation: &RuntimeActivation,
    ) -> Result<RuntimeApplyResponse, DaemonError> {
        let mut state = self.0.lock().unwrap();
        state.activations += 1;
        if state.fail_activation {
            return Err(DaemonError::helper("activation failed"));
        }
        let generation = state.live.as_ref().map_or(0, |plan| plan.generation);
        if activation.expected_generation != generation {
            return Err(DaemonError::helper("stale expected generation"));
        }
        let plan = compile_profile(
            &activation.profile,
            &TopologySnapshot::empty(),
            &activation.resolutions,
            generation + 1,
        )
        .unwrap();
        let mut policy = policy_hash(&activation.profile, &activation.resolutions);
        if state.wrong_policy {
            policy = policy_hash(
                &activation.profile,
                &ResolutionSnapshot::from_rule(
                    RuleId::from("other"),
                    vec!["192.0.2.0/24".parse().unwrap()],
                ),
            );
        }
        let mut mode = activation.profile.routing.unmatched_traffic;
        if state.wrong_mode {
            mode = match mode {
                UnmatchedTraffic::PreserveSystem => UnmatchedTraffic::UsePrimary,
                UnmatchedTraffic::UsePrimary => UnmatchedTraffic::PreserveSystem,
            };
        }
        state.previous = Some((state.live.clone(), state.runtime.clone()));
        state.runtime = RuntimeStatus {
            unmatched_traffic: Some(mode),
            grant: Some(RuntimeGrant {
                owner_uid: 1000,
                boot_id: "boot".into(),
                network_namespace: 1,
                policy_hash: policy,
                interfaces: Default::default(),
            }),
            generation: plan.generation,
            decision: Some(plan.primary.clone()),
            phase: RuntimePhase::Armed,
            last_error: None,
            failback_in_ms: None,
        };
        state.live = Some(plan.clone());
        Ok(RuntimeApplyResponse {
            status: state.runtime.clone(),
            plan,
            rollback_receipt: format!("receipt-{generation}"),
        })
    }

    async fn restore_runtime(&self, receipt: &str) -> Result<RuntimeStatus, DaemonError> {
        let mut state = self.0.lock().unwrap();
        state.restores.push(receipt.to_owned());
        let (plan, runtime) = state.previous.take().expect("a receipt was issued");
        state.live = plan;
        state.runtime = runtime;
        Ok(state.runtime.clone())
    }

    async fn inspect(&self) -> Result<InspectResponse, DaemonError> {
        let state = self.0.lock().unwrap();
        let plan = state.live.clone();
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
            applied_plan: plan,
            routelane_table_present: state.live.is_some(),
            snapshot,
            diagnostics: vec![],
            runtime: state.runtime.clone(),
        })
    }

    async fn cleanup(&self) -> Result<CleanupResponse, DaemonError> {
        let mut state = self.0.lock().unwrap();
        state.cleanups += 1;
        state.live = None;
        state.runtime = RuntimeStatus::default();
        Ok(CleanupResponse {
            cleaned: true,
            diagnostics: vec![],
        })
    }

    async fn recover_orphaned_resources(&self) -> Result<OrphanRecoveryResponse, DaemonError> {
        Ok(OrphanRecoveryResponse {
            cleaned: false,
            diagnostics: vec![],
        })
    }
}

/// A Helper1-only peer: no runtime methods, not even the protocol query.
#[derive(Clone, Default)]
struct LegacyHelper(RuntimeHelper);

#[async_trait]
impl PrivilegedHelperClient for LegacyHelper {
    async fn apply(
        &self,
        plan: &KernelPlan,
    ) -> Result<routelane::backend::ApplyResponse, DaemonError> {
        self.0.apply(plan).await
    }
    async fn restore(
        &self,
        failed: &KernelPlan,
        previous: Option<&KernelPlan>,
    ) -> Result<RestoreResponse, DaemonError> {
        self.0.restore(failed, previous).await
    }
    async fn inspect(&self) -> Result<InspectResponse, DaemonError> {
        self.0.inspect().await
    }
    async fn cleanup(&self) -> Result<CleanupResponse, DaemonError> {
        self.0.cleanup().await
    }
    async fn recover_orphaned_resources(&self) -> Result<OrphanRecoveryResponse, DaemonError> {
        self.0.recover_orphaned_resources().await
    }
}

#[derive(Clone, Default)]
struct MemoryConfig(Arc<Mutex<(Option<Profile>, usize)>>);

impl ConfigRepository for MemoryConfig {
    fn load(&self) -> Result<Option<Profile>, DaemonError> {
        Ok(self.0.lock().unwrap().0.clone())
    }
    fn save(&self, desired: Option<&Profile>) -> Result<(), DaemonError> {
        let mut state = self.0.lock().unwrap();
        state.0 = desired.cloned();
        state.1 += 1;
        Ok(())
    }
}

fn profile() -> Profile {
    Profile {
        routing: Default::default(),
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

type Service<H> = DaemonService<EmptyTopology, NoResolutions, H, MemoryConfig>;

fn service<H: PrivilegedHelperClient>(helper: H, config: MemoryConfig) -> Arc<Service<H>> {
    Arc::new(DaemonService::new(EmptyTopology, NoResolutions, helper, config).unwrap())
}

async fn status<H: PrivilegedHelperClient>(service: &Service<H>) -> DaemonStatus {
    let response = service
        .handle(DaemonEnvelope::new("status", DaemonRequest::GetStatus))
        .await;
    match response.result {
        Some(DaemonResult::Status(status)) => status,
        other => panic!("unexpected status response {other:?} {:?}", response.error),
    }
}

#[tokio::test]
async fn helper_transition_updates_status_without_gui() {
    let helper = RuntimeHelper::default();
    let service = service(helper.clone(), MemoryConfig::default());
    service.apply_profile(profile()).await.unwrap();
    let mut events = service.subscribe();

    let automatic = helper.transition(RuntimePhase::Armed);
    service.on_runtime_changed(automatic).await;

    let status = status(&service).await;
    assert_eq!(status.generation, 2);
    assert_eq!(status.primary.generation, status.generation);
    assert_eq!(status.primary.phase, RuntimePhase::Armed);
    assert_eq!(
        status.primary.decision.unwrap().effective.v4,
        FamilyExit::Blocked
    );
    match events.recv().await.unwrap() {
        DaemonEvent::PrimaryChanged {
            sequence,
            generation,
            primary,
        } => {
            assert!(sequence > 1);
            assert_eq!(generation, 2);
            assert_eq!(primary.generation, 2);
        }
        other => panic!("expected PrimaryChanged, got {other:?}"),
    }
}

#[tokio::test]
async fn reactivation_after_cleanup_uses_the_helpers_empty_generation() {
    let helper = RuntimeHelper::default();
    let service = service(helper.clone(), MemoryConfig::default());
    service.apply_profile(profile()).await.unwrap();
    let automatic = helper.transition(RuntimePhase::Armed);
    service.on_runtime_changed(automatic).await;
    assert_eq!(service.state().await.generation, 2);
    let mut events = service.subscribe();
    let before = service.state().await.event_sequence;

    for _ in 0..2 {
        service.deactivate().await.unwrap();
        let inactive = service.state().await;
        let applied = service.apply_profile(profile()).await.unwrap();
        assert!(inactive.applied.is_none());
        assert_eq!(inactive.generation, 0);
        assert_eq!(inactive.primary.generation, 0);
        assert!(inactive.event_sequence > before);
        assert!(matches!(
            events.recv().await.unwrap(),
            DaemonEvent::StateChanged { generation: 0, .. }
        ));

        assert_eq!(applied.generation, 1);
        let active = service.state().await;
        assert_eq!(active.generation, 1);
        assert_eq!(active.primary.generation, 1);
        assert!(active.applied.is_some());
        assert!(active.last_error.is_none());
        assert!(active.event_sequence > inactive.event_sequence);
        assert!(matches!(
            events.recv().await.unwrap(),
            DaemonEvent::StateChanged { generation: 1, .. }
        ));
    }
}

#[tokio::test]
async fn queued_runtime_announcement_cannot_rearm_a_cleaned_runtime() {
    let helper = RuntimeHelper::default();
    let service = service(helper.clone(), MemoryConfig::default());
    service.apply_profile(profile()).await.unwrap();
    let queued = helper.transition(RuntimePhase::Armed);
    service.on_runtime_changed(queued.clone()).await;
    service.deactivate().await.unwrap();

    service.on_runtime_changed(queued).await;
    let inactive = service.state().await;
    assert_eq!(inactive.generation, 0);
    assert_eq!(inactive.primary.phase, RuntimePhase::Inactive);
    assert!(inactive.applied.is_none());
    assert!(inactive.applied_profile.is_none());
    assert!(inactive.primary.grant.is_none());
}

#[tokio::test]
async fn daemon_restart_reconciles_valid_runtime() {
    let helper = RuntimeHelper::default();
    let config = MemoryConfig::default();
    service(helper.clone(), config.clone())
        .apply_profile(profile())
        .await
        .unwrap();
    helper.transition(RuntimePhase::Armed);
    // The desired profile changes on disk; it must not be presented as applied.
    let mut edited = profile();
    edited.name = "Edited".into();
    config.save(Some(&edited)).unwrap();

    let restarted = service(helper.clone(), config);
    let status = status(&restarted).await;
    assert_eq!(status.generation, 2);
    assert_eq!(status.primary.generation, 2);
    assert_eq!(status.primary.phase, RuntimePhase::Armed);
    assert_eq!(status.primary, helper.0.lock().unwrap().runtime);
}

#[tokio::test]
async fn old_protocol_is_rejected_before_mutation() {
    let helper = RuntimeHelper::default();
    let config = MemoryConfig::default();
    let service = service(helper.clone(), config.clone());
    let response = service
        .handle(DaemonEnvelope {
            protocol_version: 1,
            request_id: "old".into(),
            request: DaemonRequest::ApplyProfile(profile()),
        })
        .await;
    assert_eq!(
        response.error.unwrap().code,
        "IPC_UNSUPPORTED_PROTOCOL_VERSION"
    );
    assert_eq!(helper.0.lock().unwrap().activations, 0);
    assert_eq!(config.0.lock().unwrap().1, 0);
}

#[tokio::test]
async fn protocol2_peers_are_rejected_before_mutation() {
    assert_eq!((IPC_PROTOCOL_VERSION, RUNTIME_PROTOCOL_VERSION), (3, 3));
    // A protocol-2 GUI/CLI request.
    let helper = RuntimeHelper::default();
    let config = MemoryConfig::default();
    let response = service(helper.clone(), config.clone())
        .handle(DaemonEnvelope {
            protocol_version: 2,
            request_id: "old".into(),
            request: DaemonRequest::ApplyProfile(profile()),
        })
        .await;
    assert_eq!(
        response.error.unwrap().code,
        "IPC_UNSUPPORTED_PROTOCOL_VERSION"
    );
    assert_eq!(config.0.lock().unwrap().1, 0);
    assert_eq!(helper.0.lock().unwrap().activations, 0);

    // A Helper1-only peer and a Helper2 peer that reports protocol 2 (or an
    // unknown newer one): mismatch before the desired profile is saved or
    // the kernel is touched, with no retry and no downgrade.
    let legacy = LegacyHelper::default();
    let reports_two = RuntimeHelper::default();
    reports_two.0.lock().unwrap().protocol = Some(2);
    let reports_four = RuntimeHelper::default();
    reports_four.0.lock().unwrap().protocol = Some(4);
    for (name, state) in [
        ("legacy", rejected_before_mutation(legacy.clone()).await),
        ("two", rejected_before_mutation(reports_two.clone()).await),
        ("four", rejected_before_mutation(reports_four.clone()).await),
    ] {
        assert!(state.desired.is_none(), "{name}");
        assert!(state.applied.is_none(), "{name}");
    }
    for helper in [&legacy.0, &reports_two, &reports_four] {
        let helper = helper.0.lock().unwrap();
        assert_eq!((helper.activations, helper.cleanups), (0, 0));
    }
    for helper in [&reports_two, &reports_four] {
        assert_eq!(
            helper.0.lock().unwrap().protocol_queries,
            2,
            "one per request"
        );
    }
}

/// Apply and deactivate against an incompatible helper.
async fn rejected_before_mutation<H: PrivilegedHelperClient>(
    helper: H,
) -> routelane::daemon::DaemonState {
    let config = MemoryConfig::default();
    let service = service(helper, config.clone());
    let error = service.apply_profile(profile()).await.unwrap_err();
    assert_eq!(error.code(), "DAEMON_HELPER_PROTOCOL_MISMATCH");
    assert!(error.message().contains("routelane-helper"));
    let config_saves_after_request = config.0.lock().unwrap().1;
    assert_eq!(config_saves_after_request, 0);
    let error = service.deactivate().await.unwrap_err();
    assert_eq!(error.code(), "DAEMON_HELPER_PROTOCOL_MISMATCH");
    assert_eq!(config.0.lock().unwrap().1, 0);
    service.state().await
}

#[tokio::test]
async fn status_reports_applied_not_saved_mode() {
    let helper = RuntimeHelper::default();
    let config = MemoryConfig::default();
    let service = service(helper.clone(), config.clone());
    service.apply_profile(profile()).await.unwrap();
    let armed = status(&service).await;
    assert_eq!(
        armed.primary.unmatched_traffic,
        Some(UnmatchedTraffic::PreserveSystem)
    );

    // A broader mode is saved but its activation fails: the helper stays
    // Armed with the mode it authorized.
    helper.0.lock().unwrap().fail_activation = true;
    let mut broad = profile();
    broad.routing.unmatched_traffic = UnmatchedTraffic::UsePrimary;
    service.apply_profile(broad).await.unwrap_err();
    let saved = config.0.lock().unwrap().0.clone().unwrap();
    assert_eq!(
        saved.routing.unmatched_traffic,
        UnmatchedTraffic::UsePrimary
    );

    // An automatic transition, a helper restart and a daemon restart keep it.
    let automatic = helper.transition(RuntimePhase::Armed);
    service.on_runtime_changed(automatic).await;
    service.resync_runtime().await;
    let restarted = self::service(helper.clone(), config);
    for current in [status(&service).await, status(&restarted).await] {
        assert_eq!(current.primary.phase, RuntimePhase::Armed);
        assert_eq!(
            current.primary.unmatched_traffic,
            Some(UnmatchedTraffic::PreserveSystem)
        );
    }
}

#[tokio::test]
async fn helper_mode_must_pair_with_its_plan() {
    let helper = RuntimeHelper::default();
    helper.0.lock().unwrap().wrong_mode = true;
    let service = service(helper.clone(), MemoryConfig::default());
    let error = service.apply_profile(profile()).await.unwrap_err();
    assert_eq!(error.code(), "DAEMON_HELPER_RESPONSE_MISMATCH");
    assert_eq!(
        helper.0.lock().unwrap().restores,
        vec!["receipt-0".to_owned()]
    );
    assert!(service.state().await.applied.is_none());
}

#[tokio::test]
async fn stale_helper_generation_cannot_replace_new_status() {
    let helper = RuntimeHelper::default();
    let service = service(helper.clone(), MemoryConfig::default());
    service.apply_profile(profile()).await.unwrap();
    let first = helper.0.lock().unwrap().runtime.clone();
    let newer = helper.transition(RuntimePhase::Armed);
    service.on_runtime_changed(newer.clone()).await;
    service.on_runtime_changed(first).await;
    let status = status(&service).await;
    assert_eq!(status.generation, 2);
    assert_eq!(status.primary, newer);
}

#[tokio::test]
async fn transition_failure_is_not_healthy() {
    let helper = RuntimeHelper::default();
    let service = service(helper.clone(), MemoryConfig::default());
    service.apply_profile(profile()).await.unwrap();
    let degraded = helper.transition(RuntimePhase::Degraded);
    service.on_runtime_changed(degraded).await;
    let status = status(&service).await;
    let failed_status = &status.primary;
    assert_ne!(failed_status.phase, RuntimePhase::Armed);
    assert_ne!(status.health.state, HealthState::Healthy);
    assert!(status.primary.last_error.is_some());
}

#[tokio::test]
async fn activation_with_another_policy_is_restored_with_its_receipt() {
    let helper = RuntimeHelper::default();
    helper.0.lock().unwrap().wrong_policy = true;
    let service = service(helper.clone(), MemoryConfig::default());
    let error = service.apply_profile(profile()).await.unwrap_err();
    assert_eq!(error.code(), "DAEMON_HELPER_RESPONSE_MISMATCH");
    {
        let state = helper.0.lock().unwrap();
        assert_eq!(state.restores, vec!["receipt-0".to_owned()]);
        assert!(state.live.is_none());
    }
    assert!(service.state().await.applied.is_none());
}

#[tokio::test]
async fn helper_reappearing_resyncs_from_inspect() {
    let helper = RuntimeHelper::default();
    let service = service(helper.clone(), MemoryConfig::default());
    service.apply_profile(profile()).await.unwrap();
    // A transition happened while the daemon missed the announcement.
    helper.transition(RuntimePhase::Armed);
    service.resync_runtime().await;
    let status = status(&service).await;
    assert_eq!(status.generation, 2);
    assert_eq!(status.primary.generation, 2);
}
