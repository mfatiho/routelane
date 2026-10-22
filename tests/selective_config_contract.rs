use std::os::unix::fs::PermissionsExt;

use routelane::config::{ConfigDocument, ConfigRepository};
use routelane::daemon::{AtomicJsonConfigRepository, ConfigRepository as DesiredRepository};
use serde_json::{json, Value};

fn v3() -> Value {
    json!({"schema_version":3,"language":"en","profiles":[{
        "id":"office","name":"Office","enabled":true,
        "lanes":[
            {"id":"ethernet","name":"Ethernet","interface":"eth0","fallback_lane":null,"failure_policy":"Block"},
            {"id":"vinn","name":"VINN","interface":"usb0","fallback_lane":null,"failure_policy":"Inactive"}
        ],
        "rules":[{"id":"browser","enabled":true,"priority":12,
            "source":{"Application":{"desktop_id":"firefox.desktop","executable_path":"/usr/bin/firefox","include_children":true,
                "launch":{"executable":"/usr/bin/firefox","arguments":["--new-window","a b"]}}},
            "target":"Any","action":{"Route":"vinn"}}],
        "routing":{"primary":{"Lane":"ethernet"},"backups":["vinn"],"automatic_failover":true}
    }]})
}

#[test]
fn v3_migration_preserves_rules_and_primary_but_proposes_system_routing() {
    let old = v3();
    let migrated = ConfigDocument::from_json(&old.to_string())
        .unwrap()
        .into_current()
        .unwrap();
    let value = serde_json::to_value(migrated).unwrap();
    assert_eq!(value["schema_version"], 4);
    assert_eq!(value["routing_notice_pending"], true);
    assert_eq!(value["language"], "en");
    assert_eq!(value["profiles"][0]["enabled"], false);
    for field in ["id", "name", "lanes", "rules"] {
        assert_eq!(value["profiles"][0][field], old["profiles"][0][field]);
    }
    for field in ["primary", "backups", "automatic_failover"] {
        assert_eq!(
            value["profiles"][0]["routing"][field],
            old["profiles"][0]["routing"][field]
        );
    }
    assert_eq!(
        value["profiles"][0]["routing"]["unmatched_traffic"],
        "PreserveSystem"
    );
}

#[test]
fn v4_requires_explicit_unmatched_mode() {
    let mut value = v3();
    value["schema_version"] = json!(4);
    assert!(ConfigDocument::from_json(&value.to_string()).is_err());
    for mode in ["PreserveSystem", "UsePrimary"] {
        value["profiles"][0]["routing"]["unmatched_traffic"] = json!(mode);
        let current = ConfigDocument::from_json(&value.to_string())
            .unwrap()
            .into_current()
            .unwrap();
        assert_eq!(
            serde_json::to_value(current).unwrap()["profiles"][0]["routing"]["unmatched_traffic"],
            mode
        );
    }
    value["profiles"][0]["routing"]["unmatched_traffic"] = json!("Unrecognized");
    assert!(ConfigDocument::from_json(&value.to_string()).is_err());
    value["schema_version"] = json!(5);
    assert!(ConfigDocument::from_json(&value.to_string()).is_err());
}

#[test]
fn desired_schema4_round_trip() {
    let current = ConfigDocument::from_json(&v3().to_string())
        .unwrap()
        .into_current()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("desired.json");
    let repository = AtomicJsonConfigRepository::new(path.clone());
    repository.save(Some(&current.profiles[0])).unwrap();
    let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["schema_version"], 4);
    assert_eq!(
        saved["profile"]["routing"]["unmatched_traffic"],
        "PreserveSystem"
    );
    assert_eq!(
        repository.load().unwrap(),
        Some(current.profiles[0].clone())
    );
}

#[test]
fn migration_load_never_saves_or_activates() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let old = v3().to_string();
    std::fs::write(&path, &old).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let migrated = ConfigRepository::new(&path).load().unwrap();
    assert_eq!(migrated.schema_version, 4);
    assert!(migrated.profiles.iter().all(|profile| !profile.enabled));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), old);

    let desired_path = directory.path().join("desired.json");
    let desired = json!({"schema_version":3,"profile":v3()["profiles"][0]}).to_string();
    std::fs::write(&desired_path, &desired).unwrap();
    std::fs::set_permissions(&desired_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let profile = AtomicJsonConfigRepository::new(desired_path.clone())
        .load()
        .unwrap()
        .unwrap();
    assert!(!profile.enabled);
    assert_eq!(
        serde_json::to_value(profile).unwrap()["routing"]["unmatched_traffic"],
        "PreserveSystem"
    );
    assert_eq!(std::fs::read_to_string(desired_path).unwrap(), desired);
}
