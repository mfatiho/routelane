use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::domain::{
    normalize_lane_names, validate_profile, InterfaceSelector, Lane, LaneFailurePolicy, LaneId,
    Profile, ProfileId, Rule, RuleAction, RuleId, RuleSource, RuleTarget,
};
use anyhow::{bail, Context};

pub const MAX_CONFIG_BYTES: usize = 1_048_576;

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
#[serde(deny_unknown_fields)]
pub struct PersistedState {
    #[serde(default)]
    pub language: Language,
    #[serde(default)]
    pub alt_interface: Option<String>,
    #[serde(default)]
    pub rules: Vec<PersistedRule>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct PersistedRule {
    pub target_str: String,
    pub is_domain: bool,
    pub interface: String,
}

pub const CURRENT_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        let object = value
            .as_object()
            .context("configuration root must be an object")?;
        if let Some(raw_version) = object.get("schema_version") {
            match raw_version.as_u64() {
                Some(version) if version == u64::from(CURRENT_SCHEMA_VERSION) => {
                    let current = serde_json::from_value(value)
                        .context("version 2 configuration is invalid")?;
                    Ok(Self::V2(current))
                }
                Some(version) => bail!("unsupported configuration schema version {version}"),
                None => bail!("schema_version must be the integer {CURRENT_SCHEMA_VERSION}"),
            }
        } else {
            let recognized = object
                .keys()
                .all(|key| matches!(key.as_str(), "language" | "alt_interface" | "rules"))
                && (object.contains_key("alt_interface") || object.contains_key("rules"));
            if !recognized {
                bail!("configuration does not match the legacy schema");
            }
            serde_json::from_value(value)
                .map(Self::Legacy)
                .context("legacy configuration is invalid")
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
            normalize_lane_names(profile);
            validate_profile(profile)
                .map_err(|error| anyhow::anyhow!("invalid profile after migration: {error:?}"))?;
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

#[derive(Debug, Clone)]
pub struct PersistenceRequest {
    pub generation: u64,
    pub config: ConfigV2,
}

impl PersistenceRequest {
    pub fn new(generation: u64, config: ConfigV2) -> Self {
        Self { generation, config }
    }
}

#[derive(Debug)]
pub struct PersistenceResult {
    pub generation: u64,
    pub result: Result<(), String>,
}

pub struct PersistenceWorker {
    pub requests: async_channel::Sender<PersistenceRequest>,
    pub results: async_channel::Receiver<PersistenceResult>,
}

impl PersistenceWorker {
    pub fn spawn(repository: ConfigRepository) -> Self {
        let (requests, request_receiver) = async_channel::bounded::<PersistenceRequest>(1);
        let (result_sender, results) = async_channel::bounded::<PersistenceResult>(1);
        std::thread::Builder::new()
            .name("routelane-config-writer".to_owned())
            .spawn(move || {
                while let Ok(request) = request_receiver.recv_blocking() {
                    let result = repository
                        .save(&request.config)
                        .map_err(|error| error.to_string());
                    if result_sender
                        .send_blocking(PersistenceResult {
                            generation: request.generation,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("configuration persistence worker could not start");
        Self { requests, results }
    }
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
        load_config_file(&self.path)
    }

    pub fn save(&self, config: &ConfigV2) -> anyhow::Result<()> {
        let mut current = config.clone();
        current.schema_version = CURRENT_SCHEMA_VERSION;
        for profile in &mut current.profiles {
            profile.enabled = false;
            validate_profile(profile)
                .map_err(|error| anyhow::anyhow!("refusing to save invalid profile: {error:?}"))?;
        }
        let bytes = serde_json::to_vec_pretty(&current)?;

        let parent = self
            .path
            .parent()
            .context("configuration path has no parent directory")?;
        ensure_private_directory(parent)?;
        if self.path.exists() {
            validate_existing_file(&self.path)?;
        }

        let temp_path = parent.join(format!(
            ".{}.{}.tmp",
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("config"),
            uuid::Uuid::new_v4()
        ));
        let mut temporary = TemporaryPath::new(temp_path);
        write_private_file(temporary.path(), &bytes)?;

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
            let mut backup_temporary = TemporaryPath::new(backup_temp);
            copy_private_file(&self.path, backup_temporary.path())?;
            std::fs::rename(backup_temporary.path(), &backup).with_context(|| {
                format!(
                    "could not atomically replace configuration backup {}",
                    backup.display()
                )
            })?;
            backup_temporary.commit();
        }

        std::fs::rename(temporary.path(), &self.path).with_context(|| {
            format!(
                "could not atomically replace configuration {}",
                self.path.display()
            )
        })?;
        temporary.commit();
        sync_directory(parent)?;
        Ok(())
    }
}

pub fn load_config_file(path: &Path) -> anyhow::Result<ConfigV2> {
    validate_existing_file(path)?;
    let mut file = open_existing_file_nonblocking(path)?;
    let metadata = file.metadata()?;
    validate_metadata(&metadata)?;
    let maximum = u64::try_from(MAX_CONFIG_BYTES).expect("config limit fits u64");
    if metadata.len() > maximum {
        bail!(
            "configuration is {} bytes; maximum is {MAX_CONFIG_BYTES}",
            metadata.len()
        );
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::io::Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("could not read {}", path.display()))?;
    if bytes.len() > MAX_CONFIG_BYTES {
        bail!("configuration is larger than the maximum of {MAX_CONFIG_BYTES} bytes");
    }
    let json = std::str::from_utf8(&bytes).context("configuration is not valid UTF-8")?;
    ConfigDocument::from_json(json)?.into_current()
}

struct TemporaryPath {
    path: PathBuf,
    committed: bool,
}

impl TemporaryPath {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for TemporaryPath {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
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

fn open_existing_file_nonblocking(path: &Path) -> anyhow::Result<File> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    options
        .open(path)
        .with_context(|| format!("could not safely open {}", path.display()))
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
    use crate::domain::RuleAction;
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

    #[test]
    fn schema_version_is_strict_and_never_falls_back_to_legacy() {
        for invalid in [
            r#"{"schema_version":"2","language":"tr","profiles":[]}"#,
            r#"{"schema_version":3,"language":"tr","profiles":[]}"#,
            r#"{"schema_version":2.0,"language":"tr","profiles":[]}"#,
            r#"{"language":"tr","profiles":[]}"#,
        ] {
            assert!(ConfigDocument::from_json(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn invalid_v2_load_never_changes_the_original_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let invalid = br#"{"schema_version":"2","profiles":[BROKEN]}"#;
        std::fs::write(&path, invalid).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        assert!(ConfigRepository::new(&path).load().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), invalid);
        assert!(!path.with_extension("json.bak").exists());
    }

    #[test]
    fn persistence_worker_serializes_rapid_edits_in_generation_order() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let worker = PersistenceWorker::spawn(ConfigRepository::new(&path));
        let first = ConfigV2 {
            language: Language::Tr,
            ..ConfigV2::default()
        };
        let mut second = first.clone();
        second.language = Language::En;

        worker
            .requests
            .send_blocking(PersistenceRequest::new(1, first))
            .unwrap();
        worker
            .requests
            .send_blocking(PersistenceRequest::new(2, second))
            .unwrap();

        assert_eq!(worker.results.recv_blocking().unwrap().generation, 1);
        assert_eq!(worker.results.recv_blocking().unwrap().generation, 2);
        assert_eq!(
            ConfigRepository::new(path).load().unwrap().language,
            Language::En
        );
    }

    #[test]
    fn invalid_document_is_rejected_before_existing_file_is_touched() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let original = br#"{"schema_version":2,"language":"tr","profiles":[]}"#;
        std::fs::write(&path, original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut invalid = ConfigV2::default();
        invalid.profiles.push(Profile {
            id: ProfileId::from("bad"),
            name: "Bad".to_owned(),
            enabled: false,
            lanes: vec![
                Lane {
                    id: LaneId::from("a"),
                    name: "Ärea".to_owned(),
                    interface: InterfaceSelector::from("tun0"),
                    fallback_lane: None,
                    failure_policy: LaneFailurePolicy::Inactive,
                },
                Lane {
                    id: LaneId::from("b"),
                    name: "äREA".to_owned(),
                    interface: InterfaceSelector::from("tun1"),
                    fallback_lane: None,
                    failure_policy: LaneFailurePolicy::Inactive,
                },
            ],
            rules: vec![],
        });

        assert!(ConfigRepository::new(&path).save(&invalid).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(!path.with_extension("json.bak").exists());
    }
}
