use async_trait::async_trait;
use routelane::{
    backend::*,
    daemon::*,
    domain::*,
    ipc::*,
    usage::{calendar::*, counters::*, events::*, service::*, store::*},
};
use std::os::unix::fs::PermissionsExt;
use std::sync::{
    atomic::{AtomicI64, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

#[derive(Clone, Default)]
struct Forbidden(Arc<AtomicUsize>);
impl Forbidden {
    fn called(&self) -> ! {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("usage touched routing")
    }
}
#[async_trait]
impl TopologyProvider for Forbidden {
    async fn snapshot(&self, _: &Profile) -> Result<TopologySnapshot, DaemonError> {
        self.called()
    }
}
#[async_trait]
impl ResolutionProvider for Forbidden {
    async fn snapshot(&self, _: &Profile) -> Result<ResolutionSnapshot, DaemonError> {
        self.called()
    }
}
#[async_trait]
impl PrivilegedHelperClient for Forbidden {
    async fn runtime_protocol_version(&self) -> Result<u32, DaemonError> {
        self.called()
    }
    async fn apply(&self, _: &KernelPlan) -> Result<ApplyResponse, DaemonError> {
        self.called()
    }
    async fn restore(
        &self,
        _: &KernelPlan,
        _: Option<&KernelPlan>,
    ) -> Result<RestoreResponse, DaemonError> {
        self.called()
    }
    async fn inspect(&self) -> Result<InspectResponse, DaemonError> {
        self.called()
    }
    async fn cleanup(&self) -> Result<CleanupResponse, DaemonError> {
        self.called()
    }
    async fn recover_orphaned_resources(&self) -> Result<OrphanRecoveryResponse, DaemonError> {
        self.called()
    }
}
#[derive(Clone)]
struct Config {
    profile: Profile,
    saves: Arc<AtomicUsize>,
}
impl ConfigRepository for Config {
    fn load(&self) -> Result<Option<Profile>, DaemonError> {
        Ok(Some(self.profile.clone()))
    }
    fn save(&self, _: Option<&Profile>) -> Result<(), DaemonError> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        panic!("usage saved profile")
    }
}
#[derive(Default)]
struct Counters(Mutex<Option<InterfaceSample>>);
impl InterfaceCounters for Counters {
    fn sample(&self, _: &str) -> Option<InterfaceSample> {
        self.0.lock().unwrap().clone()
    }
}
fn lane(id: &str) -> Lane {
    Lane {
        id: id.into(),
        name: id.into(),
        interface: id.into(),
        fallback_lane: None,
        failure_policy: LaneFailurePolicy::Inactive,
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
struct Fixture {
    _dir: tempfile::TempDir,
    usage: Arc<UsageService>,
    daemon: DaemonService<Forbidden, Forbidden, Forbidden, Config>,
    clock: Arc<AtomicI64>,
    counters: Arc<Counters>,
    forbidden: Forbidden,
    saves: Arc<AtomicUsize>,
    settings: std::path::PathBuf,
}
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(AtomicI64::new(
        local_midnight(LocalDate::parse("2026-11-09").unwrap()) + 3600,
    ));
    let counters = Arc::new(Counters::default());
    let settings = dir.path().join("usage-settings.json");
    let (store, _) = UsageStore::open(dir.path().join("usage-v1.json"), "boot");
    let (events, _) = EventStore::open(dir.path().join("events-v1.jsonl"));
    let c = clock.clone();
    let usage = Arc::new(UsageService::new(
        store,
        settings.clone(),
        events,
        counters.clone(),
        Arc::new(move || c.load(Ordering::SeqCst)),
    ));
    let saves = Arc::new(AtomicUsize::new(0));
    let forbidden = Forbidden::default();
    let config = Config {
        profile: Profile {
            id: "default".into(),
            name: "Default".into(),
            enabled: true,
            lanes: vec![lane("vinn"), lane("ethernet")],
            rules: vec![],
            routing: Default::default(),
        },
        saves: saves.clone(),
    };
    let daemon = DaemonService::new(
        forbidden.clone(),
        forbidden.clone(),
        forbidden.clone(),
        config,
    )
    .unwrap()
    .with_usage(usage.clone());
    Fixture {
        _dir: dir,
        usage,
        daemon,
        clock,
        counters,
        forbidden,
        saves,
        settings,
    }
}
async fn request(f: &Fixture, request: DaemonRequest) -> DaemonResponse {
    f.daemon.handle(DaemonEnvelope::new("test", request)).await
}

#[tokio::test]
async fn usage_requests_never_touch_routing_or_config() {
    let f = fixture();
    let before = f.daemon.state().await;
    for req in [
        DaemonRequest::GetUsage { fast_lane: None },
        DaemonRequest::SetQuota {
            lane_id: "vinn".into(),
            quota: Some(quota()),
        },
        DaemonRequest::CorrectUsage {
            lane_id: "vinn".into(),
            used_bytes: 12_400_000_000,
        },
        DaemonRequest::ListEvents {
            category: EventCategory::All,
        },
        DaemonRequest::ClearEvents,
    ] {
        assert!(request(&f, req).await.error.is_none());
    }
    let after = f.daemon.state().await;
    assert_eq!(
        (before.generation, before.event_sequence),
        (after.generation, after.event_sequence)
    );
    assert_eq!(f.forbidden.0.load(Ordering::SeqCst), 0);
    assert_eq!(f.saves.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn quota_report_matches_the_vinn_example() {
    let f = fixture();
    f.usage.set_quota(&"vinn".into(), Some(quota())).unwrap();
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 0,
        tx_bytes: 0,
    });
    f.usage.tick(&[lane("vinn")]);
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 11_100_000_000,
        tx_bytes: 1_300_000_000,
    });
    f.usage.tick(&[lane("vinn")]);
    let response = request(&f, DaemonRequest::GetUsage { fast_lane: None }).await;
    let Some(DaemonResult::Usage(report)) = response.result else {
        panic!("{response:?}")
    };
    let q = report.lanes[0].quota.as_ref().unwrap();
    assert_eq!(q.used_bytes, 12_400_000_000);
    assert_eq!(
        q.measured,
        routelane::usage::UsageTotals {
            rx: 11_100_000_000,
            tx: 1_300_000_000
        }
    );
    assert_eq!(q.period_start.to_string(), "2026-11-01");
    assert_eq!(q.next_reset.to_string(), "2026-12-01");
    assert_eq!(report.lanes[0].daily.len(), 9);
    assert!(report.lanes[1].quota.is_none());
    assert_eq!(report.lanes[1].daily.len(), 7);
}
#[tokio::test]
async fn invalid_quota_is_rejected_without_saving() {
    let f = fixture();
    for req in [
        DaemonRequest::SetQuota {
            lane_id: "vinn".into(),
            quota: Some(LaneQuota {
                reset_day: 32,
                ..quota()
            }),
        },
        DaemonRequest::CorrectUsage {
            lane_id: "vinn".into(),
            used_bytes: 1,
        },
    ] {
        assert_eq!(
            request(&f, req).await.error.unwrap().code,
            "DAEMON_PROFILE_INVALID"
        );
    }
    assert!(!f.settings.exists());
}
#[tokio::test]
async fn protocol3_client_gets_an_actionable_mismatch() {
    let f = fixture();
    let before = f.daemon.state().await;
    let response = f
        .daemon
        .handle(DaemonEnvelope {
            protocol_version: 3,
            request_id: "old".into(),
            request: DaemonRequest::ApplyProfile(f.daemon.state().await.desired.unwrap()),
        })
        .await;
    let error = response.error.unwrap();
    assert_eq!(error.code, "IPC_UNSUPPORTED_PROTOCOL_VERSION");
    assert!(error.message.contains("4"));
    assert!(error.message.contains("matching"));
    let after = f.daemon.state().await;
    assert_eq!(
        (before.generation, before.event_sequence),
        (after.generation, after.event_sequence)
    );
    assert_eq!(f.forbidden.0.load(Ordering::SeqCst), 0);
    assert_eq!(f.saves.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn fast_sampling_lasts_five_seconds_after_the_last_request() {
    let f = fixture();
    assert_eq!(f.usage.interval(), Duration::from_secs(30));
    request(
        &f,
        DaemonRequest::GetUsage {
            fast_lane: Some("vinn".into()),
        },
    )
    .await;
    assert_eq!(f.usage.interval(), Duration::from_secs(1));
    f.clock.fetch_add(4, Ordering::SeqCst);
    request(
        &f,
        DaemonRequest::GetUsage {
            fast_lane: Some("vinn".into()),
        },
    )
    .await;
    f.clock.fetch_add(2, Ordering::SeqCst);
    assert_eq!(f.usage.interval(), Duration::from_secs(1));
    f.clock.fetch_add(4, Ordering::SeqCst);
    assert_eq!(f.usage.interval(), Duration::from_secs(30));
}
#[test]
fn removed_lanes_are_hidden_and_retained_when_they_return() {
    let f = fixture();
    let lanes = [lane("vinn")];
    f.usage.set_quota(&"vinn".into(), Some(quota())).unwrap();
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 0,
        tx_bytes: 0,
    });
    f.usage.tick(&lanes);
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 11_100_000_000,
        tx_bytes: 1_300_000_000,
    });
    f.usage.tick(&lanes);
    f.usage.correct(&"vinn".into(), 13_000_000_000).unwrap();
    f.usage.tick(&[]);
    assert!(f.usage.report(&[], None).lanes.is_empty());
    let (store, note) = UsageStore::open(f._dir.path().join("usage-v1.json"), "boot");
    assert!(note.is_none());
    let (events, _) = EventStore::open(f._dir.path().join("events-v1.jsonl"));
    let c = f.clock.clone();
    let restarted = UsageService::new(
        store,
        f.settings.clone(),
        events,
        f.counters.clone(),
        Arc::new(move || c.load(Ordering::SeqCst)),
    );
    let mut renamed = lane("vinn");
    renamed.interface = "wwan9".into();
    let report = restarted.report(&[renamed], None);
    let q = report.lanes[0].quota.as_ref().unwrap();
    assert_eq!(q.used_bytes, 13_000_000_000);
    assert_eq!(
        q.measured,
        routelane::usage::UsageTotals {
            rx: 11_100_000_000,
            tx: 1_300_000_000
        }
    );
    assert_eq!(report.lanes[0].interface, "wwan9");
}

