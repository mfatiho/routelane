use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use async_channel::{Receiver, Sender};
use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};
use routelane::config::{ConfigRepository, ConfigV4, PersistenceWorker};
use routelane::domain::{HealthState, Profile, Rule, RuleAction, RuleId, RuleSource};
use routelane::ipc::{DaemonRequest, DaemonResult};

use crate::models::{DaemonToUi, UiToDaemon};
use crate::ui::application_editor::open_application_editor;
use crate::ui::application_row::{
    application_action_request, create_compact_application_row, ApplicationAction,
    CompactApplicationRow,
};
use crate::ui::confirm::{confirm_destructive_action, confirm_destructive_action_with_label};
use crate::ui::connections::ConnectionsPage;
use crate::ui::controller::UiController;
use crate::ui::desktop_catalog::{application_display_name, load_catalog, DesktopCatalog};
use crate::ui::experience::{dashboard_state, PrimaryAction};
use crate::ui::form::MenuAction;
use crate::ui::i18n::{LanguageTexts, Texts};
use crate::ui::list_page::ListPage;
use crate::ui::overview::{status_copy, Overview};
use crate::ui::rule_editor::{
    action_text, move_rule, open_rule_editor, rule_matches_query, target_text,
};
use crate::ui::rule_row::{
    create_compact_rule_row, set_rule_enabled_request, CompactRuleRow, ReorderDirection,
};
use crate::ui::session::{AfterSave, SaveState, UiSession};
use crate::ui::settings::build_general_settings_dialog;
use crate::ui::shell::{Section, Shell};
use crate::ui::tray::{spawn_tray, TrayCommand, APP_ICON_NAME};

fn empty_default_profile() -> Profile {
    Profile {
        routing: Default::default(),
        id: "default".into(),
        name: "Default".into(),
        enabled: false,
        lanes: vec![],
        rules: vec![],
    }
}

/// The approved lane order, primary first, by display name.
fn failover_order(profile: &Profile, texts: crate::ui::i18n::PrimaryTexts) -> String {
    let lane_name = |id: &routelane::domain::LaneId| {
        profile
            .lanes
            .iter()
            .find(|lane| &lane.id == id)
            .map_or_else(|| id.as_str().to_owned(), |lane| lane.name.clone())
    };
    let primary = match &profile.routing.primary {
        routelane::domain::PrimarySelection::FollowSystemDefault => texts.system_default.to_owned(),
        routelane::domain::PrimarySelection::Lane(id) => lane_name(id),
    };
    std::iter::once(primary)
        .chain(profile.routing.backups.iter().map(lane_name))
        .collect::<Vec<_>>()
        .join(" → ")
}

/// What happens to traffic that no rule matches, in the given mode.
pub(crate) fn scope_line(
    mode: routelane::domain::UnmatchedTraffic,
    texts: crate::ui::i18n::PrimaryTexts,
) -> &'static str {
    match mode {
        routelane::domain::UnmatchedTraffic::PreserveSystem => texts.other_traffic_system,
        routelane::domain::UnmatchedTraffic::UsePrimary => texts.other_traffic_primary,
    }
}

fn store_profile(document: &mut ConfigV4, current: Profile) {
    if let Some(stored) = document
        .profiles
        .iter_mut()
        .find(|profile| profile.id == current.id)
    {
        *stored = current;
    } else {
        document.profiles.push(current);
    }
}

fn reconcile_profiles(
    document: &mut ConfigV4,
    selected: &mut Profile,
    canonical: Vec<Profile>,
    replace_selected: bool,
) {
    for profile in canonical {
        if profile.id == selected.id && !replace_selected {
            continue;
        }
        if profile.id == selected.id {
            *selected = profile.clone();
        }
        store_profile(document, profile);
    }
    document.profiles.sort_by(|a, b| a.id.cmp(&b.id));
}

struct WindowUi {
    window: glib::WeakRef<adw::ApplicationWindow>,
    texts: Texts,
    document: Rc<RefCell<ConfigV4>>,
    profile: Rc<RefCell<Profile>>,
    controller: Rc<UiController>,
    shell: Shell,
    overview: Overview,
    rules: ListPage,
    applications: ListPage,
    connections: ConnectionsPage,
    catalog: RefCell<DesktopCatalog>,
    rendered: RefCell<Option<Profile>>,
    rule_rows: RefCell<BTreeMap<RuleId, CompactRuleRow>>,
    app_rows: RefCell<BTreeMap<RuleId, CompactApplicationRow>>,
    settings: RefCell<Option<adw::PreferencesWindow>>,
    toast: adw::ToastOverlay,
    last_notice: RefCell<Option<String>>,
    /// Last verified effective exit, to announce each transition once.
    last_effective: RefCell<Option<routelane::backend::FamilyPair<routelane::backend::FamilyExit>>>,
    recovery_dialog_open: Cell<bool>,
    quick_application: gtk::Button,
    preferences: gtk::Button,
}

impl WindowUi {
    fn save_profile(
        self: &Rc<Self>,
        candidate: Profile,
        enabled_change: Option<(RuleId, bool)>,
    ) -> bool {
        if let Err(error) = routelane::domain::validate_profile(&candidate) {
            log::warn!("invalid UI candidate: {error:?}");
            return false;
        }
        let mut document = self.document.borrow().clone();
        store_profile(&mut document, candidate.clone());
        let session = self.controller.session();
        let active_here = session
            .confirmed_status()
            .is_some_and(|s| s.applied_profile_id.as_ref() == Some(&candidate.id));
        let aligned = !session.needs_reapply(&self.profile.borrow());
        drop(session);
        let follow_up = if active_here {
            match enabled_change.filter(|_| aligned) {
                // Disabling a rule only narrows the managed scope.
                Some((id, false)) => {
                    AfterSave::Request(set_rule_enabled_request(candidate.id.clone(), id, false))
                }
                // Anything else can widen it: preview, disclose, then apply.
                _ => return self.preview_then_activate(candidate, false),
            }
        } else {
            AfterSave::None
        };
        if !self
            .controller
            .save(document.clone(), candidate.clone(), follow_up)
        {
            return false;
        }
        *self.document.borrow_mut() = document;
        *self.profile.borrow_mut() = candidate;
        self.render();
        true
    }

    fn apply(self: &Rc<Self>) {
        let profile = self.profile.borrow().clone();
        let first_activation = self
            .controller
            .session()
            .confirmed_status()
            .is_none_or(|status| status.applied_profile_id.is_none());
        let fails_over = profile.routing.automatic_failover && !profile.routing.backups.is_empty();
        self.preview_then_activate(profile, first_activation && fails_over);
    }

