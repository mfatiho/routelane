use std::sync::Arc;

use routelane::daemon::{
    AtomicJsonConfigRepository, DaemonService, SystemPrivilegedHelperClient,
    SystemResolutionProvider, SystemTopologyProvider,
};
use routelane::ipc::{socket_path, UnixSocketServer};
use routelane::usage::{
    counters::SysfsCounters,
    events::{EventKind, EventStore},
    store::UsageStore,
    usage_settings_path, usage_state_dir, UsageService,
};

#[tokio::main]
async fn main() {
    env_logger::init();
    if let Err(error) = run().await {
        eprintln!("routelane daemon failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let topology = SystemTopologyProvider;
    let resolutions = SystemResolutionProvider;
    let helper = SystemPrivilegedHelperClient::connect().await?;
    // Subscribe before the first snapshot so no helper transition is missed.
    let runtime_changes = helper.runtime_changes().await;
    let config = AtomicJsonConfigRepository::from_environment()?;
    let usage = open_usage(
        usage_state_dir(),
        usage_settings_path(),
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id"),
    );
    let mut service = DaemonService::new(topology, resolutions, helper, config)?;
    if let Some(usage) = &usage {
        service = service.with_usage(usage.clone());
    }
    let service = Arc::new(service);
    let server = UnixSocketServer::bind_at(socket_path()?, service.clone()).await?;
    let sampler = usage.as_ref().map(|usage| {
        let usage = usage.clone();
        let service = service.clone();
        tokio::spawn(async move {
            loop {
                service.sample_usage().await;
                tokio::select! {
                    _ = tokio::time::sleep(usage.interval()) => {},
                    _ = usage.sampling_requested() => {},
                }
            }
        })
    });
    match runtime_changes {
        Ok(mut changes) => {
            let service = Arc::clone(&service);
            tokio::spawn(async move {
                service.resync_runtime().await;
                while let Some(change) = changes.recv().await {
                    match change {
                        Some(status) => service.on_runtime_changed(status).await,
                        None => service.resync_runtime().await,
                    }
                }
            });
        }
        Err(error) => eprintln!("routelane daemon cannot follow helper runtime changes: {error}"),
    }
    let result = tokio::select! {
        result = server.run() => result,
        _ = terminate.recv() => Ok(()),
        _ = interrupt.recv() => Ok(()),
    };
    // Wait for the sampler to stop before the final write so it cannot dirty the
    // store again between flush and process exit.
    if let Some(sampler) = sampler {
        sampler.abort();
        let _ = sampler.await;
    }
    if let Some(usage) = usage {
        usage.flush()?;
    }
    result?;
    Ok(())
}

/// Resolve usage independently so missing session paths or boot identity do not
/// prevent routing IPC from starting. Results are injected for environment-safe tests.
fn open_usage(
    state_dir: anyhow::Result<std::path::PathBuf>,
    settings_path: anyhow::Result<std::path::PathBuf>,
    boot_id: std::io::Result<String>,
) -> Option<Arc<UsageService>> {
    let result = (|| -> anyhow::Result<Arc<UsageService>> {
        let state_dir = state_dir?;
        let settings_path = settings_path?;
        let boot_id = boot_id?;
        anyhow::ensure!(!boot_id.trim().is_empty(), "boot id is empty");
        routelane::usage::prepare_state_dir(&state_dir)?;
        let (store, usage_note) = UsageStore::open(state_dir.join("usage-v1.json"), boot_id.trim());
        let (events, events_note) = EventStore::open(state_dir.join("events-v1.jsonl"));
        let usage = Arc::new(UsageService::new(
            store,
            settings_path,
            events,
            Arc::new(SysfsCounters::default()),
            Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .min(i64::MAX as u64) as i64
            }),
        ));
        for (file, note) in [
            ("usage-v1.json", usage_note),
            ("events-v1.jsonl", events_note),
        ] {
            if let Some(note) = note {
                log::warn!("{file}: {note}");
                if let Err(error) = usage.record(EventKind::StoreRecovered { file: file.into() }) {
                    log::warn!("record store recovery: {error}");
                }
            }
        }
        Ok(usage)
    })();
    match result {
        Ok(usage) => Some(usage),
        Err(error) => {
            log::warn!("usage unavailable: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn open_usage_without_home_or_xdg_state_home_returns_none() {
        // Inject the state-directory resolver's missing-HOME result; never mutate process env.
        let dir = tempfile::tempdir().unwrap();
        assert!(open_usage(
            Err(anyhow::anyhow!("HOME is not set")),
            Ok(dir.path().join("settings.json")),
            Ok("boot".into()),
        )
        .is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    #[test]
    fn open_usage_without_a_boot_id_or_usable_state_directory_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        assert!(open_usage(
            Ok(state.clone()),
            Ok(dir.path().join("settings.json")),
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        )
        .is_none());
        assert!(!state.exists());
        assert!(open_usage(
            Ok(state.clone()),
            Ok(dir.path().join("settings.json")),
            Ok(" \n".into())
        )
        .is_none());
        std::fs::write(&state, b"a file, not a directory").unwrap();
        assert!(open_usage(
            Ok(state.clone()),
            Ok(dir.path().join("settings.json")),
            Ok("boot".into())
        )
        .is_none());
        assert_eq!(std::fs::read(&state).unwrap(), b"a file, not a directory");
    }
}