#[tokio::test]
async fn usage_requests_report_unavailable_without_a_usage_service() {
    let f = fixture();
    let config = Config {
        profile: f.daemon.state().await.desired.unwrap(),
        saves: f.saves.clone(),
    };
    let daemon = DaemonService::new(
        f.forbidden.clone(),
        f.forbidden.clone(),
        f.forbidden.clone(),
        config,
    )
    .unwrap();
    for request in [
        DaemonRequest::GetUsage { fast_lane: None },
        DaemonRequest::SetQuota {
            lane_id: "vinn".into(),
            quota: None,
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
        assert_eq!(
            daemon
                .handle(DaemonEnvelope::new("no-usage", request))
                .await
                .error
                .unwrap()
                .code,
            "DAEMON_USAGE_UNAVAILABLE"
        );
    }
}

#[test]
fn rates_use_elapsed_seconds_expire_and_clear_on_disconnect() {
    let f = fixture();
    let lanes = [lane("vinn")];
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 100,
        tx_bytes: 10,
    });
    f.usage.tick(&lanes);
    assert!(f.usage.report(&lanes, None).lanes[0].rate.is_none());
    f.clock.fetch_add(2, Ordering::SeqCst);
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 2100,
        tx_bytes: 210,
    });
    f.usage.tick(&lanes);
    assert_eq!(
        f.usage.report(&lanes, None).lanes[0].rate,
        Some(UsageRate {
            rx_per_second: 1000,
            tx_per_second: 100
        })
    );
    f.clock.fetch_add(60, Ordering::SeqCst);
    assert!(f.usage.report(&lanes, None).lanes[0]
        .recent_rates
        .is_empty());
    *f.counters.0.lock().unwrap() = None;
    f.usage.tick(&lanes);
    let report = f.usage.report(&lanes, None);
    assert!(!report.lanes[0].available);
    assert!(report.lanes[0].rate.is_none());
    assert_eq!(
        report.lanes[0].today,
        routelane::usage::UsageTotals { rx: 2000, tx: 200 }
    );
}

