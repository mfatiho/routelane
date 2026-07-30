mod models;
mod ui;

use std::collections::VecDeque;
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

#[derive(Debug)]
struct EventSubscription {
    receiver: tokio::sync::mpsc::Receiver<routelane::ipc::DaemonEvent>,
    buffered: VecDeque<routelane::ipc::DaemonEvent>,
}

impl EventSubscription {
    async fn recv(&mut self) -> Option<routelane::ipc::DaemonEvent> {
        match self.buffered.pop_front() {
            Some(event) => Some(event),
            None => self.receiver.recv().await,
        }
    }
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

    let mut subscription = match establish_subscription_and_refresh(&client, &to_ui, policy).await {
        Ok(events) => Some(events),
        Err(first_error) => {
            if let Err(activation_error) = activate_user_daemon(policy).await {
                let _ = to_ui
                    .send(DaemonToUi::DaemonUnavailable(format!(
                        "{first_error}; systemd user activation failed: {activation_error}"
                    )))
                    .await;
                None
            } else {
                match wait_for_daemon(&client, &to_ui, policy).await {
                    Ok(events) => Some(events),
                    Err(error) => {
                        let _ = to_ui
                            .send(DaemonToUi::DaemonUnavailable(error.to_string()))
                            .await;
                        None
                    }
                }
            }
        }
    };
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
                match establish_subscription_and_refresh(&client, &to_ui, policy).await {
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
                }
            }
        }
    }
}

async fn request_status(
    client: &DaemonClient,
    policy: ConnectionPolicy,
) -> Result<routelane::ipc::DaemonStatus, IpcError> {
    let response = send_with_timeout(client, DaemonRequest::GetStatus, policy).await?;
    if let Some(error) = response.error {
        return Err(error);
    }
    match response.result {
        Some(DaemonResult::Status(status)) => Ok(status),
        _ => Err(IpcError::new(
            "IPC_INVALID_RESPONSE",
            "status request returned an unexpected response",
        )),
    }
}

