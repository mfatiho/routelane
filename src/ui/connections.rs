use crate::ui::{
    confirm::{confirm_destructive_action, confirm_destructive_action_with_label},
    controller::UiController,
    form::Form,
    i18n::Texts,
    list_page::ListPage,
    primary::{
        proposed_backups, validate_primary_draft, PrimaryChange, PrimaryDraft, PrimaryEffect,
    },
    settings::{add_lane, delete_lane, lane_edit_error_text, rename_lane, routed_rule_count},
    shell::Section,
    usage_widgets::{format_bytes, format_gb, format_limit, QuotaBar},
};
use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};
use routelane::domain::{
    canonical_lane_name, validate_profile, Lane, LaneFailurePolicy, LaneId, PrimarySelection,
    Profile, RoutingPolicy, RuleAction, RuleSource, UnmatchedTraffic,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// The report supplies the same seven daily buckets formerly shown on Overview.
fn seven_day_totals(usage: &routelane::usage::service::LaneUsage, texts: Texts) -> String {
    let (rx, tx) = usage
        .daily
        .iter()
        .fold((0u64, 0u64), |(rx, tx), (_, totals)| {
            (rx.saturating_add(totals.rx), tx.saturating_add(totals.tx))
        });
    texts
        .ux
        .seven_day_totals
        .replace("{down}", &format_gb(rx, texts.language))
        .replace("{up}", &format_gb(tx, texts.language))
}

#[derive(Debug, Clone)]
pub struct LaneDraft {
    pub name: String,
    pub interface: String,
    pub failure_policy: LaneFailurePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneEditError {
    EmptyName,
    InvalidName,
    MissingInterface,
    NotFound,
    FallbackInUse,
    /// The routing policy names this lane as primary or backup.
    RoutingInUse,
}

pub fn validate_lane_draft(
    profile: &Profile,
    lane_id: Option<&LaneId>,
    draft: &LaneDraft,
    interfaces: &[String],
) -> Result<LaneDraft, LaneEditError> {
    if lane_id.is_some_and(|id| !profile.lanes.iter().any(|lane| &lane.id == id)) {
        return Err(LaneEditError::NotFound);
    }
    if draft.name.trim().is_empty() {
        return Err(LaneEditError::EmptyName);
    }
    let name = canonical_lane_name(profile, lane_id, &draft.name)
        .map_err(|_| LaneEditError::InvalidName)?;
    if !interfaces.contains(&draft.interface) {
        return Err(LaneEditError::MissingInterface);
    }
    Ok(LaneDraft {
        name,
        ..draft.clone()
    })
}

pub fn lane_delete_impact(profile: &Profile, lane_id: &LaneId) -> (usize, usize) {
    let apps = profile
        .rules
        .iter()
        .filter(|rule| matches!(&rule.action, RuleAction::Route(id) if id == lane_id))
        .filter(|rule| matches!(rule.source, RuleSource::Application(_)))
        .count();
    (routed_rule_count(profile, lane_id) - apps, apps)
}

pub fn delete_candidate(profile: &Profile, lane_id: &LaneId) -> Result<Profile, LaneEditError> {
    if !profile.lanes.iter().any(|lane| &lane.id == lane_id) {
        return Err(LaneEditError::NotFound);
    }
    if profile
        .lanes
        .iter()
        .any(|lane| lane.fallback_lane.as_ref() == Some(lane_id))
    {
        return Err(LaneEditError::FallbackInUse);
    }
    // The primary/backup policy must be revised explicitly first; a dangling
    // reference is never pruned or redirected silently.
    if crate::ui::primary::routing_references(profile, lane_id) {
        return Err(LaneEditError::RoutingInUse);
    }
    let mut candidate = profile.clone();
    delete_lane(&mut candidate, lane_id);
    validate_profile(&candidate).map_err(|_| LaneEditError::FallbackInUse)?;
    Ok(candidate)
}

fn interfaces() -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir("/sys/class/net").map_err(|error| error.to_string())?;
    let mut interfaces = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    interfaces.sort();
    Ok(interfaces)
}

fn discover_interfaces(callback: impl FnOnce(Result<Vec<String>, String>) + 'static) {
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        sender.send_blocking(interfaces()).ok();
    });
    glib::MainContext::ref_thread_default().spawn_local(async move {
        if let Ok(result) = receiver.recv().await {
            callback(result);
        }
    });
}

