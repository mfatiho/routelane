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
    let config = AtomicJsonConfigRepository::from_environment()?;
    let service = Arc::new(DaemonService::new(topology, resolutions, helper, config)?);
    let server = UnixSocketServer::bind_at(socket_path()?, service).await?;
    server.run().await?;
    Ok(())
}
