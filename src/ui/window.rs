use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{atomic::Ordering, Arc, Mutex};

use async_channel::{Receiver, Sender};
use glib::signal::Inhibit;
use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};
use routelane::domain::{Profile, ProfileId, Rule, RuleAction, RuleId, RuleSource, RuleTarget};
use routelane::health::HealthReport;
use routelane::ipc::{DaemonEvent, DaemonRequest, DaemonStatus};

use crate::config::{ConfigRepository, Language, PersistenceRequest, PersistenceWorker};
use crate::models::{DaemonToUi, UiToDaemon};
use crate::ui::rule_row::{create_rule_row, set_rule_enabled_request, RuleEdit, RuleRowContext};
use crate::ui::settings::build_settings_dialog;
use crate::ui::tray::{spawn_tray, TrayCommand, APP_ICON_NAME};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthDiagnosticView {
    pub summary: String,
    pub action: Option<String>,
    pub technical_detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionHealthView {
    pub aggregate: String,
    pub apply_generation: Option<String>,
    pub lanes: Vec<(String, String)>,
    pub diagnostics: Vec<HealthDiagnosticView>,
}

pub fn health_view(report: &HealthReport, generation: Option<u64>) -> ConnectionHealthView {
    ConnectionHealthView {
        aggregate: format!("{:?}", report.state),
        apply_generation: generation.map(|value| value.to_string()),
        lanes: report
            .lanes
            .iter()
            .map(|(lane, state)| (lane.as_str().to_owned(), format!("{state:?}")))
            .collect(),
        diagnostics: report
            .diagnostics
            .iter()
            .map(|diagnostic| HealthDiagnosticView {
                summary: diagnostic.summary.clone(),
                action: diagnostic
                    .suggested_action
                    .as_ref()
                    .map(|action| action.description.clone()),
                technical_detail: diagnostic.technical_detail.clone(),
            })
            .collect(),
    }
}

pub fn daemon_unavailable_view(detail: impl Into<String>) -> ConnectionHealthView {
    ConnectionHealthView {
        aggregate: "DaemonUnavailable".to_owned(),
        apply_generation: None,
        lanes: Vec::new(),
        diagnostics: vec![HealthDiagnosticView {
            summary: "RouteLane daemon is unavailable".to_owned(),
            action: Some("Start the RouteLane user service and try again".to_owned()),
            technical_detail: detail.into(),
        }],
    }
}

pub fn configuration_error_view(
    detail: impl Into<String>,
    texts: crate::ui::i18n::Texts,
) -> ConnectionHealthView {
    ConnectionHealthView {
        aggregate: "ConfigurationError".to_owned(),
        apply_generation: None,
        lanes: Vec::new(),
        diagnostics: vec![HealthDiagnosticView {
            summary: texts.configuration_error_summary.to_owned(),
            action: Some(texts.configuration_error_action.to_owned()),
            technical_detail: detail.into(),
        }],
    }
}

fn health_sections(
    config_error: Option<&str>,
    daemon: ConnectionHealthView,
    texts: crate::ui::i18n::Texts,
) -> (Option<ConnectionHealthView>, ConnectionHealthView) {
    (
        config_error.map(|error| configuration_error_view(error, texts)),
        daemon,
    )
}

fn reconcile_profiles(
    document: &mut crate::config::ConfigV2,
    selected: &mut Profile,
    mut canonical: Vec<Profile>,
) {
    if canonical.is_empty() {
        return;
    }
    canonical.sort_by(|left, right| left.id.cmp(&right.id));
    let previous_selected_id = selected.id.clone();
    let selection = canonical
        .iter()
        .find(|profile| profile.id == selected.id)
        .cloned()
        .unwrap_or_else(|| canonical[0].clone());
    for profile in canonical {
        store_profile(document, profile);
    }
    if selection.id != previous_selected_id {
        document
            .profiles
            .retain(|profile| profile.id != previous_selected_id);
    }
    *selected = selection;
}

fn switch_state_after_persistence_failure(applied_profile_id: Option<&ProfileId>) -> bool {
    applied_profile_id.is_some()
}

fn status_is_active(status: &DaemonStatus) -> bool {
    status.applied_profile_id.is_some()
}

#[derive(Debug)]
enum PostSaveAction {
    None,
    Request(DaemonRequest),
}

#[derive(Clone)]
struct RuleListContext {
    group: adw::PreferencesGroup,
    rows: Rc<RefCell<Vec<adw::ActionRow>>>,
    profile: Rc<RefCell<Profile>>,
    routing_active: Rc<Cell<bool>>,
    texts: crate::ui::i18n::Texts,
    save_profile: Rc<dyn Fn(PostSaveAction)>,
}

pub fn build_window(
    app: &adw::Application,
    sender: Sender<UiToDaemon>,
    receiver: Receiver<DaemonToUi>,
) {
    gtk::Window::set_default_icon_name(APP_ICON_NAME);
    let repository = ConfigRepository::user_default();
    let (mut document, load_error) = match repository.load() {
        Ok(document) => (document, None),
        Err(error) => (Default::default(), Some(error.to_string())),
    };
    if document.profiles.is_empty() {
        document.profiles.push(empty_default_profile());
    }
    let language = document.language;
    let texts = language.texts();
    let document = Rc::new(RefCell::new(document));
    let profile = Rc::new(RefCell::new(document.borrow().profiles[0].clone()));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("RouteLane")
        .default_width(720)
        .default_height(720)
        .build();
    let header = adw::HeaderBar::new();
    let title = adw::WindowTitle::builder()
        .title("RouteLane")
        .subtitle(texts.inactive)
        .build();
    header.set_title_widget(Some(&title));
    let active = gtk::Switch::builder()
        .tooltip_text(texts.service_tooltip)
        .valign(gtk::Align::Center)
        .build();
    let updating_switch = Rc::new(Cell::new(false));
    let routing_active = Rc::new(Cell::new(false));
    let last_applied_profile_id = Rc::new(RefCell::new(None::<ProfileId>));
    let settings = gtk::Button::builder()
        .icon_name("preferences-system-symbolic")
        .tooltip_text(texts.settings_title)
        .build();
    settings.add_css_class("flat");
    let editable = load_error.is_none();
    let config_writable = Rc::new(Cell::new(editable));
    active.set_sensitive(editable);
    settings.set_sensitive(editable);
    header.pack_end(&settings);
    header.pack_end(&active);

    let page = adw::PreferencesPage::new();
    let config_error_group = adw::PreferencesGroup::builder()
        .title(texts.configuration_title)
        .build();
    let config_error_content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let config_error_row = adw::ActionRow::new();
    config_error_row.set_child(Some(&config_error_content));
    config_error_group.add(&config_error_row);
    let (initial_config_error, initial_daemon_health) = health_sections(
        load_error.as_deref(),
        health_view(&HealthReport::inactive(), None),
        texts,
    );
    config_error_group.set_visible(initial_config_error.is_some());
    if let Some(error) = initial_config_error {
        render_health(&config_error_content, &error, texts);
    }
    page.add(&config_error_group);
    let health_group = adw::PreferencesGroup::builder()
        .title(texts.health_title)
        .build();
    let health_content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let health_row = adw::ActionRow::new();
    health_row.set_child(Some(&health_content));
    health_group.add(&health_row);
    render_health(&health_content, &initial_daemon_health, texts);
    page.add(&health_group);

    let rules_group = adw::PreferencesGroup::builder()
        .title(texts.rules_title)
        .description(texts.rules_description)
        .build();
    rules_group.set_sensitive(editable);
    let add_row = adw::ActionRow::builder()
        .title(texts.add_address_title)
        .build();
    let target = gtk::Entry::builder()
        .placeholder_text(texts.address_placeholder)
        .hexpand(true)
        .build();
    let actions =
        gtk::StringList::new(&[texts.action_route, texts.action_direct, texts.action_block]);
    let action = gtk::DropDown::builder().model(&actions).build();
    let priority = gtk::SpinButton::with_range(1.0, 4_294_967_295.0, 1.0);
    priority.set_value(100.0);
    let add = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text(texts.add_rule_tooltip)
        .build();
    add.set_sensitive(editable);
    add_row.add_suffix(&target);
    add_row.add_suffix(&action);
    add_row.add_suffix(&priority);
    add_row.add_suffix(&add);
    rules_group.add(&add_row);
    page.add(&rules_group);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&page));
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&header);
    body.append(&toast_overlay);
    window.set_content(Some(&body));

    let persistence = load_error
        .is_none()
        .then(|| PersistenceWorker::spawn(repository));
    let persistence_requests = persistence.as_ref().map(|worker| worker.requests.clone());
    let persistence_results = persistence.as_ref().map(|worker| worker.results.clone());
    let persistence_generation = Rc::new(Cell::new(0_u64));
    let pending_actions = Rc::new(RefCell::new(BTreeMap::<u64, PostSaveAction>::new()));
    let persist_document: Rc<dyn Fn(PostSaveAction)> = {
        let document = document.clone();
        let generation = persistence_generation.clone();
        let pending = pending_actions.clone();
        Rc::new(move |action| {
            let Some(requests) = persistence_requests.clone() else {
                return;
            };
            let next = generation.get().saturating_add(1);
            generation.set(next);
            pending.borrow_mut().insert(next, action);
            let request = PersistenceRequest::new(next, document.borrow().clone());
            glib::MainContext::default().spawn_local(async move {
                if requests.send(request).await.is_err() {
                    log::warn!("configuration persistence worker stopped");
                }
            });
        })
    };
    let save_profile: Rc<dyn Fn(PostSaveAction)> = {
        let document = document.clone();
        let profile = profile.clone();
        let persist_document = persist_document.clone();
        Rc::new(move |action| {
            let current = profile.borrow().clone();
            store_profile(&mut document.borrow_mut(), current);
            persist_document(action);
        })
    };

    if let Some(results) = persistence_results {
        let pending = pending_actions.clone();
        let sender = sender.clone();
        let toast_overlay = toast_overlay.clone();
        let config_error_content = config_error_content.clone();
        let config_error_group = config_error_group.clone();
        let active = active.clone();
        let updating_switch = updating_switch.clone();
        let last_applied_profile_id = last_applied_profile_id.clone();
        let config_writable = config_writable.clone();
        let settings = settings.clone();
        let add = add.clone();
        let rules_group = rules_group.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(result) = results.recv().await {
                let action = pending
                    .borrow_mut()
                    .remove(&result.generation)
                    .unwrap_or(PostSaveAction::None);
                match result.result {
                    Ok(()) => {
                        if let PostSaveAction::Request(request) = action {
                            sender.send(UiToDaemon::Request(request)).await.ok();
                        }
                    }
                    Err(error) => {
                        config_writable.set(false);
                        let restored_active = switch_state_after_persistence_failure(
                            last_applied_profile_id.borrow().as_ref(),
                        );
                        updating_switch.set(true);
                        active.set_active(restored_active);
                        updating_switch.set(false);
                        active.set_sensitive(false);
                        settings.set_sensitive(false);
                        add.set_sensitive(false);
                        rules_group.set_sensitive(false);
                        config_error_group.set_visible(true);
                        render_health(
                            &config_error_content,
                            &configuration_error_view(&error, texts),
                            texts,
                        );
                        toast_overlay
                            .add_toast(&adw::Toast::builder().title(&error).timeout(7).build());
                        sender
                            .send(UiToDaemon::Request(DaemonRequest::GetStatus))
                            .await
                            .ok();
                    }
                }
            }
        });
    }

    let rows = Rc::new(RefCell::new(Vec::<adw::ActionRow>::new()));
    let rule_list = RuleListContext {
        group: rules_group.clone(),
        rows: rows.clone(),
        profile: profile.clone(),
        routing_active: routing_active.clone(),
        texts,
        save_profile: save_profile.clone(),
    };
    for rule in profile.borrow().rules.clone() {
        append_rule(&rule_list, &rule);
    }

    {
        let window = window.clone();
        let profile = profile.clone();
        let save_profile = save_profile.clone();
        let document = document.clone();
        let persist_document = persist_document.clone();
        let rule_list = rule_list.clone();
        let routing_active = routing_active.clone();
        settings.connect_clicked(move |_| {
            let initial_language = document.borrow().language;
            let on_language: Rc<dyn Fn(Language)> = {
                let document = document.clone();
                let persist_document = persist_document.clone();
                Rc::new(move |language| {
                    {
                        document.borrow_mut().language = language;
                    }
                    persist_document(PostSaveAction::None);
                })
            };
            let on_lane_change: Rc<dyn Fn()> = {
                let save_profile = save_profile.clone();
                let rule_list = rule_list.clone();
                let routing_active = routing_active.clone();
                let profile = profile.clone();
                Rc::new(move || {
                    let action = if routing_active.get() {
                        PostSaveAction::Request(apply_request(&profile.borrow()))
                    } else {
                        PostSaveAction::None
                    };
                    save_profile(action);
                    rebuild_rule_rows(&rule_list);
                })
            };
            let dialog = build_settings_dialog(
                &window,
                texts,
                profile.clone(),
                on_lane_change,
                initial_language,
                on_language,
            );
            dialog.present();
        });
    }
    {
        let sender = sender.clone();
        let profile = profile.clone();
        let save_profile = save_profile.clone();
        let updating_switch = updating_switch.clone();
        active.connect_state_set(move |_, state| {
            if updating_switch.get() {
                return Inhibit(false);
            }
            if state {
                save_profile(PostSaveAction::Request(apply_request(&profile.borrow())));
            } else {
                let sender = sender.clone();
                glib::MainContext::default().spawn_local(async move {
                    sender
                        .send(UiToDaemon::Request(DaemonRequest::Deactivate))
                        .await
                        .ok();
                });
            }
            Inhibit(false)
        });
    }
    {
        let profile = profile.clone();
        let save_profile = save_profile.clone();
        let routing_active = routing_active.clone();
        let rule_list = rule_list.clone();
        let target_for_add = target.clone();
        let add_rule = move || {
            let text = target_for_add.text().trim().to_owned();
            let Some(target_value) = parse_target(&text) else {
                return;
            };
            let rule_action = match action.selected() {
                1 => RuleAction::Direct,
                2 => RuleAction::Block,
                _ => profile
                    .borrow()
                    .lanes
                    .first()
                    .map(|lane| RuleAction::Route(lane.id.clone()))
                    .unwrap_or(RuleAction::Direct),
            };
            let rule = Rule {
                id: RuleId::from(format!("rule-{}", uuid::Uuid::new_v4()).as_str()),
                enabled: true,
                priority: priority.value_as_int().max(1) as u32,
                source: RuleSource::Any,
                target: target_value,
                action: rule_action,
            };
            {
                profile.borrow_mut().rules.push(rule.clone());
            }
            let action = if routing_active.get() {
                PostSaveAction::Request(apply_request(&profile.borrow()))
            } else {
                PostSaveAction::None
            };
            save_profile(action);
            append_rule(&rule_list, &rule);
            target_for_add.set_text("");
        };
        add.connect_clicked({
            let add_rule = add_rule.clone();
            move |_| add_rule()
        });
        target.connect_activate(move |_| add_rule());
    }

    {
        let active = active.clone();
        let updating_switch = updating_switch.clone();
        let routing_active = routing_active.clone();
        let title = title.clone();
        let health_content = health_content.clone();
        let profile = profile.clone();
        let rule_list = rule_list.clone();
        let document = document.clone();
        let persist_document = persist_document.clone();
        let last_applied_profile_id = last_applied_profile_id.clone();
        let config_writable = config_writable.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = receiver.recv().await {
                match event {
                    DaemonToUi::Status(status) => {
                        let is_active = status_is_active(&status);
                        *last_applied_profile_id.borrow_mut() = status.applied_profile_id.clone();
                        routing_active.set(is_active);
                        active.set_sensitive(config_writable.get());
                        updating_switch.set(true);
                        active.set_active(is_active);
                        updating_switch.set(false);
                        title.set_subtitle(&format!("{:?}", status.health.state));
                        render_health(
                            &health_content,
                            &health_view(&status.health, Some(status.generation)),
                            texts,
                        );
                    }
                    DaemonToUi::Event(event) => {
                        let (generation, health) = match event {
                            DaemonEvent::StateChanged { generation, health }
                            | DaemonEvent::OperationFailed {
                                generation, health, ..
                            } => (generation, health),
                        };
                        title.set_subtitle(&format!("{:?}", health.state));
                        render_health(
                            &health_content,
                            &health_view(&health, Some(generation)),
                            texts,
                        );
                        let sender = sender.clone();
                        glib::MainContext::default().spawn_local(async move {
                            sender
                                .send(UiToDaemon::Request(DaemonRequest::GetStatus))
                                .await
                                .ok();
                            sender
                                .send(UiToDaemon::Request(DaemonRequest::ListProfiles))
                                .await
                                .ok();
                        });
                    }
                    DaemonToUi::DaemonUnavailable(detail) => {
                        active.set_sensitive(false);
                        title.set_subtitle("DaemonUnavailable");
                        render_health(&health_content, &daemon_unavailable_view(detail), texts);
                    }
                    DaemonToUi::Error(error) => {
                        toast_overlay.add_toast(
                            &adw::Toast::builder()
                                .title(&error.to_string())
                                .timeout(7)
                                .build(),
                        );
                        let sender = sender.clone();
                        glib::MainContext::default().spawn_local(async move {
                            sender
                                .send(UiToDaemon::Request(DaemonRequest::GetStatus))
                                .await
                                .ok();
                            sender
                                .send(UiToDaemon::Request(DaemonRequest::ListProfiles))
                                .await
                                .ok();
                        });
                    }
                    DaemonToUi::Applied(result) => {
                        rebuild_rule_rows(&rule_list);
                        title.set_subtitle(&format!("Applying generation {}", result.generation));
                        let sender = sender.clone();
                        glib::MainContext::default().spawn_local(async move {
                            sender
                                .send(UiToDaemon::Request(DaemonRequest::GetStatus))
                                .await
                                .ok();
                        });
                    }
                    DaemonToUi::Deactivated => {
                        routing_active.set(false);
                        active.set_sensitive(config_writable.get());
                        updating_switch.set(true);
                        active.set_active(false);
                        updating_switch.set(false);
                        render_health(
                            &health_content,
                            &health_view(&HealthReport::inactive(), None),
                            texts,
                        );
                    }
                    DaemonToUi::Profiles(profiles) => {
                        log::debug!("daemon returned {} profile(s)", profiles.len());
                        if !profiles.is_empty() {
                            {
                                let mut document = document.borrow_mut();
                                let mut selected = profile.borrow_mut();
                                reconcile_profiles(&mut document, &mut selected, profiles);
                            }
                            persist_document(PostSaveAction::None);
                            rebuild_rule_rows(&rule_list);
                        }
                    }
                }
            }
        });
    }

    let (tray_sender, tray_receiver) =
        glib::MainContext::channel::<TrayCommand>(glib::PRIORITY_DEFAULT);
    let tray_language = Arc::new(Mutex::new(language));
    let tray_available = spawn_tray(tray_sender, tray_language);
    {
        let window = window.clone();
        let app = app.clone();
        tray_receiver.attach(None, move |command| {
            match command {
                TrayCommand::ShowWindow => window.present(),
                TrayCommand::Quit => app.quit(),
            }
            glib::Continue(true)
        });
    }
    window.connect_close_request(move |window| {
        if tray_available.load(Ordering::Relaxed) {
            window.hide();
            Inhibit(true)
        } else {
            Inhibit(false)
        }
    });
    window.present();
}