pub fn open_connection_editor(
    parent: &adw::ApplicationWindow,
    texts: Texts,
    profile: Rc<RefCell<Profile>>,
    editing: Option<LaneId>,
    on_save: Rc<dyn Fn(Profile) -> bool>,
) {
    let original = editing.as_ref().and_then(|id| {
        profile
            .borrow()
            .lanes
            .iter()
            .find(|lane| &lane.id == id)
            .cloned()
    });
    let form = Form::new(
        parent,
        if editing.is_some() {
            texts.ux.edit
        } else {
            texts.ux.add_connection
        },
        texts,
    );
    let name = gtk::Entry::builder()
        .activates_default(true)
        .text(
            original
                .as_ref()
                .map(|lane| lane.name.as_str())
                .unwrap_or(""),
        )
        .build();
    name.set_widget_name("connection-name");
    let interface = gtk::DropDown::new(None::<&gtk::StringList>, None::<&gtk::Expression>);
    interface.set_selected(gtk::INVALID_LIST_POSITION);
    interface.set_widget_name("connection-interface");
    let policy = gtk::DropDown::from_strings(&[
        texts.ux.fail_block,
        texts.ux.fail_direct,
        texts.ux.fail_inactive,
    ]);
    policy.set_selected(match original.as_ref().map(|lane| &lane.failure_policy) {
        Some(LaneFailurePolicy::Direct) => 1,
        Some(LaneFailurePolicy::Inactive) => 2,
        _ => 0,
    });
    let warning = gtk::Label::builder()
        .label(texts.ux.leak_warning)
        .wrap(true)
        .xalign(0.0)
        .visible(policy.selected() != 0)
        .build();
    let warning_copy = warning.clone();
    policy.connect_selected_notify(move |policy| warning_copy.set_visible(policy.selected() != 0));
    form.field(texts.lane_name, &name);
    form.field(texts.ux.connection, &interface);
    form.field(texts.ux.failure_policy, &policy);
    form.fields.append(&warning);
    form.submit.set_sensitive(false);
    let available = Rc::new(RefCell::new(Vec::<String>::new()));
    let choices = available.clone();
    let dropdown = interface.clone();
    let form_ready = form.clone();
    let selected = original
        .as_ref()
        .map(|lane| lane.interface.as_str().to_owned());
    discover_interfaces(move |result| {
        if !form_ready.dialog.is_visible() {
            return;
        }
        match result {
            Ok(names) => {
                let labels = names.iter().map(String::as_str).collect::<Vec<_>>();
                dropdown.set_model(Some(&gtk::StringList::new(&labels)));
                dropdown.set_selected(
                    selected
                        .as_ref()
                        .and_then(|selected| names.iter().position(|name| name == selected))
                        .map(|i| i as u32)
                        .unwrap_or(gtk::INVALID_LIST_POSITION),
                );
                *choices.borrow_mut() = names;
                form_ready.submit.set_sensitive(true);
            }
            Err(_) => {
                form_ready.feedback.set_label(texts.ux.missing_interface);
                form_ready.feedback.set_visible(true);
            }
        }
    });
    let submit_form = form.clone();
    let initial_focus = name.clone();
    let handler = form.submit.connect_clicked(move |_| {
        if !submit_form.begin_submit() {
            return;
        }
        let draft = LaneDraft {
            name: name.text().to_string(),
            interface: available
                .borrow()
                .get(interface.selected() as usize)
                .cloned()
                .unwrap_or_default(),
            failure_policy: match policy.selected() {
                1 => LaneFailurePolicy::Direct,
                2 => LaneFailurePolicy::Inactive,
                _ => LaneFailurePolicy::Block,
            },
        };
        let form = submit_form.clone();
        let profile = profile.clone();
        let editing = editing.clone();
        let on_save = on_save.clone();
        let name = name.clone();
        let interface = interface.clone();
        discover_interfaces(move |result| {
            if !form.dialog.is_visible() {
                return;
            }
            let current = profile.borrow().clone();
            let names = result.unwrap_or_default();
            match validate_lane_draft(&current, editing.as_ref(), &draft, &names) {
                Ok(draft) => {
                    let mut candidate = current;
                    if let Some(id) = &editing {
                        if let Err(error) = rename_lane(&mut candidate, id, &draft.name) {
                            form.error(lane_edit_error_text(error, texts), &name);
                            return;
                        }
                        let lane = candidate
                            .lanes
                            .iter_mut()
                            .find(|lane| &lane.id == id)
                            .expect("validated lane");
                        lane.interface = draft.interface.as_str().into();
                        lane.failure_policy = draft.failure_policy;
                    } else {
                        if let Err(error) = add_lane(
                            &mut candidate,
                            &draft.name,
                            &draft.interface,
                            draft.failure_policy,
                        ) {
                            form.error(lane_edit_error_text(error, texts), &name);
                            return;
                        }
                    }
                    if on_save(candidate) {
                        form.dialog.close();
                    } else {
                        form.error(texts.ux.request_failed, &name);
                    }
                }
                Err(error) => form.error(
                    match error {
                        LaneEditError::EmptyName => texts.lane_name_empty,
                        LaneEditError::InvalidName => texts.lane_name_duplicate,
                        LaneEditError::MissingInterface => texts.ux.missing_interface,
                        _ => texts.lane_not_found,
                    },
                    if error == LaneEditError::MissingInterface {
                        interface.upcast_ref::<gtk::Widget>()
                    } else {
                        name.upcast_ref()
                    },
                ),
            }
        });
    });
    form.disconnect_on_close(handler);
    form.dialog.present();
    initial_focus.grab_focus();
}