fn saved_profile(interface: &str) -> Profile {
    let mut lane = lane("vinn");
    lane.interface = interface.into();
    Profile {
        id: "saved".into(),
        name: "Saved".into(),
        enabled: false,
        lanes: vec![lane],
        rules: vec![],
        routing: Default::default(),
    }
}

fn saved_lane_daemon(
    f: &Fixture,
    desired: Option<&Profile>,
    profiles: Vec<Profile>,
) -> DaemonService<Forbidden, Forbidden, Forbidden, AtomicJsonConfigRepository> {
    routelane::config::ConfigRepository::new(f._dir.path().join("config.json"))
        .save(&routelane::config::ConfigV4 {
            profiles,
            ..Default::default()
        })
        .unwrap();
    let repository = AtomicJsonConfigRepository::new(f._dir.path().join("desired-profile.json"));
    if desired.is_some() {
        repository.save(desired).unwrap();
    }
    DaemonService::new(
        f.forbidden.clone(),
        f.forbidden.clone(),
        f.forbidden.clone(),
        repository,
    )
    .unwrap()
    .with_usage(f.usage.clone())
}

#[tokio::test]
async fn saved_but_unapplied_lanes_are_sampled_and_accept_a_quota() {
    let f = fixture();
    let daemon = saved_lane_daemon(&f, None, vec![saved_profile("usb0")]);
    let config_path = f._dir.path().join("config.json");
    let before = std::fs::read(&config_path).unwrap();
    assert!(daemon.state().await.desired.is_none());
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 100,
        tx_bytes: 10,
    });
    let response = daemon
        .handle(DaemonEnvelope::new(
            "read",
            DaemonRequest::GetUsage { fast_lane: None },
        ))
        .await;
    let Some(DaemonResult::Usage(report)) = response.result else {
        panic!("{response:?}")
    };
    assert_eq!(report.lanes.len(), 1);
    assert_eq!(report.lanes[0].interface, "usb0");
    assert!(report.lanes[0].available);
    let response = daemon
        .handle(DaemonEnvelope::new(
            "quota",
            DaemonRequest::SetQuota {
                lane_id: "vinn".into(),
                quota: Some(quota()),
            },
        ))
        .await;
    assert!(response.error.is_none(), "{response:?}");
    assert!(matches!(response.result, Some(DaemonResult::QuotaSaved)));
    daemon.sample_usage().await;
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 1100,
        tx_bytes: 110,
    });
    daemon.sample_usage().await;
    let response = daemon
        .handle(DaemonEnvelope::new(
            "sampled",
            DaemonRequest::GetUsage { fast_lane: None },
        ))
        .await;
    let Some(DaemonResult::Usage(report)) = response.result else {
        panic!("{response:?}")
    };
    assert_eq!(
        report.lanes[0].today,
        routelane::usage::UsageTotals { rx: 1000, tx: 100 }
    );
    let response = daemon
        .handle(DaemonEnvelope::new(
            "correct",
            DaemonRequest::CorrectUsage {
                lane_id: "vinn".into(),
                used_bytes: 1200,
            },
        ))
        .await;
    assert!(
        matches!(response.result, Some(DaemonResult::QuotaSaved)),
        "{response:?}"
    );
    assert_eq!(
        std::fs::read(config_path).unwrap(),
        before,
        "usage never rewrites UI config"
    );
    assert!(!f._dir.path().join("desired-profile.json").exists());
    assert_eq!(f.forbidden.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn applied_lane_wins_over_a_saved_lane_with_the_same_id() {
    let f = fixture();
    let desired = saved_profile("usb0");
    let mut second = saved_profile("wlan10");
    second.id = "second".into();
    second.name = "Second".into();
    let daemon = saved_lane_daemon(&f, Some(&desired), vec![saved_profile("wlan9"), second]);
    let response = daemon
        .handle(DaemonEnvelope::new(
            "read",
            DaemonRequest::GetUsage { fast_lane: None },
        ))
        .await;
    let Some(DaemonResult::Usage(report)) = response.result else {
        panic!("{response:?}")
    };
    assert_eq!(report.lanes.len(), 1);
    assert_eq!(report.lanes[0].lane_id.as_str(), "vinn");
    assert_eq!(report.lanes[0].interface, "usb0");
    assert_eq!(f.forbidden.0.load(Ordering::SeqCst), 0);
}

#[test]
fn saved_lane_discovery_is_read_only_and_tolerates_missing_or_invalid_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let repository = AtomicJsonConfigRepository::new(dir.path().join("desired-profile.json"));
    assert!(repository.saved_lanes().is_empty());
    assert!(!path.exists());
    std::fs::write(&path, b"invalid config").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(repository.saved_lanes().is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), b"invalid config");
    let legacy = br#"{"schema_version":2,"language":"tr","profiles":[]}"#;
    std::fs::write(&path, legacy).unwrap();
    assert!(repository.saved_lanes().is_empty());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        legacy,
        "legacy config is never migrated on disk"
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "no backup or replacement files"
    );
    let mut second = saved_profile("wlan9");
    second.id = "second".into();
    second.name = "Second".into();
    routelane::config::ConfigRepository::new(&path)
        .save(&routelane::config::ConfigV4 {
            profiles: vec![saved_profile("usb0"), second],
            ..Default::default()
        })
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let lanes = repository.saved_lanes();
    assert_eq!(lanes.len(), 2, "all saved profiles are read");
    assert_eq!(lanes[0].interface.as_str(), "usb0");
    assert_eq!(lanes[1].interface.as_str(), "wlan9");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
    assert!(!dir.path().join("desired-profile.json").exists());
}