fn append_rule(context: &RuleListContext, rule: &Rule) {
    let lanes = context
        .profile
        .borrow()
        .lanes
        .iter()
        .map(|lane| (lane.id.clone(), lane.name.clone()))
        .collect::<Vec<_>>();
    let on_change: Rc<dyn Fn(Rule, RuleEdit)> = {
        let profile = context.profile.clone();
        let save = context.save_profile.clone();
        let routing_active = context.routing_active.clone();
        Rc::new(move |updated, edit| {
            let updated_id = updated.id.clone();
            let changed = {
                let mut profile = profile.borrow_mut();
                if let Some(rule) = profile.rules.iter_mut().find(|rule| rule.id == updated.id) {
                    *rule = updated;
                    true
                } else {
                    false
                }
            };
            if changed {
                let action = if routing_active.get() {
                    match edit {
                        RuleEdit::Enabled(enabled) => {
                            PostSaveAction::Request(set_rule_enabled_request(
                                profile.borrow().id.clone(),
                                updated_id,
                                enabled,
                            ))
                        }
                        RuleEdit::Definition => {
                            PostSaveAction::Request(apply_request(&profile.borrow()))
                        }
                    }
                } else {
                    PostSaveAction::None
                };
                save(action);
            }
        })
    };
    let on_delete: Rc<dyn Fn(RuleId)> = {
        let profile = context.profile.clone();
        let save = context.save_profile.clone();
        let routing_active = context.routing_active.clone();
        Rc::new(move |id| {
            {
                profile.borrow_mut().rules.retain(|rule| rule.id != id);
            }
            let action = if routing_active.get() {
                PostSaveAction::Request(apply_request(&profile.borrow()))
            } else {
                PostSaveAction::None
            };
            save(action);
        })
    };
    let row_context = RuleRowContext {
        lanes,
        texts: context.texts,
        on_change,
        on_delete,
    };
    let row = create_rule_row(rule, row_context);
    context.group.add(&row);
    context.rows.borrow_mut().push(row);
}