/// The OS IPv4 default interface, from the lowest-metric default in
/// /proc/net/route (read-only, unprivileged).
fn system_default_interface() -> Option<String> {
    let table = std::fs::read_to_string("/proc/net/route").ok()?;
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            (fields.len() > 7 && fields[1] == "00000000" && fields[7] == "00000000").then(|| {
                (
                    fields[6].parse::<u32>().unwrap_or(u32::MAX),
                    fields[0].to_owned(),
                )
            })
        })
        .min()
        .map(|(_, interface)| interface)
}

/// What the primary controls were last rendered from.
type PrimaryRenderKey = (RoutingPolicy, Vec<Lane>, Option<String>);
/// Primary connection, ordered backups and automatic failover.
struct PrimaryControls {
    group: adw::PreferencesGroup,
    combo: adw::ComboRow,
    /// Which new traffic the policy manages (`routing-scope`).
    scope: adw::ComboRow,
    backups: adw::PreferencesGroup,
    automatic: gtk::Switch,
    backup_rows: RefCell<Vec<adw::ActionRow>>,
    rendered: RefCell<Option<PrimaryRenderKey>>,
    updating: Cell<bool>,
    system_default: RefCell<Option<String>>,
    change: RefCell<PrimaryChange>,
    feedback: gtk::Label,
    texts: Texts,
    /// Every control writes through the live profile and the page's save path.
    live: Rc<RefCell<Profile>>,
    on_save: Rc<dyn Fn(Profile) -> bool>,
}

impl PrimaryControls {
    fn new(texts: Texts, live: Rc<RefCell<Profile>>, on_save: Rc<dyn Fn(Profile) -> bool>) -> Self {
        let group = adw::PreferencesGroup::builder()
            .title(texts.primary.title)
            .description(texts.primary.description)
            .build();
        let combo = adw::ComboRow::builder().title(texts.primary.row).build();
        combo.set_widget_name("primary-selector");
        let scope = adw::ComboRow::builder()
            .title(texts.primary.scope_row)
            .build();
        scope.set_widget_name("routing-scope");
        scope.set_model(Some(&gtk::StringList::new(&[
            texts.primary.scope_selective,
            texts.primary.scope_use_primary,
        ])));
        scope.update_property(&[gtk::accessible::Property::Label(texts.primary.scope_row)]);
        let automatic = gtk::Switch::builder().valign(gtk::Align::Center).build();
        automatic.set_widget_name("primary-automatic");
        automatic.update_property(&[gtk::accessible::Property::Label(texts.primary.automatic)]);
        let automatic_row = adw::ActionRow::builder()
            .title(texts.primary.automatic)
            .subtitle(texts.primary.automatic_subtitle)
            .activatable_widget(&automatic)
            .build();
        automatic_row.add_suffix(&automatic);
        group.add(&scope);
        group.add(&combo);
        group.add(&automatic_row);
        let backups = adw::PreferencesGroup::builder()
            .title(texts.primary.backups)
            .description(texts.primary.backups_description)
            .build();
        let feedback = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .build();
        feedback.add_css_class("error");
        Self {
            group,
            combo,
            scope,
            backups,
            automatic,
            backup_rows: RefCell::new(vec![]),
            rendered: RefCell::new(None),
            updating: Cell::new(false),
            system_default: RefCell::new(None),
            change: RefCell::new(PrimaryChange::default()),
            feedback,
            texts,
            live,
            on_save,
        }
    }

    fn choice_label(&self, texts: Texts, choice: &PrimarySelection, profile: &Profile) -> String {
        match choice {
            PrimarySelection::FollowSystemDefault => format!(
                "{} ({})",
                texts.primary.system_default,
                self.system_default
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| texts.primary.system_default_unknown.to_owned())
            ),
            PrimarySelection::Lane(id) => profile
                .lanes
                .iter()
                .find(|lane| &lane.id == id)
                .map_or_else(|| id.as_str().to_owned(), |lane| lane.name.clone()),
        }
    }

    fn choices(profile: &Profile) -> Vec<PrimarySelection> {
        std::iter::once(PrimarySelection::FollowSystemDefault)
            .chain(
                profile
                    .lanes
                    .iter()
                    .map(|lane| PrimarySelection::Lane(lane.id.clone())),
            )
            .collect()
    }

    /// The lane the preference resolves to now (FollowSystemDefault maps the
    /// OS default interface back to a configured lane, if any).
    fn resolved_primary(&self, profile: &Profile, primary: &PrimarySelection) -> Option<LaneId> {
        match primary {
            PrimarySelection::Lane(id) => Some(id.clone()),
            PrimarySelection::FollowSystemDefault => {
                let default = self.system_default.borrow().clone()?;
                profile
                    .lanes
                    .iter()
                    .find(|lane| lane.interface.as_str() == default)
                    .map(|lane| lane.id.clone())
            }
        }
    }
}

