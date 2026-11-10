use async_trait::async_trait;
use routelane::{
    backend::*,
    config::Language,
    daemon::*,
    domain::*,
    ipc::*,
    usage::{calendar::*, counters::*, events::*, notify::*, store::*, UsageService, UsageTotals},
};
use std::sync::{
    atomic::{AtomicI64, Ordering},
    Arc, Mutex,
};

struct Empty;
#[async_trait]
impl TopologyProvider for Empty {
    async fn snapshot(&self, _: &Profile) -> Result<TopologySnapshot, DaemonError> {
        Ok(TopologySnapshot::empty())
    }
}
#[async_trait]
impl ResolutionProvider for Empty {
    async fn snapshot(&self, _: &Profile) -> Result<ResolutionSnapshot, DaemonError> {
        Ok(ResolutionSnapshot::empty())
    }
}
struct Config(Profile, Language);
impl ConfigRepository for Config {
    fn load(&self) -> Result<Option<Profile>, DaemonError> {
        Ok(Some(self.0.clone()))
    }
    fn save(&self, _: Option<&Profile>) -> Result<(), DaemonError> {
        Ok(())
    }
    fn language(&self) -> Language {
        self.1
    }
}
#[derive(Default)]
struct HelperState {
    plan: Option<KernelPlan>,
    runtime: RuntimeStatus,
    fail: bool,
    bad_response: bool,
    rollback_fails: bool,
    activation_gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
}
#[derive(Clone, Default)]
struct Helper(Arc<Mutex<HelperState>>);
#[async_trait]
impl PrivilegedHelperClient for Helper {
    async fn runtime_protocol_version(&self) -> Result<u32, DaemonError> {
        Ok(RUNTIME_PROTOCOL_VERSION)
    }
    async fn apply(&self, _: &KernelPlan) -> Result<ApplyResponse, DaemonError> {
        panic!("legacy apply")
    }
    async fn restore(
        &self,
        _: &KernelPlan,
        _: Option<&KernelPlan>,
    ) -> Result<RestoreResponse, DaemonError> {
        panic!("legacy restore")
    }
    async fn activate_runtime(
        &self,
        a: &RuntimeActivation,
    ) -> Result<RuntimeApplyResponse, DaemonError> {
        let gate = self.0.lock().unwrap().activation_gate.clone();
        if let Some((entered, proceed)) = gate {
            entered.notify_one();
            proceed.notified().await;
        }
        let mut state = self.0.lock().unwrap();
        if state.fail {
            return Err(DaemonError::helper("activation failed"));
        }
        let plan = compile_profile(
            &a.profile,
            &TopologySnapshot::empty(),
            &a.resolutions,
            a.expected_generation + 1,
        )
        .unwrap();
        let runtime = RuntimeStatus {
            generation: plan.generation,
            phase: RuntimePhase::Armed,
            decision: Some(plan.primary.clone()),
            unmatched_traffic: Some(plan.unmatched_traffic),
            grant: Some(RuntimeGrant {
                owner_uid: 1000,
                boot_id: "boot".into(),
                network_namespace: 1,
                policy_hash: policy_hash(&a.profile, &a.resolutions),
                interfaces: Default::default(),
            }),
            ..Default::default()
        };
        state.plan = Some(plan.clone());
        state.runtime = runtime.clone();
        let mut response = RuntimeApplyResponse {
            plan,
            status: runtime,
            rollback_receipt: "receipt".into(),
        };
        if state.bad_response {
            response.status.generation += 1;
        }
        Ok(response)
    }
    async fn restore_runtime(&self, _: &str) -> Result<RuntimeStatus, DaemonError> {
        let mut state = self.0.lock().unwrap();
        if state.rollback_fails {
            return Err(DaemonError::helper("restore failed"));
        }
        state.plan = None;
        state.runtime = RuntimeStatus::default();
        Ok(state.runtime.clone())
    }
    async fn inspect(&self) -> Result<InspectResponse, DaemonError> {
        let state = self.0.lock().unwrap();
        let nft = if state.plan.is_some() {
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
        let snapshot = state.plan.as_ref().map_or(snapshot.clone(), |plan| {
            snapshot.with_nft_ownership(plan.generation, plan.content_hash())
        });
        Ok(InspectResponse {
            generation: state.plan.as_ref().map(|p| p.generation),
            plan_hash: state.plan.as_ref().map(KernelPlan::content_hash),
            applied_plan: state.plan.clone(),
            routelane_table_present: state.plan.is_some(),
            snapshot,
            diagnostics: vec![],
            observed_connected_networks: None,
            runtime: state.runtime.clone(),
        })
    }
    async fn cleanup(&self) -> Result<CleanupResponse, DaemonError> {
        let mut s = self.0.lock().unwrap();
        s.plan = None;
        s.runtime = RuntimeStatus::default();
        Ok(CleanupResponse {
            cleaned: true,
            diagnostics: vec![],
        })
    }
    async fn recover_orphaned_resources(&self) -> Result<OrphanRecoveryResponse, DaemonError> {
        panic!("unexpected recovery")
    }
}
#[derive(Default)]
struct Counters(Mutex<Option<InterfaceSample>>);
impl InterfaceCounters for Counters {
    fn sample(&self, _: &str) -> Option<InterfaceSample> {
        self.0.lock().unwrap().clone()
    }
}
#[derive(Default)]
struct Notifications(Mutex<Vec<(String, String)>>);
impl Notifier for Notifications {
    fn notify(&self, summary: &str, body: &str) {
        self.0.lock().unwrap().push((summary.into(), body.into()));
    }
}
fn profile() -> Profile {
    Profile {
        id: "default".into(),
        name: "Default".into(),
        enabled: true,
        lanes: vec![Lane {
            id: "vinn".into(),
            name: "VINN".into(),
            interface: "wwan0".into(),
            fallback_lane: None,
            failure_policy: LaneFailurePolicy::Inactive,
        }],
        rules: vec![Rule {
            id: "block".into(),
            priority: 10,
            enabled: true,
            source: RuleSource::Any,
            target: RuleTarget::Any,
            action: RuleAction::Block,
        }],
        routing: Default::default(),
    }
}
fn quota() -> LaneQuota {
    LaneQuota {
        enabled: true,
        limit_bytes: 20_000_000_000,
        reset_day: 1,
        warn_percent: 80,
    }
}
type Service = DaemonService<Empty, Empty, Helper, Config>;
struct Fixture {
    _dir: tempfile::TempDir,
    usage: Arc<UsageService>,
    daemon: Service,
    helper: Helper,
    notifications: Arc<Notifications>,
    clock: Arc<AtomicI64>,
    counters: Arc<Counters>,
}
fn fixture(language: Language) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(AtomicI64::new(
        local_midnight(LocalDate::parse("2026-11-09").unwrap()) + 3600,
    ));
    let c = clock.clone();
    let (store, _) = UsageStore::open(dir.path().join("usage-v1.json"), "boot");
    let (events, _) = EventStore::open(dir.path().join("events-v1.jsonl"));
    let counters = Arc::new(Counters::default());
    let usage = Arc::new(UsageService::new(
        store,
        dir.path().join("usage-settings.json"),
        events,
        counters.clone(),
        Arc::new(move || c.load(Ordering::SeqCst)),
    ));
    let helper = Helper::default();
    let notifications = Arc::new(Notifications::default());
    let daemon = DaemonService::new(Empty, Empty, helper.clone(), Config(profile(), language))
        .unwrap()
        .with_usage(usage.clone())
        .with_notifier(notifications.clone());
    Fixture {
        _dir: dir,
        usage,
        daemon,
        helper,
        notifications,
        clock,
        counters,
    }
}
#[tokio::test]
async fn apply_and_deactivate_are_recorded() {
    let f = fixture(Language::Tr);
    // Use a profile with no lanes: fake topology has no interface evidence.
    let mut p = profile();
    p.lanes.clear();
    f.daemon.apply_profile(p.clone()).await.unwrap();
    assert_eq!(
        f.usage
            .events(EventCategory::All)
            .iter()
            .map(|e| e.kind.clone())
            .collect::<Vec<_>>(),
        vec![
            EventKind::RuleApplied {
                rule_id: "block".into()
            },
            EventKind::ProfileApplied {
                profile: "Default".into()
            }
        ]
    );
    f.daemon.apply_profile(p.clone()).await.unwrap();
    assert_eq!(f.usage.events(EventCategory::Rules).len(), 1);
    p.rules[0].enabled = false;
    f.daemon.apply_profile(p.clone()).await.unwrap();
    p.rules[0].enabled = true;
    f.daemon.apply_profile(p.clone()).await.unwrap();
    assert_eq!(f.usage.events(EventCategory::Rules).len(), 2);
    f.helper.0.lock().unwrap().fail = true;
    f.daemon.apply_profile(p).await.unwrap_err();
    assert!(
        matches!(&f.usage.events(EventCategory::Problems)[0].kind, EventKind::ApplyFailed { code, message, rolled_back: true } if code == "DAEMON_HELPER_FAILED" && message == "activation failed")
    );
    f.daemon.deactivate().await.unwrap();
    assert_eq!(
        f.usage.events(EventCategory::Transitions)[0].kind,
        EventKind::ProfileDeactivated
    );
}
#[tokio::test]
async fn quota_crossing_notifies_desktop_only_without_an_open_ui() {
    for subscribed in [false, true] {
        let f = fixture(Language::Tr);
        f.usage.set_quota(&"vinn".into(), Some(quota())).unwrap();
        let mut receiver = subscribed.then(|| f.daemon.subscribe());
        assert_eq!(f.daemon.subscriber_count(), usize::from(subscribed));
        let response = f
            .daemon
            .handle(DaemonEnvelope::new(
                "correct",
                DaemonRequest::CorrectUsage {
                    lane_id: "vinn".into(),
                    used_bytes: 16_000_000_000,
                },
            ))
            .await;
        assert!(response.error.is_none());
        assert_eq!(
            f.notifications.0.lock().unwrap().len(),
            usize::from(!subscribed)
        );
        if let Some(receiver) = &mut receiver {
            assert!(
                matches!(receiver.try_recv().unwrap(), DaemonEvent::QuotaCrossed { lane_id, crossing: QuotaCrossing::Warning { used: 16_000_000_000, limit: 20_000_000_000 }, .. } if lane_id == LaneId::from("vinn"))
            );
            assert!(receiver.try_recv().is_err());
        } else {
            assert_eq!(f.notifications.0.lock().unwrap()[0], ("RouteLane".into(), "VINN kotasının %80'i doldu — 16,0 / 20 GB · 4 GB kaldı · 1 Aralık'ta yenilenir".into()));
        }
        assert_eq!(f.usage.events(EventCategory::Quota).len(), 1);
        f.daemon
            .handle(DaemonEnvelope::new(
                "same",
                DaemonRequest::CorrectUsage {
                    lane_id: "vinn".into(),
                    used_bytes: 17_000_000_000,
                },
            ))
            .await;
        assert_eq!(f.usage.events(EventCategory::Quota).len(), 1);
    }
}
#[tokio::test]
async fn sampler_crossings_and_english_full_notification_are_recorded_once() {
    let f = fixture(Language::En);
    f.usage.set_quota(&"vinn".into(), Some(quota())).unwrap();
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 0,
        tx_bytes: 0,
    });
    f.daemon.sample_usage().await;
    f.clock.fetch_add(30, Ordering::SeqCst);
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 16_000_000_000,
        tx_bytes: 0,
    });
    f.daemon.sample_usage().await;
    assert_eq!(
        f.notifications.0.lock().unwrap()[0].1,
        "VINN quota is 80% used — 16.0 / 20 GB · 4 GB left · renews on 1 December"
    );
    f.clock.fetch_add(30, Ordering::SeqCst);
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 20_000_000_000,
        tx_bytes: 0,
    });
    f.daemon.sample_usage().await;
    f.daemon.sample_usage().await;
    assert_eq!(
        f.notifications.0.lock().unwrap()[1].1,
        "VINN quota is full — traffic was not stopped; your operator may charge extra."
    );
    assert_eq!(f.usage.events(EventCategory::Quota).len(), 2);
    assert_eq!(
        f.usage.report(&profile().lanes, None).lanes[0].today,
        UsageTotals {
            rx: 20_000_000_000,
            tx: 0
        }
    );
}
#[tokio::test]
async fn rollback_failure_is_recorded_as_not_rolled_back() {
    let f = fixture(Language::Tr);
    {
        let mut helper = f.helper.0.lock().unwrap();
        helper.bad_response = true;
        helper.rollback_fails = true;
    }
    let mut p = profile();
    p.lanes.clear();
    assert_eq!(
        f.daemon.apply_profile(p).await.unwrap_err().code(),
        "DAEMON_KERNEL_ROLLBACK_FAILED"
    );
    assert!(matches!(
        &f.usage.events(EventCategory::Problems)[0].kind,
        EventKind::ApplyFailed {
            rolled_back: false,
            ..
        }
    ));
    assert_eq!(f.usage.events(EventCategory::Problems).len(), 1);
}
#[test]
fn config_repository_reads_language_without_rewriting_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let document = routelane::config::ConfigV4 {
        language: Language::En,
        ..Default::default()
    };
    routelane::config::ConfigRepository::new(&path)
        .save(&document)
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    let repository = AtomicJsonConfigRepository::new(dir.path().join("desired-profile.json"));
    assert_eq!(repository.language(), Language::En);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!dir.path().join("desired-profile.json").exists());
}

