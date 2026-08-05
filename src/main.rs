mod models;
mod ui;

use std::{cell::RefCell, rc::Rc, time::Duration};

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
    minimum_sequence: u64,
}

impl EventSubscription {
    async fn recv(&mut self) -> Option<routelane::ipc::DaemonEvent> {
        while let Some(event) = self.receiver.recv().await {
            let sequence = daemon_event_sequence(&event);
            if sequence > self.minimum_sequence {
                self.minimum_sequence = sequence;
                return Some(event);
            }
        }
        None
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
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("{info}");
        default_panic_hook(info);
    }));

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
        let main_window = Rc::new(RefCell::new(None::<adw::ApplicationWindow>));
        app.connect_activate(move |app| {
            let has_existing_window = main_window.borrow().is_some();
            match ui::experience::activation_action(has_existing_window) {
                ui::experience::ActivationAction::PresentExisting => {
                    let existing = main_window.borrow().clone();
                    if let Some(window) = existing {
                        window.present();
                    }
                }
                ui::experience::ActivationAction::CreateWindow => {
                    let window =
                        ui::window::build_window(app, ui_sender.clone(), daemon_receiver.clone());
                    *main_window.borrow_mut() = Some(window);
                }
            }
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
    let mut connection_epoch = 0;

    let mut subscription =
        match establish_subscription_and_refresh(&client, &to_ui, policy, &mut connection_epoch)
            .await
        {
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
                    match wait_for_daemon(&client, &to_ui, policy, &mut connection_epoch).await {
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
                                close_subscription(&mut subscription);
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
                        close_subscription(&mut subscription);
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
                match establish_subscription_and_refresh(
                    &client,
                    &to_ui,
                    policy,
                    &mut connection_epoch,
                ).await {
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
) -> Result<(u64, u64), IpcError> {
    let status = request_status(client, policy).await?;
    let profiles = request_profiles(client, policy).await?;
    let generation = status.generation;
    let event_sequence = status.event_sequence;
    let _ = sender.send(DaemonToUi::Status(status)).await;
    let _ = sender.send(DaemonToUi::Profiles(profiles)).await;
    Ok((generation, event_sequence))
}

async fn establish_subscription_and_refresh(
    client: &DaemonClient,
    sender: &Sender<DaemonToUi>,
    policy: ConnectionPolicy,
    connection_epoch: &mut u64,
) -> Result<EventSubscription, IpcError> {
    let receiver = subscribe_with_timeout(client, policy).await?;
    let epoch = advance_connection_epoch(connection_epoch);
    let _ = sender
        .send(DaemonToUi::ConnectionEstablished { epoch })
        .await;
    let (_, minimum_sequence) = refresh_daemon_state(client, sender, policy).await?;
    Ok(EventSubscription {
        receiver,
        minimum_sequence,
    })
}

async fn wait_for_daemon(
    client: &DaemonClient,
    sender: &Sender<DaemonToUi>,
    policy: ConnectionPolicy,
    connection_epoch: &mut u64,
) -> Result<EventSubscription, IpcError> {
    let mut last_error = IpcError::new("IPC_CONNECT_FAILED", "daemon did not create its socket");
    for attempt in 0..6 {
        match establish_subscription_and_refresh(client, sender, policy, connection_epoch).await {
            Ok(events) => return Ok(events),
            Err(error) => last_error = error,
        }
        tokio::time::sleep(policy.reconnect_delay(attempt)).await;
    }
    Err(last_error)
}

fn advance_connection_epoch(epoch: &mut u64) -> u64 {
    *epoch = epoch
        .checked_add(1)
        .expect("daemon connection epoch exhausted");
    *epoch
}

fn close_subscription(subscription: &mut Option<EventSubscription>) {
    subscription.take();
}

fn daemon_event_sequence(event: &routelane::ipc::DaemonEvent) -> u64 {
    match event {
        routelane::ipc::DaemonEvent::StateChanged { sequence, .. }
        | routelane::ipc::DaemonEvent::OperationFailed { sequence, .. } => *sequence,
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
        Some(DaemonResult::ApplicationLaunched(result)) => {
            Some(DaemonToUi::ApplicationLaunched(result))
        }
        Some(DaemonResult::ApplicationProcessesAdopted(result)) => {
            Some(DaemonToUi::ApplicationProcessesAdopted(result))
        }
        Some(DaemonResult::OrphanedResourcesRecovered(result)) => {
            Some(DaemonToUi::OrphanedResourcesRecovered(result))
        }
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
    use super::{
        advance_connection_epoch, close_subscription, establish_subscription_and_refresh,
        ConnectionPolicy, EventSubscription,
    };
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
        event_sequences: Vec<u64>,
    ) -> (Vec<DaemonEvent>, Vec<DaemonToUi>, Vec<DaemonRequest>) {
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
                        event_sequence: 5,
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
                    subscription = Some(stream);
                } else if matches!(envelope.request, DaemonRequest::ListProfiles) {
                    for &sequence in &event_sequences {
                        write_frame(
                            subscription.as_mut().unwrap(),
                            &DaemonEvent::StateChanged {
                                generation: 2,
                                sequence,
                                health: HealthReport::applying(),
                            },
                        )
                        .await
                        .unwrap();
                    }
                }
            }
            drop(subscription);
        });
        let client = DaemonClient::new(path);
        let (ui_sender, ui_receiver) = bounded(4);
        let mut connection_epoch = 0;
        let mut events = establish_subscription_and_refresh(
            &client,
            &ui_sender,
            ConnectionPolicy::default(),
            &mut connection_epoch,
        )
        .await
        .unwrap();
        let mut delivered = Vec::new();
        tokio::time::timeout(Duration::from_millis(100), async {
            while let Some(event) = events.recv().await {
                delivered.push(event);
            }
        })
        .await
        .unwrap();
        let ui = vec![
            ui_receiver.recv().await.unwrap(),
            ui_receiver.recv().await.unwrap(),
            ui_receiver.recv().await.unwrap(),
        ];
        let order = vec![
            order_receiver.recv().await.unwrap(),
            order_receiver.recv().await.unwrap(),
            order_receiver.recv().await.unwrap(),
        ];
        (delivered, ui, order)
    }

    #[test]
    fn connection_policy_bounds_io_and_exponential_reconnect_delay() {
        let policy = ConnectionPolicy::default();

        assert!(policy.request_timeout <= Duration::from_secs(5));
        assert!(policy.activation_timeout <= Duration::from_secs(10));
        assert_eq!(policy.reconnect_delay(0), Duration::from_millis(250));
        assert_eq!(policy.reconnect_delay(20), policy.maximum_reconnect_delay);
    }

    #[test]
    fn connection_epoch_is_monotonic_for_each_successful_subscription() {
        let mut epoch = 0;

        assert_eq!(advance_connection_epoch(&mut epoch), 1);
        assert_eq!(advance_connection_epoch(&mut epoch), 2);
        assert_eq!(epoch, 2);
    }

    #[tokio::test]
    async fn closing_a_subscription_drops_the_previous_event_receiver() {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let mut subscription = Some(EventSubscription {
            receiver,
            minimum_sequence: 0,
        });

        close_subscription(&mut subscription);

        assert!(subscription.is_none());
        assert!(
            sender
                .send(DaemonEvent::StateChanged {
                    sequence: 1,
                    generation: 1,
                    health: HealthReport::applying(),
                })
                .await
                .is_err(),
            "the old socket reader must not retain a live event receiver"
        );
    }

    #[tokio::test]
    async fn buffered_event_not_newer_than_snapshot_is_discarded() {
        let (events, ui, order) = run_snapshot_scenario(vec![5]).await;

        assert_eq!(
            order,
            vec![
                DaemonRequest::Subscribe,
                DaemonRequest::GetStatus,
                DaemonRequest::ListProfiles,
            ]
        );
        assert!(events.is_empty());
        assert!(matches!(
            &ui[0],
            DaemonToUi::ConnectionEstablished { epoch: 1 }
        ));
        assert!(matches!(
            &ui[1],
            DaemonToUi::Status(DaemonStatus { generation: 2, .. })
        ));
        assert!(matches!(
            &ui[2],
            DaemonToUi::Profiles(profiles)
                if profiles.len() == 1 && profiles[0].id == ProfileId::from("canonical")
        ));
    }

    #[tokio::test]
    async fn buffered_event_newer_than_snapshot_is_delivered() {
        let (events, ui, _) = run_snapshot_scenario(vec![5, 6, 6, 4, 7]).await;

        let sequences = events
            .iter()
            .map(|event| match event {
                DaemonEvent::StateChanged { sequence, .. }
                | DaemonEvent::OperationFailed { sequence, .. } => *sequence,
            })
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![6, 7]);
        assert!(matches!(
            &ui[0],
            DaemonToUi::ConnectionEstablished { epoch: 1 }
        ));
        assert!(matches!(
            &ui[1],
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
                            event_sequence: 5,
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
                            sequence: 5,
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
        let mut connection_epoch = 0;

        let error = establish_subscription_and_refresh(
            &client,
            &ui_sender,
            ConnectionPolicy::default(),
            &mut connection_epoch,
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, "CONFIG_SNAPSHOT_FAILED");
        assert!(matches!(
            ui_receiver.recv().await.unwrap(),
            DaemonToUi::ConnectionEstablished { epoch: 1 }
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), ui_receiver.recv())
                .await
                .is_err(),
            "failed refresh must not expose a partial snapshot or buffered event"
        );
    }
}
