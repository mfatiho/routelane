use super::*;

fn settings(metric: Option<i64>) -> Settings {
    let mut ipv4 = BTreeMap::from([(
        "method".into(),
        OwnedValue::from(zbus::zvariant::Str::from("auto")),
    )]);
    if let Some(metric) = metric {
        ipv4.insert("route-metric".into(), metric.into());
    }
    BTreeMap::from([("ipv4".into(), ipv4)])
}
struct Fake {
    current: Snapshot,
    writes: usize,
    fail_reapply: bool,
}
impl Fake {
    fn new() -> Self {
        Self {
            current: Snapshot {
                device_path: "/devices/2".into(),
                profile_path: "/profiles/1".into(),
                identity: Identity {
                    interface: "eth0".into(),
                    uuid: "ethernet-profile".into(),
                    ifindex: 2,
                    hardware: "aa:bb:cc:dd:ee:ff".into(),
                },
                owner: ":1.15".into(),
                saved_version: 7,
                applied_version: 12,
                saved: settings(None),
                applied: settings(Some(-1)),
                default_interface: Some("usb0".into()),
                defaults: vec![("usb0".into(), 113), ("eth0".into(), 114)],
                ready: true,
            },
            writes: 0,
            fail_reapply: false,
        }
    }
}
#[async_trait]
impl Backend for Fake {
    async fn snapshot(&mut self, _: &str) -> Result<Snapshot> {
        Ok(self.current.clone())
    }
    async fn update(&mut self, expected: &Snapshot, metric: Option<i64>) -> Result<()> {
        assert_eq!(
            expected.saved_version, self.current.saved_version,
            "CAS protects other writers"
        );
        self.writes += 1;
        match metric {
            Some(v) => {
                self.current
                    .saved
                    .get_mut("ipv4")
                    .unwrap()
                    .insert("route-metric".into(), v.into());
            }
            None => {
                self.current
                    .saved
                    .get_mut("ipv4")
                    .unwrap()
                    .remove("route-metric");
            }
        }
        self.current.saved_version += 1;
        Ok(())
    }
    async fn reapply(&mut self, expected: &Snapshot, metric: Option<i64>) -> Result<()> {
        if self.fail_reapply {
            return Err(Error::new(ErrorKind::Authorization, "denied"));
        }
        assert_eq!(expected.applied_version, self.current.applied_version);
        self.writes += 1;
        match metric {
            Some(v) => {
                self.current
                    .applied
                    .get_mut("ipv4")
                    .unwrap()
                    .insert("route-metric".into(), v.into());
            }
            None => {
                self.current
                    .applied
                    .get_mut("ipv4")
                    .unwrap()
                    .remove("route-metric");
            }
        }
        self.current.applied_version += 1;
        self.current.default_interface =
            Some(if metric == Some(50) { "eth0" } else { "usb0" }.into());
        self.current.defaults = vec![
            ("usb0".into(), 113),
            ("eth0".into(), if metric == Some(50) { 50 } else { 114 }),
        ];
        Ok(())
    }
}

#[tokio::test]
async fn inspection_of_wrong_system_default_is_read_only_and_proposes_lower_priority() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.json");
    let mut backend = Fake::new();
    let view = inspect(&mut backend, &path, "eth0").await.unwrap();
    assert_eq!(view.default_interface.as_deref(), Some("usb0"));
    assert_eq!(view.approval.metric, 50);
    assert_eq!(backend.writes, 0);
    assert!(!path.exists(), "inspection never creates a ledger");
}

#[tokio::test]
async fn make_primary_persists_original_values_and_restore_preserves_missing_property() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.json");
    let mut backend = Fake::new();
    let view = inspect(&mut backend, &path, "eth0").await.unwrap();
    make_primary(&mut backend, &path, &view.approval)
        .await
        .unwrap();
    assert_eq!(backend.current.default_interface.as_deref(), Some("eth0"));
    let view = inspect(&mut backend, &path, "eth0").await.unwrap();
    assert!(view.can_restore);
    restore(&mut backend, &path, &view.approval).await.unwrap();
    assert!(!backend.current.saved["ipv4"].contains_key("route-metric"));
    assert_eq!(
        i64::try_from(&backend.current.applied["ipv4"]["route-metric"]).unwrap(),
        -1
    );
}

