use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use routelane::domain::{
    InterfaceSelector, Lane, LaneFailurePolicy, LaneId, Profile, ProfileId, Rule, RuleAction,
    RuleId, RuleSource, RuleTarget,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    En,
    #[default]
    Tr,
}

impl Language {
    pub fn from_index(index: u32) -> Self {
        match index {
            1 => Self::En,
            _ => Self::Tr,
        }
    }

    pub fn index(self) -> u32 {
        match self {
            Self::Tr => 0,
            Self::En => 1,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct PersistedState {
    #[serde(default)]
    pub language: Language,
    #[serde(default)]
    pub alt_interface: Option<String>,
    #[serde(default)]
    pub rules: Vec<PersistedRule>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PersistedRule {
    pub target_str: String,
    pub is_domain: bool,
    pub interface: String,
}

pub const CURRENT_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigV2 {
    pub schema_version: u32,
    #[serde(default)]
    pub language: Language,
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

#[derive(Debug)]
pub enum ConfigDocument {
    V2(ConfigV2),
    Legacy(PersistedState),
}

impl ConfigDocument {
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        let value: serde_json::Value =
            serde_json::from_str(json).context("configuration is not valid JSON")?;
        match value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
        {
            Some(version) if version == u64::from(CURRENT_SCHEMA_VERSION) => {
                let current =
                    serde_json::from_value(value).context("version 2 configuration is invalid")?;
                Ok(Self::V2(current))
            }
            Some(version) => bail!("unsupported configuration schema version {version}"),
            None => serde_json::from_value(value)
                .map(Self::Legacy)
                .context("legacy configuration is invalid"),
        }
    }

    pub fn into_current(self) -> anyhow::Result<ConfigV2> {
        let mut current = match self {
            Self::V2(current) => current,
            Self::Legacy(legacy) => migrate_legacy(legacy)?,
        };
        current.schema_version = CURRENT_SCHEMA_VERSION;
        for profile in &mut current.profiles {
            profile.enabled = false;
        }
        Ok(current)
    }
}

impl Default for ConfigV2 {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            language: Language::default(),
            profiles: Vec::new(),
        }
    }
}

fn migrate_legacy(legacy: PersistedState) -> anyhow::Result<ConfigV2> {
    let mut interfaces = Vec::new();
    if let Some(interface) = legacy
        .alt_interface
        .as_deref()
        .filter(|interface| !interface.trim().is_empty())
    {
        interfaces.push(interface.to_owned());
    }
    for rule in &legacy.rules {
        if !rule.interface.trim().is_empty() && !interfaces.contains(&rule.interface) {
            interfaces.push(rule.interface.clone());
        }
    }

    let mut lane_by_interface = BTreeMap::new();
    let lanes = interfaces
        .into_iter()
        .enumerate()
        .map(|(index, interface)| {
            let lane_id = if index == 0 {
                LaneId::from("default")
            } else {
                LaneId::from(format!("legacy-{}", index + 1).as_str())
            };
            lane_by_interface.insert(interface.clone(), lane_id.clone());
            Lane {
                id: lane_id,
                name: if index == 0 {
                    "Default".to_owned()
                } else {
                    format!("Legacy {}", index + 1)
                },
                interface: InterfaceSelector::from(interface.as_str()),
                fallback_lane: None,
                failure_policy: LaneFailurePolicy::Inactive,
            }
        })
        .collect::<Vec<_>>();

    let rules = legacy
        .rules
        .into_iter()
        .enumerate()
        .map(|(index, legacy_rule)| {
            let lane_id = lane_by_interface
                .get(&legacy_rule.interface)
                .cloned()
                .with_context(|| {
                    format!(
                        "legacy rule '{}' has no usable interface",
                        legacy_rule.target_str
                    )
                })?;
            let target = legacy_target(&legacy_rule)?;
            Ok(Rule {
                id: RuleId::from(format!("legacy-{}", index + 1).as_str()),
                enabled: true,
                priority: u32::try_from(index + 1).unwrap_or(u32::MAX),
                source: RuleSource::Any,
                target,
                action: RuleAction::Route(lane_id),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let profiles = if lanes.is_empty() && rules.is_empty() {
        Vec::new()
    } else {
        vec![Profile {
            id: ProfileId::from("default"),
            name: "Default".to_owned(),
            enabled: false,
            lanes,
            rules,
        }]
    };

    Ok(ConfigV2 {
        schema_version: CURRENT_SCHEMA_VERSION,
        language: legacy.language,
        profiles,
    })
}

fn legacy_target(rule: &PersistedRule) -> anyhow::Result<RuleTarget> {
    if rule.is_domain {
        return Ok(RuleTarget::Domain(rule.target_str.clone()));
    }
    if let Ok(network) = rule.target_str.parse() {
        return Ok(RuleTarget::Cidr(network));
    }
    rule.target_str
        .parse()
        .map(RuleTarget::Ip)
        .with_context(|| format!("invalid legacy target '{}'", rule.target_str))
}

#[derive(Debug, Clone)]
pub struct ConfigRepository {
    path: PathBuf,
}

impl ConfigRepository {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn user_default() -> Self {
        Self::new(config_path())
    }

    pub fn load(&self) -> anyhow::Result<ConfigV2> {
        if !self.path.exists() {
            return Ok(ConfigV2::default());
        }
        validate_existing_file(&self.path)?;
        let mut json = String::new();
        open_existing_file(&self.path)?
            .read_to_string(&mut json)
            .with_context(|| format!("could not read {}", self.path.display()))?;
        ConfigDocument::from_json(&json)?.into_current()
    }

    pub fn save(&self, config: &ConfigV2) -> anyhow::Result<()> {
        let parent = self
            .path
            .parent()
            .context("configuration path has no parent directory")?;
        ensure_private_directory(parent)?;
        if self.path.exists() {
            validate_existing_file(&self.path)?;
        }

        let mut current = config.clone();
        current.schema_version = CURRENT_SCHEMA_VERSION;
        for profile in &mut current.profiles {
            profile.enabled = false;
        }
        let bytes = serde_json::to_vec_pretty(&current)?;
        let temp_path = parent.join(format!(
            ".{}.{}.tmp",
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("config"),
            uuid::Uuid::new_v4()
        ));
        write_private_file(&temp_path, &bytes)?;

        let backup = self.path.with_extension("json.bak");
        if self.path.exists() {
            let backup_temp = parent.join(format!(
                ".{}.{}.bak.tmp",
                self.path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("config"),
                uuid::Uuid::new_v4()
            ));
            copy_private_file(&self.path, &backup_temp)?;
            std::fs::rename(&backup_temp, &backup).with_context(|| {
                format!(
                    "could not atomically replace configuration backup {}",
                    backup.display()
                )
            })?;
        }

        std::fs::rename(&temp_path, &self.path).with_context(|| {
            format!(
                "could not atomically replace configuration {}",
                self.path.display()
            )
        })?;
        sync_directory(parent)?;
        Ok(())
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let mut file = options
        .open(path)
        .with_context(|| format!("could not safely create {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn copy_private_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let mut bytes = Vec::new();
    open_existing_file(source)?.read_to_end(&mut bytes)?;
    write_private_file(destination, &bytes)
}

fn open_existing_file(path: &Path) -> anyhow::Result<File> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .with_context(|| format!("could not safely open {}", path.display()))?;
    validate_metadata(&file.metadata()?)?;
    Ok(file)
}

fn ensure_private_directory(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(path)
        .with_context(|| format!("could not create {}", path.display()))?;
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("configuration directory is not a real directory");
    }
    #[cfg(unix)]
    {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn validate_existing_file(path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("configuration path is not a regular file");
    }
    validate_metadata(&metadata)
}

fn validate_metadata(metadata: &std::fs::Metadata) -> anyhow::Result<()> {
    if !metadata.is_file() {
        bail!("configuration path is not a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!("configuration is not owned by the current user");
        }
        if metadata.permissions().mode() & 0o022 != 0 {
            bail!("configuration permissions must not grant group or other write access");
        }
    }
    Ok(())
}

fn sync_directory(path: &Path) -> anyhow::Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
            PathBuf::from(home).join(".config")
        });
    base.join("routelane").join("config.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use routelane::domain::RuleAction;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn persisted_state_defaults_to_turkish_for_existing_configs() {
        let state: PersistedState = serde_json::from_str(r#"{"alt_interface":null,"rules":[]}"#)
            .expect("legacy config should deserialize");

        assert_eq!(state.language, Language::Tr);
    }

    #[test]
    fn migrates_legacy_rules_into_default_lane_profile() {
        let legacy = r#"{
          "language":"tr",
          "alt_interface":"tun0",
          "rules":[
            {"target_str":"203.0.113.0/24","is_domain":false,"interface":"tun0"}
          ]
        }"#;

        let migrated = ConfigDocument::from_json(legacy)
            .unwrap()
            .into_current()
            .unwrap();

        assert_eq!(migrated.schema_version, 2);
        assert_eq!(migrated.profiles[0].name, "Default");
        assert_eq!(migrated.profiles[0].lanes[0].interface.as_str(), "tun0");
        assert_eq!(
            migrated.profiles[0].rules[0].action,
            RuleAction::Route(migrated.profiles[0].lanes[0].id.clone())
        );
        assert!(!migrated.profiles[0].enabled);
    }

    #[test]
    fn current_profiles_always_load_inactive() {
        let json = r#"{
          "schema_version":2,
          "language":"en",
          "profiles":[{
            "id":"work","name":"Work","enabled":true,"lanes":[],"rules":[]
          }]
        }"#;

        let current = ConfigDocument::from_json(json)
            .unwrap()
            .into_current()
            .unwrap();

        assert!(!current.profiles[0].enabled);
    }

    #[test]
    fn migration_save_preserves_legacy_backup_and_uses_private_atomic_target() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let legacy = r#"{"language":"tr","alt_interface":"tun0","rules":[]}"#;
        std::fs::write(&path, legacy).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let repository = ConfigRepository::new(&path);
        let migrated = repository.load().unwrap();

        repository.save(&migrated).unwrap();

        let stored = ConfigDocument::from_json(&std::fs::read_to_string(&path).unwrap())
            .unwrap()
            .into_current()
            .unwrap();
        assert_eq!(stored.schema_version, 2);
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.bak")).unwrap(),
            legacy
        );
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn refuses_to_follow_a_configuration_symlink() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("real.json");
        let path = directory.path().join("config.json");
        std::fs::write(&target, "{}").unwrap();
        symlink(&target, &path).unwrap();

        let error = ConfigRepository::new(path).load().unwrap_err();

        assert!(error.to_string().contains("not a regular file"));
    }
}
