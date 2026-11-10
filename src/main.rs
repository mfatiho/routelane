mod models;
mod ui;

use std::{cell::RefCell, rc::Rc, time::Duration};

use async_channel::{bounded, Receiver, Sender};
use libadwaita::{self as adw, prelude::*};
use routelane::ipc::{
    socket_path, DaemonClient, DaemonEnvelope, DaemonRequest, DaemonResult, IpcError,
};

use crate::models::{DaemonToUi, UiRequest, UiToDaemon};

#[derive(Debug, Clone, Copy)]
struct ConnectionPolicy {
    request_timeout: Duration,
    mutation_timeout: Duration,
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
            mutation_timeout: Duration::from_secs(180),
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
                .send(DaemonToUi::DaemonUnavailable {
                    epoch: 0,
                    request_id: None,
                    error,
                })
                .await;
            return;
        }
    };
    run_bridge(from_ui, to_ui, DaemonClient::new(path), policy).await;
}

async fn run_bridge(
    from_ui: Receiver<UiToDaemon>,
    to_ui: Sender<DaemonToUi>,
    client: DaemonClient,
    policy: ConnectionPolicy,
) {
    let mut connection_epoch = 0;

    let mut subscription =
        match establish_subscription_and_refresh(&client, &to_ui, policy, &mut connection_epoch)
            .await
        {
            Ok(events) => Some(events),
            Err(first_error) => {
                if let Err(activation_error) = activate_user_daemon(policy).await {
                    let _ = to_ui
                        .send(DaemonToUi::DaemonUnavailable {
                            epoch: connection_epoch,
                            request_id: None,
                            error: IpcError::new(
                                "DAEMON_ACTIVATION_FAILED",
                                format!(
                            "{first_error}; systemd user activation failed: {activation_error}"
                        ),
                            ),
                        })
                        .await;
                    None
                } else {
                    match wait_for_daemon(&client, &to_ui, policy, &mut connection_epoch).await {
                        Ok(events) => Some(events),
                        Err(error) => {
                            let _ = to_ui
                                .send(DaemonToUi::DaemonUnavailable {
                                    epoch: connection_epoch,
                                    request_id: None,
                                    error,
                                })
                                .await;
                            None
                        }
                    }
                }
            }
        };
    let (delivery_sender, mut deliveries) = tokio::sync::mpsc::channel(64);
    let mut reconnect_attempt = 0;
    let mut next_reconnect = tokio::time::Instant::now();
    loop {
        let reconnecting = subscription.is_none();
        let retry_deadline = next_reconnect;
        tokio::select! {
            command = from_ui.recv() => {
                match command {
                    Ok(UiToDaemon::Request(request)) => {
                        let client = client.clone();
                        let sender = to_ui.clone();
                        let completed = delivery_sender.clone();
                        let epoch = connection_epoch;
                        let poll = matches!(request.request,
                            DaemonRequest::GetUsage { .. } | DaemonRequest::ListEvents { .. });
                        tokio::spawn(async move {
                            let result = deliver_request(&client, request, epoch, &sender, policy).await;
                            let _ = completed.send((epoch, result.is_err() && !poll)).await;
                        });
                    }
                    Ok(UiToDaemon::RefreshSnapshot) => {
                        if let Err(error) = refresh_daemon_state(&client, &to_ui, policy, connection_epoch).await {
                            let _ = to_ui.send(DaemonToUi::DaemonUnavailable { epoch: connection_epoch, request_id: None, error }).await;
                            close_subscription(&mut subscription);
                            next_reconnect = tokio::time::Instant::now() + policy.reconnect_delay(reconnect_attempt);
                            reconnect_attempt = reconnect_attempt.saturating_add(1);
                        }
                    }
                    Ok(UiToDaemon::Shutdown) | Err(_) => break,
                }
            }
            Some((epoch, reconnect)) = deliveries.recv() => {
                if reconnect && epoch == connection_epoch {
                    close_subscription(&mut subscription);
                    next_reconnect = tokio::time::Instant::now()
                        + policy.reconnect_delay(reconnect_attempt);
                    reconnect_attempt = reconnect_attempt.saturating_add(1);
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
                        let _ = to_ui.send(DaemonToUi::Event { epoch: connection_epoch, event }).await;
                    }
                    None => {
                        close_subscription(&mut subscription);
                        let _ = to_ui
                            .send(DaemonToUi::DaemonUnavailable { epoch: connection_epoch, request_id: None, error: IpcError::new("IPC_DISCONNECTED", "daemon event subscription disconnected") })
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
                        let _ = to_ui.send(DaemonToUi::DaemonUnavailable { epoch: connection_epoch, request_id: None, error }).await;
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
    let response =
        send_with_timeout(client, UiRequest::new(DaemonRequest::GetStatus), policy).await?;
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
    let response =
        send_with_timeout(client, UiRequest::new(DaemonRequest::ListProfiles), policy).await?;
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
    epoch: u64,
) -> Result<(u64, u64), IpcError> {
    let status = request_status(client, policy).await?;
    let profiles = request_profiles(client, policy).await?;
    let generation = status.generation;
    let event_sequence = status.event_sequence;
    let _ = sender
        .send(DaemonToUi::Snapshot {
            epoch,
            status,
            profiles,
        })
        .await;
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
    let (_, minimum_sequence) = refresh_daemon_state(client, sender, policy, epoch).await?;
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
        | routelane::ipc::DaemonEvent::OperationFailed { sequence, .. }
        | routelane::ipc::DaemonEvent::PrimaryChanged { sequence, .. }
        | routelane::ipc::DaemonEvent::QuotaCrossed { sequence, .. }
        | routelane::ipc::DaemonEvent::EventRecorded { sequence } => *sequence,
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
    request: UiRequest,
    policy: ConnectionPolicy,
) -> Result<routelane::ipc::DaemonResponse, IpcError> {
    let timeout = match &request.request {
        DaemonRequest::GetStatus
        | DaemonRequest::ListProfiles
        | DaemonRequest::Subscribe
        | DaemonRequest::GetUsage { .. }
        | DaemonRequest::ListEvents { .. } => policy.request_timeout,
        _ => policy.mutation_timeout,
    };
    tokio::time::timeout(
        timeout,
        client.send(DaemonEnvelope::new(request.request_id, request.request)),
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

async fn forward_response(
    response: routelane::ipc::DaemonResponse,
    epoch: u64,
    sender: &Sender<DaemonToUi>,
) {
    let _ = sender.send(DaemonToUi::Response { epoch, response }).await;
}

async fn deliver_request(
    client: &DaemonClient,
    request: UiRequest,
    epoch: u64,
    sender: &Sender<DaemonToUi>,
    policy: ConnectionPolicy,
) -> Result<(), IpcError> {
    let request_id = request.request_id.clone();
    match send_with_timeout(client, request, policy).await {
        Ok(response) => {
            forward_response(response, epoch, sender).await;
            Ok(())
        }
        Err(error) => {
            let _ = sender
                .send(DaemonToUi::DaemonUnavailable {
                    epoch,
                    request_id: Some(request_id),
                    error: error.clone(),
                })
                .await;
            Err(error)
        }
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
    use crate::models::{DaemonToUi, UiRequest};
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

    async fn bridge_fake(
        listener: UnixListener,
        mut events: tokio::sync::mpsc::Receiver<DaemonEvent>,
        mutation_started: async_channel::Sender<()>,
        hold_polls: bool,
    ) {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let envelope: DaemonEnvelope = read_frame(&mut stream).await.unwrap();
            if matches!(envelope.request, DaemonRequest::Subscribe) {
                write_frame(
                    &mut stream,
                    &DaemonResponse::success(envelope.request_id, DaemonResult::Subscribed),
                )
                .await
                .unwrap();
                let mut subscription = stream;
                // A single subscription is expected; reconnects are observed by the UI test.
                tokio::spawn(async move {
                    while let Some(event) = events.recv().await {
                        if write_frame(&mut subscription, &event).await.is_err() {
                            break;
                        }
                    }
                });
                // Keep servicing reconnect attempts without taking the event receiver again.
                events = tokio::sync::mpsc::channel(1).1;
                continue;
            }
            let started = mutation_started.clone();
            tokio::spawn(async move {
                let result = match envelope.request {
                    DaemonRequest::GetStatus => DaemonResult::Status(DaemonStatus {
                        generation: 2,
                        event_sequence: 5,
                        health: HealthReport::inactive(),
                        desired_profile_id: None,
                        applied_profile_id: None,
                        last_error: None,
                        primary: Default::default(),
                    }),
                    DaemonRequest::ListProfiles => DaemonResult::Profiles(vec![]),
                    DaemonRequest::ApplyProfile(_) => {
                        started.send(()).await.unwrap();
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        write_frame(
                            &mut stream,
                            &DaemonResponse::failure(
                                envelope.request_id,
                                routelane::ipc::IpcError::new("DENIED", "authorization denied"),
                            ),
                        )
                        .await
                        .ok();
                        return;
                    }
                    DaemonRequest::GetUsage { .. } | DaemonRequest::ListEvents { .. }
                        if hold_polls =>
                    {
                        std::future::pending::<()>().await;
                        return;
                    }
                    DaemonRequest::GetUsage { .. } => {
                        DaemonResult::Usage(routelane::usage::service::UsageReport {
                            lanes: vec![],
                        })
                    }
                    DaemonRequest::ListEvents { .. } => DaemonResult::Events(vec![]),
                    other => panic!("unexpected request: {other:?}"),
                };
                write_frame(
                    &mut stream,
                    &DaemonResponse::success(envelope.request_id, result),
                )
                .await
                .ok();
            });
        }
    }

    async fn bridge_message(receiver: &async_channel::Receiver<DaemonToUi>) -> DaemonToUi {
        tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn events_and_polls_flow_while_a_mutation_is_waiting() {
        use crate::models::UiToDaemon;
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("bridge.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (events, event_rx) = tokio::sync::mpsc::channel(4);
        let (started, start_rx) = bounded(1);
        let server = tokio::spawn(bridge_fake(listener, event_rx, started, false));
        let (commands, command_rx) = bounded(8);
        let (ui, ui_rx) = bounded(8);
        let bridge = tokio::spawn(super::run_bridge(
            command_rx,
            ui,
            DaemonClient::new(path),
            ConnectionPolicy::default(),
        ));
        assert!(matches!(
            bridge_message(&ui_rx).await,
            DaemonToUi::ConnectionEstablished { epoch: 1 }
        ));
        assert!(matches!(
            bridge_message(&ui_rx).await,
            DaemonToUi::Snapshot { epoch: 1, .. }
        ));
        commands
            .send(UiToDaemon::Request(UiRequest {
                request_id: "apply".into(),
                request: DaemonRequest::ApplyProfile(Profile {
                    id: "work".into(),
                    name: "Work".into(),
                    enabled: true,
                    lanes: vec![],
                    rules: vec![],
                    routing: Default::default(),
                }),
            }))
            .await
            .unwrap();
        start_rx.recv().await.unwrap();
        events
            .send(DaemonEvent::EventRecorded { sequence: 6 })
            .await
            .unwrap();
        for (id, request) in [
            ("usage", DaemonRequest::GetUsage { fast_lane: None }),
            (
                "events",
                DaemonRequest::ListEvents {
                    category: routelane::usage::events::EventCategory::All,
                },
            ),
        ] {
            commands
                .send(UiToDaemon::Request(UiRequest {
                    request_id: id.into(),
                    request,
                }))
                .await
                .unwrap();
        }
        let mut seen = Vec::new();
        for _ in 0..3 {
            match bridge_message(&ui_rx).await {
                DaemonToUi::Event {
                    epoch: 1,
                    event: DaemonEvent::EventRecorded { sequence: 6 },
                } => seen.push("notice".to_owned()),
                DaemonToUi::Response { epoch: 1, response } => {
                    assert_ne!(
                        response.request_id, "apply",
                        "polls and events must precede authorization completion"
                    );
                    assert!(response.error.is_none());
                    seen.push(response.request_id);
                }
                other => panic!("unexpected bridge message: {other:?}"),
            }
        }
        seen.sort();
        assert_eq!(seen, ["events", "notice", "usage"]);
        assert!(
            matches!(bridge_message(&ui_rx).await, DaemonToUi::Response { epoch: 1, response } if response.request_id == "apply")
        );
        commands.send(UiToDaemon::Shutdown).await.unwrap();
        bridge.await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn a_usage_timeout_does_not_reconnect() {
        use crate::models::UiToDaemon;
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("bridge.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (events, event_rx) = tokio::sync::mpsc::channel(4);
        let (started, _) = bounded(1);
        let server = tokio::spawn(bridge_fake(listener, event_rx, started, true));
        let (commands, command_rx) = bounded(8);
        let (ui, ui_rx) = bounded(8);
        let policy = ConnectionPolicy {
            request_timeout: Duration::from_millis(20),
            initial_reconnect_delay: Duration::from_millis(1),
            ..ConnectionPolicy::default()
        };
        let bridge = tokio::spawn(super::run_bridge(
            command_rx,
            ui,
            DaemonClient::new(path),
            policy,
        ));
        assert!(matches!(
            bridge_message(&ui_rx).await,
            DaemonToUi::ConnectionEstablished { epoch: 1 }
        ));
        assert!(matches!(
            bridge_message(&ui_rx).await,
            DaemonToUi::Snapshot { epoch: 1, .. }
        ));
        for (index, request) in [
            DaemonRequest::GetUsage { fast_lane: None },
            DaemonRequest::ListEvents {
                category: routelane::usage::events::EventCategory::All,
            },
        ]
        .into_iter()
        .enumerate()
        {
            let id = format!("poll-{index}");
            commands
                .send(UiToDaemon::Request(UiRequest {
                    request_id: id.clone(),
                    request,
                }))
                .await
                .unwrap();
            assert!(
                matches!(tokio::time::timeout(Duration::from_millis(150), ui_rx.recv()).await.unwrap().unwrap(),
                DaemonToUi::DaemonUnavailable { epoch: 1, request_id: Some(actual), error } if actual == id && error.code == "IPC_TIMEOUT")
            );
            tokio::time::sleep(Duration::from_millis(40)).await;
            events
                .send(DaemonEvent::EventRecorded {
                    sequence: 6 + index as u64,
                })
                .await
                .unwrap();
            assert!(matches!(
                bridge_message(&ui_rx).await,
                DaemonToUi::Event { epoch: 1, .. }
            ));
            assert!(
                ui_rx.try_recv().is_err(),
                "poll failure must not establish another epoch"
            );
        }
        commands.send(UiToDaemon::Shutdown).await.unwrap();
        bridge.await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn forward_response_retains_id_error_and_epoch() {
        let (sender, receiver) = async_channel::bounded(1);
        let response = DaemonResponse::failure(
            "apply-1",
            routelane::ipc::IpcError::new("DAEMON_NEEDS_AUTHORIZATION", "denied"),
        );
        super::forward_response(response.clone(), 7, &sender).await;
        assert!(
            matches!(receiver.recv().await.unwrap(), crate::models::DaemonToUi::Response { epoch: 7, response: actual } if actual == response)
        );
    }

    #[tokio::test]
    async fn atomic_snapshot_requires_both_reads() {
        let (_, ui, order) = run_snapshot_scenario(vec![]).await;
        assert_eq!(
            ui.len(),
            2,
            "one connection notice plus one atomic snapshot"
        );
        assert_eq!(
            order,
            [
                DaemonRequest::Subscribe,
                DaemonRequest::GetStatus,
                DaemonRequest::ListProfiles
            ]
        );
    }

    #[tokio::test]
    async fn transport_failure_identifies_request() {
        let temp = TempDir::new().unwrap();
        let client = DaemonClient::new(temp.path().join("missing.sock"));
        let (sender, receiver) = bounded(1);
        let request = crate::models::UiRequest {
            request_id: "apply-42".into(),
            request: DaemonRequest::Deactivate,
        };
        assert!(
            super::deliver_request(&client, request, 3, &sender, ConnectionPolicy::default())
                .await
                .is_err()
        );
        assert!(
            matches!(receiver.recv().await.unwrap(), crate::models::DaemonToUi::DaemonUnavailable { epoch: 3, request_id: Some(id), .. } if id == "apply-42")
        );
    }

    #[tokio::test]
    async fn refresh_snapshot_sends_only_status_and_profiles() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("refresh.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let worker = tokio::spawn(async move {
            let mut requests = vec![];
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let envelope: DaemonEnvelope = read_frame(&mut stream).await.unwrap();
                requests.push(envelope.request.clone());
                let result = match envelope.request {
                    DaemonRequest::GetStatus => DaemonResult::Status(DaemonStatus {
                        generation: 2,
                        event_sequence: 5,
                        health: HealthReport::inactive(),
                        desired_profile_id: None,
                        applied_profile_id: None,
                        last_error: None,
                        primary: Default::default(),
                    }),
                    DaemonRequest::ListProfiles => DaemonResult::Profiles(vec![]),
                    other => panic!("refresh must not mutate: {other:?}"),
                };
                write_frame(
                    &mut stream,
                    &DaemonResponse::success(envelope.request_id, result),
                )
                .await
                .unwrap();
            }
            requests
        });
        let (sender, receiver) = bounded(1);
        super::refresh_daemon_state(
            &DaemonClient::new(path),
            &sender,
            ConnectionPolicy::default(),
            4,
        )
        .await
        .unwrap();
        assert!(matches!(
            receiver.recv().await.unwrap(),
            DaemonToUi::Snapshot { epoch: 4, .. }
        ));
        assert_eq!(
            worker.await.unwrap(),
            [DaemonRequest::GetStatus, DaemonRequest::ListProfiles]
        );
    }

    #[test]
    fn request_ids_are_not_reused_for_recovery_or_retry() {
        let recover = crate::models::UiRequest::new(DaemonRequest::RecoverOrphanedResources);
        let retry = crate::models::UiRequest::new(DaemonRequest::Deactivate);
        assert_ne!(recover.request_id, retry.request_id);
    }

    #[tokio::test]
    async fn mutation_waits_longer_than_status_for_user_authorization() {
        let runtime = TempDir::new().unwrap();
        let path = runtime.path().join("authorization.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let worker = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let envelope: DaemonEnvelope = read_frame(&mut stream).await.unwrap();
            tokio::time::sleep(Duration::from_millis(80)).await;
            write_frame(
                &mut stream,
                &DaemonResponse::success(envelope.request_id, DaemonResult::Deactivated),
            )
            .await
            .unwrap();
        });
        let policy = ConnectionPolicy {
            request_timeout: Duration::from_millis(20),
            ..ConnectionPolicy::default()
        };
        let request = UiRequest {
            request_id: "user-deactivate-42".into(),
            request: DaemonRequest::Deactivate,
        };
        let result = super::send_with_timeout(&DaemonClient::new(path), request, policy).await;
        assert!(
            result.is_ok(),
            "the short status timeout must not cancel authorization: {result:?}"
        );
        assert_eq!(result.unwrap().request_id, "user-deactivate-42");
        worker.await.unwrap();
    }

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
                        primary: Default::default(),
                    }),
                    DaemonRequest::ListProfiles => DaemonResult::Profiles(vec![Profile {
                        routing: Default::default(),
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
            DaemonToUi::Snapshot { status: DaemonStatus { generation: 2, .. }, profiles, epoch: 1 }
                if profiles.len() == 1 && profiles[0].id == ProfileId::from("canonical")
        ));
    }

    #[tokio::test]
    async fn buffered_event_newer_than_snapshot_is_delivered() {
        let (events, ui, _) = run_snapshot_scenario(vec![5, 6, 6, 4, 7]).await;

        let sequences = events
            .iter()
            .map(super::daemon_event_sequence)
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![6, 7]);
        assert!(matches!(
            &ui[0],
            DaemonToUi::ConnectionEstablished { epoch: 1 }
        ));
        assert!(matches!(
            &ui[1],
            DaemonToUi::Snapshot {
                status: DaemonStatus { generation: 2, .. },
                ..
            }
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
                            primary: Default::default(),
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