#[tokio::test]
async fn stale_confirmation_never_changes_network() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.json");
    let mut backend = Fake::new();
    let view = inspect(&mut backend, &path, "eth0").await.unwrap();
    backend.current.saved_version += 1;
    assert_eq!(
        make_primary(&mut backend, &path, &view.approval)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Stale
    );
    assert_eq!(backend.writes, 0);
    assert!(!path.exists());
}

#[tokio::test]
async fn interrupted_reapply_keeps_recoverable_record_and_restore_does_not_touch_foreign_metric() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.json");
    let mut backend = Fake::new();
    let view = inspect(&mut backend, &path, "eth0").await.unwrap();
    backend.fail_reapply = true;
    assert!(make_primary(&mut backend, &path, &view.approval)
        .await
        .is_err());
    assert!(path.exists());
    backend
        .current
        .saved
        .get_mut("ipv4")
        .unwrap()
        .insert("route-metric".into(), 70i64.into());
    let writes = backend.writes;
    let view = inspect(&mut backend, &path, "eth0").await.unwrap();
    assert!(
        !view.can_restore,
        "a foreign priority cannot be overwritten by Undo"
    );
    assert!(restore(&mut backend, &path, &view.approval).await.is_err());
    assert_eq!(backend.writes, writes);
}

#[tokio::test]
async fn partial_saved_change_can_be_restored_without_replaying_activation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.json");
    let mut backend = Fake::new();
    let approval = inspect(&mut backend, &path, "eth0").await.unwrap().approval;
    backend.fail_reapply = true;
    assert!(make_primary(&mut backend, &path, &approval).await.is_err());
    backend.fail_reapply = false;
    let view = inspect(&mut backend, &path, "eth0").await.unwrap();
    assert!(view.can_restore);
    restore(&mut backend, &path, &view.approval).await.unwrap();
    assert_eq!(
        backend.writes, 2,
        "save then restore saved field; applied state was never changed"
    );
    assert!(
        !inspect(&mut backend, &path, "eth0")
            .await
            .unwrap()
            .has_record
    );
}

#[tokio::test]
async fn unrelated_settings_and_reused_device_are_never_overwritten_by_restore() {
    for changed_device in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.json");
        let mut backend = Fake::new();
        let approval = inspect(&mut backend, &path, "eth0").await.unwrap().approval;
        make_primary(&mut backend, &path, &approval).await.unwrap();
        if changed_device {
            backend.current.identity.hardware = "different-device".into();
        } else {
            backend
                .current
                .saved
                .get_mut("ipv4")
                .unwrap()
                .insert("dns-priority".into(), 25i32.into());
        }
        let view = inspect(&mut backend, &path, "eth0").await.unwrap();
        assert!(!view.can_restore);
        let writes = backend.writes;
        assert!(restore(&mut backend, &path, &view.approval).await.is_err());
        assert_eq!(backend.writes, writes);
    }
}

#[tokio::test]
async fn duplicate_confirmation_does_not_overwrite_the_original_undo_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.json");
    let mut backend = Fake::new();
    let approval = inspect(&mut backend, &path, "eth0").await.unwrap().approval;
    make_primary(&mut backend, &path, &approval).await.unwrap();
    let original = std::fs::read(&path).unwrap();
    assert!(make_primary(&mut backend, &path, &approval).await.is_err());
    let latest = inspect(&mut backend, &path, "eth0").await.unwrap().approval;
    assert!(make_primary(&mut backend, &path, &latest).await.is_err());
    assert_eq!(backend.writes, 2);
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[tokio::test]
async fn disconnected_or_zero_metric_competitor_cannot_start_a_change() {
    for disconnected in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let mut backend = Fake::new();
        if disconnected {
            backend.current.ready = false;
        } else {
            backend.current.defaults[0].1 = 0;
        }
        assert!(
            inspect(&mut backend, &dir.path().join("journal.json"), "eth0")
                .await
                .is_err()
        );
        assert_eq!(backend.writes, 0);
    }
}

