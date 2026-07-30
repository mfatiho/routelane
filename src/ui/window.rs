use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{atomic::Ordering, Arc, Mutex};

use async_channel::{Receiver, Sender};
use glib::signal::Inhibit;
use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};
use routelane::domain::{
    HealthState, Profile, ProfileId, Rule, RuleAction, RuleId, RuleSource, RuleTarget,
};
use routelane::health::HealthReport;
use routelane::ipc::{DaemonEvent, DaemonRequest};

use crate::config::{ConfigRepository, Language};
use crate::models::{DaemonToUi, UiToDaemon};
use crate::ui::rule_row::create_rule_row;
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
    pub last_apply_generation: Option<String>,
    pub lanes: Vec<(String, String)>,
    pub diagnostics: Vec<HealthDiagnosticView>,
}

pub fn health_view(report: &HealthReport, generation: Option<u64>) -> ConnectionHealthView {
    ConnectionHealthView {
        aggregate: format!("{:?}", report.state),
        last_apply_generation: generation.map(|value| value.to_string()),
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
        last_apply_generation: None,
        lanes: Vec::new(),
        diagnostics: vec![HealthDiagnosticView {
            summary: "RouteLane daemon is unavailable".to_owned(),
            action: Some("Start the RouteLane user service and try again".to_owned()),
            technical_detail: detail.into(),
        }],
    }
}

