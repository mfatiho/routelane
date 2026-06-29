use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    pub alt_interface: Option<String>,
    pub rules: Vec<PersistedRule>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PersistedRule {
    pub target_str: String,
    pub is_domain: bool,
    pub interface: String,
}

impl PersistedState {
    pub fn load() -> Self {
        match std::fs::read_to_string(config_path()) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn save_language(language: Language) -> anyhow::Result<()> {
        let mut state = Self::load();
        state.language = language;
        state.save()
    }
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

    #[test]
    fn persisted_state_defaults_to_turkish_for_existing_configs() {
        let state: PersistedState = serde_json::from_str(r#"{"alt_interface":null,"rules":[]}"#)
            .expect("legacy config should deserialize");

        assert_eq!(state.language, Language::Tr);
    }
}