    /// Every GUI activation asks the daemon for a read-only preview first.
    /// Nothing is saved or applied until it returns for this exact draft; an
    /// unavailable preview stops here.
    fn preview_then_activate(
        self: &Rc<Self>,
        candidate: Profile,
        failover_disclosure: bool,
    ) -> bool {
        let weak = Rc::downgrade(self);
        let previewed = candidate.clone();
        self.controller.preview(
            candidate,
            Box::new(move |result| {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(preview) => ui.confirm_impact(previewed, preview, failover_disclosure),
                    Err(error) => {
                        let message = if error.code == "DAEMON_PREVIEW_UNAVAILABLE" {
                            ui.texts.primary.preview_unavailable.to_owned()
                        } else {
                            format!("{} ({})", ui.texts.primary.preview_unavailable, error.code)
                        };
                        ui.toast.add_toast(&adw::Toast::new(&message));
                        ui.render();
                    }
                }
            }),
        )
    }

    /// Discloses the scope, the migration notice, automatic failover and
    /// every preview warning before any save or apply. Declining (Cancel,
    /// Escape or closing) saves and activates nothing.
    fn confirm_impact(
        self: &Rc<Self>,
        candidate: Profile,
        preview: routelane::backend::RoutingPreview,
        failover_disclosure: bool,
    ) {
        use crate::ui::primary::{activation_reasons, ActivationReason};
        use routelane::backend::RoutingWarning;
        let migrated = self.document.borrow().routing_notice_pending;
        let reasons = activation_reasons(&candidate, migrated, &preview);
        if reasons.is_empty() && !failover_disclosure {
            self.activate_now(candidate, false);
            return;
        }
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let texts = self.texts.primary;
        let mut paragraphs = vec![];
        if failover_disclosure {
            paragraphs.push(
                texts
                    .activation_order
                    .replace("{}", &failover_order(&candidate, texts)),
            );
            paragraphs.push(texts.activation_body.to_owned());
        }
        paragraphs.push(scope_line(candidate.routing.unmatched_traffic, texts).to_owned());
        for reason in &reasons {
            paragraphs.push(match reason {
                ActivationReason::Migrated => texts.impact_migrated.to_owned(),
                ActivationReason::UsePrimary => texts.impact_use_primary.to_owned(),
                ActivationReason::Warning(RoutingWarning::LocalNetworkRule {
                    rule_id,
                    network,
                }) => texts
                    .impact_local_network
                    .replace("{rule}", rule_id.as_str())
                    .replace("{network}", &network.to_string()),
                ActivationReason::Warning(RoutingWarning::AllTrafficRule { rule_id }) => {
                    texts.impact_all_traffic.replace("{rule}", rule_id.as_str())
                }
            });
        }
        paragraphs.push(texts.impact_footer.to_owned());
        let heading = if reasons.is_empty() {
            texts.activation_heading
        } else {
            texts.impact_heading
        };
        let weak = Rc::downgrade(self);
        let cancelled = Rc::downgrade(self);
        confirm_destructive_action_with_label(
            &window,
            self.texts,
            heading,
            &paragraphs.join("\n\n"),
            self.texts.enable_routing,
            Rc::new(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.activate_now(candidate.clone(), true);
                }
            }),
            Rc::new(move || {
                if let Some(ui) = cancelled.upgrade() {
                    ui.render();
                }
            }),
        );
    }

    /// Saves and applies the previewed draft; a stale preview sends nothing.
    fn activate_now(self: &Rc<Self>, candidate: Profile, acknowledged: bool) {
        let mut document = self.document.borrow().clone();
        if acknowledged {
            document.routing_notice_pending = false;
        }
        store_profile(&mut document, candidate.clone());
        if self
            .controller
            .confirm_activation(document.clone(), candidate.clone())
        {
            *self.document.borrow_mut() = document;
            *self.profile.borrow_mut() = candidate;
        }
        self.render();
    }

    /// Announces each verified change of the effective IPv4 exit once, as a
    /// toast and a desktop notification; never a dialog.
    fn announce_primary_transition(&self, status: Option<&routelane::ipc::DaemonStatus>) {
        use routelane::backend::FamilyExit;
        let Some(decision) = status.and_then(|status| status.primary.decision.clone()) else {
            return;
        };
        let previous = self
            .last_effective
            .replace(Some(decision.effective.clone()));
        let Some(previous) = previous else {
            return;
        };
        if previous.v4 == decision.effective.v4 {
            return;
        }
        let profile = self.profile.borrow();
        let message = match &decision.effective.v4 {
            FamilyExit::Blocked => self.texts.primary.blocked_notice.to_owned(),
            FamilyExit::Lane(id) if decision.preferred.v4.as_ref() == Some(id) => {
                self.texts.primary.failback_notice.to_owned()
            }
            FamilyExit::Lane(id) => self.texts.primary.failover_notice.replace(
                "{}",
                &profile
                    .lanes
                    .iter()
                    .find(|lane| &lane.id == id)
                    .map_or_else(|| id.as_str().to_owned(), |lane| lane.name.clone()),
            ),
        };
        self.toast.add_toast(&adw::Toast::new(&message));
        if let Some(application) = self
            .window
            .upgrade()
            .and_then(|window| window.application())
        {
            let notification = gtk::gio::Notification::new(self.texts.primary.title);
            notification.set_body(Some(&message));
            application.send_notification(Some("routelane-primary"), &notification);
        }
    }

    fn save_rule(self: &Rc<Self>, rule: Rule) -> bool {
        let mut candidate = self.profile.borrow().clone();
        if let Some(existing) = candidate
            .rules
            .iter_mut()
            .find(|existing| existing.id == rule.id)
        {
            *existing = rule;
        } else {
            candidate.rules.push(rule);
        }
        self.save_profile(candidate, None)
    }

    fn toggle(self: &Rc<Self>, id: RuleId, enabled: bool) {
        let mut candidate = self.profile.borrow().clone();
        let Some(rule) = candidate.rules.iter_mut().find(|rule| rule.id == id) else {
            return;
        };
        if rule.enabled == enabled {
            return;
        }
        rule.enabled = enabled;
        if !self.save_profile(candidate, Some((id, enabled))) {
            self.rendered.borrow_mut().take();
            self.render();
        }
    }

    fn open_rule(self: &Rc<Self>, editing: Option<RuleId>) {
        if !self.editable() {
            return;
        }
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        open_rule_editor(
            &window,
            self.texts,
            self.profile.clone(),
            editing,
            Rc::new(move |rule| weak.upgrade().is_some_and(|ui| ui.save_rule(rule))),
        );
    }

    fn open_application(self: &Rc<Self>, editing: Option<RuleId>) {
        if !self.editable() {
            return;
        }
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        open_application_editor(
            &window,
            self.texts,
            self.profile.clone(),
            editing,
            Rc::new(move |rule| weak.upgrade().is_some_and(|ui| ui.save_rule(rule))),
        );
    }

    fn editable(&self) -> bool {
        let session = self.controller.session();
        session.config_writable() && session.snapshot_known() && !session.is_busy()
    }

    fn delete(self: &Rc<Self>, id: RuleId) {
        if !self.editable() {
            return;
        }
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let current = self
            .profile
            .borrow()
            .rules
            .iter()
            .find(|rule| rule.id == id)
            .cloned();
        let Some(rule) = current else {
            return;
        };
        let title = match &rule.source {
            RuleSource::Application(selector) => {
                application_display_name(selector, &self.catalog.borrow())
            }
            _ => target_text(&rule.target),
        };
        let weak = Rc::downgrade(self);
        confirm_destructive_action(
            &window,
            self.texts,
            self.texts.confirm_delete_rule_heading,
            &title,
            Rc::new(move || {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                let mut candidate = ui.profile.borrow().clone();
                if !candidate.rules.iter().any(|rule| rule.id == id) {
                    return;
                }
                candidate.rules.retain(|rule| rule.id != id);
                ui.save_profile(candidate, None);
            }),
        );
    }

    fn move_rule(self: &Rc<Self>, id: RuleId, direction: ReorderDirection) {
        if !self.editable() {
            return;
        }
        let mut candidate = self.profile.borrow().clone();
        if move_rule(&mut candidate, &id, direction) {
            self.save_profile(candidate, None);
        }
    }

    fn application_action(self: &Rc<Self>, id: RuleId, action: ApplicationAction) {
        let profile = self.profile.borrow().clone();
        let Some(rule) = profile.rules.iter().find(|rule| rule.id == id) else {
            return;
        };
        if !self
            .controller
            .session()
            .application_actions_enabled(&profile, rule)
        {
            return;
        }
        let request = application_action_request(profile.id.clone(), id, action);
        if action == ApplicationAction::Launch {
            self.controller.request(request, Some(profile));
            return;
        }
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        confirm_destructive_action_with_label(
            &window,
            self.texts,
            self.texts.ux.adopt_confirm,
            self.texts.ux.scope_warning,
            self.texts.ux.adopt_confirm,
            Rc::new(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.controller
                        .request(request.clone(), Some(ui.profile.borrow().clone()));
                }
            }),
            Rc::new(|| {}),
        );
    }

    fn menu(self: &Rc<Self>, rule: &Rule, index: usize, total: usize) -> Vec<MenuAction> {
        let mut actions: Vec<MenuAction> = vec![];
        let weak = Rc::downgrade(self);
        let id = rule.id.clone();
        let app = matches!(rule.source, RuleSource::Application(_));
        actions.push((
            self.texts.ux.edit,
            true,
            Rc::new(move || {
                if let Some(ui) = weak.upgrade() {
                    if app {
                        ui.open_application(Some(id.clone()));
                    } else {
                        ui.open_rule(Some(id.clone()));
                    }
                }
            }),
        ));
        if app {
            let weak = Rc::downgrade(self);
            let id = rule.id.clone();
            let eligible = self
                .controller
                .session()
                .application_actions_enabled(&self.profile.borrow(), rule);
            actions.push((
                self.texts.ux.adopt_confirm,
                eligible,
                Rc::new(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.application_action(id.clone(), ApplicationAction::AdoptRunningProcesses);
                    }
                }),
            ));
        } else {
            for (label, direction, allowed) in [
                (self.texts.move_rule_up, ReorderDirection::Up, index > 0),
                (
                    self.texts.move_rule_down,
                    ReorderDirection::Down,
                    index + 1 < total,
                ),
            ] {
                let weak = Rc::downgrade(self);
                let id = rule.id.clone();
                actions.push((
                    label,
                    allowed,
                    Rc::new(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.move_rule(id.clone(), direction);
                        }
                    }),
                ));
            }
        }
        let weak = Rc::downgrade(self);
        let id = rule.id.clone();
        actions.push((
            self.texts.delete,
            true,
            Rc::new(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.delete(id.clone());
                }
            }),
        ));
        actions
    }

    fn rebuild_rows(self: &Rc<Self>, profile: &Profile) {
        for row in self.rule_rows.borrow_mut().values() {
            self.rules.group.remove(&row.row);
        }
        for row in self.app_rows.borrow_mut().values() {
            self.applications.group.remove(&row.row);
        }
        self.rule_rows.borrow_mut().clear();
        self.app_rows.borrow_mut().clear();
        let mut ordered = profile.rules.clone();
        ordered.sort_by(Rule::execution_cmp);
        for (index, rule) in ordered.iter().enumerate() {
            let weak = Rc::downgrade(self);
            let on_toggle: Rc<dyn Fn(RuleId, bool)> = Rc::new(move |id, enabled| {
                if let Some(ui) = weak.upgrade() {
                    ui.toggle(id, enabled);
                }
            });
            let actions = self.menu(rule, index, ordered.len());
            let subtitle = action_text(&rule.action, &profile.lanes, self.texts);
            match &rule.source {
                RuleSource::Any => {
                    let row = create_compact_rule_row(
                        rule,
                        &target_text(&rule.target),
                        &subtitle,
                        self.texts,
                        on_toggle,
                        actions,
                    );
                    self.rules.group.add(&row.row);
                    self.rule_rows.borrow_mut().insert(rule.id.clone(), row);
                }
                RuleSource::Application(selector) => {
                    let title = application_display_name(selector, &self.catalog.borrow());
                    let catalog = self.catalog.borrow();
                    let icon = catalog
                        .find(selector)
                        .and_then(|choice| choice.icon_name.as_deref())
                        .unwrap_or("application-x-executable-symbolic");
                    let weak = Rc::downgrade(self);
                    let id = rule.id.clone();
                    let row = create_compact_application_row(
                        rule,
                        (&title, icon),
                        &subtitle,
                        self.texts,
                        on_toggle,
                        Rc::new(move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.application_action(id.clone(), ApplicationAction::Launch);
                            }
                        }),
                        actions,
                    );
                    if let RuleAction::Route(lane) = &rule.action {
                        let lanes = profile
                            .lanes
                            .iter()
                            .map(|lane| (lane.id.clone(), lane.name.clone()))
                            .collect::<Vec<_>>();
                        row.row.set_tooltip_text(Some(
                            &crate::ui::application_row::application_subtitle(
                                lane,
                                &lanes,
                                self.texts.application_scope_prefix,
                                self.texts.application_scope_suffix,
                            ),
                        ));
                    }
                    self.applications.group.add(&row.row);
                    self.app_rows.borrow_mut().insert(rule.id.clone(), row);
                }
            }
        }
    }

    fn filter(&self) {
        let profile = self.profile.borrow();
        let query = self.rules.search.text();
        let mut visible = 0;
        for rule in &profile.rules {
            if let Some(row) = self.rule_rows.borrow().get(&rule.id) {
                let matches = rule_matches_query(rule, &profile.lanes, &query);
                row.row.set_visible(matches);
                if matches {
                    visible += 1;
                }
            }
        }
        self.rules
            .set_empty(self.rule_rows.borrow().is_empty(), visible == 0);
        let query = self.applications.search.text().to_lowercase();
        let mut visible = 0;
        for rule in &profile.rules {
            if let Some(row) = self.app_rows.borrow().get(&rule.id) {
                let name = match &rule.source {
                    RuleSource::Application(selector) => {
                        application_display_name(selector, &self.catalog.borrow())
                    }
                    _ => String::new(),
                };
                let matches = name.to_lowercase().contains(query.trim())
                    || action_text(&rule.action, &profile.lanes, self.texts)
                        .to_lowercase()
                        .contains(query.trim());
                row.row.set_visible(matches);
                if matches {
                    visible += 1;
                }
            }
        }
        self.applications
            .set_empty(self.app_rows.borrow().is_empty(), visible == 0);
    }

    fn render(self: &Rc<Self>) {
        let profile = self.profile.borrow().clone();
        let rebuild = self.rendered.borrow().as_ref() != Some(&profile);
        let view = rebuild.then(|| {
            (
                self.rules.remember_view(),
                self.applications.remember_view(),
            )
        });
        if rebuild {
            self.rebuild_rows(&profile);
            *self.rendered.borrow_mut() = Some(profile.clone());
        }
        let session = self.controller.session();
        let state = dashboard_state(&profile, &session);
        let (title, detail) = status_copy(&session, self.texts);
        self.shell.status_row.set_title(title);
        self.shell.status_row.set_subtitle(detail);
        self.shell.primary.set_label(match state.primary_action {
            PrimaryAction::OpenConnections => self.texts.ux.add_connection,
            PrimaryAction::OpenRuleDialog => self.texts.add_rule,
            PrimaryAction::ReviewDefinitions => self.texts.ux.review_definitions,
            PrimaryAction::RefreshStatus => self.texts.ux.refresh,
            PrimaryAction::EnableRouting => self.texts.enable_routing,
            PrimaryAction::DisableRouting => self.texts.disable_routing,
        });
        self.shell
            .primary
            .set_sensitive(state.primary_action_enabled);
        let editable = session.config_writable() && session.snapshot_known() && !session.is_busy();
        self.preferences
            .set_sensitive(session.config_writable() && !session.is_busy());
        self.rules.add.set_sensitive(editable);
        self.rules.group.set_sensitive(editable);
        self.applications.add.set_sensitive(editable);
        for rule in &profile.rules {
            if let Some(row) = self.app_rows.borrow().get(&rule.id) {
                row.enabled.set_sensitive(editable);
                row.menu.set_sensitive(editable);
                let supported = match &rule.source {
                    RuleSource::Application(selector) => self
                        .catalog
                        .borrow()
                        .find(selector)
                        .is_none_or(|choice| choice.unsupported_reason.is_none()),
                    _ => false,
                };
                let eligible = supported && session.application_actions_enabled(&profile, rule);
                if let Some(adopt) =
                    crate::ui::form::menu_action(&row.menu, self.texts.ux.adopt_confirm)
                {
                    adopt.set_sensitive(eligible);
                }
                row.launch.set_sensitive(eligible);
                row.launch
                    .set_tooltip_text((!eligible).then_some(self.texts.ux.app_needs_apply));
                let lane_healthy = match &rule.action {
                    RuleAction::Route(id) => session
                        .confirmed_status()
                        .and_then(|s| s.health.lanes.get(id))
                        .is_some_and(|health| *health == HealthState::Healthy),
                    _ => false,
                };
                row.launch.set_label(if lane_healthy {
                    self.texts.ux.launch
                } else {
                    self.texts.ux.launch_neutral
                });
                row.row.set_sensitive(!session.is_busy());
            }
        }
        self.quick_application.set_visible(
            !profile.lanes.is_empty()
                && !profile
                    .rules
                    .iter()
                    .any(|rule| matches!(rule.source, RuleSource::Application(_))),
        );
        self.quick_application.set_sensitive(editable);
        self.overview.update(&profile, &session);
        self.connections.update(editable);
        let status = session.confirmed_status().cloned();
        drop(session);
        self.announce_primary_transition(status.as_ref());
        self.filter();
        if let Some((rules, apps)) = view {
            self.rules.restore_view(&rules);
            self.applications.restore_view(&apps);
        }
        let completed = self.controller.last_completed_id();
        if completed.is_some() && *self.last_notice.borrow() != completed {
            *self.last_notice.borrow_mut() = completed;
            let session = self.controller.session();
            let message = match session.last_result() {
                Some(DaemonResult::ApplicationLaunched(result)) => {
                    Some(format!("{} · PID {}", self.texts.ux.starting, result.pid))
                }
                Some(DaemonResult::ApplicationProcessesAdopted(result)) => Some(format!(
                    "{}: {} · {}: {}",
                    self.texts.ux.adopting,
                    result.adopted_count,
                    self.texts.ux.disabled,
                    result.skipped_count
                )),
                _ => None,
            };
            if let Some(message) = message {
                self.toast.add_toast(&adw::Toast::new(&message));
            }
        }
        self.ask_recovery();
    }

    fn ask_recovery(self: &Rc<Self>) {
        if !self.controller.recovery_available() || self.recovery_dialog_open.replace(true) {
            return;
        }
        let Some(window) = self.window.upgrade() else {
            self.recovery_dialog_open.set(false);
            return;
        };
        let weak = Rc::downgrade(self);
        let cancel = Rc::downgrade(self);
        confirm_destructive_action_with_label(
            &window,
            self.texts,
            self.texts.orphan_recovery_heading,
            self.texts.orphan_recovery_body,
            self.texts.recover_and_retry,
            Rc::new(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.controller.confirm_recovery();
                    ui.recovery_dialog_open.set(false);
                }
            }),
            Rc::new(move || {
                if let Some(ui) = cancel.upgrade() {
                    ui.controller.cancel_recovery();
                    ui.recovery_dialog_open.set(false);
                }
            }),
        );
    }

    fn receive(self: &Rc<Self>, message: DaemonToUi) {
        let snapshot = matches!(message, DaemonToUi::Snapshot { .. });
        self.controller.receive(message);
        if snapshot && self.controller.session().snapshot_known() {
            let session = self.controller.session();
            let canonical = session.daemon_profiles().to_vec();
            let replace_selected = session.save_state() == SaveState::Clean && !session.is_busy();
            drop(session);
            reconcile_profiles(
                &mut self.document.borrow_mut(),
                &mut self.profile.borrow_mut(),
                canonical,
                replace_selected,
            );
        }
        self.render();
    }

    fn open_settings(self: &Rc<Self>) {
        if let Some(dialog) = self.settings.borrow().as_ref() {
            dialog.present();
            return;
        }
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        let language = self.document.borrow().language;
        let dialog = build_general_settings_dialog(
            &window,
            self.texts,
            language,
            Rc::new(move |language| {
                if let Some(ui) = weak.upgrade() {
                    let mut document = ui.document.borrow().clone();
                    document.language = language;
                    if ui.controller.save(
                        document.clone(),
                        ui.profile.borrow().clone(),
                        AfterSave::QuitApplication,
                    ) {
                        *ui.document.borrow_mut() = document;
                    }
                }
            }),
        );
        *self.settings.borrow_mut() = Some(dialog.clone());
        dialog.present();
    }
}

