//! Explicit, user-approved NetworkManager IPv4 priority changes. Never part of
//! routing activation, polling, helper recovery, or an automatic failover.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use zbus::zvariant::OwnedValue;

mod nm;
pub use nm::NetworkManager;

pub type Settings = BTreeMap<String, BTreeMap<String, OwnedValue>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub interface: String,
    pub uuid: String,
    pub ifindex: u32,
    pub hardware: String,
}

#[derive(Clone)]
pub struct Snapshot {
    pub identity: Identity,
    pub device_path: String,
    pub profile_path: String,
    pub owner: String,
    pub saved_version: u64,
    pub applied_version: u64,
    pub saved: Settings,
    pub applied: Settings,
    pub default_interface: Option<String>,
    pub defaults: Vec<(String, u32)>,
    pub ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    pub identity: Identity,
    pub device_path: String,
    pub profile_path: String,
    pub owner: String,
    pub saved_version: u64,
    pub applied_version: u64,
    pub saved_hash: [u8; 32],
    pub applied_hash: [u8; 32],
    pub saved_metric: Option<i64>,
    pub applied_metric: Option<i64>,
    pub defaults: Vec<(String, u32)>,
    pub metric: i64,
}

#[derive(Debug, Clone)]
pub struct Inspection {
    pub approval: Approval,
    pub default_interface: Option<String>,
    pub can_restore: bool,
    pub has_record: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Unavailable,
    Unsupported,
    Disconnected,
    Conflict,
    Stale,
    Storage,
    Authorization,
    Incomplete,
}

#[derive(Debug, Clone)]
pub struct Error {
    pub kind: ErrorKind,
    pub detail: String,
}
impl Error {
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

#[async_trait]
pub trait Backend: Send {
    async fn snapshot(&mut self, interface: &str) -> Result<Snapshot>;
    async fn update(&mut self, expected: &Snapshot, metric: Option<i64>) -> Result<()>;
    async fn reapply(&mut self, expected: &Snapshot, metric: Option<i64>) -> Result<()>;
}

pub fn journal_path() -> Result<PathBuf> {
    crate::usage::usage_state_dir()
        .map(|p| p.join("system-primary-v1.json"))
        .map_err(|e| Error::new(ErrorKind::Storage, e.to_string()))
}

fn metric(settings: &Settings) -> Result<Option<i64>> {
    settings
        .get("ipv4")
        .and_then(|v| v.get("route-metric"))
        .map(|v| {
            i64::try_from(v).map_err(|_| Error::new(ErrorKind::Unsupported, "invalid IPv4 metric"))
        })
        .transpose()
}

fn settings_hash(settings: &Settings) -> Result<[u8; 32]> {
    let mut settings = settings.clone();
    if let Some(ipv4) = settings.get_mut("ipv4") {
        ipv4.remove("route-metric");
    }
    if let Some(connection) = settings.get_mut("connection") {
        connection.remove("timestamp");
    }
    serde_json::to_vec(&settings)
        .map(|bytes| *blake3::hash(&bytes).as_bytes())
        .map_err(|e| Error::new(ErrorKind::Unsupported, e.to_string()))
}

fn approve(snapshot: &Snapshot) -> Result<Approval> {
    if !snapshot.ready {
        return Err(Error::new(
            ErrorKind::Disconnected,
            "selected connection needs a live IPv4 address and default route",
        ));
    }
    let competitor = snapshot
        .defaults
        .iter()
        .filter(|(name, _)| name != &snapshot.identity.interface)
        .map(|(_, metric)| *metric)
        .min()
        .unwrap_or(100);
    if competitor == 0 {
        return Err(Error::new(
            ErrorKind::Conflict,
            "another default has metric zero",
        ));
    }
    let saved_metric = metric(&snapshot.saved)?;
    let proposed = i64::from(50.min(competitor - 1));
    // Do not make an already explicit, better priority worse merely to use
    // our preferred number. An automatic metric still needs a persistent pin.
    let proposed = saved_metric
        .filter(|value| *value >= 0)
        .map_or(proposed, |value| value.min(proposed));
    Ok(Approval {
        identity: snapshot.identity.clone(),
        device_path: snapshot.device_path.clone(),
        profile_path: snapshot.profile_path.clone(),
        owner: snapshot.owner.clone(),
        saved_version: snapshot.saved_version,
        applied_version: snapshot.applied_version,
        saved_hash: settings_hash(&snapshot.saved)?,
        applied_hash: settings_hash(&snapshot.applied)?,
        saved_metric,
        applied_metric: metric(&snapshot.applied)?,
        defaults: snapshot.defaults.clone(),
        metric: proposed,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    identity: Identity,
    saved_hash: [u8; 32],
    applied_hash: [u8; 32],
    old_saved: Option<i64>,
    old_applied: Option<i64>,
    written: i64,
    #[serde(default)]
    pending: Option<PendingWrite>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum PendingWrite {
    Saved(Option<i64>),
    Applied(Option<i64>),
}

impl Record {
    fn matches(&self, snapshot: &Snapshot) -> Result<bool> {
        // Device indices can change on reboot; UUID, interface and physical
        // identity must not. Unrelated settings must still match as well.
        let pending_visible = match self.pending {
            Some(PendingWrite::Saved(value)) => metric(&snapshot.saved)? == value,
            Some(PendingWrite::Applied(value)) => metric(&snapshot.applied)? == value,
            None => true,
        };
        Ok(pending_visible
            && self.identity.uuid == snapshot.identity.uuid
            && self.identity.interface == snapshot.identity.interface
            && self.identity.hardware == snapshot.identity.hardware
            && self.saved_hash == settings_hash(&snapshot.saved)?
            && self.applied_hash == settings_hash(&snapshot.applied)?
            && [self.old_saved, Some(self.written)].contains(&metric(&snapshot.saved)?)
            && [self.old_applied, Some(self.written)].contains(&metric(&snapshot.applied)?))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    records: BTreeMap<String, Record>,
}
impl Journal {
    fn load(path: &Path) -> Result<Self> {
        use std::{
            fs::OpenOptions,
            io::Read,
            os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        };
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    version: 1,
                    records: BTreeMap::new(),
                })
            }
            Err(e) => return Err(Error::new(ErrorKind::Storage, e.to_string())),
        };
        let meta = file.metadata().map_err(storage)?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
            || meta.len() > 65536
        {
            return Err(Error::new(
                ErrorKind::Storage,
                "unsafe or oversized priority ledger",
            ));
        }
        let mut bytes = vec![];
        file.take(65537).read_to_end(&mut bytes).map_err(storage)?;
        let journal: Self = serde_json::from_slice(&bytes).map_err(storage)?;
        if journal.version != 1 || journal.records.len() > 64 {
            return Err(Error::new(
                ErrorKind::Storage,
                "unsupported priority ledger",
            ));
        }
        Ok(journal)
    }
    fn save(&self, path: &Path) -> Result<()> {
        let bytes = serde_json::to_vec(self).map_err(storage)?;
        if self.records.len() > 64 || bytes.len() > 65536 {
            return Err(storage("priority ledger limit reached"));
        }
        crate::usage::atomic_save(path, &bytes).map_err(storage)
    }
}
fn storage(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Storage, e.to_string())
}

