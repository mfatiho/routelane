use std::sync::Arc;

use routelane::daemon::{
    AtomicJsonConfigRepository, DaemonService, SystemPrivilegedHelperClient,
    SystemResolutionProvider, SystemTopologyProvider,
};
use routelane::ipc::{socket_path, UnixSocketServer};

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
    let service = Arc::new(DaemonService::new(topology, resolutions, helper, config)?);
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