pub fn build_window(
    app: &adw::Application,
    sender: Sender<UiToDaemon>,
    receiver: Receiver<DaemonToUi>,
) -> adw::ApplicationWindow {
    build_window_with_repository(app, sender, receiver, ConfigRepository::user_default())
}

fn build_window_with_repository(
    app: &adw::Application,
    sender: Sender<UiToDaemon>,
    receiver: Receiver<DaemonToUi>,
    repository: ConfigRepository,
) -> adw::ApplicationWindow {
    let (mut document, load_error) = match repository.load() {
        Ok(document) => (document, None),
        Err(error) => (ConfigV4::default(), Some(error.to_string())),
    };
    if document.profiles.is_empty() {
        document.profiles.push(empty_default_profile());
    }
    let language = document.language;
    let texts = language.texts();
    let profile = Rc::new(RefCell::new(document.profiles[0].clone()));
    let mut session = UiSession::new(profile.borrow().clone(), load_error.is_none());
    if let Some(error) = &load_error {
        session.configuration_failed(error.clone());
    }
    let controller = UiController::new(
        session,
        sender,
        load_error
            .is_none()
            .then(|| PersistenceWorker::spawn(repository)),
    );
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("RouteLane")
        .default_width(900)
        .default_height(720)
        .build();
    gtk::Window::set_default_icon_name(APP_ICON_NAME);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("RouteLane", "")));
    let preferences = gtk::Button::builder()
        .icon_name("preferences-system-symbolic")
        .tooltip_text(texts.settings_title)
        .build();
    preferences.set_widget_name("preferences");
    preferences.update_property(&[gtk::accessible::Property::Label(texts.settings_title)]);
    header.pack_end(&preferences);
    let shell = Shell::new(texts);
    let overview = Overview::new(texts);
    let rules = ListPage::new(texts, Section::Rules);
    let applications = ListPage::new(texts, Section::Applications);
    let weak_slot = Rc::new(RefCell::new(Weak::<WindowUi>::new()));
    let save_slot = weak_slot.clone();
    let connections = ConnectionsPage::new(
        &window,
        texts,
        profile.clone(),
        Rc::new(move |candidate| {
            save_slot
                .borrow()
                .upgrade()
                .is_some_and(|ui| ui.save_profile(candidate, None))
        }),
    );
    shell.add_page(Section::Overview, &overview.root);
    shell.add_page(Section::Rules, &rules.root);
    shell.add_page(Section::Applications, &applications.root);
    shell.add_page(Section::Connections, &connections.list.root);
    let quick_application = gtk::Button::with_label(texts.add_application);
    quick_application.set_halign(gtk::Align::Start);
    overview.root.prepend(&quick_application);
    let toast = adw::ToastOverlay::new();
    toast.set_child(Some(&shell.root));
    toast.set_vexpand(true);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&header);
    body.append(&toast);
    window.set_content(Some(&body));
    let ui = Rc::new(WindowUi {
        window: window.downgrade(),
        texts,
        document: Rc::new(RefCell::new(document)),
        profile,
        controller: controller.clone(),
        shell,
        overview,
        rules,
        applications,
        connections,
        catalog: RefCell::new(DesktopCatalog::default()),
        rendered: RefCell::new(None),
        rule_rows: RefCell::new(BTreeMap::new()),
        app_rows: RefCell::new(BTreeMap::new()),
        settings: RefCell::new(None),
        toast,
        last_notice: RefCell::new(None),
        last_effective: RefCell::new(None),
        recovery_dialog_open: Cell::new(false),
        quick_application,
        preferences,
    });
    *weak_slot.borrow_mut() = Rc::downgrade(&ui);
    let weak = Rc::downgrade(&ui);
    controller.connect_changed(Rc::new(move || {
        if let Some(ui) = weak.upgrade() {
            ui.render();
        }
    }));
    let app_quit = app.clone();
    controller.connect_quit(Rc::new(move || app_quit.quit()));
    let weak = Rc::downgrade(&ui);
    ui.shell.primary.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            let state = dashboard_state(&ui.profile.borrow(), &ui.controller.session());
            if !state.primary_action_enabled {
                return;
            }
            match state.primary_action {
                PrimaryAction::OpenConnections => {
                    ui.shell.select(Section::Connections);
                    ui.connections.open_add();
                }
                PrimaryAction::OpenRuleDialog => {
                    ui.shell.select(Section::Rules);
                    ui.open_rule(None);
                }
                PrimaryAction::ReviewDefinitions => {
                    ui.shell.select(
                        if ui
                            .profile
                            .borrow()
                            .rules
                            .iter()
                            .any(|rule| matches!(rule.source, RuleSource::Any))
                        {
                            Section::Rules
                        } else {
                            Section::Applications
                        },
                    );
                }
                PrimaryAction::RefreshStatus => ui.controller.refresh(),
                PrimaryAction::EnableRouting => ui.apply(),
                PrimaryAction::DisableRouting => {
                    ui.controller.request(DaemonRequest::Deactivate, None);
                }
            }
        }
    });
    for (button, section) in [
        (ui.rules.add.clone(), Section::Rules),
        (ui.applications.add.clone(), Section::Applications),
        (ui.quick_application.clone(), Section::Applications),
    ] {
        let weak = Rc::downgrade(&ui);
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                if section == Section::Rules {
                    ui.open_rule(None);
                } else {
                    ui.open_application(None);
                }
            }
        });
    }
    let weak = Rc::downgrade(&ui);
    ui.preferences.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.open_settings();
        }
    });
    let weak = Rc::downgrade(&ui);
    ui.overview.reapply.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.apply();
        }
    });
    let weak = Rc::downgrade(&ui);
    ui.overview.refresh.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.controller.refresh();
        }
    });
    for search in [&ui.rules.search, &ui.applications.search] {
        let weak = Rc::downgrade(&ui);
        search.connect_search_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.filter();
            }
        });
    }
    let search = gtk::gio::SimpleAction::new("search", None);
    let weak = Rc::downgrade(&ui);
    search.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            match ui.shell.section() {
                Section::Applications => {
                    ui.applications.search.grab_focus();
                }
                _ => {
                    ui.shell.select(Section::Rules);
                    ui.rules.search.grab_focus();
                }
            }
        }
    });
    window.add_action(&search);
    app.set_accels_for_action("win.search", &["<Primary>f"]);
    let add = gtk::gio::SimpleAction::new("new", None);
    let weak = Rc::downgrade(&ui);
    add.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            if !ui.editable() {
                return;
            }
            match ui.shell.section() {
                Section::Connections => ui.connections.open_add(),
                Section::Applications => ui.open_application(None),
                _ => ui.open_rule(None),
            }
        }
    });
    window.add_action(&add);
    app.set_accels_for_action("win.new", &["<Primary>n"]);
    let weak = Rc::downgrade(&ui);
    load_catalog(move |result| {
        if let Some(ui) = weak.upgrade() {
            match result {
                Ok(catalog) => {
                    *ui.catalog.borrow_mut() = catalog;
                    ui.rendered.borrow_mut().take();
                    ui.render();
                }
                Err(error) => log::warn!("desktop catalog: {error}"),
            }
        }
    });
    ui.render();
    let live_ui = ui.clone();
    let source = glib::MainContext::ref_thread_default().spawn_local(async move {
        while let Ok(message) = receiver.recv().await {
            live_ui.receive(message);
        }
    });
    let source = Rc::new(RefCell::new(Some(source)));
    window.connect_destroy(move |_| {
        if let Some(source) = source.borrow_mut().take() {
            source.remove();
        }
    });
    let (tray_sender, tray_receiver) =
        glib::MainContext::channel::<TrayCommand>(glib::PRIORITY_DEFAULT);
    let tray_available = spawn_tray(tray_sender, Arc::new(Mutex::new(language)));
    let weak_window = window.downgrade();
    let app_quit = app.clone();
    tray_receiver.attach(None, move |command| {
        match command {
            TrayCommand::ShowWindow => {
                if let Some(window) = weak_window.upgrade() {
                    window.present();
                }
            }
            TrayCommand::Quit => app_quit.quit(),
        };
        glib::Continue(true)
    });
    window.connect_close_request(move |window| {
        if tray_available.load(Ordering::Relaxed) {
            window.hide();
            glib::signal::Inhibit(true)
        } else {
            glib::signal::Inhibit(false)
        }
    });
    window.present();
    window
}