fn rebuild_rule_rows(context: &RuleListContext) {
    for row in context.rows.borrow_mut().drain(..) {
        context.group.remove(&row);
    }
    let rules = context.profile.borrow().rules.clone();
    for rule in rules {
        append_rule(context, &rule);
    }
}

fn apply_request(profile: &Profile) -> DaemonRequest {
    let mut candidate = profile.clone();
    candidate.enabled = true;
    DaemonRequest::ApplyProfile(candidate)
}

fn store_profile(document: &mut crate::config::ConfigV2, current: Profile) {
    if let Some(stored) = document
        .profiles
        .iter_mut()
        .find(|stored| stored.id == current.id)
    {
        *stored = current;
    } else {
        document.profiles.push(current);
    }
}

fn parse_target(value: &str) -> Option<RuleTarget> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value == "*" {
        return Some(RuleTarget::Any);
    }
    if let Ok(network) = value.parse() {
        return Some(RuleTarget::Cidr(network));
    }
    if let Ok(address) = value.parse() {
        return Some(RuleTarget::Ip(address));
    }
    value
        .strip_prefix("*.")
        .map(|_| RuleTarget::WildcardDomain(value.to_owned()))
        .or_else(|| Some(RuleTarget::Domain(value.to_ascii_lowercase())))
}

