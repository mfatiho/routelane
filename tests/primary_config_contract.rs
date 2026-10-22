use std::os::unix::fs::PermissionsExt;

use routelane::config::{ConfigDocument, ConfigRepository, ConfigV2};
use routelane::domain::{validate_profile, Profile};
use serde_json::{json, Value};

fn old_document() -> Value {
    json!({"schema_version":2,"language":"en","profiles":[{
        "id":"work","name":"Office","enabled":true,
        "lanes":[
            {"id":"ethernet","name":"Ethernet","interface":"eth0","fallback_lane":null,"failure_policy":"Block"},
            {"id":"vinn","name":"VINN","interface":"usb0","fallback_lane":null,"failure_policy":"Direct"}
        ],
        "rules":[{
            "id":"browser","enabled":true,"priority":12,
            "source":{"Application":{"desktop_id":"firefox.desktop","executable_path":"/usr/bin/firefox","include_children":true,
                "launch":{"executable":"/usr/bin/firefox","arguments":["--new-window","a b","--profile=/tmp/test"]}}},
            "target":"Any","action":{"Route":"vinn"}
        }]
    }]})
}

fn policy(primary: Value, backups: Value) -> Value {
    json!({"primary":primary,"backups":backups,"automatic_failover":true})
}

fn current_policy(primary: Value, backups: Value) -> Value {
    let mut policy = policy(primary, backups);
    policy["unmatched_traffic"] = json!("PreserveSystem");
    policy
}

#[test]
fn v2_migration_preserves_ids_language_and_launcher_arguments() {
    let old = old_document();
    let current = ConfigDocument::from_json(&old.to_string())
        .unwrap()
        .into_current()
        .unwrap();
    let current = serde_json::to_value(current).unwrap();
    assert_eq!(current["schema_version"], 4);
    assert_eq!(current["language"], "en");
    assert_eq!(current["profiles"][0]["enabled"], false);
    for key in ["id", "name", "lanes", "rules"] {
        assert_eq!(current["profiles"][0][key], old["profiles"][0][key]);
    }
    assert_eq!(
        current["profiles"][0]["routing"],
        current_policy(json!("FollowSystemDefault"), json!([]))
    );
    assert_eq!(current["routing_notice_pending"], true);
}

#[test]
fn legacy_migration_never_activates() {
    let current =
        ConfigDocument::from_json(r#"{"language":"tr","alt_interface":"usb0","rules":[]}"#)
            .unwrap()
            .into_current()
            .unwrap();
    assert!(current.profiles.iter().all(|profile| !profile.enabled));
    let stored = serde_json::to_value(current).unwrap();
    assert_eq!(stored["schema_version"], 4);
    assert_eq!(
        stored["profiles"][0]["routing"]["primary"],
        "FollowSystemDefault"
    );
}

#[test]
fn invalid_primary_and_duplicate_backups_are_rejected() {
    for routing in [
        policy(json!({"Lane":"missing"}), json!([])),
        policy(json!({"Lane":"ethernet"}), json!(["vinn", "vinn"])),
        policy(json!({"Lane":"ethernet"}), json!(["ethernet"])),
        policy(json!("FollowSystemDefault"), json!(["missing"])),
    ] {
        let mut value = old_document()["profiles"][0].clone();
        value["routing"] = routing.clone();
        value["routing"]["unmatched_traffic"] = json!("PreserveSystem");
        let profile: Profile = serde_json::from_value(value).unwrap();
        assert!(validate_profile(&profile).is_err(), "accepted {routing}");
    }
}

#[test]
fn v3_cannot_be_read_as_v2() {
    let mut document = old_document();
    document["schema_version"] = json!(3);
    document["profiles"][0]["routing"] = policy(json!({"Lane":"ethernet"}), json!(["vinn"]));
    assert!(serde_json::from_value::<ConfigV2>(document.clone()).is_err());
    let current = ConfigDocument::from_json(&document.to_string())
        .unwrap()
        .into_current()
        .unwrap();
    let current = serde_json::to_value(current).unwrap();
    assert_eq!(
        current["profiles"][0]["routing"]["primary"],
        json!({"Lane":"ethernet"})
    );
}

#[test]
fn save_failure_preserves_original_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let bytes = old_document().to_string();
    std::fs::write(&path, &bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let repository = ConfigRepository::new(&path);
    let mut document = repository.load().unwrap();
    let mut value = serde_json::to_value(&document.profiles[0]).unwrap();
    value["routing"] = current_policy(json!({"Lane":"missing"}), json!([]));
    document.profiles[0] = serde_json::from_value(value).unwrap();
    assert!(repository.save(&document).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
    assert!(!path.with_extension("json.bak").exists());
}

#[test]
fn v2_with_new_routing_semantics_is_not_silently_migrated() {
    let mut old = old_document();
    old["profiles"][0]["routing"] = policy(json!({"Lane":"ethernet"}), json!([]));
    assert!(ConfigDocument::from_json(&old.to_string()).is_err());
}

#[test]
fn daemon_saved_profile_migration_is_read_only() {
    use routelane::daemon::{AtomicJsonConfigRepository, ConfigRepository as DaemonRepository};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("desired-profile.json");
    let old = old_document()["profiles"][0].to_string();
    std::fs::write(&path, &old).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let repository = AtomicJsonConfigRepository::new(path.clone());
    let migrated = repository.load().unwrap().unwrap();
    assert_eq!(migrated.id.as_str(), "work");
    assert!(!migrated.enabled);
    assert_eq!(
        serde_json::to_value(&migrated).unwrap()["routing"]["primary"],
        "FollowSystemDefault"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
    repository.save(Some(&migrated)).unwrap();
    let current: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(current["schema_version"], 4);
    assert_eq!(current["profile"]["id"], "work");
    assert_eq!(repository.load().unwrap(), Some(migrated));
}
