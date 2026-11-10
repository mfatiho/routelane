use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

use routelane::domain::LaneId;
use routelane::usage::calendar::{local_date, local_midnight, period_start, LocalDate};
use routelane::usage::quota::{LaneQuota, QuotaSettings, QUOTA_SETTINGS_VERSION};
use routelane::usage::{usage_settings_path, usage_state_dir};

// libc does not expose tzset on Linux; bind the C function in this test binary.
mod libc {
    extern "C" {
        pub fn tzset();
    }
}

#[test]
fn local_midnight_round_trips_and_settings_file_is_private() {
    std::env::set_var("TZ", "Europe/Istanbul");
    unsafe {
        libc::tzset();
    }
    let date = LocalDate {
        year: 2026,
        month: 11,
        day: 10,
    };
    assert_eq!(local_midnight(date), 1_794_258_000);
    assert_eq!(local_date(1_794_258_000), date);
    assert_eq!(
        local_date(1_794_257_999),
        LocalDate {
            year: 2026,
            month: 11,
            day: 9
        }
    );
    // Exercise a 23-hour DST day and its single local reset boundary too.
    std::env::set_var("TZ", "Europe/Berlin");
    unsafe {
        libc::tzset();
    }
    let dst = LocalDate {
        year: 2026,
        month: 3,
        day: 29,
    };
    let following = LocalDate { day: 30, ..dst };
    assert_eq!(local_midnight(following) - local_midnight(dst), 23 * 3600);
    assert_eq!(local_date(local_midnight(dst)), dst);
    assert_eq!(
        period_start(local_date(local_midnight(dst) - 1), 29),
        LocalDate {
            year: 2026,
            month: 2,
            day: 28
        }
    );
    assert_eq!(period_start(local_date(local_midnight(dst)), 29), dst);
    assert_eq!(
        period_start(local_date(local_midnight(following) - 1), 29),
        dst
    );
    std::env::set_var("TZ", "Europe/Istanbul");
    unsafe {
        libc::tzset();
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private/usage-settings.json");
    assert_eq!(QuotaSettings::load(&path).unwrap().lanes.len(), 0);
    let settings = QuotaSettings {
        version: QUOTA_SETTINGS_VERSION,
        lanes: BTreeMap::from([(
            LaneId::from("vinn"),
            LaneQuota {
                enabled: true,
                limit_bytes: 20_000_000_000,
                reset_day: 1,
                warn_percent: 80,
            },
        )]),
    };
    settings.save(&path).unwrap();
    settings.save(&path).unwrap();
    assert_eq!(QuotaSettings::load(&path).unwrap(), settings);
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
    for invalid in [
        r#"{"version":2,"lanes":{}}"#,
        r#"{"version":1,"lanes":{},"extra":1}"#,
        r#"{"version":1,"lanes":{"vinn":{"enabled":true,"limit_bytes":20000000000,"reset_day":1,"warn_percent":80,"extra":1}}}"#,
        r#"{"version":1,"lanes":{"vinn":{"enabled":true,"limit_bytes":0,"reset_day":1,"warn_percent":80}}}"#,
    ] {
        std::fs::write(&path, invalid).unwrap();
        assert!(QuotaSettings::load(&path).is_err());
    }
    let mut invalid = settings.clone();
    invalid.version = 2;
    assert!(invalid.save(&path).is_err());
    invalid = settings;
    invalid
        .lanes
        .get_mut(&LaneId::from("vinn"))
        .unwrap()
        .reset_day = 0;
    assert!(invalid.save(&path).is_err());

    // Environment mutation is confined to this single-test process.
    std::env::set_var("HOME", dir.path());
    std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config"));
    std::env::set_var("XDG_STATE_HOME", dir.path().join("state"));
    assert_eq!(
        usage_settings_path().unwrap(),
        dir.path().join("config/routelane/usage-settings.json")
    );
    assert_eq!(
        usage_state_dir().unwrap(),
        dir.path().join("state/routelane")
    );
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("XDG_STATE_HOME");
    assert_eq!(
        usage_settings_path().unwrap(),
        dir.path().join(".config/routelane/usage-settings.json")
    );
    assert_eq!(
        usage_state_dir().unwrap(),
        dir.path().join(".local/state/routelane")
    );
    std::env::remove_var("HOME");
    assert!(usage_settings_path().is_err());
    assert!(usage_state_dir().is_err());
}
