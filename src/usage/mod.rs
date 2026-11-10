pub mod calendar;
pub mod counters;
pub mod events;
pub mod quota;
pub mod store;

pub use calendar::LocalDate;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub rx: u64,
    pub tx: u64,
}

impl UsageTotals {
    pub(crate) fn total(self) -> u64 {
        self.rx.saturating_add(self.tx)
    }
    pub(crate) fn add(self, other: Self) -> Self {
        Self {
            rx: self.rx.saturating_add(other.rx),
            tx: self.tx.saturating_add(other.tx),
        }
    }
}

pub fn usage_settings_path() -> anyhow::Result<PathBuf> {
    Ok(xdg_directory("XDG_CONFIG_HOME", ".config")?.join("routelane/usage-settings.json"))
}

pub fn usage_state_dir() -> anyhow::Result<PathBuf> {
    Ok(xdg_directory("XDG_STATE_HOME", ".local/state")?.join("routelane"))
}

fn xdg_directory(variable: &str, fallback: &str) -> anyhow::Result<PathBuf> {
    if let Some(value) = std::env::var_os(variable).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return Ok(path);
        }
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home).join(fallback))
}

pub(crate) fn atomic_save(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    crate::config::ensure_private_directory(parent)?;
    let temporary = parent.join(format!(".usage-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        crate::config::write_private_file(&temporary, bytes)?;
        std::fs::rename(&temporary, path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn preserve_corrupt(path: &Path) -> anyhow::Result<PathBuf> {
    let mut name = path.as_os_str().to_os_string();
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    name.push(format!(".corrupt-{unix}"));
    let destination = PathBuf::from(name);
    std::fs::rename(path, &destination)?;
    Ok(destination)
}
