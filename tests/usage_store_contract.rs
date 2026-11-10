use std::os::unix::fs::PermissionsExt;

use routelane::domain::LaneId;
use routelane::usage::counters::{
    counter_delta, InterfaceCounters, InterfaceSample, SysfsCounters,
};
use routelane::usage::quota::LaneQuota;
use routelane::usage::store::{QuotaCrossing, UsageStore};
use routelane::usage::{LocalDate, UsageTotals};

fn date(day: u8) -> LocalDate {
    LocalDate {
        year: 2026,
        month: 11,
        day,
    }
}
fn sample(ifindex: u32, rx_bytes: u64, tx_bytes: u64) -> InterfaceSample {
    InterfaceSample {
        ifindex,
        rx_bytes,
        tx_bytes,
    }
}

#[test]
fn deltas_survive_restart_and_never_go_negative() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private/usage-v1.json");
    let lane = LaneId::from("vinn");
    let (mut store, note) = UsageStore::open(&path, "boot-a");
    assert!(note.is_none());
    assert_eq!(
        store.record_sample(&lane, date(10), sample(3, 1000, 100)),
        UsageTotals::default()
    );
    assert_eq!(
        counter_delta(None, &sample(3, 1000, 100)),
        UsageTotals::default()
    );
    assert_eq!(
        store.record_sample(&lane, date(10), sample(3, 1500, 150)),
        UsageTotals { rx: 500, tx: 50 }
    );
    store.save().unwrap();
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
    let (mut store, note) = UsageStore::open(&path, "boot-a");
    assert!(note.is_none());
    assert_eq!(
        store.record_sample(&lane, date(10), sample(3, 1700, 170)),
        UsageTotals { rx: 200, tx: 20 }
    );
    assert_eq!(store.day(&lane, date(10)), UsageTotals { rx: 700, tx: 70 });
    store.save().unwrap();
    let (mut store, note) = UsageStore::open(&path, "boot-b");
    assert!(note.is_none());
    assert_eq!(
        store.record_sample(&lane, date(10), sample(3, 300, 30)),
        UsageTotals { rx: 300, tx: 30 }
    );
    assert_eq!(
        store.day(&lane, date(10)),
        UsageTotals { rx: 1000, tx: 100 }
    );
    assert_eq!(
        store.record_sample(&lane, date(10), sample(9, 80, 8)),
        UsageTotals { rx: 80, tx: 8 }
    );
    assert_eq!(
        store.record_sample(&lane, date(10), sample(9, 50, 10)),
        UsageTotals { rx: 50, tx: 10 }
    );
    assert_eq!(
        store.record_sample(&lane, date(10), sample(9, 60, 2)),
        UsageTotals { rx: 60, tx: 2 }
    );
    assert_eq!(
        store.day(&lane, date(10)),
        UsageTotals { rx: 1190, tx: 120 }
    );
    store.record_sample(&lane, date(11), sample(9, 70, 3));
    assert_eq!(
        store.period_measured(&lane, date(1), date(11)),
        UsageTotals { rx: 1200, tx: 121 }
    );
    assert_eq!(store.lifetime_bytes(&lane), 1321);
    assert_eq!(
        store.day(&LaneId::from("other"), date(10)),
        UsageTotals::default()
    );
    assert_eq!(
        store.period_measured(&lane, date(11), date(10)),
        UsageTotals::default()
    );
    store.save().unwrap();
    // A reboot marker must survive a save before the first sample too.
    let (store, _) = UsageStore::open(&path, "boot-c");
    store.save().unwrap();
    let (mut store, _) = UsageStore::open(&path, "boot-c");
    assert_eq!(
        store.record_sample(&lane, date(11), sample(9, 10, 1)),
        UsageTotals { rx: 10, tx: 1 }
    );
}