#[derive(Clone)]
pub struct ConnectionsPage {
    pub list: ListPage,
    profile: Rc<RefCell<Profile>>,
    parent: glib::WeakRef<adw::ApplicationWindow>,
    texts: Texts,
    on_save: Rc<dyn Fn(Profile) -> bool>,
    rows: Rc<RefCell<Vec<adw::ActionRow>>>,
    rendered: Rc<RefCell<Option<Vec<Lane>>>>,
    primary: Rc<PrimaryControls>,
    controller: Rc<UiController>,
    usage_rows: RefCell<Vec<(LaneId, adw::ActionRow, gtk::Box)>>,
}

impl ConnectionsPage {
    pub fn new(
        parent: &adw::ApplicationWindow,
        texts: Texts,
        profile: Rc<RefCell<Profile>>,
        on_save: Rc<dyn Fn(Profile) -> bool>,
        controller: Rc<UiController>,
    ) -> Self {
        let primary = Rc::new(PrimaryControls::new(
            texts,
            profile.clone(),
            on_save.clone(),
        ));
        let page = Self {
            list: ListPage::new(texts, Section::Connections),
            profile,
            parent: parent.downgrade(),
            texts,
            on_save,
            rows: Rc::new(RefCell::new(vec![])),
            rendered: Rc::new(RefCell::new(None)),
            primary,
            controller,
            usage_rows: RefCell::new(vec![]),
        };
        // Below the connection list: Add stays the first control of the page.
        page.list.root.append(&page.primary.group);
        page.list.root.append(&page.primary.feedback);
        page.list.root.append(&page.primary.backups);
        page.connect_primary_controls();
        page.refresh_system_default();
        let parent = page.parent.clone();
        let profile = page.profile.clone();
        let on_save = page.on_save.clone();
        page.list.add.connect_clicked(move |button| {
            if button.is_sensitive() {
                if let Some(parent) = parent.upgrade() {
                    open_connection_editor(&parent, texts, profile.clone(), None, on_save.clone());
                }
            }
        });
        page
    }
    pub fn open_add(&self) {
        if !self.list.add.is_sensitive() {
            return;
        }
        let Some(parent) = self.parent.upgrade() else {
            return;
        };
        open_connection_editor(
            &parent,
            self.texts,
            self.profile.clone(),
            None,
            self.on_save.clone(),
        );
    }
    /// Reads the OS default interface off the main thread, then re-renders.
    fn refresh_system_default(&self) {
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            sender.send_blocking(system_default_interface()).ok();
        });
        let controls = self.primary.clone();
        glib::MainContext::ref_thread_default().spawn_local(async move {
            if let Ok(default) = receiver.recv().await {
                *controls.system_default.borrow_mut() = default;
                render_primary(&controls);
            }
        });
    }

    fn connect_primary_controls(&self) {
        let controls = self.primary.clone();
        let profile = self.profile.clone();
        let parent = self.parent.clone();
        let on_save = self.on_save.clone();
        let texts = self.texts;
        self.primary.combo.connect_selected_notify(move |combo| {
            if controls.updating.get() {
                return;
            }
            let current = profile.borrow().clone();
            let Some(choice) = PrimaryControls::choices(&current)
                .get(combo.selected() as usize)
                .cloned()
            else {
                return;
            };
            if choice == current.routing.primary {
                return;
            }
            let resolved = controls.resolved_primary(&current, &choice);
            let backups = if current.lanes.len() == 2 {
                proposed_backups(&current, resolved.as_ref())
            } else {
                current
                    .routing
                    .backups
                    .iter()
                    .filter(|id| Some(*id) != resolved.as_ref())
                    .cloned()
                    .collect()
            };
            let name = controls.choice_label(texts, &choice, &current);
            controls.change.borrow_mut().request(PrimaryDraft {
                primary: choice,
                backups,
                ..PrimaryDraft::of(&current)
            });
            confirm_primary_change(
                &controls,
                &parent,
                &profile,
                &on_save,
                texts.primary.change_heading,
                &texts.primary.change_body.replace("{}", &name),
            );
        });
        let controls = self.primary.clone();
        let profile = self.profile.clone();
        let parent = self.parent.clone();
        let on_save = self.on_save.clone();
        self.primary.scope.connect_selected_notify(move |scope| {
            if controls.updating.get() {
                return;
            }
            let current = profile.borrow().clone();
            let mode = match scope.selected() {
                0 => UnmatchedTraffic::PreserveSystem,
                1 => UnmatchedTraffic::UsePrimary,
                _ => return,
            };
            if mode == current.routing.unmatched_traffic {
                return;
            }
            controls.change.borrow_mut().request(PrimaryDraft {
                unmatched_traffic: mode,
                ..PrimaryDraft::of(&current)
            });
            let body = match mode {
                UnmatchedTraffic::PreserveSystem => texts.primary.scope_to_selective_body,
                UnmatchedTraffic::UsePrimary => texts.primary.scope_to_primary_body,
            };
            confirm_primary_change(
                &controls,
                &parent,
                &profile,
                &on_save,
                texts.primary.scope_change_heading,
                body,
            );
        });
        let controls = self.primary.clone();
        self.primary.automatic.connect_state_set(move |_, enabled| {
            if controls.updating.get() {
                return glib::signal::Inhibit(false);
            }
            let current = controls.live.borrow().clone();
            let draft = PrimaryDraft {
                automatic_failover: enabled,
                ..PrimaryDraft::of(&current)
            };
            save_primary_draft(&controls, &current, &draft);
            glib::signal::Inhibit(false)
        });
    }

    pub fn update(&self, editing_enabled: bool) {
        self.list.add.set_sensitive(editing_enabled);
        self.list.group.set_sensitive(editing_enabled);
        self.primary.group.set_sensitive(editing_enabled);
        self.primary.backups.set_sensitive(editing_enabled);
        render_primary(&self.primary);
        let profile = self.profile.borrow().clone();
        self.update_usage();
        if self.rendered.borrow().as_ref() == Some(&profile.lanes) {
            return;
        }
        *self.rendered.borrow_mut() = Some(profile.lanes.clone());
        self.usage_rows.borrow_mut().clear();
        for row in self.rows.borrow_mut().drain(..) {
            self.list.group.remove(&row);
        }
        self.list.set_empty(profile.lanes.is_empty(), false);
        for lane in &profile.lanes {
            let row = adw::ActionRow::builder()
                .title(&glib::markup_escape_text(&lane.name))
                .activatable(true)
                .build();
            row.set_widget_name(&format!("lane-row-{}", lane.id.as_str()));
            let usage_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
            usage_box.set_valign(gtk::Align::Center);
            row.add_suffix(&usage_box);
            self.usage_rows
                .borrow_mut()
                .push((lane.id.clone(), row.clone(), usage_box));
            let parent = self.parent.clone();
            let lane_copy = lane.clone();
            let controller = self.controller.clone();
            let texts = self.texts;
            let live_profile = self.profile.clone();
            let on_save = self.on_save.clone();
            row.connect_activated(move |_| {
                if let Some(window) = parent.upgrade() {
                    let parent = parent.clone();
                    let profile = live_profile.clone();
                    let save = on_save.clone();
                    let id = lane_copy.id.clone();
                    let edit = Rc::new(move || {
                        if let Some(parent) = parent.upgrade() {
                            open_connection_editor(
                                &parent,
                                texts,
                                profile.clone(),
                                Some(id.clone()),
                                save.clone(),
                            );
                        }
                    });
                    crate::ui::lane_detail::open(
                        &window,
                        texts,
                        lane_copy.clone(),
                        controller.clone(),
                        edit,
                    );
                }
            });
            let edit = gtk::Button::with_label(self.texts.ux.edit);
            edit.set_valign(gtk::Align::Center);
            let parent = self.parent.clone();
            let texts = self.texts;
            let live_profile = self.profile.clone();
            let on_save = self.on_save.clone();
            let id = lane.id.clone();
            edit.connect_clicked(move |_| {
                let Some(parent) = parent.upgrade() else {
                    return;
                };
                open_connection_editor(
                    &parent,
                    texts,
                    live_profile.clone(),
                    Some(id.clone()),
                    on_save.clone(),
                )
            });
            let delete = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text(self.texts.delete_lane)
                .valign(gtk::Align::Center)
                .build();
            delete.update_property(&[
                gtk::accessible::Property::Label(self.texts.delete_lane),
                gtk::accessible::Property::Description(&lane.name),
            ]);
            let parent = self.parent.clone();
            let texts = self.texts;
            let live_profile = self.profile.clone();
            let on_save = self.on_save.clone();
            let feedback = self.list.feedback.clone();
            let id = lane.id.clone();
            let title = lane.name.clone();
            delete.connect_clicked(move |_| {
                let Some(parent) = parent.upgrade() else {
                    return;
                };
                let (rules, apps) = lane_delete_impact(&live_profile.borrow(), &id);
                let body = format!(
                    "{title}\n{}: {rules} {} · {apps} {}",
                    texts.ux.remove_impact, texts.ux.rules, texts.ux.applications
                );
                let live_profile = live_profile.clone();
                let on_save = on_save.clone();
                let feedback = feedback.clone();
                let id = id.clone();
                confirm_destructive_action(
                    &parent,
                    texts,
                    texts.confirm_delete_lane_heading,
                    &body,
                    Rc::new(move || {
                        let candidate = delete_candidate(&live_profile.borrow(), &id);
                        let message = match candidate {
                            Ok(candidate) => {
                                if on_save(candidate) {
                                    None
                                } else {
                                    Some(texts.ux.request_failed)
                                }
                            }
                            Err(LaneEditError::FallbackInUse) => Some(texts.ux.fallback_in_use),
                            Err(LaneEditError::RoutingInUse) => Some(texts.primary.routing_in_use),
                            Err(_) => Some(texts.lane_not_found),
                        };
                        feedback.set_label(message.unwrap_or(""));
                        feedback.set_visible(message.is_some());
                    }),
                );
            });
            row.add_suffix(&edit);
            row.add_suffix(&delete);
            self.list.group.add(&row);
            self.rows.borrow_mut().push(row);
        }
        self.update_usage();
    }

    fn update_usage(&self) {
        let session = self.controller.session();
        for (id, row, box_) in self.usage_rows.borrow().iter() {
            let profile = self.profile.borrow();
            let Some(lane) = profile.lanes.iter().find(|lane| &lane.id == id) else {
                continue;
            };
            let usage = session
                .usage()
                .and_then(|report| report.lanes.iter().find(|u| &u.lane_id == id));
            let texts = self.texts.usage;
            let language = self.texts.language;
            let subtitle = match usage {
                Some(usage) if !usage.available && usage.quota.is_none() => {
                    texts.not_connected.to_owned()
                }
                Some(usage) => match usage.quota.as_ref().filter(|q| q.settings.enabled) {
                    Some(quota) => texts
                        .period_used
                        .replace("{used}", &format_gb(quota.used_bytes, language))
                        .replace(
                            "{limit}",
                            &format_limit(quota.settings.limit_bytes, language),
                        ),
                    None => texts
                        .today_down_up
                        .replace("{down}", &format_bytes(usage.today.rx, language))
                        .replace("{up}", &format_bytes(usage.today.tx, language)),
                },
                None if session.usage().is_none() && session.usage_error().is_some() => {
                    self.texts.ux.request_failed.to_owned()
                }
                None => self.texts.ux.checking.to_owned(),
            };
            let subtitle = if usage.is_some_and(|usage| !usage.available && usage.quota.is_some()) {
                format!("{subtitle} · {}", texts.not_connected)
            } else {
                subtitle
            };
            row.set_tooltip_text(
                session
                    .usage_error()
                    .filter(|_| session.usage().is_none())
                    .map(|error| error.to_string())
                    .as_deref(),
            );
            row.set_subtitle(&glib::markup_escape_text(&format!(
                "{} · {subtitle}",
                lane.interface.as_str()
            )));
            while let Some(child) = box_.first_child() {
                box_.remove(&child);
            }
            if let Some(quota) = usage
                .and_then(|u| u.quota.as_ref())
                .filter(|q| q.settings.enabled)
            {
                let percent = (quota.used_bytes as f64 / quota.settings.limit_bytes.max(1) as f64
                    * 100.0)
                    .round();
                let badge = gtk::Label::new(Some(
                    &texts
                        .quota_badge
                        .replace("{percent}", &format!("{percent:.0}")),
                ));
                badge.add_css_class("caption");
                box_.append(&badge);
                let bar = QuotaBar::new(quota, language).area;
                bar.set_widget_name(&format!("quota-bar-{}", id.as_str()));
                box_.append(&bar);
            } else if let Some(usage) = usage {
                let label = gtk::Label::new(Some(&seven_day_totals(usage, self.texts)));
                label.set_widget_name(&format!("lane-usage-{}", id.as_str()));
                label.add_css_class("caption");
                box_.append(&label);
            }
        }
    }
}

