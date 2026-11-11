//! An explicit settings operation, separate from RouteLane routing activation.
use super::{confirm::confirm_destructive_action_with_label, i18n::Texts};
use gtk4 as gtk;
use libadwaita::{self as adw, prelude::*};
use routelane::{
    domain::{PrimarySelection, Profile},
    system_primary::{self as network, Approval, Error, ErrorKind, Inspection},
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

#[derive(Clone)]
enum Job {
    Inspect(String),
    Make(Approval),
    Restore(Approval),
}

type Runner = Rc<dyn Fn(Job, async_channel::Sender<network::Result<Inspection>>)>;
type Changed = Rc<dyn Fn()>;

fn execute(job: Job, sender: async_channel::Sender<network::Result<Inspection>>) {
    std::thread::spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::new(ErrorKind::Unavailable, e.to_string()))
            .and_then(|runtime| {
                runtime.block_on(async {
                    tokio::time::timeout(Duration::from_secs(120), async {
                        let mut manager = network::NetworkManager::connect().await?;
                        let path = network::journal_path()?;
                        let interface = match job {
                            Job::Inspect(interface) => interface,
                            Job::Make(approval) => {
                                network::make_primary(&mut manager, &path, &approval).await?;
                                approval.identity.interface
                            }
                            Job::Restore(approval) => {
                                network::restore(&mut manager, &path, &approval).await?;
                                approval.identity.interface
                            }
                        };
                        network::inspect(&mut manager, &path, &interface).await
                    })
                    .await
                    .unwrap_or_else(|_| {
                        Err(Error::new(
                            ErrorKind::Incomplete,
                            "NetworkManager operation timed out; outcome must be inspected",
                        ))
                    })
                })
            });
        let _ = sender.send_blocking(result);
    });
}

pub struct SystemPrimary {
    pub root: adw::PreferencesGroup,
    parent: glib::WeakRef<adw::ApplicationWindow>,
    profile: Rc<RefCell<Profile>>,
    texts: Texts,
    summary: gtk::Label,
    make: gtk::Button,
    undo: gtk::Button,
    refresh: gtk::Button,
    view: RefCell<Option<Inspection>>,
    last_error: RefCell<Option<Error>>,
    notice: RefCell<Option<&'static str>>,
    selected: RefCell<Option<String>>,
    reading: Cell<bool>,
    mutating: Cell<bool>,
    confirming: Cell<bool>,
    editable: Cell<bool>,
    changed: RefCell<Changed>,
    runner: Runner,
}