async fn request_profiles(
    client: &DaemonClient,
    policy: ConnectionPolicy,
) -> Result<Vec<routelane::domain::Profile>, IpcError> {
    let response = send_with_timeout(client, DaemonRequest::ListProfiles, policy).await?;
    if let Some(error) = response.error {
        return Err(error);
    }
    match response.result {
        Some(DaemonResult::Profiles(profiles)) => Ok(profiles),
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
) -> Result<u64, IpcError> {
    let status = request_status(client, policy).await?;
    let profiles = request_profiles(client, policy).await?;
    let generation = status.generation;
    let _ = sender.send(DaemonToUi::Status(status)).await;
    let _ = sender.send(DaemonToUi::Profiles(profiles)).await;
    Ok(generation)
}

async fn establish_subscription_and_refresh(
    client: &DaemonClient,
    sender: &Sender<DaemonToUi>,
    policy: ConnectionPolicy,
) -> Result<EventSubscription, IpcError> {
    let mut receiver = subscribe_with_timeout(client, policy).await?;
    let snapshot_generation = refresh_daemon_state(client, sender, policy).await?;
    tokio::task::yield_now().await;
    let mut buffered = VecDeque::new();
    while let Ok(event) = receiver.try_recv() {
        if daemon_event_generation(&event) > snapshot_generation {
            buffered.push_back(event);
        }
    }
    Ok(EventSubscription { receiver, buffered })
}

async fn wait_for_daemon(
    client: &DaemonClient,
    sender: &Sender<DaemonToUi>,
    policy: ConnectionPolicy,
) -> Result<EventSubscription, IpcError> {
    let mut last_error = IpcError::new("IPC_CONNECT_FAILED", "daemon did not create its socket");
    for attempt in 0..6 {
        match establish_subscription_and_refresh(client, sender, policy).await {
            Ok(events) => return Ok(events),
            Err(error) => last_error = error,
        }
        tokio::time::sleep(policy.reconnect_delay(attempt)).await;
    }
    Err(last_error)
}

fn daemon_event_generation(event: &routelane::ipc::DaemonEvent) -> u64 {
    match event {
        routelane::ipc::DaemonEvent::StateChanged { generation, .. }
        | routelane::ipc::DaemonEvent::OperationFailed { generation, .. } => *generation,
    }
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
    use super::{establish_subscription_and_refresh, ConnectionPolicy};
    use crate::models::DaemonToUi;
    use async_channel::bounded;
    use routelane::domain::{Profile, ProfileId};
    use routelane::health::HealthReport;
    use routelane::ipc::{
        read_frame, write_frame, DaemonClient, DaemonEnvelope, DaemonEvent, DaemonRequest,
        DaemonResponse, DaemonResult, DaemonStatus,
    };
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::net::UnixListener;

    async fn run_snapshot_scenario(
        event_generation: u64,
    ) -> (Option<DaemonEvent>, Vec<DaemonToUi>, Vec<DaemonRequest>) {
        let runtime = TempDir::new().unwrap();
        let path = runtime.path().join("daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (order_sender, order_receiver) = async_channel::bounded(3);
        tokio::spawn(async move {
            let mut subscription = None;
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let envelope: DaemonEnvelope = read_frame(&mut stream).await.unwrap();
                order_sender.send(envelope.request.clone()).await.unwrap();
                let result = match envelope.request {
                    DaemonRequest::Subscribe => DaemonResult::Subscribed,
                    DaemonRequest::GetStatus => DaemonResult::Status(DaemonStatus {
                        generation: 2,
                        health: HealthReport::inactive(),
                        desired_profile_id: None,
                        applied_profile_id: None,
                        last_error: None,
                    }),
                    DaemonRequest::ListProfiles => DaemonResult::Profiles(vec![Profile {
                        id: ProfileId::from("canonical"),
                        name: "Canonical".to_owned(),
                        enabled: false,
                        lanes: vec![],
                        rules: vec![],
                    }]),
                    other => panic!("unexpected request: {other:?}"),
                };
                write_frame(
                    &mut stream,
                    &DaemonResponse::success(envelope.request_id, result),
                )
                .await
                .unwrap();
                if matches!(envelope.request, DaemonRequest::Subscribe) {
                    write_frame(
                        &mut stream,
                        &DaemonEvent::StateChanged {
                            generation: event_generation,
                            health: HealthReport::applying(),
                        },
                    )
                    .await
                    .unwrap();
                    subscription = Some(stream);
                }
            }
            drop(subscription);
        });
        let client = DaemonClient::new(path);
        let (ui_sender, ui_receiver) = bounded(4);
        let mut events =
            establish_subscription_and_refresh(&client, &ui_sender, ConnectionPolicy::default())
                .await
                .unwrap();
        let event = tokio::time::timeout(Duration::from_millis(100), events.recv())
            .await
            .ok()
            .flatten();
        let ui = vec![
            ui_receiver.recv().await.unwrap(),
            ui_receiver.recv().await.unwrap(),
        ];
        let order = vec![
            order_receiver.recv().await.unwrap(),
            order_receiver.recv().await.unwrap(),
            order_receiver.recv().await.unwrap(),
        ];
        (event, ui, order)
    }

    #[test]
    fn connection_policy_bounds_io_and_exponential_reconnect_delay() {
        let policy = ConnectionPolicy::default();

        assert!(policy.request_timeout <= Duration::from_secs(5));
        assert!(policy.activation_timeout <= Duration::from_secs(10));
        assert_eq!(policy.reconnect_delay(0), Duration::from_millis(250));
        assert_eq!(policy.reconnect_delay(20), policy.maximum_reconnect_delay);
    }

    #[tokio::test]
    async fn buffered_event_not_newer_than_snapshot_is_discarded() {
        let (event, ui, order) = run_snapshot_scenario(1).await;

        assert_eq!(
            order,
            vec![
                DaemonRequest::Subscribe,
                DaemonRequest::GetStatus,
                DaemonRequest::ListProfiles,
            ]
        );
        assert_eq!(event, None);
        assert!(matches!(
            &ui[0],
            DaemonToUi::Status(DaemonStatus { generation: 2, .. })
        ));
        assert!(matches!(
            &ui[1],
            DaemonToUi::Profiles(profiles)
                if profiles.len() == 1 && profiles[0].id == ProfileId::from("canonical")
        ));
    }

    #[tokio::test]
    async fn buffered_event_newer_than_snapshot_is_delivered() {
        let (event, ui, _) = run_snapshot_scenario(3).await;

        assert!(matches!(
            event,
            Some(DaemonEvent::StateChanged { generation: 3, .. })
        ));
        assert!(matches!(
            &ui[0],
            DaemonToUi::Status(DaemonStatus { generation: 2, .. })
        ));
    }

    #[tokio::test]
    async fn refresh_failure_drops_buffered_events_and_partial_snapshot() {
        let runtime = TempDir::new().unwrap();
        let path = runtime.path().join("daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let mut subscription = None;
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let envelope: DaemonEnvelope = read_frame(&mut stream).await.unwrap();
                let response = match envelope.request {
                    DaemonRequest::Subscribe => {
                        DaemonResponse::success(envelope.request_id, DaemonResult::Subscribed)
                    }
                    DaemonRequest::GetStatus => DaemonResponse::success(
                        envelope.request_id,
                        DaemonResult::Status(DaemonStatus {
                            generation: 2,
                            health: HealthReport::inactive(),
                            desired_profile_id: None,
                            applied_profile_id: None,
                            last_error: None,
                        }),
                    ),
                    DaemonRequest::ListProfiles => DaemonResponse::failure(
                        envelope.request_id,
                        routelane::ipc::IpcError::new(
                            "CONFIG_SNAPSHOT_FAILED",
                            "profile snapshot failed",
                        ),
                    ),
                    other => panic!("unexpected request: {other:?}"),
                };
                write_frame(&mut stream, &response).await.unwrap();
                if matches!(envelope.request, DaemonRequest::Subscribe) {
                    write_frame(
                        &mut stream,
                        &DaemonEvent::StateChanged {
                            generation: 1,
                            health: HealthReport::applying(),
                        },
                    )
                    .await
                    .unwrap();
                    subscription = Some(stream);
                }
            }
            drop(subscription);
        });
        let client = DaemonClient::new(path);
        let (ui_sender, ui_receiver) = bounded(4);

        let error =
            establish_subscription_and_refresh(&client, &ui_sender, ConnectionPolicy::default())
                .await
                .unwrap_err();

        assert_eq!(error.code, "CONFIG_SNAPSHOT_FAILED");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), ui_receiver.recv())
                .await
                .is_err(),
            "failed refresh must not expose a partial snapshot or buffered event"
        );
    }
}