fn render_health(container: &gtk::Box, view: &ConnectionHealthView, texts: crate::ui::i18n::Texts) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    container.append(
        &gtk::Label::builder()
            .label(&view.aggregate)
            .xalign(0.0)
            .build(),
    );
    if let Some(last) = &view.apply_generation {
        container.append(
            &gtk::Label::builder()
                .label(&format!("{}: {}", texts.apply_generation, last))
                .xalign(0.0)
                .build(),
        );
    }
    for (lane, state) in &view.lanes {
        container.append(
            &gtk::Label::builder()
                .label(&format!("{lane}: {state}"))
                .xalign(0.0)
                .build(),
        );
    }
    for diagnostic in &view.diagnostics {
        let summary = diagnostic
            .action
            .as_ref()
            .map(|action| format!("{} — {}", diagnostic.summary, action))
            .unwrap_or_else(|| diagnostic.summary.clone());
        container.append(
            &gtk::Label::builder()
                .label(&summary)
                .xalign(0.0)
                .wrap(true)
                .build(),
        );
        let expander = gtk::Expander::builder()
            .label(texts.technical_details)
            .child(
                &gtk::Label::builder()
                    .label(&diagnostic.technical_detail)
                    .xalign(0.0)
                    .selectable(true)
                    .wrap(true)
                    .build(),
            )
            .build();
        container.append(&expander);
    }
}