pub fn build_window(
    app: &adw::Application,
    sender: Sender<UiToDaemon>,
    receiver: Receiver<DaemonToUi>,
) {
    gtk::Window::set_default_icon_name(APP_ICON_NAME);
    let repository = ConfigRepository::user_default();
    let mut document = repository.load().unwrap_or_default();
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
    let settings = gtk::Button::builder()
        .icon_name("preferences-system-symbolic")
        .tooltip_text(texts.settings_title)
        .build();
    settings.add_css_class("flat");
    header.pack_end(&settings);
    header.pack_end(&active);

    let page = adw::PreferencesPage::new();
    let health_group = adw::PreferencesGroup::builder()
        .title(texts.health_title)
        .build();
    let health_content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let health_row = adw::ActionRow::new();
    health_row.set_child(Some(&health_content));
    health_group.add(&health_row);
    render_health(
        &health_content,
        &health_view(&HealthReport::inactive(), None),
        texts,
    );
    page.add(&health_group);

    let rules_group = adw::PreferencesGroup::builder()
        .title(texts.rules_title)
        .description(texts.rules_description)
        .build();
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

    let save_profile: Rc<dyn Fn()> = {
        let document = document.clone();
        let profile = profile.clone();
        let active = active.clone();
        let sender = sender.clone();
        Rc::new(move || {
            let mut document = document.borrow_mut();
            let current = profile.borrow().clone();
            if let Some(stored) = document
                .profiles
                .iter_mut()
                .find(|stored| stored.id == current.id)
            {
                *stored = current;
            } else {
                document.profiles.push(current);
            }
            if let Err(error) = repository.save(&document) {
                log::warn!("Could not save RouteLane configuration: {error}");
            }
            if active.is_active() {
                let mut candidate = profile.borrow().clone();
                candidate.enabled = true;
                let sender = sender.clone();
                glib::MainContext::default().spawn_local(async move {
                    sender
                        .send(UiToDaemon::Request(DaemonRequest::ApplyProfile(candidate)))
                        .await
                        .ok();
                });
            }
        })
    };

    let rows = Rc::new(RefCell::new(Vec::<adw::ActionRow>::new()));
    for rule in profile.borrow().rules.clone() {
        append_rule(
            &rules_group,
            &rows,
            &profile,
            &rule,
            &sender,
            routing_active.clone(),
            texts,
            save_profile.clone(),
        );
    }

    {
        let window = window.clone();
        let profile = profile.clone();
        let save_profile = save_profile.clone();
        let document = document.clone();
        settings.connect_clicked(move |_| {
            let initial_language = document.borrow().language;
            let on_language: Rc<dyn Fn(Language)> = {
                let document = document.clone();
                Rc::new(move |language| {
                    document.borrow_mut().language = language;
                    if let Err(error) = ConfigRepository::user_default().save(&document.borrow()) {
                        log::warn!("Could not save language preference: {error}");
                    }
                })
            };
            let dialog = build_settings_dialog(
                &window,
                texts,
                profile.clone(),
                save_profile.clone(),
                initial_language,
                on_language,
            );
            dialog.present();
        });
    }
    {
        let sender = sender.clone();
        let profile = profile.clone();
        let updating_switch = updating_switch.clone();
        active.connect_state_set(move |_, state| {
            if updating_switch.get() {
                return Inhibit(false);
            }
            let request = if state {
                let mut candidate = profile.borrow().clone();
                candidate.enabled = true;
                DaemonRequest::ApplyProfile(candidate)
            } else {
                DaemonRequest::Deactivate
            };
            let sender = sender.clone();
            glib::MainContext::default().spawn_local(async move {
                sender.send(UiToDaemon::Request(request)).await.ok();
            });
            Inhibit(false)
        });
    }
    {
        let rules_group = rules_group.clone();
        let rows = rows.clone();
        let profile = profile.clone();
        let sender = sender.clone();
        let save_profile = save_profile.clone();
        let routing_active = routing_active.clone();
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
            profile.borrow_mut().rules.push(rule.clone());
            save_profile();
            append_rule(
                &rules_group,
                &rows,
                &profile,
                &rule,
                &sender,
                routing_active.clone(),
                texts,
                save_profile.clone(),
            );
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
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = receiver.recv().await {
                match event {
                    DaemonToUi::Status(status) => {
                        routing_active.set(status.applied_profile_id.is_some());
                        updating_switch.set(true);
                        active.set_active(status.applied_profile_id.is_some());
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
                        routing_active.set(!matches!(
                            health.state,
                            HealthState::Inactive | HealthState::Failed
                        ));
                        updating_switch.set(true);
                        active.set_active(!matches!(health.state, HealthState::Inactive));
                        updating_switch.set(false);
                        title.set_subtitle(&format!("{:?}", health.state));
                        render_health(
                            &health_content,
                            &health_view(&health, Some(generation)),
                            texts,
                        );
                    }
                    DaemonToUi::DaemonUnavailable(detail) => {
                        routing_active.set(false);
                        updating_switch.set(true);
                        active.set_active(false);
                        updating_switch.set(false);
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
                    }
                    DaemonToUi::Applied(result) => {
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

fn append_rule(
    group: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<adw::ActionRow>>>,
    profile: &Rc<RefCell<Profile>>,
    rule: &Rule,
    sender: &Sender<UiToDaemon>,
    routing_active: Rc<Cell<bool>>,
    texts: crate::ui::i18n::Texts,
    save: Rc<dyn Fn()>,
) {
    let lanes = profile
        .borrow()
        .lanes
        .iter()
        .map(|lane| (lane.id.clone(), lane.name.clone()))
        .collect::<Vec<_>>();
    let on_change: Rc<dyn Fn(Rule)> = {
        let profile = profile.clone();
        let save = save.clone();
        Rc::new(move |updated| {
            if let Some(rule) = profile
                .borrow_mut()
                .rules
                .iter_mut()
                .find(|rule| rule.id == updated.id)
            {
                *rule = updated;
                save();
            }
        })
    };
    let on_delete: Rc<dyn Fn(RuleId)> = {
        let profile = profile.clone();
        let save = save.clone();
        Rc::new(move |id| {
            profile.borrow_mut().rules.retain(|rule| rule.id != id);
            save();
        })
    };
    let row = create_rule_row(
        rule,
        profile.borrow().id.clone(),
        &lanes,
        sender.clone(),
        routing_active,
        texts,
        on_change,
        on_delete,
    );
    group.add(&row);
    rows.borrow_mut().push(row);
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
    if let Some(last) = &view.last_apply_generation {
        container.append(
            &gtk::Label::builder()
                .label(&format!("{}: {}", texts.last_apply, last))
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
    use super::{daemon_unavailable_view, health_view, refreshed_interface_selection};
    use routelane::domain::{HealthState, LaneId};
    use routelane::health::{
        Diagnostic, DiagnosticCode, DiagnosticSubject, HealthReport, LaneHealth, Severity,
        SuggestedAction,
    };

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
        assert_eq!(view.last_apply_generation.as_deref(), Some("42"));
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
}
