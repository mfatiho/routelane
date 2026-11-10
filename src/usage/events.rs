use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::domain::{LaneId, RuleId};

pub const MAX_EVENTS: usize = 5_000;
pub const RETENTION_DAYS: i64 = 30;
const RETENTION_MS: i64 = RETENTION_DAYS * 86_400_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum EventKind {
    ProfileApplied {
        profile: String,
    },
    ProfileDeactivated,
    ApplyFailed {
        code: String,
        message: String,
        rolled_back: bool,
    },
    RuleApplied {
        rule_id: RuleId,
    },
    Failover {
        from: Option<LaneId>,
        to: LaneId,
        families: Vec<String>,
    },
    Failback {
        to: LaneId,
        backup_seconds: u64,
        backup_bytes: Option<u64>,
    },
    RuntimeAttention {
        phase: String,
        message: Option<String>,
    },
    QuotaWarning {
        lane_id: LaneId,
        used: u64,
        limit: u64,
    },
    QuotaFull {
        lane_id: LaneId,
        used: u64,
        limit: u64,
    },
    StoreRecovered {
        file: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageEvent {
    pub at_unix_ms: i64,
    pub kind: EventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventCategory {
    All,
    Problems,
    Transitions,
    Rules,
    Quota,
}

impl EventKind {
    pub fn category(&self) -> EventCategory {
        match self {
            Self::ApplyFailed { .. }
            | Self::RuntimeAttention { .. }
            | Self::StoreRecovered { .. } => EventCategory::Problems,
            Self::ProfileApplied { .. }
            | Self::ProfileDeactivated
            | Self::Failover { .. }
            | Self::Failback { .. } => EventCategory::Transitions,
            Self::RuleApplied { .. } => EventCategory::Rules,
            Self::QuotaWarning { .. } | Self::QuotaFull { .. } => EventCategory::Quota,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventStore {
    path: PathBuf,
    events: Vec<UsageEvent>,
}

impl EventStore {
    pub fn open(path: impl AsRef<Path>) -> (Self, Option<String>) {
        let mut store = Self {
            path: path.as_ref().to_owned(),
            events: Vec::new(),
        };
        let bytes = match std::fs::read(&store.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (store, None),
            Err(error) => {
                return (
                    store,
                    Some(format!("could not read event history: {error}")),
                )
            }
        };
        let mut corrupt = 0;
        let mut lines = bytes.split(|b| *b == b'\n').peekable();
        while let Some(line) = lines.next() {
            // A terminal newline is not an extra record; an interior empty line is corrupt.
            if line.is_empty() && lines.peek().is_none() {
                continue;
            }
            match serde_json::from_slice::<UsageEvent>(line) {
                Ok(event) => store.events.push(event),
                Err(_) => corrupt += 1,
            }
        }
        retain(&mut store.events, now_unix_ms());
        let mut notes = Vec::new();
        if corrupt > 0 {
            notes.push(format!("{corrupt} corrupt event lines skipped"));
            if let Err(error) = super::preserve_corrupt(&store.path) {
                notes.push(format!("could not preserve corrupt event file: {error}"));
                return (store, Some(notes.join("; ")));
            }
        }
        if let Err(error) = store.rewrite(&store.events) {
            notes.push(format!("could not compact event history: {error}"));
        }
        let note = (!notes.is_empty()).then(|| notes.join("; "));
        (store, note)
    }

    pub fn append(&mut self, event: UsageEvent) -> anyhow::Result<()> {
        let mut events = self.events.clone();
        events.push(event.clone());
        let before = events.len();
        retain(&mut events, now_unix_ms());
        if events.len() != before {
            self.rewrite(&events)?;
        } else {
            let parent = self
                .path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            crate::config::ensure_private_directory(parent)?;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&self.path)?;
            anyhow::ensure!(
                file.metadata()?.is_file(),
                "event history is not a regular file"
            );
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            let mut bytes = serde_json::to_vec(&event)?;
            bytes.push(b'\n');
            file.write_all(&bytes)?;
            file.sync_data()?;
        }
        self.events = events;
        Ok(())
    }

    pub fn list(&self, category: EventCategory, now_unix_ms: i64) -> Vec<UsageEvent> {
        self.events
            .iter()
            .rev()
            .filter(|event| {
                event.at_unix_ms >= now_unix_ms.saturating_sub(RETENTION_MS)
                    && (category == EventCategory::All || event.kind.category() == category)
            })
            .cloned()
            .collect()
    }

    /// Removes every event. On a write failure nothing is cleared, so the
    /// caller can report it instead of showing an empty but stale history.
    pub fn clear(&mut self) -> anyhow::Result<()> {
        self.rewrite(&[])?;
        self.events.clear();
        Ok(())
    }

    fn rewrite(&self, events: &[UsageEvent]) -> anyhow::Result<()> {
        let mut bytes = Vec::new();
        for event in events {
            serde_json::to_writer(&mut bytes, event)?;
            bytes.push(b'\n');
        }
        super::atomic_save(&self.path, &bytes)
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn retain(events: &mut Vec<UsageEvent>, now: i64) {
    events.retain(|event| event.at_unix_ms >= now.saturating_sub(RETENTION_MS));
    events.sort_by_key(|event| event.at_unix_ms);
    if events.len() > MAX_EVENTS {
        events.drain(..events.len() - MAX_EVENTS);
    }
}