fn empty_default_profile() -> Profile {
    Profile {
        id: ProfileId::from("default"),
        name: "Default".to_owned(),
        enabled: false,
        lanes: Vec::new(),
        rules: Vec::new(),
    }
}

#[allow(dead_code)]
fn refreshed_interface_selection(previous_name: Option<&str>, names: &[String]) -> Option<usize> {
    let previous_name = previous_name?;
    names.iter().position(|name| name == previous_name)
}

#[cfg(test)]
mod tests {
    use super::{
        configuration_error_view, daemon_unavailable_view, health_sections, health_view,
        reconcile_profiles, refreshed_interface_selection, status_is_active,
        switch_state_after_persistence_failure,
    };
    use routelane::domain::{HealthState, LaneId, ProfileId};
    use routelane::health::{
        Diagnostic, DiagnosticCode, DiagnosticSubject, HealthReport, LaneHealth, Severity,
        SuggestedAction,
    };
    use routelane::ipc::DaemonStatus;

    #[test]
    fn clears_selection_when_previous_interface_disappears() {
        let names = ["wlan0".to_owned(), "tun0".to_owned()];
        assert_eq!(refreshed_interface_selection(Some("old-vpn"), &names), None);
    }

    #[test]
    fn preserves_selection_when_previous_interface_still_exists() {
        let names = ["wlan0".to_owned(), "tun0".to_owned()];
        assert_eq!(refreshed_interface_selection(Some("tun0"), &names), Some(1));
    }