#[cfg(test)]
mod tests {
    use super::*;
    use routelane::health::HealthReport;
    use routelane::ipc::DaemonStatus;

    fn find_named(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
        if widget.widget_name().as_str() == name {
            return Some(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(found) = find_named(&current, name) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
    }
    fn find_button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.label().as_deref() == Some(label) {
                return Some(button.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(found) = find_button(&current, label) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
    }
    fn drain() {
        let context = glib::MainContext::default();
        for _ in 0..100 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
    }

    fn wait_for(mut ready: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !ready() {
            assert!(
                std::time::Instant::now() < deadline,
                "GTK callback timed out"
            );
            drain();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        drain();
    }
    fn transient(parent: &impl IsA<gtk::Window>) -> gtk::Window {
        gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.is_visible() && w.transient_for().as_ref() == Some(parent.as_ref()))
            .expect("visible modal")
    }
    fn escape(window: &gtk::Window) {
        let controllers = window.observe_controllers();
        let key = (0..controllers.n_items())
            .filter_map(|i| {
                controllers
                    .item(i)
                    .and_then(|c| c.downcast::<gtk::EventControllerKey>().ok())
            })
            .next()
            .unwrap();
        assert!(key.emit_by_name::<bool>(
            "key-pressed",
            &[
                &gtk::gdk::Key::Escape,
                &0u32,
                &gtk::gdk::ModifierType::empty()
            ]
        ));
        drain();
    }

    #[test]
    #[ignore = "requires display; run bash scripts/run-gtk-smoke.sh"]
    fn gtk_smoke_window_survives_profile_refresh_and_add_dialogs() {
        adw::init().unwrap();
        crate::ui::shell::tests::assert_four_sections();
        let temp = tempfile::TempDir::new().unwrap();
        let app = adw::Application::builder()
            .application_id("io.github.routelane.SmokeTest")
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let (sender, requests) = async_channel::unbounded();
        let (events, receiver) = async_channel::unbounded();
        let window = build_window_with_repository(
            &app,
            sender,
            receiver,
            ConfigRepository::new(temp.path().join("config.json")),
        );
        let canceled = Rc::new(Cell::new(0));
        let confirmed = Rc::new(Cell::new(0));
        let c = canceled.clone();
        let ok = confirmed.clone();
        let texts = routelane::config::Language::Tr.texts();
        confirm_destructive_action_with_label(
            &window,
            texts,
            "Confirmation test",
            "Cancel must be safe",
            texts.confirm,
            Rc::new(move || ok.set(ok.get() + 1)),
            Rc::new(move || c.set(c.get() + 1)),
        );
        drain();
        let confirmation = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Confirmation test"))
            .unwrap();
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
        drain();
        assert_eq!(
            canceled.get(),
            1,
            "window close must cancel recovery rather than leave it pending"
        );
        assert_eq!(confirmed.get(), 0);
        assert!(find_named(window.upcast_ref(), "routing-primary").is_some());
        assert_eq!(window.default_width(), 900);
        assert_eq!(window.default_height(), 720);
        events
            .try_send(DaemonToUi::ConnectionEstablished { epoch: 1 })
            .unwrap();
        events
            .try_send(DaemonToUi::Snapshot {
                epoch: 1,
                status: DaemonStatus {
                    generation: 0,
                    event_sequence: 0,
                    health: HealthReport::inactive(),
                    desired_profile_id: None,
                    applied_profile_id: None,
                    last_error: None,
                    primary: Default::default(),
                },
                profiles: vec![],
            })
            .unwrap();
        drain();
        for (section, label) in [
            ("rules", "Kural ekle"),
            ("applications", "Uygulama ekle"),
            ("connections", "Bağlantı Ekle"),
        ] {
            let nav = find_named(window.upcast_ref(), section)
                .unwrap()
                .downcast::<gtk::ToggleButton>()
                .unwrap();
            nav.emit_clicked();
            drain();
            let stack = find_named(window.upcast_ref(), "sections")
                .unwrap()
                .downcast::<gtk::Stack>()
                .unwrap();
            let button = find_button(&stack.visible_child().unwrap(), label).unwrap();
            assert_eq!(
                button.parent().unwrap().first_child().unwrap(),
                button.clone().upcast::<gtk::Widget>(),
                "add remains above the list"
            );
            assert!(button.is_sensitive());
            button.emit_clicked();
            drain();
            let dialogs = gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Window>().ok())
                .filter(|dialog| {
                    dialog.is_visible()
                        && dialog.transient_for().as_ref() == Some(window.upcast_ref())
                })
                .collect::<Vec<_>>();
            assert_eq!(dialogs.len(), 1);
            assert!(dialogs[0].is_modal());
            if section == "rules" {
                let target = find_named(dialogs[0].upcast_ref(), "rule-target")
                    .unwrap()
                    .downcast::<gtk::Entry>()
                    .unwrap();
                target.set_text("https://invalid.example/path");
                target
                    .delegate()
                    .unwrap()
                    .emit_by_name::<()>("activate", &[]);
                let feedback = find_named(dialogs[0].upcast_ref(), "form-feedback").unwrap();
                wait_for(|| feedback.is_visible());
                let focus = gtk::prelude::GtkWindowExt::focus(&dialogs[0]).unwrap();
                assert!(
                    focus == target.clone().upcast::<gtk::Widget>() || focus.is_ancestor(&target),
                    "invalid target receives focus"
                );
                assert!(requests.try_recv().is_err());
            } else if section == "connections" {
                find_named(dialogs[0].upcast_ref(), "connection-name")
                    .unwrap()
                    .downcast::<gtk::Entry>()
                    .unwrap()
                    .set_text("Draft only");
                drain();
                assert!(
                    !temp.path().join("config.json").exists(),
                    "editing fields cannot autosave"
                );
            }
            for dialog in dialogs {
                escape(&dialog);
            }
            drain();
        }
        assert!(
            requests.try_recv().is_err(),
            "navigation and canceled forms must not mutate routing"
        );
        crate::ui::application_editor::tests::assert_picker_behavior(&window, texts);
        let accepted = Rc::new(RefCell::new(Vec::new()));
        let output = accepted.clone();
        open_rule_editor(
            &window,
            texts,
            Rc::new(RefCell::new(empty_default_profile())),
            None,
            Rc::new(move |rule| {
                output.borrow_mut().push(rule);
                true
            }),
        );
        drain();
        let rule_dialog = transient(&window);
        find_named(rule_dialog.upcast_ref(), "rule-action")
            .unwrap()
            .downcast::<gtk::DropDown>()
            .unwrap()
            .set_selected(2);
        let entry = find_named(rule_dialog.upcast_ref(), "rule-target")
            .unwrap()
            .downcast::<gtk::Entry>()
            .unwrap();
        entry.set_text("Claude.AI");
        entry
            .delegate()
            .unwrap()
            .emit_by_name::<()>("activate", &[]);
        wait_for(|| !accepted.borrow().is_empty());
        assert_eq!(
            accepted.borrow()[0].target,
            routelane::domain::RuleTarget::Domain("claude.ai".into())
        );
        assert_eq!(accepted.borrow()[0].action, RuleAction::Block);
        assert!(!rule_dialog.is_visible());
        assert!(requests.try_recv().is_err());

        // The real dialog must retain the draft, show actionable feedback and
        // avoid invoking save when a canonical equivalent already exists.
        let mut existing = empty_default_profile();
        existing.rules.push(accepted.borrow()[0].clone());
        let output = accepted.clone();
        open_rule_editor(
            &window,
            texts,
            Rc::new(RefCell::new(existing)),
            None,
            Rc::new(move |rule| {
                output.borrow_mut().push(rule);
                true
            }),
        );
        drain();
        let dialog = transient(&window);
        find_named(dialog.upcast_ref(), "rule-action")
            .unwrap()
            .downcast::<gtk::DropDown>()
            .unwrap()
            .set_selected(2);
        let entry = find_named(dialog.upcast_ref(), "rule-target")
            .unwrap()
            .downcast::<gtk::Entry>()
            .unwrap();
        entry.set_text("CLAUDE.AI.");
        dialog
            .default_widget()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        drain();
        let feedback = find_named(dialog.upcast_ref(), "form-feedback")
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap();
        assert!(feedback.is_visible());
        assert_eq!(feedback.text(), texts.ux.duplicate_rule);
        assert_eq!(accepted.borrow().len(), 1);
        assert!(dialog.is_visible());
        entry.set_text("10.65.201.25/24");
        dialog
            .default_widget()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        wait_for(|| accepted.borrow().len() == 2);
        assert_eq!(
            accepted.borrow()[1].target,
            routelane::domain::RuleTarget::Cidr("10.65.201.0/24".parse().unwrap())
        );
        assert!(!dialog.is_visible());
        assert!(requests.try_recv().is_err());
        let sample = Rule {
            id: "plain-label".into(),
            enabled: true,
            priority: 10,
            source: RuleSource::Any,
            target: routelane::domain::RuleTarget::Any,
            action: RuleAction::Block,
        };
        let row = create_compact_rule_row(
            &sample,
            "Browser <Work> & Home",
            "VPN & fallback",
            texts,
            Rc::new(|_, _| {}),
            vec![],
        );
        assert_eq!(
            row.row.title().as_str(),
            "Browser &lt;Work&gt; &amp; Home",
            "untrusted display names are escaped for the GTK 1.1 markup API"
        );
        let mut populated = empty_default_profile();
        for index in 0..40 {
            populated.rules.push(Rule {
                id: format!("rule-{index}").as_str().into(),
                enabled: true,
                priority: index * 10,
                source: RuleSource::Any,
                target: routelane::domain::RuleTarget::Domain(format!("host{index}.example")),
                action: RuleAction::Block,
            });
        }
        let status = DaemonStatus {
            generation: 0,
            event_sequence: 0,
            health: HealthReport::inactive(),
            desired_profile_id: None,
            applied_profile_id: None,
            last_error: None,
            primary: Default::default(),
        };
        events
            .try_send(DaemonToUi::Snapshot {
                epoch: 1,
                status: status.clone(),
                profiles: vec![populated.clone()],
            })
            .unwrap();
        drain();
        find_named(window.upcast_ref(), "rules")
            .unwrap()
            .downcast::<gtk::ToggleButton>()
            .unwrap()
            .emit_clicked();
        drain();
        let first = find_named(window.upcast_ref(), "rule-rule-0").unwrap();
        let switch = find_named(&first, "rule-enable").unwrap();
        switch.grab_focus();
        drain();
        let root = first
            .ancestor(adw::PreferencesGroup::static_type())
            .unwrap()
            .parent()
            .unwrap();
        let query = find_named(window.upcast_ref(), "rules-search")
            .unwrap()
            .downcast::<gtk::SearchEntry>()
            .unwrap();
        query.set_text("host");
        query.emit_by_name::<()>("search-changed", &[]);
        let scroll = root
            .ancestor(gtk::ScrolledWindow::static_type())
            .unwrap()
            .downcast::<gtk::ScrolledWindow>()
            .unwrap();
        wait_for(|| scroll.vadjustment().upper() > scroll.vadjustment().page_size());
        scroll.vadjustment().set_value(120.0);
        let page_state = crate::ui::list_page::ListViewState::capture(&root);
        assert_eq!(page_state.focused_rule, Some("rule-0".into()));
        events
            .try_send(DaemonToUi::Snapshot {
                epoch: 1,
                status: status.clone(),
                profiles: vec![populated.clone()],
            })
            .unwrap();
        drain();
        assert_eq!(
            find_named(window.upcast_ref(), "rule-rule-0").unwrap(),
            first,
            "health refresh must preserve row identity"
        );
        assert_eq!(query.text().as_str(), "host");
        assert_eq!(scroll.vadjustment().value(), page_state.scroll);
        let add = find_button(&root, texts.add_rule).unwrap();
        query.set_text("host39");
        query.emit_by_name::<()>("search-changed", &[]);
        drain();
        assert!(!first.is_visible());
        assert_eq!(
            root.first_child().unwrap(),
            add.clone().upcast::<gtk::Widget>()
        );
        query.set_text("host");
        query.emit_by_name::<()>("search-changed", &[]);
        switch.grab_focus();
        drain();
        populated.rules.remove(0);
        events
            .try_send(DaemonToUi::Snapshot {
                epoch: 1,
                status,
                profiles: vec![populated],
            })
            .unwrap();
        drain();
        let focused = gtk::prelude::GtkWindowExt::focus(&window).unwrap();
        assert!(
            focused.ancestor(adw::ActionRow::static_type()).is_some(),
            "deleted row must focus a surviving neighbor"
        );
        assert_eq!(query.text().as_str(), "host");
        gtk::prelude::WidgetExt::activate_action(&window, "win.search", None).unwrap();
        drain();
        let focus = gtk::prelude::GtkWindowExt::focus(&window).unwrap();
        assert!(focus == query.clone().upcast::<gtk::Widget>() || focus.is_ancestor(&query));
        gtk::prelude::WidgetExt::activate_action(&window, "win.new", None).unwrap();
        drain();
        escape(&transient(&window));
        assert!(requests.try_recv().is_err());

        find_named(window.upcast_ref(), "preferences")
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        drain();
        let preferences = transient(&window);
        let version = find_named(preferences.upcast_ref(), "app-version")
            .expect("Preferences shows the application version")
            .downcast::<adw::ActionRow>()
            .unwrap();
        assert_eq!(
            version.subtitle().as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        let language = find_named(preferences.upcast_ref(), "language-choice")
            .unwrap()
            .downcast::<adw::ComboRow>()
            .unwrap();
        language.set_selected(1);
        drain();
        assert_eq!(
            language.selected(),
            0,
            "the choice remains unchanged until confirmation"
        );
        escape(&transient(&preferences));
        preferences.close();
        drain();
        assert!(
            !temp.path().join("config.json").exists(),
            "language cancellation must not persist"
        );
        assert!(requests.try_recv().is_err());

        let mut app_profile = empty_default_profile();
        app_profile.lanes.push(routelane::domain::Lane {
            id: "vpn".into(),
            name: "VPN".into(),
            interface: "tun0".into(),
            fallback_lane: None,
            failure_policy: routelane::domain::LaneFailurePolicy::Block,
        });
        app_profile.rules.push(Rule {
            id: "browser".into(),
            enabled: true,
            priority: 10,
            source: RuleSource::Application(routelane::domain::ApplicationSelector {
                desktop_id: None,
                executable_path: "/usr/bin/browser".into(),
                include_children: true,
                launch: None,
            }),
            target: routelane::domain::RuleTarget::Any,
            action: RuleAction::Route("vpn".into()),
        });
        events
            .try_send(DaemonToUi::Snapshot {
                epoch: 1,
                status: DaemonStatus {
                    generation: 0,
                    event_sequence: 0,
                    health: HealthReport::inactive(),
                    desired_profile_id: None,
                    applied_profile_id: None,
                    last_error: None,
                    primary: Default::default(),
                },
                profiles: vec![app_profile.clone()],
            })
            .unwrap();
        drain();
        find_named(window.upcast_ref(), "applications")
            .unwrap()
            .downcast::<gtk::ToggleButton>()
            .unwrap()
            .emit_clicked();
        drain();
        let row = find_named(window.upcast_ref(), "rule-browser").unwrap();
        let launch = find_named(&row, "rule-launch")
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        let menu = find_named(&row, "rule-menu")
            .unwrap()
            .downcast::<gtk::MenuButton>()
            .unwrap();
        assert!(!launch.is_sensitive());
        assert!(!crate::ui::form::menu_action(&menu, texts.ux.adopt_confirm)
            .unwrap()
            .is_sensitive());
        let primary = find_named(window.upcast_ref(), "routing-primary")
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        primary.emit_clicked();
        primary.emit_clicked();
        // A read-only preview always comes first, and only once.
        let preview = next_request(&requests);
        assert!(matches!(preview.request, DaemonRequest::PreviewProfile(_)));
        assert!(
            requests.try_recv().is_err(),
            "double activation previews once"
        );
        reply_preview(&events, 1, &preview, vec![]);
        let apply = next_request(&requests);
        assert!(matches!(apply.request, DaemonRequest::ApplyProfile(_)));
        assert!(
            requests.try_recv().is_err(),
            "double activation submits once"
        );
        events
            .try_send(DaemonToUi::Response {
                epoch: 1,
                response: routelane::ipc::DaemonResponse::success(
                    apply.request_id,
                    DaemonResult::Applied(routelane::ipc::ApplyProfileResult {
                        generation: 1,
                        plan_hash: routelane::backend::KernelPlan {
                            unmatched_traffic: routelane::domain::UnmatchedTraffic::UsePrimary,
                            primary: Default::default(),
                            local_networks: vec![],
                            local_addresses: vec![],
                            generation: 1,
                            profile_id: app_profile.id.clone(),
                            lanes: vec![],
                            rules: vec![],
                        }
                        .content_hash(),
                    }),
                ),
            })
            .unwrap();
        drain();
        while requests.try_recv().is_ok() {}
        events
            .try_send(DaemonToUi::Snapshot {
                epoch: 1,
                status: DaemonStatus {
                    generation: 1,
                    event_sequence: 1,
                    health: HealthReport::inactive(),
                    desired_profile_id: Some(app_profile.id.clone()),
                    applied_profile_id: Some(app_profile.id.clone()),
                    last_error: None,
                    primary: Default::default(),
                },
                profiles: vec![app_profile],
            })
            .unwrap();
        drain();
        assert!(launch.is_sensitive());
        assert_eq!(
            launch.label().as_deref(),
            Some(texts.ux.launch_neutral),
            "unavailable lane must not claim verified interface use"
        );
        assert!(
            crate::ui::form::menu_action(&menu, texts.ux.adopt_confirm)
                .unwrap()
                .is_sensitive(),
            "menu eligibility must refresh without recreating the row"
        );
        crate::ui::form::menu_action(&menu, texts.ux.adopt_confirm)
            .unwrap()
            .emit_clicked();
        drain();
        escape(&transient(&window));
        assert!(requests.try_recv().is_err(), "cancel adoption is read-only");
        launch.emit_clicked();
        launch.emit_clicked();
        drain();
        assert!(matches!(
            requests.try_recv().unwrap(),
            UiToDaemon::Request(crate::models::UiRequest {
                request: DaemonRequest::LaunchApplication { .. },
                ..
            })
        ));
        assert!(requests.try_recv().is_err(), "double launch submits once");
        let settings = gtk::Settings::default().unwrap();
        let original_font = settings.gtk_font_name();
        settings.set_gtk_font_name(Some("Sans 22"));
        for language in [
            routelane::config::Language::Tr,
            routelane::config::Language::En,
        ] {
            let shell = Shell::new(language.texts());
            let overview = Overview::new(language.texts());
            let mut long_profile = empty_default_profile();
            long_profile.lanes.push(routelane::domain::Lane {
                id: "long".into(),
                name: "VeryLongConnectionNameWithoutWhitespace".repeat(3),
                interface: "tun0".into(),
                fallback_lane: None,
                failure_policy: routelane::domain::LaneFailurePolicy::Block,
            });
            overview.update(&long_profile, &UiSession::new(long_profile.clone(), true));
            shell.add_page(Section::Overview, &overview.root);
            let list = ListPage::new(language.texts(), Section::Applications);
            let long_title = "VeryLongBrowserApplicationNameWithoutWhitespace".repeat(3);
            let row = create_compact_application_row(
                &sample,
                (&long_title, "application-x-executable-symbolic"),
                &long_profile.lanes[0].name,
                language.texts(),
                Rc::new(|_, _| {}),
                Rc::new(|| {}),
                vec![],
            );
            list.group.add(&row.row);
            shell.add_page(Section::Applications, &list.root);
            shell.select(Section::Applications);
            shell.status_row.set_title(language.texts().routing_active);
            shell.primary.set_label(language.texts().disable_routing);
            assert!(
                shell.root.measure(gtk::Orientation::Horizontal, -1).0 <= 536,
                "200% text must not force a wide window"
            );
            let preview = adw::Window::builder()
                .default_width(560)
                .default_height(600)
                .content(&shell.root)
                .build();
            preview.present();
            wait_for(|| shell.root.width() > 0);
            assert!(
                shell.root.height() <= 600,
                "compact layout must fit vertically: {}",
                shell.root.height()
            );
            assert!(shell.root.width() <= 560);
            assert!(shell.primary.is_mapped());
            shell.select(Section::Overview);
            drain();
            assert!(shell.root.width() <= 560 && shell.root.height() <= 600);
            preview.set_default_size(900, 720);
            drain();
            preview.destroy();
        }
        settings.set_gtk_font_name(original_font.as_deref());
        window.destroy();
        drain();
        assert_primary_selection_is_confirmed_cancel_safe_and_reopens(&app, texts);
        for language in [
            routelane::config::Language::Tr,
            routelane::config::Language::En,
        ] {
            assert_routing_scope_and_activation_preview(&app, language);
        }
    }

    fn next_request(requests: &Receiver<UiToDaemon>) -> crate::models::UiRequest {
        wait_for(|| !requests.is_empty());
        match requests.try_recv().unwrap() {
            UiToDaemon::Request(request) => request,
            _ => panic!("a daemon request was expected"),
        }
    }

    fn reply_preview(
        events: &Sender<DaemonToUi>,
        epoch: u64,
        request: &crate::models::UiRequest,
        warnings: Vec<routelane::backend::RoutingWarning>,
    ) {
        let DaemonRequest::PreviewProfile(profile) = &request.request else {
            panic!("a preview was expected");
        };
        events
            .try_send(DaemonToUi::Response {
                epoch,
                response: routelane::ipc::DaemonResponse::success(
                    request.request_id.clone(),
                    DaemonResult::Preview(routelane::backend::RoutingPreview {
                        policy_hash: routelane::backend::policy_hash(
                            profile,
                            &routelane::backend::ResolutionSnapshot::empty(),
                        ),
                        warnings,
                    }),
                ),
            })
            .unwrap();
        drain();
    }

    /// Writes a PNG of `widget` when ROUTELANE_SMOKE_SHOTS names a directory.
    fn screenshot(widget: &impl IsA<gtk::Widget>, name: &str) {
        let Some(directory) = std::env::var_os("ROUTELANE_SMOKE_SHOTS") else {
            return;
        };
        let widget = widget.as_ref();
        wait_for(|| widget.width() > 0 && widget.height() > 0);
        let paintable = gtk::WidgetPaintable::new(Some(widget));
        // The paintable has content only after the widget drew a frame.
        let mut node = None;
        for _ in 0..200 {
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(&snapshot, widget.width() as f64, widget.height() as f64);
            node = snapshot.to_node();
            if node.is_some() {
                break;
            }
            widget.queue_draw();
            drain();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let Some(node) = node else {
            eprintln!("screenshot {name}: NOT CAPTURED (no rendered frame)");
            return;
        };
        let renderer = widget.native().expect("a native surface").renderer();
        let texture = renderer.render_texture(&node, None);
        texture
            .save_to_png(std::path::Path::new(&directory).join(name))
            .unwrap();
    }

    fn visible_dialog(window: &adw::ApplicationWindow) -> Option<gtk::Window> {
        gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Window>().ok())
            .find(|dialog| {
                dialog.is_visible() && dialog.transient_for().as_ref() == Some(window.upcast_ref())
            })
    }

    /// Scope selection and the preview gate on a profile with a lab rule.
    fn assert_routing_scope_and_activation_preview(
        app: &adw::Application,
        language: routelane::config::Language,
    ) {
        use routelane::backend::RoutingWarning;
        use routelane::domain::{
            InterfaceSelector, Lane, LaneFailurePolicy, RuleTarget, UnmatchedTraffic,
        };
        let texts = language.texts();
        let tag = format!("{language:?}").to_lowercase();
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("config.json");
        let mut document = ConfigV4 {
            language,
            ..ConfigV4::default()
        };
        let mut profile = empty_default_profile();
        profile.lanes = [("ethernet", "eth0"), ("vinn", "usb0")]
            .into_iter()
            .map(|(id, interface)| Lane {
                id: id.into(),
                name: id.to_owned(),
                interface: InterfaceSelector::from(interface),
                fallback_lane: None,
                failure_policy: LaneFailurePolicy::Block,
            })
            .collect();
        profile.rules.push(Rule {
            id: "lab-route".into(),
            enabled: true,
            priority: 10,
            source: RuleSource::Any,
            target: RuleTarget::Cidr("10.65.201.0/24".parse().unwrap()),
            action: RuleAction::Route("ethernet".into()),
        });
        document.profiles.push(profile);
        ConfigRepository::new(&path).save(&document).unwrap();
        let stored = || std::fs::read(&path).unwrap();
        let before = stored();

        let (sender, requests) = async_channel::unbounded();
        let (events, receiver) = async_channel::unbounded();
        let window =
            build_window_with_repository(app, sender, receiver, ConfigRepository::new(&path));
        let snapshot = |epoch| {
            events
                .try_send(DaemonToUi::ConnectionEstablished { epoch })
                .unwrap();
            events
                .try_send(DaemonToUi::Snapshot {
                    epoch,
                    status: DaemonStatus {
                        generation: 0,
                        event_sequence: 0,
                        health: HealthReport::inactive(),
                        desired_profile_id: None,
                        applied_profile_id: None,
                        last_error: None,
                        primary: Default::default(),
                    },
                    profiles: vec![],
                })
                .unwrap();
            drain();
        };
        snapshot(1);
        find_named(window.upcast_ref(), "connections")
            .unwrap()
            .downcast::<gtk::ToggleButton>()
            .unwrap()
            .emit_clicked();
        drain();
        let scope = find_named(window.upcast_ref(), "routing-scope")
            .unwrap()
            .downcast::<adw::ComboRow>()
            .unwrap();
        assert_eq!(scope.selected(), 0, "selective is the default scope");
        assert!(
            scope.is_focusable() || scope.can_focus(),
            "keyboard reachable"
        );
        screenshot(&window, &format!("connections-scope-{tag}.png"));

        // Escape on the scope confirmation changes nothing.
        scope.set_selected(1);
        drain();
        let dialog = transient(&window);
        assert_eq!(
            dialog.title().as_deref(),
            Some(texts.primary.scope_change_heading)
        );
        screenshot(&dialog, &format!("scope-confirmation-{tag}.png"));
        escape(&dialog);
        drain();
        assert_eq!(scope.selected(), 0, "Escape restores the saved scope");
        assert_eq!(stored(), before);
        assert!(requests.try_recv().is_err());

        let activate = || {
            find_named(window.upcast_ref(), "routing-primary")
                .unwrap()
                .downcast::<gtk::Button>()
                .unwrap()
                .emit_clicked();
            let request = next_request(&requests);
            assert!(matches!(request.request, DaemonRequest::PreviewProfile(_)));
            request
        };

        // The lab rule is disclosed before any save; Cancel sends nothing.
        let preview = activate();
        reply_preview(
            &events,
            1,
            &preview,
            vec![RoutingWarning::LocalNetworkRule {
                rule_id: "lab-route".into(),
                network: "10.65.201.0/24".parse().unwrap(),
            }],
        );
        let dialog = transient(&window);
        assert_eq!(
            dialog.title().as_deref(),
            Some(texts.primary.impact_heading)
        );
        screenshot(&dialog, &format!("impact-confirmation-{tag}.png"));
        escape(&dialog);
        drain();
        assert!(
            requests.try_recv().is_err(),
            "cancelled impact applies nothing"
        );
        assert_eq!(stored(), before, "and saves nothing");

        // A helper that cannot observe the networks: no dialog, no apply.
        let preview = activate();
        events
            .try_send(DaemonToUi::Response {
                epoch: 1,
                response: routelane::ipc::DaemonResponse::failure(
                    preview.request_id.clone(),
                    routelane::ipc::IpcError::new("DAEMON_PREVIEW_UNAVAILABLE", "not observed"),
                ),
            })
            .unwrap();
        drain();
        assert!(visible_dialog(&window).is_none());
        assert!(requests.try_recv().is_err());

        // A reply from before a reconnect approves nothing.
        let preview = activate();
        snapshot(2);
        reply_preview(&events, 1, &preview, vec![]);
        assert!(visible_dialog(&window).is_none());
        assert!(requests.try_recv().is_err());
        assert_eq!(stored(), before);

        // A current preview without warnings activates exactly once.
        let preview = activate();
        reply_preview(&events, 2, &preview, vec![]);
        let apply = next_request(&requests);
        let DaemonRequest::ApplyProfile(applied) = &apply.request else {
            panic!("ApplyProfile expected after a current preview");
        };
        assert_eq!(
            applied.routing.unmatched_traffic,
            UnmatchedTraffic::PreserveSystem
        );
        assert!(requests.try_recv().is_err());

        find_named(window.upcast_ref(), "overview")
            .unwrap()
            .downcast::<gtk::ToggleButton>()
            .unwrap()
            .emit_clicked();
        drain();
        screenshot(&window, &format!("overview-scope-{tag}.png"));
        window.destroy();
        drain();
    }

    fn assert_primary_selection_is_confirmed_cancel_safe_and_reopens(
        app: &adw::Application,
        texts: Texts,
    ) {
        use routelane::domain::{InterfaceSelector, Lane, LaneFailurePolicy, PrimarySelection};
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("config.json");
        let mut document = ConfigV4::default();
        let mut profile = empty_default_profile();
        profile.lanes = [("ethernet", "eth0"), ("vinn", "usb0")]
            .into_iter()
            .map(|(id, interface)| Lane {
                id: id.into(),
                name: id.to_owned(),
                interface: InterfaceSelector::from(interface),
                fallback_lane: None,
                failure_policy: LaneFailurePolicy::Block,
            })
            .collect();
        document.profiles.push(profile);
        ConfigRepository::new(&path).save(&document).unwrap();
        let saved_primary = || {
            ConfigRepository::new(&path).load().unwrap().profiles[0]
                .routing
                .clone()
        };
        let open = || {
            let (sender, requests) = async_channel::unbounded();
            let (events, receiver) = async_channel::unbounded();
            let window =
                build_window_with_repository(app, sender, receiver, ConfigRepository::new(&path));
            events
                .try_send(DaemonToUi::ConnectionEstablished { epoch: 1 })
                .unwrap();
            events
                .try_send(DaemonToUi::Snapshot {
                    epoch: 1,
                    status: DaemonStatus {
                        generation: 0,
                        event_sequence: 0,
                        health: HealthReport::inactive(),
                        desired_profile_id: None,
                        applied_profile_id: None,
                        last_error: None,
                        primary: Default::default(),
                    },
                    profiles: vec![],
                })
                .unwrap();
            drain();
            find_named(window.upcast_ref(), "connections")
                .unwrap()
                .downcast::<gtk::ToggleButton>()
                .unwrap()
                .emit_clicked();
            drain();
            let selector = find_named(window.upcast_ref(), "primary-selector")
                .unwrap()
                .downcast::<adw::ComboRow>()
                .unwrap();
            (window, selector, requests, events)
        };

        let (window, selector, requests, _events) = open();
        assert_eq!(
            selector.selected(),
            0,
            "the OS default is the initial preference"
        );
        selector.set_selected(1);
        drain();
        let dialog = transient(&window);
        assert_eq!(
            dialog.title().as_deref(),
            Some(texts.primary.change_heading)
        );
        assert_eq!(
            dialog
                .default_widget()
                .unwrap()
                .downcast::<gtk::Button>()
                .unwrap()
                .label()
                .as_deref(),
            Some(texts.cancel),
            "Cancel is the default action"
        );
        escape(&dialog);
        drain();
        assert_eq!(
            selector.selected(),
            0,
            "Escape restores the saved preference"
        );
        assert_eq!(
            saved_primary().primary,
            PrimarySelection::FollowSystemDefault
        );
        assert!(requests.try_recv().is_err());

        selector.set_selected(1);
        drain();
        let dialog = transient(&window);
        find_button(dialog.upcast_ref(), texts.confirm)
            .unwrap()
            .emit_clicked();
        drain();
        wait_for(|| saved_primary().primary == PrimarySelection::Lane("ethernet".into()));
        assert_eq!(saved_primary().backups, vec!["vinn".into()]);
        assert!(
            requests.try_recv().is_err(),
            "an inactive profile is saved without activating routing"
        );
        // Backup controls stay live after the primary change re-rendered them.
        // Editing is disabled while a save is in flight; rows rebuild after it.
        let settled = |name: &str| {
            let mut found = None;
            wait_for(|| {
                found =
                    find_named(window.upcast_ref(), name).filter(|widget| widget.is_sensitive());
                found.is_some()
            });
            found.unwrap()
        };
        let backup = settled("primary-backup-vinn")
            .downcast::<gtk::CheckButton>()
            .unwrap();
        assert!(backup.is_active());
        backup.set_active(false);
        drain();
        wait_for(|| saved_primary().backups.is_empty());
        let automatic = settled("primary-automatic")
            .downcast::<gtk::Switch>()
            .unwrap();
        automatic.set_active(false);
        drain();
        wait_for(|| !saved_primary().automatic_failover);
        window.destroy();
        drain();

        let (window, selector, _requests, _events) = open();
        assert_eq!(
            selector.selected(),
            1,
            "the confirmed preference survives reopening"
        );
        window.destroy();
        drain();
    }

    #[test]
    fn partial_daemon_profile_snapshot_merges_without_deleting_local_profiles() {
        let mut document = ConfigV4::default();
        let mut selected = empty_default_profile();
        document.profiles.push(selected.clone());
        let mut other = selected.clone();
        other.id = "other".into();
        document.profiles.push(other);
        let mut replacement = selected.clone();
        replacement.name = "Canonical".into();
        reconcile_profiles(&mut document, &mut selected, vec![replacement], true);
        assert_eq!(selected.name, "Canonical");
        assert_eq!(document.profiles.len(), 2);
    }

    #[test]
    fn snapshot_preserves_local_failed_apply_draft() {
        let mut selected = empty_default_profile();
        selected.name = "Local edit".into();
        let mut document = ConfigV4 {
            profiles: vec![selected.clone()],
            ..Default::default()
        };
        reconcile_profiles(
            &mut document,
            &mut selected,
            vec![empty_default_profile()],
            false,
        );
        assert_eq!(selected.name, "Local edit");
        assert_eq!(document.profiles[0].name, "Local edit");
    }

    #[test]
    fn canonical_profile_survives_a_later_language_save() {
        let mut selected = empty_default_profile();
        let mut document = ConfigV4::default();
        let mut canonical = selected.clone();
        canonical.name = "Canonical".into();
        reconcile_profiles(&mut document, &mut selected, vec![canonical], true);
        document.language = routelane::config::Language::En;
        assert_eq!(document.profiles[0].name, "Canonical");
        assert_eq!(selected.id.as_str(), "default");
    }
}
