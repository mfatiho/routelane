use crate::models::{DaemonToUi, UiToDaemon};
use crate::ui::session::{AfterSave, UiEffect, UiSession};
use async_channel::Sender;
use routelane::backend::RoutingPreview;
use routelane::config::{ConfigV4, PersistenceRequest, PersistenceWorker};
use routelane::domain::Profile;
use routelane::ipc::{DaemonRequest, DaemonResult, IpcError};
use std::cell::{Cell, Ref, RefCell};
use std::rc::Rc;

pub struct UiController {
    session: RefCell<UiSession>,
    sender: Sender<UiToDaemon>,
    persistence: Option<Sender<PersistenceRequest>>,
    context: glib::MainContext,
    observers: RefCell<Vec<Rc<dyn Fn()>>>,
    recovery: RefCell<Option<Profile>>,
    retry_used: Cell<bool>,
    recovery_started: Cell<bool>,
    retry_after_refresh: Cell<bool>,
    completed_id: RefCell<Option<String>>,
    quit: RefCell<Option<Rc<dyn Fn()>>>,
    /// The outstanding preview request, its draft and the caller's callback.
    preview_ready: RefCell<Option<PreviewWait>>,
}

type PreviewCallback = Box<dyn FnOnce(Result<RoutingPreview, IpcError>)>;

struct PreviewWait {
    request_id: String,
    profile: Profile,
    on_ready: PreviewCallback,
}

