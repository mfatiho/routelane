mod config;
mod models;
mod ui;

use std::time::Duration;

use async_channel::{bounded, Receiver, Sender};
use libadwaita::{self as adw, prelude::*};
use routelane::ipc::{
    socket_path, DaemonClient, DaemonEnvelope, DaemonRequest, DaemonResult, IpcError,
};

use crate::models::{DaemonToUi, UiToDaemon};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let (ui_sender, ui_receiver) = bounded::<UiToDaemon>(64);
    let (daemon_sender, daemon_receiver) = bounded::<DaemonToUi>(64);
    let runtime = tokio::runtime::Runtime::new().expect("Tokio runtime could not be created");
    std::thread::spawn(move || {
        runtime.block_on(daemon_bridge(ui_receiver, daemon_sender));
    });

    let app = adw::Application::builder()
        .application_id(ui::tray::APP_ID)
        .build();
    {
        let ui_sender = ui_sender.clone();
        app.connect_activate(move |app| {
            ui::window::build_window(app, ui_sender.clone(), daemon_receiver.clone());
        });
    }
    app.connect_shutdown(move |_| {
        ui_sender.try_send(UiToDaemon::Shutdown).ok();
    });
    app.run();
}

async fn daemon_bridge(from_ui: Receiver<UiToDaemon>, to_ui: Sender<DaemonToUi>) {
    let path = match socket_path() {
        Ok(path) => path,
        Err(error) => {
            let _ = to_ui
                .send(DaemonToUi::DaemonUnavailable(error.to_string()))
                .await;
            return;
        }
    };
    let client = DaemonClient::new(path);

    if let Err(first_error) = request_status(&client, &to_ui).await {
        if let Err(activation_error) = activate_user_daemon().await {
            let _ = to_ui
                .send(DaemonToUi::DaemonUnavailable(format!(
                    "{first_error}; systemd user activation failed: {activation_error}"
                )))
                .await;
        } else if let Err(error) = wait_for_daemon(&client, &to_ui).await {
            let _ = to_ui
                .send(DaemonToUi::DaemonUnavailable(error.to_string()))
                .await;
        }
    }

    let mut subscription = client.subscribe(request_id()).await.ok();
    loop {
        tokio::select! {
            command = from_ui.recv() => {
                match command {
                    Ok(UiToDaemon::Request(request)) => {
                        match client.send(DaemonEnvelope::new(request_id(), request)).await {
                            Ok(response) => forward_response(response, &to_ui).await,
                            Err(error) => {
                                let _ = to_ui.send(DaemonToUi::DaemonUnavailable(error.to_string())).await;
                            }
                        }
                    }
                    Ok(UiToDaemon::Shutdown) | Err(_) => break,
                }
            }
            event = async {
                match subscription.as_mut() {
                    Some(receiver) => receiver.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match event {
                    Some(event) => {
                        let _ = to_ui.send(DaemonToUi::Event(event)).await;
                    }
                    None => subscription = None,
                }
            }
        }
    }
}

async fn request_status(
    client: &DaemonClient,
    sender: &Sender<DaemonToUi>,
) -> Result<(), IpcError> {
    let response = client
        .send(DaemonEnvelope::new(request_id(), DaemonRequest::GetStatus))
        .await?;
    if let Some(error) = response.error {
        return Err(error);
    }
    match response.result {
        Some(DaemonResult::Status(status)) => {
            let _ = sender.send(DaemonToUi::Status(status)).await;
            Ok(())
        }
        _ => Err(IpcError::new(
            "IPC_INVALID_RESPONSE",
            "status request returned an unexpected response",
        )),
    }
}

async fn wait_for_daemon(
    client: &DaemonClient,
    sender: &Sender<DaemonToUi>,
) -> Result<(), IpcError> {
    let mut last_error = IpcError::new("IPC_CONNECT_FAILED", "daemon did not create its socket");
    for _ in 0..20 {
        match request_status(client, sender).await {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(last_error)
}

async fn activate_user_daemon() -> std::io::Result<()> {
    let status = tokio::process::Command::new("systemctl")
        .args(["--user", "start", "routelane-daemon.service"])
        .status()
        .await?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "systemctl exited with {status}"
        )))
    }
}

async fn forward_response(response: routelane::ipc::DaemonResponse, sender: &Sender<DaemonToUi>) {
    if let Some(error) = response.error {
        let _ = sender.send(DaemonToUi::Error(error)).await;
        return;
    }
    let event = match response.result {
        Some(DaemonResult::Status(status)) => Some(DaemonToUi::Status(status)),
        Some(DaemonResult::Profiles(profiles)) => Some(DaemonToUi::Profiles(profiles)),
        Some(DaemonResult::Applied(result)) => Some(DaemonToUi::Applied(result)),
        Some(DaemonResult::Deactivated) => Some(DaemonToUi::Deactivated),
        Some(DaemonResult::Subscribed) | None => None,
    };
    if let Some(event) = event {
        let _ = sender.send(event).await;
    }
}

fn request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