#[test]
fn same_second_samples_carry_into_the_next_rate() {
    let f = fixture();
    let lanes = [lane("vinn")];
    for (at, rx, tx, expected_count) in [
        (100, 0, 0, 0),
        (101, 1000, 100, 1),
        (101, 1500, 150, 1),
        (102, 2000, 200, 2),
    ] {
        f.clock.store(at, Ordering::SeqCst);
        *f.counters.0.lock().unwrap() = Some(InterfaceSample {
            ifindex: 1,
            rx_bytes: rx,
            tx_bytes: tx,
        });
        f.usage.tick(&lanes);
        let report = f.usage.report(&lanes, None);
        assert_eq!(report.lanes[0].recent_rates.len(), expected_count);
        if expected_count > 0 {
            assert_eq!(
                report.lanes[0].rate,
                Some(UsageRate {
                    rx_per_second: 1000,
                    tx_per_second: 100
                })
            );
        }
    }
}

#[tokio::test]
async fn a_failing_save_is_retried_on_the_thirty_second_cadence() {
    let f = fixture();
    let lanes = [lane("vinn")];
    // A non-empty directory where the store file belongs makes every save fail.
    let path = f._dir.path().join("usage-v1.json");
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir_all(path.join("blocker")).unwrap();
    let start = f.clock.load(Ordering::SeqCst);
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 1000,
        tx_bytes: 100,
    });
    assert!(f.usage.tick(&lanes).is_empty());
    f.clock.store(start + 1, Ordering::SeqCst);
    assert!(
        f.usage.interval() >= std::time::Duration::from_secs(29),
        "a failed save must not make the sampler retry every second: {:?}",
        f.usage.interval()
    );
}