fn lock(path: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    crate::config::ensure_private_directory(
        path.parent()
            .ok_or_else(|| storage("ledger needs a parent"))?,
    )
    .map_err(storage)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path.with_extension("lock"))
        .map_err(storage)?;
    let meta = file.metadata().map_err(storage)?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err(storage("unsafe priority lock"));
    }
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(storage("another priority operation is in progress"));
    }
    Ok(file)
}

/// An ambiguous call may still have reached the server. Keep its intent on
/// disk; do not offer Undo against a pre-call snapshot that a late write could
/// invalidate. A visible target proves the pending field write has taken effect.
async fn write_step<B: Backend>(
    backend: &mut B,
    journal: &mut Journal,
    path: &Path,
    current: &Snapshot,
    metric: Option<i64>,
    applied: bool,
) -> Result<()> {
    let uuid = &current.identity.uuid;
    journal
        .records
        .get_mut(uuid)
        .ok_or_else(|| storage("missing priority record"))?
        .pending = Some(if applied {
        PendingWrite::Applied(metric)
    } else {
        PendingWrite::Saved(metric)
    });
    journal.save(path)?;
    let result = if applied {
        backend.reapply(current, metric).await
    } else {
        backend.update(current, metric).await
    };
    if result
        .as_ref()
        .err()
        .is_none_or(|e| !matches!(e.kind, ErrorKind::Unavailable | ErrorKind::Incomplete))
    {
        journal
            .records
            .get_mut(uuid)
            .expect("record retained")
            .pending = None;
        journal.save(path)?;
    }
    result
}