#[tokio::test]
async fn usage_requests_finish_while_a_routing_mutation_is_locked() {
    let f = fixture(Language::Tr);
    let entered = Arc::new(tokio::sync::Notify::new());
    let proceed = Arc::new(tokio::sync::Notify::new());
    f.helper.0.lock().unwrap().activation_gate = Some((entered.clone(), proceed.clone()));
    let daemon = Arc::new(f.daemon);
    let applying = {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            let mut p = profile();
            p.lanes.clear();
            daemon.apply_profile(p).await
        })
    };
    entered.notified().await;
    let requests = async {
        for request in [
            DaemonRequest::GetUsage { fast_lane: None },
            DaemonRequest::SetQuota {
                lane_id: "vinn".into(),
                quota: Some(quota()),
            },
            DaemonRequest::CorrectUsage {
                lane_id: "vinn".into(),
                used_bytes: 0,
            },
            DaemonRequest::ListEvents {
                category: EventCategory::All,
            },
            DaemonRequest::ClearEvents,
        ] {
            // The applying profile has no lanes, so writes may fail validation,
            // but must still return while the mutation lock remains held.
            daemon
                .handle(DaemonEnvelope::new("concurrent", request))
                .await;
        }
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), requests).await;
    proceed.notify_one();
    applying.await.unwrap().unwrap();
    result.expect("usage waited for the routing mutation lock");
}