impl SystemPrimary {
    pub fn new(
        parent: &adw::ApplicationWindow,
        profile: Rc<RefCell<Profile>>,
        texts: Texts,
    ) -> Rc<Self> {
        Self::with_runner(parent, profile, texts, Rc::new(execute))
    }
    fn with_runner(
        parent: &adw::ApplicationWindow,
        profile: Rc<RefCell<Profile>>,
        texts: Texts,
        runner: Runner,
    ) -> Rc<Self> {
        let t = texts.system_primary;
        let root = adw::PreferencesGroup::builder()
            .title(t.title)
            .description(t.description)
            .build();
        root.set_widget_name("system-primary");
        let summary = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .label(t.choose)
            .build();
        summary.set_widget_name("system-primary-status");
        root.add(&summary);
        let buttons = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let make = gtk::Button::with_label(t.make);
        make.set_widget_name("system-primary-make");
        make.add_css_class("suggested-action");
        let undo = gtk::Button::with_label(t.undo);
        undo.set_widget_name("system-primary-undo");
        let refresh = gtk::Button::with_label(texts.ux.refresh);
        refresh.set_widget_name("system-primary-refresh");
        for button in [&make, &undo, &refresh] {
            super::form::wrap_button(button);
            button.set_halign(gtk::Align::Start);
            buttons.append(button);
        }
        root.add(&buttons);
        let panel = Rc::new(Self {
            root,
            parent: parent.downgrade(),
            profile,
            texts,
            summary,
            make,
            undo,
            refresh,
            view: RefCell::new(None),
            last_error: RefCell::new(None),
            notice: RefCell::new(None),
            selected: RefCell::new(None),
            reading: Cell::new(false),
            mutating: Cell::new(false),
            confirming: Cell::new(false),
            editable: Cell::new(false),
            changed: RefCell::new(Rc::new(|| {})),
            runner,
        });
        for (button, restore) in [(&panel.make, false), (&panel.undo, true)] {
            let weak = Rc::downgrade(&panel);
            button.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.confirm(restore);
                }
            });
        }
        let weak = Rc::downgrade(&panel);
        panel.refresh.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.last_error.borrow_mut().take();
                panel.refresh();
            }
        });
        let weak = Rc::downgrade(&panel);
        glib::timeout_add_seconds_local(10, move || {
            let Some(panel) = weak.upgrade() else {
                return glib::Continue(false);
            };
            if panel.parent.upgrade().is_some_and(|w| w.is_visible()) {
                panel.refresh();
            }
            glib::Continue(true)
        });
        panel.render();
        panel
    }
    pub fn is_busy(&self) -> bool {
        self.mutating.get()
    }
    pub fn observed_default(&self) -> Option<String> {
        self.view
            .borrow()
            .as_ref()
            .and_then(|view| view.default_interface.clone())
    }
    pub fn attention(&self) -> Option<String> {
        if self.mutating.get() {
            return None;
        }
        if let Some(error) = self.last_error.borrow().as_ref() {
            return (!matches!(error.kind, ErrorKind::Unsupported | ErrorKind::Unavailable))
                .then(|| self.error_text(error.kind).to_owned());
        }
        self.view
            .borrow()
            .as_ref()
            .filter(|view| {
                view.default_interface.as_deref() != Some(&view.approval.identity.interface)
                    || (view.has_record && !view.can_restore)
            })
            .map(|view| {
                format!(
                    "{} · {} → {}",
                    if view.has_record && !view.can_restore {
                        self.texts.system_primary.conflict
                    } else {
                        self.texts.system_primary.mismatch
                    },
                    self.connection_label(&view.approval.identity.interface),
                    self.connection_label(view.default_interface.as_deref().unwrap_or("—"))
                )
            })
    }
    fn error_text(&self, kind: ErrorKind) -> &'static str {
        let t = self.texts.system_primary;
        match kind {
            ErrorKind::Unsupported => t.unsupported,
            ErrorKind::Disconnected => t.disconnected,
            ErrorKind::Conflict | ErrorKind::Stale => t.conflict,
            ErrorKind::Authorization => t.denied,
            ErrorKind::Storage | ErrorKind::Incomplete => t.incomplete,
            ErrorKind::Unavailable => t.unavailable,
        }
    }
    pub fn connect_changed(&self, callback: Changed) {
        *self.changed.borrow_mut() = callback;
    }
    fn selected_interface(&self) -> Option<String> {
        let profile = self.profile.borrow();
        match &profile.routing.primary {
            PrimarySelection::Lane(id) => profile
                .lanes
                .iter()
                .find(|lane| &lane.id == id)
                .map(|lane| lane.interface.as_str().to_owned()),
            PrimarySelection::FollowSystemDefault => None,
        }
    }
    fn connection_label(&self, interface: &str) -> String {
        self.profile
            .borrow()
            .lanes
            .iter()
            .find(|lane| lane.interface.as_str() == interface)
            .map(|lane| format!("{} ({interface})", lane.name))
            .unwrap_or_else(|| interface.to_owned())
    }
    pub fn update(self: &Rc<Self>, editable: bool) {
        self.editable.set(editable);
        let selected = self.selected_interface();
        if *self.selected.borrow() != selected {
            *self.selected.borrow_mut() = selected;
            self.view.borrow_mut().take();
            self.last_error.borrow_mut().take();
            self.notice.borrow_mut().take();
            self.refresh();
        }
        self.render();
    }
    fn refresh(self: &Rc<Self>) {
        if self.mutating.get() || self.reading.get() || self.confirming.get() {
            return;
        }
        let Some(interface) = self.selected_interface() else {
            self.render();
            return;
        };
        self.start(Job::Inspect(interface));
    }
    fn start(self: &Rc<Self>, job: Job) {
        let mutation = !matches!(job, Job::Inspect(_));
        let restoring = matches!(job, Job::Restore(_));
        if self.reading.get() || self.mutating.get() {
            return;
        }
        let selected = self.selected_interface();
        self.reading.set(!mutation);
        self.mutating.set(mutation);
        if mutation {
            self.view.borrow_mut().take();
            self.last_error.borrow_mut().take();
            self.notice.borrow_mut().take();
            (self.changed.borrow())();
        }
        self.render();
        let (sender, receiver) = async_channel::bounded(1);
        (self.runner)(job, sender);
        let weak = Rc::downgrade(self);
        glib::MainContext::ref_thread_default().spawn_local(async move {
            let result = receiver.recv().await;
            let Some(panel) = weak.upgrade() else {
                return;
            };
            panel.reading.set(false);
            panel.mutating.set(false);
            if selected != panel.selected_interface() {
                panel.view.borrow_mut().take();
                panel.refresh();
            } else {
                match result {
                    Ok(Ok(view)) => {
                        *panel.view.borrow_mut() = Some(view);
                        if mutation {
                            *panel.notice.borrow_mut() = Some(if restoring {
                                panel.texts.system_primary.restored
                            } else {
                                panel.texts.system_primary.confirmed
                            });
                        }
                    }
                    Ok(Err(error)) => {
                        panel.view.borrow_mut().take();
                        *panel.last_error.borrow_mut() = Some(error);
                    }
                    Err(_) => {
                        panel.view.borrow_mut().take();
                        *panel.last_error.borrow_mut() =
                            Some(Error::new(ErrorKind::Unavailable, "worker closed"));
                    }
                }
                panel.render();
                // Read-only reconciliation exposes Undo after a partial write.
                if mutation && panel.last_error.borrow().is_some() {
                    panel.refresh();
                }
            }
            (panel.changed.borrow())();
        });
    }
    fn confirm(self: &Rc<Self>, restore: bool) {
        if !self.editable.get()
            || self.reading.get()
            || self.mutating.get()
            || self.confirming.get()
        {
            return;
        }
        let Some(view) = self.view.borrow().clone() else {
            return;
        };
        if restore && !view.can_restore {
            return;
        }
        let Some(parent) = self.parent.upgrade() else {
            return;
        };
        self.confirming.set(true);
        let t = self.texts.system_primary;
        let body = if restore {
            t.restore_confirmation.to_owned()
        } else {
            t.confirmation
                .replace("{interface}", &view.approval.identity.interface)
                .replace(
                    "{old}",
                    &view.approval.saved_metric.unwrap_or(-1).to_string(),
                )
                .replace("{new}", &view.approval.metric.to_string())
        };
        let weak = Rc::downgrade(self);
        let cancel = weak.clone();
        confirm_destructive_action_with_label(
            &parent,
            self.texts,
            if restore { t.undo } else { t.make },
            &body,
            if restore { t.undo } else { t.make },
            Rc::new(move || {
                if let Some(panel) = weak.upgrade() {
                    panel.confirming.set(false);
                    if panel.selected_interface().as_deref()
                        != Some(&view.approval.identity.interface)
                        || !panel.editable.get()
                    {
                        panel.refresh();
                        return;
                    }
                    panel.start(if restore {
                        Job::Restore(view.approval.clone())
                    } else {
                        Job::Make(view.approval.clone())
                    });
                }
            }),
            Rc::new(move || {
                if let Some(panel) = cancel.upgrade() {
                    panel.confirming.set(false);
                    panel.render();
                }
            }),
        );
    }
    fn render(&self) {
        let t = self.texts.system_primary;
        let view = self.view.borrow();
        let error = self.last_error.borrow();
        let idle = !self.reading.get() && !self.mutating.get() && !self.confirming.get();
        self.make.set_sensitive(
            self.editable.get()
                && idle
                && view.as_ref().is_some_and(|v| {
                    !v.has_record
                        && (v.approval.saved_metric != Some(v.approval.metric)
                            || v.default_interface.as_deref()
                                != Some(&v.approval.identity.interface))
                }),
        );
        self.undo.set_sensitive(
            self.editable.get() && idle && view.as_ref().is_some_and(|v| v.can_restore),
        );
        self.refresh.set_sensitive(idle);
        let label = if self.mutating.get() {
            t.busy.to_owned()
        } else if let Some(error) = error.as_ref() {
            match error.kind {
                ErrorKind::Unsupported => t.unsupported,
                ErrorKind::Disconnected => t.disconnected,
                ErrorKind::Conflict | ErrorKind::Stale => t.conflict,
                ErrorKind::Authorization => t.denied,
                ErrorKind::Storage | ErrorKind::Incomplete => t.incomplete,
                ErrorKind::Unavailable => t.unavailable,
            }
            .to_owned()
        } else if let Some(view) = view.as_ref() {
            let status = if view.has_record && !view.can_restore {
                t.conflict
            } else if view.default_interface.as_deref() == Some(&view.approval.identity.interface) {
                t.aligned
            } else {
                t.mismatch
            };
            format!(
                "{}{}\n{} → {}",
                self.notice
                    .borrow()
                    .map(|notice| format!("{notice}\n"))
                    .unwrap_or_default(),
                status,
                self.connection_label(&view.approval.identity.interface),
                self.connection_label(view.default_interface.as_deref().unwrap_or("—"))
            )
        } else if self.selected_interface().is_none() {
            t.choose.to_owned()
        } else {
            t.checking.to_owned()
        };
        self.summary.set_label(&label);
        self.summary
            .set_tooltip_text(error.as_ref().map(|e| e.detail.as_str()));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use routelane::domain::{Lane, LaneFailurePolicy};
    fn drain_until(mut done: impl FnMut() -> bool) {
        let start = std::time::Instant::now();
        while !done() {
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "system-priority GTK callback timed out"
            );
            while glib::MainContext::default().pending() {
                glib::MainContext::default().iteration(false);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn find_button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Some(button) = widget
            .downcast_ref::<gtk::Button>()
            .filter(|b| b.label().as_deref() == Some(label))
        {
            return Some(button.clone());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(button) = find_button(&current, label) {
                return Some(button);
            }
            child = current.next_sibling();
        }
        None
    }
    fn dialog(parent: &adw::ApplicationWindow) -> gtk::Window {
        gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.is_visible() && w.transient_for().as_ref() == Some(parent.upcast_ref()))
            .expect("a confirmation dialog")
    }
    /// Runs only inside the existing private-bus GTK smoke. The runner is the
    /// sole external boundary: no NetworkManager or host network is contacted.
    pub fn gtk_contract(app: &adw::Application, texts: Texts) {
        let profile = Rc::new(RefCell::new(Profile {
            id: "work".into(),
            name: "Work".into(),
            enabled: false,
            routing: routelane::domain::RoutingPolicy {
                primary: PrimarySelection::Lane("eth".into()),
                ..Default::default()
            },
            lanes: vec![Lane {
                id: "eth".into(),
                name: "Ethernet".into(),
                interface: "eth0".into(),
                fallback_lane: None,
                failure_policy: LaneFailurePolicy::Block,
            }],
            rules: vec![],
        }));
        let view = Inspection {
            approval: Approval {
                device_path: "/devices/2".into(),
                profile_path: "/profiles/1".into(),
                identity: network::Identity {
                    interface: "eth0".into(),
                    uuid: "ethernet".into(),
                    ifindex: 2,
                    hardware: "aa:bb:cc:dd:ee:ff".into(),
                },
                owner: ":1.15".into(),
                saved_version: 7,
                applied_version: 12,
                saved_hash: [1; 32],
                applied_hash: [2; 32],
                saved_metric: Some(-1),
                applied_metric: Some(-1),
                defaults: vec![("usb0".into(), 113), ("eth0".into(), 114)],
                metric: 50,
            },
            default_interface: Some("usb0".into()),
            can_restore: false,
            has_record: false,
        };
        let seen = Rc::new(RefCell::new(Vec::new()));
        let held = Rc::new(RefCell::new(None));
        let runner: Runner = Rc::new({
            let seen = seen.clone();
            let held = held.clone();
            let view = view.clone();
            move |job, sender| {
                let read = matches!(job, Job::Inspect(_));
                seen.borrow_mut().push(job);
                if read {
                    sender.try_send(Ok(view.clone())).unwrap();
                } else {
                    *held.borrow_mut() = Some(sender);
                }
            }
        });
        let parent = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(560)
            .default_height(500)
            .build();
        let panel = SystemPrimary::with_runner(&parent, profile.clone(), texts, runner);
        parent.set_content(Some(&panel.root));
        parent.present();
        panel.update(true);
        drain_until(|| !panel.reading.get());
        assert!(panel.make.is_sensitive());
        crate::ui::window::tests::screenshot(
            &parent,
            &format!(
                "system-primary-{}.png",
                if texts.language == routelane::config::Language::Tr {
                    "tr"
                } else {
                    "en"
                }
            ),
        );
        panel.make.emit_clicked();
        panel.make.emit_clicked();
        let confirmation = dialog(&parent);
        assert_eq!(
            confirmation
                .default_widget()
                .unwrap()
                .downcast::<gtk::Button>()
                .unwrap()
                .label()
                .as_deref(),
            Some(texts.cancel)
        );
        confirmation.close();
        assert_eq!(
            seen.borrow()
                .iter()
                .filter(|j| matches!(j, Job::Make(_)))
                .count(),
            0,
            "cancel sends no mutation"
        );
        // A changed selection invalidates even an already displayed approval.
        panel.make.emit_clicked();
        profile.borrow_mut().routing.primary = PrimarySelection::FollowSystemDefault;
        find_button(dialog(&parent).upcast_ref(), texts.system_primary.make)
            .unwrap()
            .emit_clicked();
        assert!(!seen.borrow().iter().any(|j| matches!(j, Job::Make(_))));
        profile.borrow_mut().routing.primary = PrimarySelection::Lane("eth".into());
        panel.update(true);
        drain_until(|| !panel.reading.get());
        panel.make.emit_clicked();
        let confirm = find_button(dialog(&parent).upcast_ref(), texts.system_primary.make).unwrap();
        confirm.emit_clicked();
        confirm.emit_clicked();
        panel.make.emit_clicked();
        assert!(panel.is_busy());
        assert!(
            !panel.make.is_sensitive()
                && !panel.undo.is_sensitive()
                && !panel.refresh.is_sensitive()
        );
        assert_eq!(
            seen.borrow()
                .iter()
                .filter(|j| matches!(j, Job::Make(_)))
                .count(),
            1
        );
        let mut changed = view.clone();
        changed.can_restore = true;
        changed.has_record = true;
        changed.default_interface = Some("eth0".into());
        changed.approval.saved_metric = Some(50);
        held.borrow_mut()
            .take()
            .unwrap()
            .try_send(Ok(changed.clone()))
            .unwrap();
        drain_until(|| !panel.is_busy());
        assert!(panel.undo.is_sensitive());
        assert!(!panel.make.is_sensitive());
        panel.undo.emit_clicked();
        find_button(dialog(&parent).upcast_ref(), texts.system_primary.undo)
            .unwrap()
            .emit_clicked();
        assert_eq!(
            seen.borrow()
                .iter()
                .filter(|j| matches!(j, Job::Restore(_)))
                .count(),
            1
        );
        held.borrow_mut()
            .take()
            .unwrap()
            .try_send(Ok(view))
            .unwrap();
        drain_until(|| !panel.is_busy());
        assert!(!panel.undo.is_sensitive());
        parent.destroy();
    }
}