    #[test]
    fn presents_aggregate_lane_diagnostic_action_and_technical_detail() {
        let lane_id = LaneId::from("work");
        let lanes = LaneHealth::from([(lane_id.clone(), HealthState::Degraded)]);
        let report = HealthReport::from_parts(
            HealthState::Degraded,
            lanes,
            vec![Diagnostic::new(
                DiagnosticCode::LaneInterfaceMissing,
                "VPN interface is missing",
                "interface tun0 was not present in the live topology",
                Severity::Warning,
                DiagnosticSubject::Lane(lane_id),
                Some(SuggestedAction::new("Connect the VPN and apply again")),
            )],
        );
        let view = health_view(&report, Some(42));
        assert_eq!(view.aggregate, "Degraded");
        assert_eq!(view.apply_generation.as_deref(), Some("42"));
        assert_eq!(view.lanes, vec![("work".to_owned(), "Degraded".to_owned())]);
        assert_eq!(view.diagnostics[0].summary, "VPN interface is missing");
        assert_eq!(
            view.diagnostics[0].action.as_deref(),
            Some("Connect the VPN and apply again")
        );
        assert_eq!(
            view.diagnostics[0].technical_detail,
            "interface tun0 was not present in the live topology"
        );
    }

    #[test]
    fn reports_daemon_unavailable_after_activation_failure() {
        let view = daemon_unavailable_view("systemd user activation failed");
        assert_eq!(view.aggregate, "DaemonUnavailable");
        assert_eq!(
            view.diagnostics[0].technical_detail,
            "systemd user activation failed"
        );
    }