#[tokio::test]
async fn runtime_changes_are_recorded_once_and_track_backup_usage() {
    let f = fixture(Language::Tr);
    let mut p = profile();
    p.lanes.clear();
    f.daemon.apply_profile(p).await.unwrap();
    // The paired fake plan has no routes; only the status decision changes.
    let transition = |id: &str, helper: &Helper| {
        let mut state = helper.0.lock().unwrap();
        let mut plan = state.plan.clone().unwrap();
        plan.generation += 1;
        plan.primary = PrimaryDecision {
            preferred: FamilyPair {
                v4: Some("ethernet".into()),
                v6: Some("ethernet".into()),
            },
            effective: FamilyPair {
                v4: FamilyExit::Lane(id.into()),
                v6: FamilyExit::Lane(id.into()),
            },
        };
        state.runtime.generation = plan.generation;
        state.runtime.decision = Some(plan.primary.clone());
        state.plan = Some(plan);
        state.runtime.clone()
    };
    // Establish the preferred exit before failing over.
    f.daemon
        .on_runtime_changed(transition("ethernet", &f.helper))
        .await;
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 1000,
        tx_bytes: 0,
    });
    f.usage.tick(&profile().lanes);
    let backup = transition("vinn", &f.helper);
    f.daemon.on_runtime_changed(backup.clone()).await;
    f.daemon.on_runtime_changed(backup).await;
    f.clock.fetch_add(420, Ordering::SeqCst);
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 410_001_000,
        tx_bytes: 0,
    });
    f.usage.tick(&profile().lanes);
    f.daemon
        .on_runtime_changed(transition("ethernet", &f.helper))
        .await;
    let events = f.usage.events(EventCategory::Transitions);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::Failover { .. }))
            .count(),
        1
    );
    assert!(matches!(
        &events[0].kind,
        EventKind::Failback {
            backup_seconds: 420,
            backup_bytes: Some(410_000_000),
            ..
        }
    ));
}