/// Saves a validated policy edit; on failure shows why and re-renders the
/// stored policy so no control claims a state the configuration lacks.
fn save_primary_draft(controls: &Rc<PrimaryControls>, current: &Profile, draft: &PrimaryDraft) {
    let saved = validate_primary_draft(current, draft)
        .ok()
        .is_some_and(|candidate| (controls.on_save)(candidate));
    controls
        .feedback
        .set_label(controls.texts.ux.request_failed);
    controls.feedback.set_visible(!saved);
    if !saved {
        controls.rendered.borrow_mut().take();
        let controls = controls.clone();
        glib::idle_add_local_once(move || render_primary(&controls));
    }
}

/// Rebuilds the controls only when the policy, lanes or OS default changed;
/// programmatic selection never triggers the change dialog.
/// The confirmation gate for a pending primary or scope change. Only Confirm
/// saves; Cancel, Escape and closing the dialog discard the draft.
fn confirm_primary_change(
    controls: &Rc<PrimaryControls>,
    parent: &glib::WeakRef<adw::ApplicationWindow>,
    profile: &Rc<RefCell<Profile>>,
    on_save: &Rc<dyn Fn(Profile) -> bool>,
    heading: &str,
    body: &str,
) {
    let texts = controls.texts;
    let Some(window) = parent.upgrade() else {
        controls.change.borrow_mut().cancel();
        return;
    };
    let confirm = {
        let controls = controls.clone();
        let profile = profile.clone();
        let on_save = on_save.clone();
        Rc::new(move || {
            let outcome = controls.change.borrow_mut().confirm(&profile.borrow());
            let saved = match outcome {
                Ok(effects) => effects.into_iter().all(|effect| {
                    let PrimaryEffect::Save(candidate) = effect;
                    on_save(candidate)
                }),
                Err(_) => false,
            };
            controls.feedback.set_label(texts.ux.request_failed);
            controls.feedback.set_visible(!saved);
            // A failed or stale save shows the stored preference again.
            controls.rendered.borrow_mut().take();
            render_primary(&controls);
        })
    };
    let cancel = {
        let controls = controls.clone();
        Rc::new(move || {
            controls.change.borrow_mut().cancel();
            controls.rendered.borrow_mut().take();
            render_primary(&controls);
        })
    };
    confirm_destructive_action_with_label(
        &window,
        texts,
        heading,
        body,
        texts.confirm,
        confirm,
        cancel,
    );
}