impl UiController {
    pub fn new(
        session: UiSession,
        sender: Sender<UiToDaemon>,
        persistence: Option<PersistenceWorker>,
    ) -> Rc<Self> {
        let controller = Rc::new(Self {
            session: RefCell::new(session),
            sender,
            persistence: persistence.as_ref().map(|worker| worker.requests.clone()),
            context: glib::MainContext::ref_thread_default(),
            observers: RefCell::new(vec![]),
            recovery: RefCell::new(None),
            retry_used: Cell::new(false),
            recovery_started: Cell::new(false),
            retry_after_refresh: Cell::new(false),
            completed_id: RefCell::new(None),
            quit: RefCell::new(None),
            preview_ready: RefCell::new(None),
        });
        if let Some(worker) = persistence {
            let weak = Rc::downgrade(&controller);
            controller.context.spawn_local(async move {
                while let Ok(result) = worker.results.recv().await {
                    let Some(controller) = weak.upgrade() else {
                        break;
                    };
                    let effects = controller
                        .session
                        .borrow_mut()
                        .handle_persistence(result.generation, result.result);
                    controller.execute(effects);
                    controller.notify();
                }
            });
        }
        controller
    }
    pub fn session(&self) -> Ref<'_, UiSession> {
        self.session.borrow()
    }
    pub fn connect_changed(&self, callback: Rc<dyn Fn()>) {
        self.observers.borrow_mut().push(callback);
    }
    pub fn connect_quit(&self, callback: Rc<dyn Fn()>) {
        *self.quit.borrow_mut() = Some(callback);
    }
    pub fn last_completed_id(&self) -> Option<String> {
        self.completed_id.borrow().clone()
    }
    fn notify(&self) {
        let callbacks = self.observers.borrow().clone();
        for callback in callbacks {
            callback();
        }
    }

    pub fn save(self: &Rc<Self>, config: ConfigV4, profile: Profile, follow_up: AfterSave) -> bool {
        let apply = matches!(
            &follow_up,
            AfterSave::Request(DaemonRequest::ApplyProfile(_))
        );
        if self.persistence.is_none() {
            self.session
                .borrow_mut()
                .configuration_failed("configuration writer is unavailable".into());
            self.notify();
            return false;
        }
        let result = self
            .session
            .borrow_mut()
            .begin_save(config, profile, follow_up);
        match result {
            Ok(effects) => {
                if apply {
                    self.retry_used.set(false);
                    self.recovery_started.set(false);
                    self.recovery.borrow_mut().take();
                }
                self.execute(effects);
                self.notify();
                true
            }
            Err(_) => false,
        }
    }
    pub fn request(
        self: &Rc<Self>,
        request: DaemonRequest,
        submitted_profile: Option<Profile>,
    ) -> bool {
        self.submit_request(request, submitted_profile, true)
    }
    fn submit_request(
        self: &Rc<Self>,
        request: DaemonRequest,
        submitted_profile: Option<Profile>,
        new_cycle: bool,
    ) -> bool {
        let apply = matches!(request, DaemonRequest::ApplyProfile(_));
        let result = self
            .session
            .borrow_mut()
            .begin_request(request, submitted_profile);
        match result {
            Ok(effects) => {
                if apply && new_cycle {
                    self.retry_used.set(false);
                    self.recovery_started.set(false);
                    self.recovery.borrow_mut().take();
                }
                self.execute(effects);
                self.notify();
                true
            }
            Err(_) => false,
        }
    }
    /// Requests a read-only preview of `profile`; `on_ready` runs once with
    /// the preview bound to this draft and connection, or with the error.
    pub fn preview(self: &Rc<Self>, profile: Profile, on_ready: PreviewCallback) -> bool {
        let result = self.session.borrow_mut().begin_preview(profile.clone());
        let Ok(effects) = result else {
            return false;
        };
        let request_id = effects.iter().find_map(|effect| match effect {
            UiEffect::Send(request) => Some(request.request_id.clone()),
            _ => None,
        });
        if let Some(request_id) = request_id {
            *self.preview_ready.borrow_mut() = Some(PreviewWait {
                request_id,
                profile,
                on_ready,
            });
        }
        self.execute(effects);
        self.notify();
        true
    }

    /// Saves and applies `profile` only while its preview is still current.
    pub fn confirm_activation(self: &Rc<Self>, config: ConfigV4, profile: Profile) -> bool {
        if self.persistence.is_none() {
            return false;
        }
        let result = self
            .session
            .borrow_mut()
            .confirm_activation(config, profile);
        match result {
            Ok(effects) => {
                self.retry_used.set(false);
                self.recovery_started.set(false);
                self.recovery.borrow_mut().take();
                self.execute(effects);
                self.notify();
                true
            }
            Err(_) => false,
        }
    }

    pub fn refresh(self: &Rc<Self>) {
        self.execute(vec![UiEffect::RefreshSnapshot]);
    }
    pub fn recovery_available(&self) -> bool {
        self.recovery.borrow().is_some()
            && !self.recovery_started.get()
            && !self.retry_used.get()
            && !self.retry_after_refresh.get()
    }
    pub fn cancel_recovery(&self) {
        self.recovery.borrow_mut().take();
        self.recovery_started.set(false);
        self.retry_after_refresh.set(false);
        self.notify();
    }
    pub fn confirm_recovery(self: &Rc<Self>) -> bool {
        if !self.recovery_available() {
            return false;
        }
        self.recovery_started.set(true);
        if self.request(DaemonRequest::RecoverOrphanedResources, None) {
            true
        } else {
            self.recovery_started.set(false);
            false
        }
    }

    pub fn receive(self: &Rc<Self>, message: DaemonToUi) {
        let is_snapshot = matches!(message, DaemonToUi::Snapshot { .. });
        if let DaemonToUi::Response { epoch, response } = &message {
            let session = self.session.borrow();
            if session.accepts_reply(*epoch, &response.request_id) {
                let kind = session.operation().map(|(kind, _)| kind);
                if response
                    .error
                    .as_ref()
                    .is_some_and(|error| error.code == "DAEMON_ORPHANED_RESOURCES")
                    && kind == Some(crate::ui::session::OperationKind::Apply)
                    && !self.retry_used.get()
                {
                    *self.recovery.borrow_mut() = session.pending_candidate().cloned();
                }
                if matches!(
                    response.result,
                    Some(DaemonResult::OrphanedResourcesRecovered(_))
                ) && kind == Some(crate::ui::session::OperationKind::Recover)
                {
                    self.retry_after_refresh.set(true);
                }
                *self.completed_id.borrow_mut() = Some(response.request_id.clone());
            }
        }
        let disconnect = matches!(message, DaemonToUi::DaemonUnavailable { .. });
        let preview_reply = match &message {
            DaemonToUi::Response { epoch, response } => self
                .preview_ready
                .borrow()
                .as_ref()
                .is_some_and(|wait| wait.request_id == response.request_id)
                .then_some(*epoch),
            _ => None,
        };
        let reconnect = matches!(message, DaemonToUi::ConnectionEstablished { .. });
        let effects = self.session.borrow_mut().handle_daemon(message);
        if preview_reply.is_some() || reconnect || disconnect {
            if let Some(wait) = self.preview_ready.borrow_mut().take() {
                let outcome = {
                    let session = self.session.borrow();
                    match session.preview_for(&wait.profile) {
                        Some(preview) if preview_reply.is_some() => Ok(preview.clone()),
                        _ => Err(session.last_error().cloned().unwrap_or_else(|| {
                            IpcError::new("UI_PREVIEW_STALE", "the preview is no longer current")
                        })),
                    }
                };
                (wait.on_ready)(outcome);
            }
        }
        // The bridge already retries failed transport with bounded backoff.
        // Do not create an unbounded UI refresh -> failure -> refresh loop.
        if !disconnect {
            self.execute(effects);
        }
        if is_snapshot
            && self.retry_after_refresh.get()
            && self.session().snapshot_known()
            && !self.session().is_busy()
        {
            self.retry_after_refresh.set(false);
            self.retry_used.set(true);
            let candidate = self.recovery.borrow_mut().take();
            if let Some(profile) = candidate {
                self.submit_request(
                    DaemonRequest::ApplyProfile(profile.clone()),
                    Some(profile),
                    false,
                );
            }
        }
        self.notify();
    }

    fn execute(self: &Rc<Self>, effects: Vec<UiEffect>) {
        for effect in effects {
            match effect {
                UiEffect::QuitApplication => {
                    if let Some(quit) = self.quit.borrow().clone() {
                        quit();
                    }
                }
                UiEffect::Persist(request) => {
                    let generation = request.generation;
                    let Some(sender) = self.persistence.clone() else {
                        continue;
                    };
                    let weak = Rc::downgrade(self);
                    self.context.spawn_local(async move {
                        if sender.send(request).await.is_err() {
                            if let Some(controller) = weak.upgrade() {
                                let effects = controller.session.borrow_mut().handle_persistence(
                                    generation,
                                    Err("configuration writer stopped".into()),
                                );
                                controller.execute(effects);
                                controller.notify();
                            }
                        }
                    });
                }
                effect => {
                    let message = match effect {
                        UiEffect::Send(request) => UiToDaemon::Request(request),
                        UiEffect::RefreshSnapshot => UiToDaemon::RefreshSnapshot,
                        _ => unreachable!(),
                    };
                    let id = match &message {
                        UiToDaemon::Request(request) => Some(request.request_id.clone()),
                        _ => None,
                    };
                    let sender = self.sender.clone();
                    let weak = Rc::downgrade(self);
                    self.context.spawn_local(async move {
                        if sender.send(message).await.is_err() {
                            if let Some(controller) = weak.upgrade() {
                                let epoch = controller.session().connection_epoch();
                                controller.receive(DaemonToUi::DaemonUnavailable {
                                    epoch,
                                    request_id: id,
                                    error: IpcError::new(
                                        "UI_BRIDGE_STOPPED",
                                        "desktop bridge stopped",
                                    ),
                                });
                            }
                        }
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::UiRequest;
    use routelane::config::PersistenceResult;
    use routelane::domain::Profile;
    use routelane::health::HealthReport;
    use routelane::ipc::{DaemonResponse, DaemonResult, DaemonStatus};

    fn profile() -> Profile {
        Profile {
            routing: Default::default(),
            id: "work".into(),
            name: "Work".into(),
            enabled: false,
            lanes: vec![],
            rules: vec![],
        }
    }
    fn snapshot(active: bool) -> DaemonToUi {
        DaemonToUi::Snapshot {
            epoch: 1,
            status: DaemonStatus {
                generation: 7,
                event_sequence: 1,
                health: HealthReport::inactive(),
                desired_profile_id: None,
                applied_profile_id: active.then(|| "work".into()),
                last_error: None,
                primary: Default::default(),
            },
            profiles: vec![profile()],
        }
    }
    fn drain(context: &glib::MainContext) {
        for _ in 0..100 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
    }
    fn run(
        active: bool,
        test: impl FnOnce(
            Rc<UiController>,
            async_channel::Receiver<UiToDaemon>,
            async_channel::Receiver<PersistenceRequest>,
            async_channel::Sender<PersistenceResult>,
            &glib::MainContext,
        ),
    ) {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let (sender, requests) = async_channel::unbounded();
                let (writes, written) = async_channel::unbounded();
                let (results, receiver) = async_channel::unbounded();
                let mut session = UiSession::new(profile(), true);
                session.handle_daemon(DaemonToUi::ConnectionEstablished { epoch: 1 });
                session.handle_daemon(snapshot(active));
                let controller = UiController::new(
                    session,
                    sender,
                    Some(PersistenceWorker {
                        requests: writes,
                        results: receiver,
                    }),
                );
                test(controller, requests, written, results, &context);
            })
            .unwrap();
    }

    #[test]
    fn save_then_apply_holds_one_gate() {
        run(false, |controller, requests, writes, results, context| {
            assert!(controller.save(
                ConfigV4::default(),
                profile(),
                AfterSave::Request(DaemonRequest::ApplyProfile(profile()))
            ));
            drain(context);
            let write = writes.try_recv().unwrap();
            assert!(requests.try_recv().is_err());
            assert!(controller.session().is_busy());
            results
                .try_send(PersistenceResult {
                    generation: write.generation,
                    result: Ok(()),
                })
                .unwrap();
            drain(context);
            assert!(matches!(
                requests.try_recv().unwrap(),
                UiToDaemon::Request(UiRequest {
                    request: DaemonRequest::ApplyProfile(_),
                    ..
                })
            ));
            assert!(controller.session().is_busy());
        });
    }

    #[test]
    fn save_failure_sends_no_apply_and_still_allows_deactivate() {
        run(true, |controller, requests, writes, results, context| {
            assert!(controller.save(
                ConfigV4::default(),
                profile(),
                AfterSave::Request(DaemonRequest::ApplyProfile(profile()))
            ));
            drain(context);
            let write = writes.try_recv().unwrap();
            results
                .try_send(PersistenceResult {
                    generation: write.generation,
                    result: Err("permission denied".into()),
                })
                .unwrap();
            drain(context);
            assert!(requests.try_recv().is_err());
            assert!(!controller.session().config_writable());
            assert!(controller.request(DaemonRequest::Deactivate, None));
            drain(context);
            assert!(matches!(
                requests.try_recv().unwrap(),
                UiToDaemon::Request(UiRequest {
                    request: DaemonRequest::Deactivate,
                    ..
                })
            ));
        });
    }

    #[test]
    fn interleaved_status_does_not_reenable_row_actions() {
        run(false, |controller, requests, _, _, context| {
            assert!(controller.request(DaemonRequest::ApplyProfile(profile()), Some(profile())));
            drain(context);
            requests.try_recv().unwrap();
            controller.receive(snapshot(false));
            assert!(controller.session().is_busy());
            assert!(!controller.request(DaemonRequest::Deactivate, None));
            drain(context);
            assert!(requests.try_recv().is_err());
        });
    }

    #[test]
    fn unrelated_error_does_not_trigger_recovery() {
        run(false, |controller, _, _, _, _| {
            controller.receive(DaemonToUi::Response {
                epoch: 1,
                response: DaemonResponse::failure(
                    "other",
                    IpcError::new("DAEMON_ORPHANED_RESOURCES", "orphan"),
                ),
            });
            assert!(!controller.recovery_available());
        });
    }

    #[test]
    fn recovery_cancel_is_readonly() {
        run(false, |controller, requests, _, _, context| {
            controller.request(DaemonRequest::ApplyProfile(profile()), Some(profile()));
            drain(context);
            let id = match requests.try_recv().unwrap() {
                UiToDaemon::Request(r) => r.request_id,
                _ => unreachable!(),
            };
            controller.receive(DaemonToUi::Response {
                epoch: 1,
                response: DaemonResponse::failure(
                    id,
                    IpcError::new("DAEMON_ORPHANED_RESOURCES", "orphan"),
                ),
            });
            drain(context);
            assert!(controller.recovery_available());
            while requests.try_recv().is_ok() {}
            controller.cancel_recovery();
            drain(context);
            assert!(!controller.recovery_available());
            assert!(requests.try_recv().is_err());
        });
    }

    #[test]
    fn recovery_retry_is_bounded() {
        run(false, |controller, requests, writes, results, context| {
            controller.request(DaemonRequest::ApplyProfile(profile()), Some(profile()));
            drain(context);
            let initial = match requests.try_recv().unwrap() {
                UiToDaemon::Request(r) => r,
                _ => unreachable!(),
            };
            controller.receive(DaemonToUi::Response {
                epoch: 1,
                response: DaemonResponse::failure(
                    initial.request_id.clone(),
                    IpcError::new("DAEMON_ORPHANED_RESOURCES", "orphan"),
                ),
            });
            drain(context);
            while requests.try_recv().is_ok() {}
            assert!(controller.confirm_recovery());
            drain(context);
            assert!(
                !controller.recovery_available(),
                "confirmation cannot be shown again during recovery"
            );
            let recovery = match requests.try_recv().unwrap() {
                UiToDaemon::Request(r) => r,
                _ => unreachable!(),
            };
            assert!(matches!(
                recovery.request,
                DaemonRequest::RecoverOrphanedResources
            ));
            controller.receive(DaemonToUi::Response {
                epoch: 1,
                response: DaemonResponse::success(
                    recovery.request_id,
                    DaemonResult::OrphanedResourcesRecovered(
                        routelane::ipc::OrphanRecoveryResult { cleaned: true },
                    ),
                ),
            });
            drain(context);
            while requests.try_recv().is_ok() {}
            controller.receive(snapshot(false));
            drain(context);
            let retry = match requests.try_recv().unwrap() {
                UiToDaemon::Request(r) => r,
                _ => unreachable!(),
            };
            assert!(matches!(retry.request, DaemonRequest::ApplyProfile(_)));
            assert_ne!(retry.request_id, initial.request_id);
            controller.receive(DaemonToUi::Response {
                epoch: 1,
                response: DaemonResponse::failure(
                    retry.request_id,
                    IpcError::new("DAEMON_ORPHANED_RESOURCES", "still orphaned"),
                ),
            });
            assert!(!controller.recovery_available());
            controller.receive(snapshot(false));
            assert!(controller.save(
                ConfigV4::default(),
                profile(),
                AfterSave::Request(DaemonRequest::ApplyProfile(profile()))
            ));
            drain(context);
            while requests.try_recv().is_ok() {}
            let write = writes.try_recv().unwrap();
            results
                .try_send(PersistenceResult {
                    generation: write.generation,
                    result: Ok(()),
                })
                .unwrap();
            drain(context);
            let id = match requests.try_recv().unwrap() {
                UiToDaemon::Request(r) => r.request_id,
                _ => unreachable!(),
            };
            controller.receive(DaemonToUi::Response {
                epoch: 1,
                response: DaemonResponse::failure(
                    id,
                    IpcError::new("DAEMON_ORPHANED_RESOURCES", "orphan"),
                ),
            });
            assert!(
                controller.recovery_available(),
                "a new explicit user apply starts a new bounded recovery cycle"
            );
        });
    }
}