    #[test]
    fn configuration_failure_is_visible_and_not_presented_as_an_empty_config() {
        let view = configuration_error_view(
            "configuration is not owned by the current user",
            crate::config::Language::En.texts(),
        );
        assert_eq!(view.aggregate, "ConfigurationError");
        assert!(view.diagnostics[0].technical_detail.contains("not owned"));
    }

    #[test]
    fn failed_health_does_not_hide_a_previously_applied_profile() {
        let status = DaemonStatus {
            generation: 7,
            health: HealthReport::from_parts(HealthState::Failed, Default::default(), vec![]),
            desired_profile_id: Some(ProfileId::from("new")),
            applied_profile_id: Some(ProfileId::from("old")),
            last_error: None,
        };

        assert!(status_is_active(&status));
    }

    #[test]
    fn configuration_error_remains_visible_after_daemon_status() {
        let daemon = health_view(&HealthReport::inactive(), Some(9));
        let (config, daemon) = health_sections(
            Some("invalid JSON"),
            daemon,
            crate::config::Language::En.texts(),
        );

        assert_eq!(config.unwrap().aggregate, "ConfigurationError");
        assert_eq!(daemon.aggregate, "Inactive");
    }

    #[test]
    fn configuration_validation_summary_is_localized() {
        let view = configuration_error_view("ayrıntı", crate::config::Language::Tr.texts());

        assert_eq!(
            view.diagnostics[0].summary,
            "RouteLane yapılandırması yüklenemedi"
        );
        assert_eq!(
            view.diagnostics[0].action.as_deref(),
            Some("Düzenlemeden önce dosya sahipliğini, izinleri veya JSON içeriğini düzeltin")
        );
    }