#[test]
fn quota_usage_correction_and_thresholds_fire_once_per_period() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage-v1.json");
    let lane = LaneId::from("vinn");
    let quota = LaneQuota {
        enabled: true,
        limit_bytes: 20_000_000_000,
        reset_day: 1,
        warn_percent: 80,
    };
    let (mut store, _) = UsageStore::open(&path, "boot");
    store.record_sample(&lane, date(1), sample(3, 0, 0));
    store.record_sample(&lane, date(9), sample(3, 11_100_000_000, 1_300_000_000));
    assert_eq!(store.used_bytes(&lane, &quota, date(9)), 12_400_000_000);
    assert!(store.crossings(&lane, &quota, date(9)).is_empty());
    store.set_used(&lane, &quota, date(9), 16_000_000_000);
    assert_eq!(
        store.crossings(&lane, &quota, date(9)),
        vec![QuotaCrossing::Warning {
            used: 16_000_000_000,
            limit: 20_000_000_000
        }]
    );
    assert!(store.crossings(&lane, &quota, date(9)).is_empty());
    store.save().unwrap();
    let (mut store, _) = UsageStore::open(&path, "boot");
    assert_eq!(store.used_bytes(&lane, &quota, date(9)), 16_000_000_000);
    store.set_used(&lane, &quota, date(9), 15_000_000_000);
    assert!(store.crossings(&lane, &quota, date(9)).is_empty());
    store.set_used(&lane, &quota, date(9), 17_000_000_000);
    assert!(store.crossings(&lane, &quota, date(9)).is_empty());
    store.record_sample(&lane, date(10), sample(3, 15_100_000_000, 1_300_000_000));
    assert_eq!(
        store.crossings(&lane, &quota, date(10)),
        vec![QuotaCrossing::Full {
            used: 21_000_000_000,
            limit: 20_000_000_000
        }]
    );
    store.save().unwrap();
    let (mut store, _) = UsageStore::open(&path, "boot");
    assert!(store.crossings(&lane, &quota, date(10)).is_empty());
    let december = LocalDate {
        year: 2026,
        month: 12,
        day: 1,
    };
    store.record_sample(&lane, december, sample(3, 16_100_000_000, 1_300_000_000));
    assert_eq!(store.used_bytes(&lane, &quota, december), 1_000_000_000);
    assert!(store.crossings(&lane, &quota, december).is_empty());
    store.set_used(&lane, &quota, december, 20_000_000_000);
    assert_eq!(
        store.crossings(&lane, &quota, december),
        vec![
            QuotaCrossing::Warning {
                used: 20_000_000_000,
                limit: 20_000_000_000
            },
            QuotaCrossing::Full {
                used: 20_000_000_000,
                limit: 20_000_000_000
            },
        ]
    );
    store.set_used(&lane, &quota, december, 0);
    assert_eq!(store.used_bytes(&lane, &quota, december), 0);
    assert!(store.crossings(&lane, &quota, december).is_empty());
    let disabled = LaneQuota {
        enabled: false,
        ..quota
    };
    let other = LaneId::from("disabled");
    store.set_used(&other, &disabled, december, 25_000_000_000);
    assert!(store.crossings(&other, &disabled, december).is_empty());
}

#[test]
fn corrupt_state_is_preserved_and_replaced_with_an_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage-v1.json");
    for corrupt in [
        "{bozuk",
        r#"{"version":2,"lanes":{}}"#,
        r#"{"version":1,"lanes":{"vinn":{"daily":{"2027-02-29":{"rx":1,"tx":2}},"last_sample":null,"boot_id":"boot","correction":null,"fired":null}}}"#,
    ] {
        std::fs::write(&path, corrupt).unwrap();
        let (store, note) = UsageStore::open(&path, "boot");
        assert!(note.is_some());
        assert_eq!(store.lifetime_bytes(&LaneId::from("vinn")), 0);
        let preserved = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("usage-v1.json.corrupt-")
                    && std::fs::read_to_string(entry.path()).unwrap() == corrupt
            });
        assert!(preserved);
        store.save().unwrap();
        assert!(UsageStore::open(&path, "boot").1.is_none());
    }
}

#[test]
fn sysfs_counters_read_only_safe_interface_names() {
    let dir = tempfile::tempdir().unwrap();
    let interface = dir.path().join("wwan0");
    std::fs::create_dir_all(interface.join("statistics")).unwrap();
    std::fs::write(interface.join("ifindex"), "3\n").unwrap();
    std::fs::write(interface.join("statistics/rx_bytes"), "1234\n").unwrap();
    std::fs::write(interface.join("statistics/tx_bytes"), "123\n").unwrap();
    let counters = SysfsCounters {
        root: dir.path().to_owned(),
    };
    assert_eq!(counters.sample("wwan0"), Some(sample(3, 1234, 123)));
    for bad in [
        "../etc",
        "a/b",
        "abcdefghijklmnop",
        "",
        "é",
        "a b",
        "a\nb",
        ".",
        "..",
    ] {
        assert_eq!(counters.sample(bad), None, "{bad}");
    }
    assert_eq!(counters.sample("missing"), None);
    std::fs::write(interface.join("statistics/tx_bytes"), "invalid").unwrap();
    assert_eq!(counters.sample("wwan0"), None);
    assert_eq!(
        std::fs::read_to_string(interface.join("statistics/rx_bytes")).unwrap(),
        "1234\n"
    );
}

