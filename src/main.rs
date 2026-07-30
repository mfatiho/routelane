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

#[derive(Debug, Clone, Copy)]
struct ConnectionPolicy {
    request_timeout: Duration,
    activation_timeout: Duration,
    initial_reconnect_delay: Duration,
    maximum_reconnect_delay: Duration,
}

impl Default for ConnectionPolicy {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(3),
            activation_timeout: Duration::from_secs(8),
            initial_reconnect_delay: Duration::from_millis(250),
            maximum_reconnect_delay: Duration::from_secs(8),
        }
    }
}

impl ConnectionPolicy {
    fn reconnect_delay(self, attempt: u32) -> Duration {
        let multiplier = 1_u32.checked_shl(attempt.min(16)).unwrap_or(u32::MAX);
        self.initial_reconnect_delay
            .saturating_mul(multiplier)
            .min(self.maximum_reconnect_delay)
    }
}

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
    let policy = ConnectionPolicy::default();
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

    if let Err(first_error) = refresh_daemon_state(&client, &to_ui, policy).await {
        if let Err(activation_error) = activate_user_daemon(policy).await {
            let _ = to_ui
                .send(DaemonToUi::DaemonUnavailable(format!(
                    "{first_error}; systemd user activation failed: {activation_error}"
                )))
                .await;
        } else if let Err(error) = wait_for_daemon(&client, &to_ui, policy).await {
            let _ = to_ui
                .send(DaemonToUi::DaemonUnavailable(error.to_string()))
                .await;
        }
    }

    let mut subscription = subscribe_with_timeout(&client, policy).await.ok();
    let mut reconnect_attempt = 0;
    let mut next_reconnect = tokio::time::Instant::now();
    loop {
        let reconnecting = subscription.is_none();
        let retry_deadline = next_reconnect;
        tokio::select! {
            command = from_ui.recv() => {
                match command {
                    Ok(UiToDaemon::Request(request)) => {
                        match send_with_timeout(&client, request, policy).await {
                            Ok(response) => forward_response(response, &to_ui).await,
                            Err(error) => {
                                let _ = to_ui.send(DaemonToUi::DaemonUnavailable(error.to_string())).await;
                                subscription = None;
                                next_reconnect = tokio::time::Instant::now()
                                    + policy.reconnect_delay(reconnect_attempt);
                                reconnect_attempt = reconnect_attempt.saturating_add(1);
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
                    None => {
                        subscription = None;
                        let _ = to_ui
                            .send(DaemonToUi::DaemonUnavailable(
                                "daemon event subscription disconnected".to_owned(),
                            ))
                            .await;
                        next_reconnect = tokio::time::Instant::now()
                            + policy.reconnect_delay(reconnect_attempt);
                        reconnect_attempt = reconnect_attempt.saturating_add(1);
                    }
                }
            }
            _ = async {
                if reconnecting {
                    tokio::time::sleep_until(retry_deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                match refresh_daemon_state(&client, &to_ui, policy).await {
                    Ok(()) => match subscribe_with_timeout(&client, policy).await {
                        Ok(events) => {
                            subscription = Some(events);
                            reconnect_attempt = 0;
                        }
                        Err(error) => {
                            let _ = to_ui.send(DaemonToUi::DaemonUnavailable(error.to_string())).await;
                            next_reconnect = tokio::time::Instant::now()
                                + policy.reconnect_delay(reconnect_attempt);
                            reconnect_attempt = reconnect_attempt.saturating_add(1);
                        }
                    },
                    Err(error) => {
                        let _ = to_ui.send(DaemonToUi::DaemonUnavailable(error.to_string())).await;
                        next_reconnect = tokio::time::Instant::now()
                            + policy.reconnect_delay(reconnect_attempt);
                        reconnect_attempt = reconnect_attempt.saturating_add(1);
                    }
                }
            }
        }
    }
}

async fn request_status(
    client: &DaemonClient,
    sender: &Sender<DaemonToUi>,
    policy: ConnectionPolicy,
) -> Result<(), IpcError> {
    let response = send_with_timeout(client, DaemonRequest::GetStatus, policy).await?;
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

async fn request_profiles(
    client: &DaemonClient,
    sender: &Sender<DaemonToUi>,
    policy: ConnectionPolicy,
) -> Result<(), IpcError> {
    let response = send_with_timeout(client, DaemonRequest::ListProfiles, policy).await?;
    if let Some(error) = response.error {
        return Err(error);
    }
    match response.result {
        Some(DaemonResult::Profiles(profiles)) => {
            let _ = sender.send(DaemonToUi::Profiles(profiles)).await;
            Ok(())
        }
        _ => Err(IpcError::new(
            "IPC_INVALID_RESPONSE",
            "profile request returned an unexpected response",
        )),
    }
}

async fn refresh_daemon_state(
    client: &DaemonClient,
    sender: &Sender<DaemonToUi>,
    policy: ConnectionPolicy,
) -> Result<(), IpcError> {
    request_status(client, sender, policy).await?;
    request_profiles(client, sender, policy).await
}

async fn wait_for_daemon(
    client: &DaemonClient,
    sender: &Sender<DaemonToUi>,
    policy: ConnectionPolicy,
) -> Result<(), IpcError> {
    let mut last_error = IpcError::new("IPC_CONNECT_FAILED", "daemon did not create its socket");
    for attempt in 0..6 {
        match refresh_daemon_state(client, sender, policy).await {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
        tokio::time::sleep(policy.reconnect_delay(attempt)).await;
    }
    Err(last_error)
}

async fn activate_user_daemon(policy: ConnectionPolicy) -> std::io::Result<()> {
    let mut command = tokio::process::Command::new("systemctl");
    command
        .args(["--user", "start", "routelane-daemon.service"])
        .kill_on_drop(true);
    let status = tokio::time::timeout(policy.activation_timeout, command.status())
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "systemd activation timed out")
        })??;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "systemctl exited with {status}"
        )))
    }
}

async fn send_with_timeout(
    client: &DaemonClient,
    request: DaemonRequest,
    policy: ConnectionPolicy,
) -> Result<routelane::ipc::DaemonResponse, IpcError> {
    tokio::time::timeout(
        policy.request_timeout,
        client.send(DaemonEnvelope::new(request_id(), request)),
    )
    .await
    .map_err(|_| IpcError::new("IPC_TIMEOUT", "daemon request timed out"))?
}

async fn subscribe_with_timeout(
    client: &DaemonClient,
    policy: ConnectionPolicy,
) -> Result<tokio::sync::mpsc::Receiver<routelane::ipc::DaemonEvent>, IpcError> {
    tokio::time::timeout(policy.request_timeout, client.subscribe(request_id()))
        .await
        .map_err(|_| IpcError::new("IPC_TIMEOUT", "daemon subscription timed out"))?
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

#[cfg(test)]
mod tests {
    use super::ConnectionPolicy;
    use std::time::Duration;

    #[test]
    fn connection_policy_bounds_io_and_exponential_reconnect_delay() {
        let policy = ConnectionPolicy::default();

        assert!(policy.request_timeout <= Duration::from_secs(5));
        assert!(policy.activation_timeout <= Duration::from_secs(10));
        assert_eq!(policy.reconnect_delay(0), Duration::from_millis(250));
        assert_eq!(policy.reconnect_delay(20), policy.maximum_reconnect_delay);
    }
}