    #[test]
    fn canonical_profile_survives_a_later_language_save() {
        let old = super::empty_default_profile();
        let mut canonical = old.clone();
        canonical.name = "Daemon canonical".to_owned();
        let mut document = crate::config::ConfigV2 {
            profiles: vec![old.clone()],
            ..Default::default()
        };
        let mut selected = old;

        reconcile_profiles(&mut document, &mut selected, vec![canonical.clone()]);
        document.language = crate::config::Language::En;

        assert_eq!(selected, canonical);
        assert_eq!(document.profiles[0], canonical);
    }

    #[test]
    fn profile_reconciliation_prefers_matching_id_then_deterministic_lowest_id() {
        let mut selected = super::empty_default_profile();
        selected.id = ProfileId::from("selected");
        let mut matching = selected.clone();
        matching.name = "Matching".to_owned();
        let mut alpha = selected.clone();
        alpha.id = ProfileId::from("alpha");
        alpha.name = "Alpha".to_owned();
        let mut document = crate::config::ConfigV2::default();

        reconcile_profiles(
            &mut document,
            &mut selected,
            vec![alpha.clone(), matching.clone()],
        );
        assert_eq!(selected, matching);

        selected.id = ProfileId::from("missing");
        document.profiles.push(selected.clone());
        reconcile_profiles(&mut document, &mut selected, vec![matching, alpha.clone()]);
        assert_eq!(selected, alpha);
        assert!(!document
            .profiles
            .iter()
            .any(|profile| profile.id == ProfileId::from("missing")));

        let unchanged = selected.clone();
        reconcile_profiles(&mut document, &mut selected, vec![]);
        assert_eq!(selected, unchanged);
    }

    #[test]
    fn failed_persistence_restores_switch_from_last_daemon_status() {
        assert!(!switch_state_after_persistence_failure(None));
        assert!(switch_state_after_persistence_failure(Some(
            &ProfileId::from("applied")
        )));
    }
}