pub async fn inspect<B: Backend>(
    backend: &mut B,
    path: &Path,
    interface: &str,
) -> Result<Inspection> {
    let current = backend.snapshot(interface).await?;
    let journal = Journal::load(path)?;
    let approval = approve(&current)?;
    let can_restore = journal
        .records
        .get(&current.identity.uuid)
        .map(|record| record.matches(&current))
        .transpose()?
        .unwrap_or(false);
    Ok(Inspection {
        approval,
        default_interface: current.default_interface.clone(),
        can_restore,
        has_record: journal.records.contains_key(&current.identity.uuid),
    })
}

pub async fn make_primary<B: Backend>(
    backend: &mut B,
    path: &Path,
    approved: &Approval,
) -> Result<()> {
    let _lock = lock(path)?;
    let current = backend.snapshot(&approved.identity.interface).await?;
    if approve(&current)? != *approved {
        return Err(Error::new(
            ErrorKind::Stale,
            "connection changed; review a fresh preview",
        ));
    }
    let mut journal = Journal::load(path)?;
    if journal.records.contains_key(&approved.identity.uuid) {
        return Err(Error::new(
            ErrorKind::Conflict,
            "restore the previous recorded change first",
        ));
    }
    let record = Record {
        identity: approved.identity.clone(),
        saved_hash: approved.saved_hash,
        applied_hash: approved.applied_hash,
        old_saved: approved.saved_metric,
        old_applied: approved.applied_metric,
        written: approved.metric,
        pending: None,
    };
    journal
        .records
        .insert(approved.identity.uuid.clone(), record.clone());
    journal.save(path)?; // durable recovery evidence before any D-Bus mutation
    write_step(
        backend,
        &mut journal,
        path,
        &current,
        Some(approved.metric),
        false,
    )
    .await?;
    let saved = backend.snapshot(&approved.identity.interface).await?;
    if saved.identity != current.identity
        || saved.owner != current.owner
        || !record.matches(&saved)?
        || metric(&saved.saved)? != Some(approved.metric)
        || saved.applied_version != current.applied_version
    {
        return Err(Error::new(
            ErrorKind::Incomplete,
            "saved state changed; no reapply attempted",
        ));
    }
    write_step(
        backend,
        &mut journal,
        path,
        &saved,
        Some(approved.metric),
        true,
    )
    .await?;
    let after = backend.snapshot(&approved.identity.interface).await?;
    if after.identity != current.identity
        || after.owner != current.owner
        || !record.matches(&after)?
        || metric(&after.saved)? != Some(approved.metric)
        || metric(&after.applied)? != Some(approved.metric)
        || after.default_interface.as_deref() != Some(&approved.identity.interface)
    {
        return Err(Error::new(
            ErrorKind::Incomplete,
            "priority is not verified as the system default; review or restore",
        ));
    }
    Ok(())
}

pub async fn restore<B: Backend>(backend: &mut B, path: &Path, approved: &Approval) -> Result<()> {
    let _lock = lock(path)?;
    let mut journal = Journal::load(path)?;
    let mut record = journal
        .records
        .get(&approved.identity.uuid)
        .cloned()
        .ok_or_else(|| Error::new(ErrorKind::Conflict, "no recorded priority change"))?;
    let current = backend.snapshot(&approved.identity.interface).await?;
    if approve(&current)? != *approved || !record.matches(&current)? {
        return Err(Error::new(
            ErrorKind::Stale,
            "priority ownership changed; nothing restored",
        ));
    }
    // The fresh snapshot has reconciled an earlier pending intent. Subsequent
    // checks must compare ownership, not require that old target after Undo.
    record.pending = None;
    if metric(&current.saved)? != record.old_saved {
        write_step(
            backend,
            &mut journal,
            path,
            &current,
            record.old_saved,
            false,
        )
        .await?;
    }
    let saved = backend.snapshot(&approved.identity.interface).await?;
    if saved.identity != current.identity
        || saved.owner != current.owner
        || !record.matches(&saved)?
        || saved.applied_version != current.applied_version
    {
        return Err(Error::new(
            ErrorKind::Incomplete,
            "saved state changed; no restore reapply attempted",
        ));
    }
    if metric(&saved.applied)? != record.old_applied {
        write_step(
            backend,
            &mut journal,
            path,
            &saved,
            record.old_applied,
            true,
        )
        .await?;
    }
    let after = backend.snapshot(&approved.identity.interface).await?;
    if after.identity != current.identity
        || after.owner != current.owner
        || !record.matches(&after)?
        || metric(&after.saved)? != record.old_saved
        || metric(&after.applied)? != record.old_applied
    {
        return Err(Error::new(
            ErrorKind::Incomplete,
            "restore has not been verified",
        ));
    }
    journal.records.remove(&approved.identity.uuid);
    journal.save(path)?;
    Ok(())
}

#[cfg(test)]
mod tests;