fn render_primary(controls: &Rc<PrimaryControls>) {
    let profile = controls.live.borrow().clone();
    let profile = &profile;
    let texts = controls.texts;
    let key = (
        profile.routing.clone(),
        profile.lanes.clone(),
        controls.system_default.borrow().clone(),
    );
    if controls.rendered.borrow().as_ref() == Some(&key) {
        return;
    }
    if controls.change.borrow().is_pending() {
        return;
    }
    controls.updating.set(true);
    let choices = PrimaryControls::choices(profile);
    let labels = choices
        .iter()
        .map(|choice| controls.choice_label(texts, choice, profile))
        .collect::<Vec<_>>();
    let labels = labels.iter().map(String::as_str).collect::<Vec<_>>();
    controls
        .combo
        .set_model(Some(&gtk::StringList::new(&labels)));
    let selected = choices
        .iter()
        .position(|choice| *choice == profile.routing.primary)
        .unwrap_or(0);
    controls.combo.set_selected(selected as u32);
    controls
        .automatic
        .set_active(profile.routing.automatic_failover);
    let mode = profile.routing.unmatched_traffic;
    controls.scope.set_selected(match mode {
        UnmatchedTraffic::PreserveSystem => 0,
        UnmatchedTraffic::UsePrimary => 1,
    });
    controls
        .scope
        .set_subtitle(crate::ui::window::scope_line(mode, texts.primary));
    for row in controls.backup_rows.borrow_mut().drain(..) {
        controls.backups.remove(&row);
    }
    let explicit_primary = match &profile.routing.primary {
        PrimarySelection::Lane(id) => Some(id.clone()),
        PrimarySelection::FollowSystemDefault => None,
    };
    let mut ordered = profile.routing.backups.clone();
    let others = profile
        .lanes
        .iter()
        .map(|lane| lane.id.clone())
        .filter(|id| !ordered.contains(id) && Some(id) != explicit_primary.as_ref())
        .collect::<Vec<_>>();
    ordered.extend(others);
    controls.backups.set_visible(!ordered.is_empty());
    for id in &ordered {
        let Some(lane) = profile.lanes.iter().find(|lane| &lane.id == id) else {
            continue;
        };
        let included = profile.routing.backups.contains(id);
        let position = profile.routing.backups.iter().position(|other| other == id);
        let check = gtk::CheckButton::builder()
            .active(included)
            .valign(gtk::Align::Center)
            .build();
        check.update_property(&[gtk::accessible::Property::Label(&lane.name)]);
        check.set_widget_name(&format!("primary-backup-{}", lane.id.as_str()));
        let row = adw::ActionRow::builder()
            .title(&glib::markup_escape_text(&lane.name))
            .subtitle(lane.interface.as_str())
            .activatable_widget(&check)
            .build();
        row.add_prefix(&check);
        let up = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text(texts.primary.move_up)
            .valign(gtk::Align::Center)
            .sensitive(position.is_some_and(|index| index > 0))
            .build();
        let down = gtk::Button::builder()
            .icon_name("go-down-symbolic")
            .tooltip_text(texts.primary.move_down)
            .valign(gtk::Align::Center)
            .sensitive(position.is_some_and(|index| index + 1 < profile.routing.backups.len()))
            .build();
        for (button, label) in [
            (&up, texts.primary.move_up),
            (&down, texts.primary.move_down),
        ] {
            button.update_property(&[
                gtk::accessible::Property::Label(label),
                gtk::accessible::Property::Description(&lane.name),
            ]);
        }
        row.add_suffix(&up);
        row.add_suffix(&down);
        {
            let change = |offset: Option<isize>, toggle: bool| {
                let controls = controls.clone();
                let id = id.clone();
                move || {
                    if controls.updating.get() {
                        return;
                    }
                    let current = controls.live.borrow().clone();
                    let mut backups = current.routing.backups.clone();
                    let index = backups.iter().position(|other| other == &id);
                    match (toggle, index, offset) {
                        (true, Some(index), _) => {
                            backups.remove(index);
                        }
                        (true, None, _) => backups.push(id.clone()),
                        (false, Some(index), Some(offset)) => {
                            let target = index as isize + offset;
                            if target >= 0 && (target as usize) < backups.len() {
                                backups.swap(index, target as usize);
                            }
                        }
                        _ => return,
                    }
                    let draft = PrimaryDraft {
                        backups,
                        ..PrimaryDraft::of(&current)
                    };
                    save_primary_draft(&controls, &current, &draft);
                }
            };
            let toggled = change(None, true);
            check.connect_toggled(move |_| toggled());
            let raise = change(Some(-1), false);
            up.connect_clicked(move |_| raise());
            let lower = change(Some(1), false);
            down.connect_clicked(move |_| lower());
        }
        controls.backups.add(&row);
        controls.backup_rows.borrow_mut().push(row);
    }
    *controls.rendered.borrow_mut() = Some(key);
    controls.updating.set(false);
}

