use std::os::unix::fs::PermissionsExt;
use std::time::{SystemTime, UNIX_EPOCH};

use routelane::domain::{LaneId, RuleId};
use routelane::usage::events::{
    EventCategory, EventKind, EventStore, UsageEvent, MAX_EVENTS, RETENTION_DAYS,
};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
fn event(at_unix_ms: i64) -> UsageEvent {
    UsageEvent {
        at_unix_ms,
        kind: EventKind::ProfileDeactivated,
    }
}
fn kinds() -> Vec<(EventKind, EventCategory)> {
    let lane = LaneId::from("vinn");
    vec![
        (
            EventKind::ProfileApplied {
                profile: "Default".into(),
            },
            EventCategory::Transitions,
        ),
        (EventKind::ProfileDeactivated, EventCategory::Transitions),
        (
            EventKind::ApplyFailed {
                code: "APPLY_FAILED".into(),
                message: "Apply failed".into(),
                rolled_back: true,
            },
            EventCategory::Problems,
        ),
        (
            EventKind::RuleApplied {
                rule_id: RuleId::from("browser"),
            },
            EventCategory::Rules,
        ),
        (
            EventKind::Failover {
                from: Some(LaneId::from("ethernet")),
                to: lane.clone(),
                families: vec!["IPv4".into(), "IPv6".into()],
            },
            EventCategory::Transitions,
        ),
        (
            EventKind::Failback {
                to: LaneId::from("ethernet"),
                backup_seconds: 420,
                backup_bytes: Some(410_000_000),
            },
            EventCategory::Transitions,
        ),
        (
            EventKind::RuntimeAttention {
                phase: "Degraded".into(),
                message: Some("Needs attention".into()),
            },
            EventCategory::Problems,
        ),
        (
            EventKind::QuotaWarning {
                lane_id: lane.clone(),
                used: 16_000_000_000,
                limit: 20_000_000_000,
            },
            EventCategory::Quota,
        ),
        (
            EventKind::QuotaFull {
                lane_id: lane,
                used: 20_000_000_000,
                limit: 20_000_000_000,
            },
            EventCategory::Quota,
        ),
        (
            EventKind::StoreRecovered {
                file: "usage-v1.json".into(),
            },
            EventCategory::Problems,
        ),
    ]
}

#[test]
fn events_are_kept_for_30_days_and_at_most_5000() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private/events-v1.jsonl");
    let (mut store, note) = EventStore::open(&path);
    assert!(note.is_none());
    let now = now();
    for i in 0..5_001 {
        store.append(event(now + i)).unwrap();
    }
    let events = store.list(EventCategory::All, now + 5_000);
    assert_eq!(events.len(), 5_000);
    assert_eq!(events.first().unwrap().at_unix_ms, now + 5_000);
    assert_eq!(events.last().unwrap().at_unix_ms, now + 1);
    assert_eq!(events.len(), MAX_EVENTS);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let (store, note) = EventStore::open(&path);
    assert!(note.is_none());
    assert_eq!(store.list(EventCategory::All, now + 5_000), events);
    // Opening drops expired records on disk, and append rejects a backdated event.
    let day_ms = 86_400_000;
    let boundary = now - 30 * day_ms;
    let mut store = store;
    store.clear().unwrap();
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            serde_json::to_string(&event(now - 31 * day_ms)).unwrap(),
            serde_json::to_string(&event(now)).unwrap()
        ),
    )
    .unwrap();
    let (mut store, note) = EventStore::open(&path);
    assert!(note.is_none());
    assert_eq!(store.list(EventCategory::All, now), vec![event(now)]);
    store.append(event(now - 31 * day_ms)).unwrap();
    let (store, _) = EventStore::open(&path);
    assert_eq!(store.list(EventCategory::All, now), vec![event(now)]);
    assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
    assert_eq!(
        store.list(EventCategory::All, now + 30 * day_ms),
        vec![event(now)]
    );
    assert!(store
        .list(EventCategory::All, now + 30 * day_ms + 1)
        .is_empty());
    assert_eq!(RETENTION_DAYS, 30);
    assert!(boundary < now);
}

#[test]
fn categories_filter_and_clear_removes_everything() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let (mut store, _) = EventStore::open(&path);
    let now = now();
    for (i, (kind, category)) in kinds().into_iter().enumerate() {
        assert_eq!(kind.category(), category);
        store
            .append(UsageEvent {
                at_unix_ms: now + i as i64,
                kind,
            })
            .unwrap();
    }
    let quota = store.list(EventCategory::Quota, now);
    assert_eq!(quota.len(), 2);
    assert!(matches!(quota[0].kind, EventKind::QuotaFull { .. }));
    assert!(matches!(quota[1].kind, EventKind::QuotaWarning { .. }));
    assert_eq!(store.list(EventCategory::Problems, now).len(), 3);
    assert_eq!(store.list(EventCategory::Transitions, now).len(), 4);
    assert_eq!(store.list(EventCategory::Rules, now).len(), 1);
    assert_eq!(store.list(EventCategory::All, now).len(), 10);
    store.clear().unwrap();
    assert!(store.list(EventCategory::All, now).is_empty());
    assert!(std::fs::read(&path).unwrap().is_empty());
    assert!(EventStore::open(&path)
        .0
        .list(EventCategory::All, now)
        .is_empty());
}

#[test]
fn a_corrupt_line_is_skipped_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let now = now();
    let bytes = format!(
        "{}\nbozuk\n{}\n",
        serde_json::to_string(&event(now)).unwrap(),
        serde_json::to_string(&event(now + 1)).unwrap()
    );
    std::fs::write(&path, &bytes).unwrap();
    let (store, note) = EventStore::open(&path);
    assert!(note.unwrap().contains("1 corrupt"));
    assert_eq!(
        store.list(EventCategory::All, now),
        vec![event(now + 1), event(now)]
    );
    assert!(std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with("events.jsonl.corrupt-")
            && std::fs::read(entry.path()).unwrap() == bytes.as_bytes()));
    let (store, note) = EventStore::open(&path);
    assert!(note.is_none());
    assert_eq!(store.list(EventCategory::All, now).len(), 2);
    std::fs::write(&path, [0xff, b'\n']).unwrap();
    let (store, note) = EventStore::open(&path);
    assert!(note.is_some());
    assert!(store.list(EventCategory::All, now).is_empty());
}

#[test]
fn event_file_holds_no_connection_addresses() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let (mut store, _) = EventStore::open(&path);
    let now = now();
    for (kind, _) in kinds() {
        let record = UsageEvent {
            at_unix_ms: now,
            kind,
        };
        let json = serde_json::to_value(&record).unwrap();
        assert!(json["kind"]["kind"].is_string());
        assert_eq!(serde_json::from_value::<UsageEvent>(json).unwrap(), record);
        store.append(record).unwrap();
    }
    let bytes = std::fs::read_to_string(path).unwrap();
    for line in bytes.lines() {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        let fields = value["kind"].as_object().unwrap();
        for forbidden in [
            "address",
            "source",
            "destination",
            "src_ip",
            "dst_ip",
            "port",
            "connection",
        ] {
            assert!(!fields.contains_key(forbidden));
        }
        for token in line.split(|c: char| !(c.is_ascii_hexdigit() || c == '.' || c == ':')) {
            assert!(
                token.parse::<std::net::IpAddr>().is_err(),
                "connection address in event: {token}"
            );
        }
    }
}
