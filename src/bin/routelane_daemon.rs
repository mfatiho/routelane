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
    let topology = SystemTopologyProvider;
    let resolutions = SystemResolutionProvider;
    let helper = SystemPrivilegedHelperClient::connect().await?;
    // Subscribe before the first snapshot so no helper transition is missed.
    let runtime_changes = helper.runtime_changes().await;
    let config = AtomicJsonConfigRepository::from_environment()?;
    let state_dir = usage_state_dir()?;
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let (store, usage_note) = UsageStore::open(state_dir.join("usage-v1.json"), boot_id.trim());
    let (events, events_note) = EventStore::open(state_dir.join("events-v1.jsonl"));
    let usage = Arc::new(UsageService::new(
        store,
        usage_settings_path()?,
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
    let service = Arc::new(
        DaemonService::new(topology, resolutions, helper, config)?.with_usage(usage.clone()),
    );
    {
        let service = service.clone();
        tokio::spawn(async move {
            loop {
                service.sample_usage().await;
                tokio::select! {
                    _ = tokio::time::sleep(usage.interval()) => {},
                    _ = usage.sampling_requested() => {},
                }
            }
        });
    }
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
    let server = UnixSocketServer::bind_at(socket_path()?, service).await?;
    server.run().await?;
    Ok(())
}