#[test]
fn daily_history_retains_400_calendar_days() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = UsageStore::open(dir.path().join("usage.json"), "boot");
    let lane = LaneId::from("vinn");
    store.record_sample(
        &lane,
        LocalDate {
            year: 2025,
            month: 10,
            day: 6,
        },
        sample(3, 0, 0),
    );
    store.record_sample(
        &lane,
        LocalDate {
            year: 2025,
            month: 10,
            day: 6,
        },
        sample(3, 100, 10),
    );
    store.record_sample(
        &lane,
        LocalDate {
            year: 2025,
            month: 10,
            day: 7,
        },
        sample(3, 200, 20),
    );
    store.record_sample(&lane, date(10), sample(3, 300, 30));
    assert_eq!(
        store.day(
            &lane,
            LocalDate {
                year: 2025,
                month: 10,
                day: 6
            }
        ),
        UsageTotals::default()
    );
    assert_eq!(
        store.day(
            &lane,
            LocalDate {
                year: 2025,
                month: 10,
                day: 7
            }
        ),
        UsageTotals { rx: 100, tx: 10 }
    );
}

#[test]
fn idle_samples_do_not_dirty_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.json");
    let lane = LaneId::from("vinn");
    let (mut store, _) = UsageStore::open(&path, "boot");
    assert!(!store.is_dirty());
    store.record_sample(&lane, date(10), sample(3, 100, 10));
    assert!(store.is_dirty(), "the first baseline must be persisted");
    store.save().unwrap();
    assert!(!store.is_dirty());
    for day in [10, 10, 11] {
        store.record_sample(&lane, date(day), sample(3, 100, 10));
        assert!(
            !store.is_dirty(),
            "idle samples, including midnight, stay clean"
        );
    }
    store.record_sample(&lane, date(11), sample(3, 101, 10));
    assert!(store.is_dirty());
    store.save().unwrap();
    store.record_sample(&lane, date(11), sample(4, 0, 0));
    assert!(
        store.is_dirty(),
        "an ifindex change with zero counters is still a new baseline"
    );
    store.save().unwrap();
    let (mut store, _) = UsageStore::open(&path, "new-boot");
    store.record_sample(&lane, date(11), sample(4, 0, 0));
    assert!(
        store.is_dirty(),
        "a boot change with zero counters must be persisted"
    );
}

#[test]
fn corrections_and_changed_crossing_state_dirty_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = UsageStore::open(dir.path().join("usage.json"), "boot");
    let lane = LaneId::from("vinn");
    let quota = LaneQuota {
        enabled: true,
        limit_bytes: 100_000_000,
        reset_day: 1,
        warn_percent: 80,
    };
    assert!(store.crossings(&lane, &quota, date(10)).is_empty());
    assert!(
        store.is_dirty(),
        "initializing the period changes crossing state"
    );
    store.save().unwrap();
    assert!(store.crossings(&lane, &quota, date(10)).is_empty());
    assert!(!store.is_dirty());
    store.set_used(&lane, &quota, date(10), 80_000_000);
    assert!(store.is_dirty());
    store.save().unwrap();
    assert!(matches!(
        store.crossings(&lane, &quota, date(10)).as_slice(),
        [QuotaCrossing::Warning { .. }]
    ));
    assert!(store.is_dirty());
    store.save().unwrap();
    assert!(store.crossings(&lane, &quota, date(11)).is_empty());
    assert!(!store.is_dirty());
    let december = LocalDate::parse("2026-12-01").unwrap();
    assert!(store.crossings(&lane, &quota, december).is_empty());
    assert!(
        store.is_dirty(),
        "rolling the period changes crossing state"
    );
}