#[tokio::test]
async fn fast_ticks_write_at_most_every_thirty_seconds_and_flush_on_demand() {
    use std::os::unix::fs::MetadataExt;
    let f = fixture();
    let lanes = [lane("vinn")];
    let path = f._dir.path().join("usage-v1.json");
    let start = f.clock.load(Ordering::SeqCst);
    let tick = |second: i64| {
        f.clock.store(start + second, Ordering::SeqCst);
        *f.counters.0.lock().unwrap() = Some(InterfaceSample {
            ifindex: 1,
            rx_bytes: second as u64 * 1000,
            tx_bytes: second as u64 * 100,
        });
        assert!(f.usage.tick(&lanes).is_empty());
    };
    let fingerprint = || {
        let metadata = std::fs::metadata(&path).unwrap();
        (metadata.ino(), metadata.modified().unwrap(), metadata.len())
    };
    tick(0);
    let first = fingerprint();
    let baseline = std::fs::read(&path).unwrap();
    for second in 1..=29 {
        tick(second);
        assert_eq!(fingerprint(), first, "no rewrite at second {second}");
        assert_eq!(std::fs::read(&path).unwrap(), baseline);
    }
    tick(30);
    assert_ne!(
        fingerprint(),
        first,
        "the thirty-second deadline saves accumulated usage"
    );
    let (store, _) = UsageStore::open(&path, "boot");
    assert_eq!(store.lifetime_bytes(&"vinn".into()), 33_000);
    tick(31);
    f.usage.flush().unwrap();
    let (store, _) = UsageStore::open(&path, "boot");
    assert_eq!(
        store.lifetime_bytes(&"vinn".into()),
        34_100,
        "flush persists the latest sample immediately"
    );
    let flushed = fingerprint();
    f.usage.flush().unwrap();
    assert_eq!(fingerprint(), flushed, "a clean flush does not write");
    for second in 32..=60 {
        tick(second);
        assert_eq!(
            fingerprint(),
            flushed,
            "flush restarts the thirty-second deadline"
        );
    }
    tick(61);
    assert_ne!(fingerprint(), flushed);
}

#[test]
fn quota_crossings_save_immediately_and_are_not_replayed_after_restart() {
    let f = fixture();
    let lanes = [lane("vinn")];
    let path = f._dir.path().join("usage-v1.json");
    f.usage.set_quota(&"vinn".into(), Some(quota())).unwrap();
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 0,
        tx_bytes: 0,
    });
    f.usage.tick(&lanes);
    f.clock.fetch_add(1, Ordering::SeqCst);
    *f.counters.0.lock().unwrap() = Some(InterfaceSample {
        ifindex: 1,
        rx_bytes: 16_000_000_000,
        tx_bytes: 0,
    });
    assert_eq!(f.usage.tick(&lanes).len(), 1);
    let (mut store, _) = UsageStore::open(&path, "boot");
    let today = local_date(f.clock.load(Ordering::SeqCst));
    assert_eq!(
        store.used_bytes(&"vinn".into(), &quota(), today),
        16_000_000_000
    );
    assert!(store.crossings(&"vinn".into(), &quota(), today).is_empty());
    assert!(!store.is_dirty());
}