#[cfg(test)]
mod tests {
    #[test]
    fn seven_day_totals_sum_all_report_days_and_use_tr_and_en_decimal_separators() {
        use super::*;
        use crate::ui::i18n::LanguageTexts;
        use routelane::{
            config::Language,
            usage::{service::LaneUsage, LocalDate, UsageTotals},
        };
        let usage = LaneUsage {
            lane_id: "ethernet".into(),
            interface: "eth0".into(),
            available: true,
            today: UsageTotals::default(),
            quota: None,
            rate: None,
            recent_rates: vec![],
            daily: [
                ("2026-11-09", 1_000_000_000, 100_000_000),
                ("2026-11-10", 2_100_000_000, 320_000_000),
            ]
            .into_iter()
            .map(|(date, rx, tx)| (LocalDate::parse(date).unwrap(), UsageTotals { rx, tx }))
            .collect(),
        };
        assert_eq!(
            seven_day_totals(&usage, Language::Tr.texts()),
            "Son 7 gün ↓ 3,1 GB ↑ 0,4 GB"
        );
        assert_eq!(
            seven_day_totals(&usage, Language::En.texts()),
            "Last 7 days ↓ 3.1 GB ↑ 0.4 GB"
        );
    }

    use super::*;
    fn profile() -> Profile {
        Profile {
            routing: Default::default(),
            id: "work".into(),
            name: "Work".into(),
            enabled: false,
            lanes: vec![Lane {
                id: "vpn".into(),
                name: "VPN".into(),
                interface: "tun0".into(),
                fallback_lane: None,
                failure_policy: LaneFailurePolicy::Block,
            }],
            rules: vec![],
        }
    }
    #[test]
    fn missing_interface_is_not_replaced_by_first_entry() {
        let draft = LaneDraft {
            name: "VPN".into(),
            interface: "tun0".into(),
            failure_policy: LaneFailurePolicy::Block,
        };
        assert!(
            validate_lane_draft(&profile(), Some(&"vpn".into()), &draft, &["eth0".into()]).is_err()
        );
        assert_eq!(draft.interface, "tun0");
    }
    #[test]
    fn duplicate_or_empty_lane_name_keeps_draft() {
        let draft = LaneDraft {
            name: "vpn".into(),
            interface: "eth0".into(),
            failure_policy: LaneFailurePolicy::Direct,
        };
        assert!(validate_lane_draft(&profile(), None, &draft, &["eth0".into()]).is_err());
        assert_eq!(draft.name, "vpn");
    }
    #[test]
    fn lane_disappearing_during_dialog_rejects_submit() {
        let draft = LaneDraft {
            name: "VPN".into(),
            interface: "tun0".into(),
            failure_policy: LaneFailurePolicy::Block,
        };
        let mut p = profile();
        p.lanes.clear();
        assert!(validate_lane_draft(&p, Some(&"vpn".into()), &draft, &["tun0".into()]).is_err());
    }
    #[test]
    fn deleting_fallback_target_never_persists_invalid_profile() {
        let mut p = profile();
        p.lanes.push(Lane {
            id: "wifi".into(),
            name: "Wi-Fi".into(),
            interface: "wlan0".into(),
            fallback_lane: Some("vpn".into()),
            failure_policy: LaneFailurePolicy::Block,
        });
        assert!(delete_candidate(&p, &"vpn".into()).is_err());
        assert_eq!(p.lanes.len(), 2);
    }