#[tokio::test]
async fn unsafe_ledger_prevents_network_mutations() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.json");
    let mut backend = Fake::new();
    let approval = inspect(&mut backend, &path, "eth0").await.unwrap().approval;
    let sentinel = dir.path().join("foreign");
    std::fs::write(&sentinel, b"untouched").unwrap();
    symlink(&sentinel, &path).unwrap();
    assert_eq!(
        make_primary(&mut backend, &path, &approval)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Storage
    );
    assert_eq!(backend.writes, 0);
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"untouched");
}

#[tokio::test]
async fn unknown_in_flight_write_cannot_be_undone_from_a_pre_call_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.json");
    let mut backend = Fake::new();
    let view = inspect(&mut backend, &path, "eth0").await.unwrap();
    let record = Record {
        identity: view.approval.identity.clone(),
        saved_hash: view.approval.saved_hash,
        applied_hash: view.approval.applied_hash,
        old_saved: None,
        old_applied: Some(-1),
        written: 50,
        pending: Some(PendingWrite::Saved(Some(50))),
    };
    Journal {
        version: 1,
        records: BTreeMap::from([("ethernet-profile".into(), record)]),
    }
    .save(&path)
    .unwrap();
    let view = inspect(&mut backend, &path, "eth0").await.unwrap();
    assert!(!view.can_restore);
    assert!(restore(&mut backend, &path, &view.approval).await.is_err());
    assert_eq!(backend.writes, 0);
    // A late success is observable; the original values are still recoverable.
    backend
        .current
        .saved
        .get_mut("ipv4")
        .unwrap()
        .insert("route-metric".into(), 50i64.into());
    let view = inspect(&mut backend, &path, "eth0").await.unwrap();
    assert!(view.can_restore);
    restore(&mut backend, &path, &view.approval).await.unwrap();
    assert!(!backend.current.saved["ipv4"].contains_key("route-metric"));
}

#[tokio::test]
async fn profile_object_path_switch_invalidates_confirmation_even_with_matching_versions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.json");
    let mut backend = Fake::new();
    let approval = inspect(&mut backend, &path, "eth0").await.unwrap().approval;
    backend.current.profile_path = "/profiles/other".into();
    assert_eq!(
        make_primary(&mut backend, &path, &approval)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Stale
    );
    assert_eq!(backend.writes, 0);
}

#[tokio::test]
async fn an_existing_better_explicit_priority_is_not_raised_unnecessarily() {
    let dir = tempfile::tempdir().unwrap();
    let mut backend = Fake::new();
    backend
        .current
        .saved
        .get_mut("ipv4")
        .unwrap()
        .insert("route-metric".into(), 20i64.into());
    let view = inspect(&mut backend, &dir.path().join("journal.json"), "eth0")
        .await
        .unwrap();
    assert_eq!(view.approval.metric, 20);
    assert_eq!(backend.writes, 0);
}

#[tokio::test]
async fn ambiguous_reapply_does_not_allow_restore_until_the_applied_target_is_visible() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.json");
    let mut backend = Fake::new();
    let initial = inspect(&mut backend, &path, "eth0").await.unwrap().approval;
    backend
        .current
        .saved
        .get_mut("ipv4")
        .unwrap()
        .insert("route-metric".into(), 50i64.into());
    let record = Record {
        identity: initial.identity,
        saved_hash: initial.saved_hash,
        applied_hash: initial.applied_hash,
        old_saved: None,
        old_applied: Some(-1),
        written: 50,
        pending: Some(PendingWrite::Applied(Some(50))),
    };
    Journal {
        version: 1,
        records: BTreeMap::from([("ethernet-profile".into(), record)]),
    }
    .save(&path)
    .unwrap();
    let view = inspect(&mut backend, &path, "eth0").await.unwrap();
    assert!(!view.can_restore);
    assert!(restore(&mut backend, &path, &view.approval).await.is_err());
    assert_eq!(backend.writes, 0);
    backend
        .current
        .applied
        .get_mut("ipv4")
        .unwrap()
        .insert("route-metric".into(), 50i64.into());
    let view = inspect(&mut backend, &path, "eth0").await.unwrap();
    assert!(view.can_restore);
    restore(&mut backend, &path, &view.approval).await.unwrap();
    assert!(!backend.current.saved["ipv4"].contains_key("route-metric"));
    assert_eq!(
        i64::try_from(&backend.current.applied["ipv4"]["route-metric"]).unwrap(),
        -1
    );
}