    #[test]
    fn lane_delete_counts_target_and_application_rules() {
        let mut p = profile();
        let rule = routelane::domain::Rule {
            id: "target".into(),
            enabled: true,
            priority: 10,
            source: RuleSource::Any,
            target: routelane::domain::RuleTarget::Any,
            action: RuleAction::Route("vpn".into()),
        };
        p.rules.push(rule.clone());
        p.rules.push(routelane::domain::Rule {
            id: "app".into(),
            source: RuleSource::Application(routelane::domain::ApplicationSelector {
                desktop_id: None,
                executable_path: "/usr/bin/browser".into(),
                include_children: true,
                launch: None,
            }),
            ..rule
        });
        assert_eq!(lane_delete_impact(&p, &"vpn".into()), (1, 1));
        let candidate = delete_candidate(&p, &"vpn".into()).unwrap();
        assert!(candidate.lanes.is_empty());
        assert!(candidate.rules.is_empty());
        assert_eq!(p.rules.len(), 2);
    }

    #[test]
    fn missing_saved_interface_can_be_replaced_only_explicitly() {
        let mut draft = LaneDraft {
            name: "VPN".into(),
            interface: "tun0".into(),
            failure_policy: LaneFailurePolicy::Block,
        };
        assert_eq!(
            validate_lane_draft(&profile(), Some(&"vpn".into()), &draft, &["eth0".into()])
                .unwrap_err(),
            LaneEditError::MissingInterface
        );
        draft.interface = "eth0".into();
        assert_eq!(
            validate_lane_draft(&profile(), Some(&"vpn".into()), &draft, &["eth0".into()])
                .unwrap()
                .interface,
            "eth0"
        );
    }
}
