use crate::ui::shell::Section;
use crate::ui::usage_widgets::{format_bytes, format_gb, ChartMode, UsageChart};
use crate::ui::{
    i18n::Texts,
    session::{OperationKind, OperationStage, SaveState, UiSession},
};
use gtk4 as gtk;
use libadwaita::{self as adw, prelude::*};
use routelane::domain::{HealthState, Profile, RuleSource};
use routelane::health::{DiagnosticCode, DiagnosticSubject};
use routelane::usage::service::{LaneUsage, UsageReport};
use routelane::{
    backend::{FamilyExit, RuntimePhase},
    domain::{LaneId, PrimarySelection},
    ipc::DaemonStatus,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub fn profile_counts(profile: &Profile) -> (usize, usize, usize) {
    let applications = profile
        .rules
        .iter()
        .filter(|r| matches!(r.source, RuleSource::Application(_)))
        .count();
    (
        profile.lanes.len(),
        profile.rules.len() - applications,
        applications,
    )
}

pub fn health_label(health: HealthState, texts: Texts) -> &'static str {
    match health {
        HealthState::Healthy => texts.status_healthy,
        HealthState::Inactive => texts.inactive,
        HealthState::Applying => texts.ux.applying,
        HealthState::Degraded => texts.status_degraded,
        HealthState::Failed => texts.status_failed,
        HealthState::NeedsAuthorization => texts.authorization_denied,
    }
}

pub fn status_copy(session: &UiSession, texts: Texts) -> (&'static str, &'static str) {
    if let Some((kind, stage)) = session.operation() {
        return (
            if stage == OperationStage::Saving {
                texts.ux.saving
            } else {
                match kind {
                    OperationKind::Deactivate => texts.ux.closing,
                    OperationKind::Launch => texts.ux.starting,
                    OperationKind::Adopt => texts.ux.adopting,
                    OperationKind::Recover => texts.orphan_recovery_progress,
                    _ => texts.ux.applying,
                }
            },
            texts.ux.auth_possible,
        );
    }
    if !session.snapshot_known() {
        return (
            if session.last_error().is_some() {
                texts.ux.unknown
            } else {
                texts.ux.checking
            },
            texts.ux.last_known,
        );
    }
    let active = session
        .confirmed_status()
        .is_some_and(|s| s.applied_profile_id.is_some());
    let detail = match session.save_state() {
        SaveState::SavedUnapplied => texts.ux.saved_unapplied,
        SaveState::SavedInactive => texts.ux.saved_inactive,
        SaveState::Applied => texts.ux.applied,
        SaveState::Failed => texts.ux.config_failed,
        _ => {
            if active {
                texts.routing_active
            } else {
                texts.routing_inactive
            }
        }
    };
    (
        if active {
            texts.routing_active
        } else {
            texts.routing_inactive
        },
        detail,
    )
}

/// Saved roles remain useful when runtime evidence is unavailable; effective roles do not.
fn lane_roles(
    profile: &Profile,
    status: Option<&DaemonStatus>,
    id: &LaneId,
    texts: Texts,
) -> Vec<&'static str> {
    let mut roles = vec![];
    if profile.routing.primary == PrimarySelection::Lane(id.clone()) {
        roles.push(texts.ux.primary_role);
    }
    if profile.routing.backups.contains(id) {
        roles.push(texts.ux.backup_role);
    }
    if crate::ui::primary::primary_overview(profile, status)
        .effective
        .is_some_and(|(v4, v6)| [v4, v6].contains(&FamilyExit::Lane(id.clone())))
    {
        roles.push(texts.ux.in_use_now);
    }
    roles
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttentionAction {
    None,
    Reapply,
    Connections,
}
struct AttentionItem {
    text: String,
    action: AttentionAction,
}
struct Attention {
    items: Vec<AttentionItem>,
    technical: Vec<String>,
}

fn attention_items(profile: &Profile, session: &UiSession, texts: Texts) -> Attention {
    let status = session
        .snapshot_known()
        .then(|| session.confirmed_status())
        .flatten();
    let mut result = Attention {
        items: vec![],
        technical: vec![],
    };
    if let Some(error) = session
        .last_error()
        .or_else(|| status.and_then(|s| s.last_error.as_ref()))
    {
        result.items.push(AttentionItem {
            text: if error.code == "UI_CONFIG_FAILED" {
                texts.ux.config_failed
            } else if error.code == "DAEMON_NEEDS_AUTHORIZATION" {
                texts.authorization_denied
            } else {
                texts.ux.request_failed
            }
            .to_owned(),
            action: AttentionAction::None,
        });
        result.technical.push(error.to_string());
    }
    if let Some(status) = status {
        for diagnostic in &status.health.diagnostics {
            let summary = match diagnostic.code {
                DiagnosticCode::LaneInterfaceMissing => texts.ux.missing_interface,
                DiagnosticCode::LaneGatewayUnreachable => texts.ux.gateway_issue,
                DiagnosticCode::NftablesFeatureMissing => texts.ux.nft_issue,
                DiagnosticCode::PolicyApplyRolledBack => texts.ux.rolled_back,
                DiagnosticCode::RoutingResourceConflict => texts.ux.conflict,
            };
            let subject = match &diagnostic.subject {
                DiagnosticSubject::Lane(id) => profile
                    .lanes
                    .iter()
                    .find(|l| &l.id == id)
                    .map(|l| l.name.as_str())
                    .unwrap_or(id.as_str()),
                _ => texts.health_title,
            };
            result.items.push(AttentionItem {
                text: format!("{subject}: {summary}"),
                action: AttentionAction::None,
            });
            result.technical.push(format!(
                "{}\n{}\n{}",
                diagnostic.summary,
                diagnostic.technical_detail,
                diagnostic
                    .suggested_action
                    .as_ref()
                    .map(|a| a.description.as_str())
                    .unwrap_or("")
            ));
        }
        if status.primary.phase == RuntimePhase::NeedsReauthorization {
            result.items.push(AttentionItem {
                text: texts.primary.needs_reauthorization.to_owned(),
                action: AttentionAction::None,
            });
        }
        if status.applied_profile_id.as_ref() == Some(&profile.id) && session.needs_reapply(profile)
        {
            result.items.push(AttentionItem {
                text: texts.ux.saved_unapplied.to_owned(),
                action: AttentionAction::Reapply,
            });
        }
    }
    if let Some(report) = session.usage() {
        for lane in &profile.lanes {
            let quota = report
                .lanes
                .iter()
                .find(|u| u.lane_id == lane.id)
                .and_then(|u| u.quota.as_ref())
                .filter(|q| q.settings.enabled);
            if let Some(quota) = quota.filter(|q| {
                u128::from(q.used_bytes) * 100
                    >= u128::from(q.settings.limit_bytes) * u128::from(q.settings.warn_percent)
            }) {
                let percent = (u128::from(quota.used_bytes) * 100
                    / u128::from(quota.settings.limit_bytes.max(1)))
                .min(u128::from(u64::MAX)) as u64;
                result.items.push(AttentionItem {
                    text: texts
                        .ux
                        .quota_attention
                        .replace("{lane}", &lane.name)
                        .replace("{percent}", &percent.to_string())
                        .replace(
                            "{suffix}",
                            routelane::usage::notify::turkish_percent_suffix(match percent % 100 {
                                0 if percent > 0 => 100,
                                remainder => remainder as u8,
                            }),
                        ),
                    action: AttentionAction::Connections,
                });
            }
        }
    }
    result
}

pub(crate) struct TrafficRow {
    pub name: &'static str,
    pub title: String,
    pub subtitle: String,
}
pub(crate) fn traffic_rows(
    profile: &Profile,
    session: &UiSession,
    texts: Texts,
) -> Vec<TrafficRow> {
    let known = session.snapshot_known();
    let status = known.then(|| session.confirmed_status()).flatten();
    let overview = crate::ui::primary::primary_overview(profile, status);
    let lane_name = |id: &LaneId| {
        profile
            .lanes
            .iter()
            .find(|l| &l.id == id)
            .map_or_else(|| id.as_str().to_owned(), |l| l.name.clone())
    };
    let health = status
        .map(|s| health_label(s.health.state, texts))
        .unwrap_or(texts.ux.unknown);
    let health = if !known {
        session
            .confirmed_status()
            .map(|s| {
                format!(
                    "{health} · {}: {}",
                    texts.ux.last_known,
                    if s.applied_profile_id.is_some() {
                        texts.routing_active
                    } else {
                        texts.routing_inactive
                    }
                )
            })
            .unwrap_or_else(|| health.to_owned())
    } else {
        health.to_owned()
    };
    let mut rows = vec![TrafficRow {
        name: "overview-status",
        title: texts.ux.status.into(),
        subtitle: health,
    }];
    for (name, title, exit) in [
        (
            "overview-ipv4",
            texts.primary.effective_v4,
            overview.effective.as_ref().map(|(v4, _)| v4),
        ),
        (
            "overview-ipv6",
            texts.primary.effective_v6,
            overview.effective.as_ref().map(|(_, v6)| v6),
        ),
    ] {
        rows.push(TrafficRow {
            name,
            title: title.into(),
            subtitle: if !known {
                texts.ux.unknown.into()
            } else {
                match exit {
                    Some(FamilyExit::Lane(id)) => lane_name(id),
                    Some(FamilyExit::Blocked) => texts.primary.blocked.into(),
                    None => texts.primary.not_active.into(),
                }
            },
        });
    }
    rows.push(TrafficRow {
        name: "overview-preferred",
        title: texts.primary.preferred.into(),
        subtitle: match overview.preferred {
            PrimarySelection::Lane(id) => lane_name(&id),
            PrimarySelection::FollowSystemDefault => texts.primary.system_default.into(),
        },
    });
    if overview.backup_in_use {
        rows.push(TrafficRow {
            name: "overview-backup",
            title: texts.primary.backup_in_use.into(),
            subtitle: String::new(),
        });
    }
    let scope = crate::ui::primary::scope_overview(profile, status);
    let mode_name = |mode| match mode {
        routelane::domain::UnmatchedTraffic::PreserveSystem => texts.primary.scope_selective,
        routelane::domain::UnmatchedTraffic::UsePrimary => texts.primary.scope_use_primary,
    };
    rows.push(TrafficRow {
        name: "overview-saved-scope",
        title: texts.primary.saved_scope.into(),
        subtitle: mode_name(scope.saved).into(),
    });
    let detail = scope
        .applied
        .map(|mode| crate::ui::window::scope_line(mode, texts.primary))
        .unwrap_or("");
    rows.push(TrafficRow {
        name: "overview-applied-scope",
        title: format!(
            "{}: {}",
            texts.primary.applied_scope,
            if !known {
                texts.ux.unknown
            } else {
                scope
                    .applied
                    .map_or(texts.primary.scope_not_applied, mode_name)
            }
        ),
        subtitle: if scope.differs() {
            format!("{detail}\n{}", texts.ux.saved_unapplied)
        } else {
            detail.into()
        },
    });
    rows
}

type SelectSection = Rc<RefCell<Rc<dyn Fn(Section)>>>;

/// Interface identity matters: a stale report must not be relabeled after an edit.
fn selected_usage<'a>(
    profile: &Profile,
    report: &'a UsageReport,
    selected: &str,
) -> Option<&'a LaneUsage> {
    let lane = profile
        .lanes
        .iter()
        .find(|lane| lane.id.as_str() == selected)?;
    report
        .lanes
        .iter()
        .find(|usage| usage.lane_id == lane.id && usage.interface == lane.interface.as_str())
}

#[derive(Clone, Copy)]
enum UsageAvailability {
    Loading,
    Unavailable,
    Ready,
}

struct OverviewUsage {
    group: adw::PreferencesGroup,
    choice: gtk::ComboBoxText,
    choices: RefCell<Vec<(String, String)>>,
    data: RefCell<Vec<LaneUsage>>,
    availability: Cell<UsageAvailability>,
    chart: UsageChart,
    caption: gtk::Label,
    today: gtk::Label,
    empty: gtk::Label,
    quota_box: gtk::Box,
    quota: gtk::ProgressBar,
    quota_label: gtk::Label,
    texts: Texts,
}

impl OverviewUsage {
    fn new(texts: Texts) -> Rc<Self> {
        let group = adw::PreferencesGroup::builder()
            .title(texts.dashboard.usage_title)
            .description(texts.dashboard.usage_note)
            .build();
        group.set_widget_name("overview-usage");
        let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
        body.add_css_class("card");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        let choice = gtk::ComboBoxText::new();
        choice.set_widget_name("overview-usage-connection");
        choice.set_hexpand(true);
        choice.update_property(&[gtk::accessible::Property::Label(
            texts.dashboard.choose_connection,
        )]);
        choice.set_tooltip_text(Some(texts.dashboard.choose_connection));
        content.append(&choice);
        let today = gtk::Label::builder().xalign(0.0).wrap(true).build();
        today.add_css_class("heading");
        today.set_widget_name("overview-usage-today");
        content.append(&today);
        let chart = UsageChart::new(texts.language);
        chart.set_mode(ChartMode::LastSevenDays);
        chart.area.set_content_width(1);
        chart.area.set_content_height(130);
        chart.area.set_widget_name("overview-usage-chart");
        content.append(&chart.area);
        let caption = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .build();
        caption.set_widget_name("overview-usage-caption");
        caption.add_css_class("dim-label");
        content.append(&caption);
        let empty = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .build();
        empty.set_widget_name("overview-usage-empty");
        empty.add_css_class("dim-label");
        content.append(&empty);
        let quota_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        quota_box.set_widget_name("overview-quota");
        let quota_label = gtk::Label::builder().xalign(0.0).wrap(true).build();
        quota_label.set_widget_name("overview-quota-label");
        let quota = gtk::ProgressBar::new();
        quota.set_widget_name("overview-quota-bar");
        quota_box.append(&quota_label);
        quota_box.append(&quota);
        content.append(&quota_box);
        content.reorder_child_after(&quota_box, Some(&today));
        body.append(&content);
        group.add(&body);
        let panel = Rc::new(Self {
            group,
            choice,
            choices: RefCell::new(vec![]),
            data: RefCell::new(vec![]),
            availability: Cell::new(UsageAvailability::Loading),
            chart,
            caption,
            today,
            empty,
            quota_box,
            quota,
            quota_label,
            texts,
        });
        let weak = Rc::downgrade(&panel);
        panel.choice.connect_changed(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.render();
            }
        });
        panel.render();
        panel
    }

    fn update(&self, profile: &Profile, session: &UiSession) {
        self.availability.set(
            if !session.snapshot_known() || session.usage_error().is_some() {
                UsageAvailability::Unavailable
            } else if session.usage().is_none() {
                UsageAvailability::Loading
            } else {
                UsageAvailability::Ready
            },
        );
        *self.data.borrow_mut() = session
            .usage()
            .map(|report| {
                profile
                    .lanes
                    .iter()
                    .filter_map(|lane| selected_usage(profile, report, lane.id.as_str()).cloned())
                    .collect()
            })
            .unwrap_or_default();
        let choices: Vec<_> = profile
            .lanes
            .iter()
            .map(|lane| (lane.id.as_str().to_owned(), lane.name.clone()))
            .collect();
        if *self.choices.borrow() != choices {
            let selected = self.choice.active_id().map(|id| id.to_string());
            *self.choices.borrow_mut() = choices.clone();
            self.choice.remove_all();
            for (id, label) in &choices {
                self.choice.append(Some(id), label);
            }
            let selected = selected
                .filter(|id| choices.iter().any(|(key, _)| key == id))
                .or_else(|| match &profile.routing.primary {
                    PrimarySelection::Lane(id)
                        if choices.iter().any(|(key, _)| key == id.as_str()) =>
                    {
                        Some(id.as_str().to_owned())
                    }
                    _ => choices.first().map(|(id, _)| id.clone()),
                });
            self.choice.set_active_id(selected.as_deref());
        }
        self.choice.set_visible(!choices.is_empty());
        self.render();
    }

    fn render(&self) {
        let data = self.data.borrow();
        let selected = self.choice.active_id();
        let lane = matches!(self.availability.get(), UsageAvailability::Ready)
            .then(|| {
                data.iter()
                    .find(|lane| Some(lane.lane_id.as_str()) == selected.as_deref())
            })
            .flatten();
        let history = lane.filter(|lane| !lane.daily.is_empty());
        self.chart.update(history.cloned().map(|mut lane| {
            // A remaining-period daily budget is not a historical daily limit.
            // Overview shows consumption only; the quota has its own progress bar.
            lane.quota = None;
            lane
        }));
        self.chart.area.set_visible(history.is_some());
        self.caption.set_visible(history.is_some());
        if let Some(lane) = history {
            let days = &lane.daily[lane.daily.len().saturating_sub(7)..];
            if let (Some((start, _)), Some((end, _))) = (days.first(), days.last()) {
                let peak = days
                    .iter()
                    .map(|(_, v)| v.rx.saturating_add(v.tx))
                    .max()
                    .unwrap_or(0);
                self.caption.set_label(
                    &self
                        .texts
                        .dashboard
                        .history_summary
                        .replace("{start}", &start.to_string())
                        .replace("{end}", &end.to_string())
                        .replace("{value}", &format_bytes(peak, self.texts.language)),
                );
            }
        }
        self.today
            .set_visible(lane.is_some_and(|lane| lane.available));
        if let Some(lane) = lane {
            self.today.set_label(&self.texts.dashboard.today.replace(
                "{value}",
                &format_bytes(
                    lane.today.rx.saturating_add(lane.today.tx),
                    self.texts.language,
                ),
            ));
        }
        self.empty
            .set_visible(history.is_none() || lane.is_some_and(|lane| !lane.available));
        self.empty.set_label(if self.choices.borrow().is_empty() {
            self.texts.ux.no_connections
        } else {
            match self.availability.get() {
                UsageAvailability::Loading => self.texts.dashboard.usage_loading,
                UsageAvailability::Unavailable => self.texts.dashboard.usage_unavailable,
                UsageAvailability::Ready if lane.is_some_and(|lane| !lane.available) => {
                    self.texts.usage.not_connected
                }
                UsageAvailability::Ready => self.texts.dashboard.usage_empty,
            }
        });
        let quota = lane
            .and_then(|lane| lane.quota.as_ref())
            .filter(|quota| quota.settings.enabled && quota.settings.limit_bytes > 0);
        self.quota_box.set_visible(quota.is_some());
        if let Some(quota) = quota {
            let fraction = quota.used_bytes as f64 / quota.settings.limit_bytes as f64;
            let label = self
                .texts
                .usage
                .used_summary
                .replace("{used}", &format_gb(quota.used_bytes, self.texts.language))
                .replace(
                    "{limit}",
                    &format_gb(quota.settings.limit_bytes, self.texts.language),
                );
            self.quota_label.set_label(&label);
            self.quota.set_fraction(fraction.clamp(0.0, 1.0));
            self.quota
                .update_property(&[gtk::accessible::Property::Label(&label)]);
        }
    }
}

#[derive(Clone)]
pub struct Overview {
    pub root: gtk::Box,
    pub reapply: gtk::Button,
    pub refresh: gtk::Button,
    attention: adw::PreferencesGroup,
    notices: gtk::ListBox,
    quotas: gtk::ListBox,
    reapply_row: adw::ActionRow,
    traffic: gtk::ListBox,
    summary: gtk::ListBox,
    usage: Rc<OverviewUsage>,
    lanes: gtk::ListBox,
    rules: adw::ActionRow,
    apps: adw::ActionRow,
    counts: adw::PreferencesGroup,
    technical: gtk::Label,
    expander: gtk::Expander,
    on_select: SelectSection,
    rendered: RefCell<String>,
    texts: Texts,
}

fn list() -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    list
}
fn clear(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}
fn row(title: &str, subtitle: &str, name: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(&glib::markup_escape_text(title))
        .subtitle(&glib::markup_escape_text(subtitle))
        .build();
    row.set_title_lines(0);
    row.set_subtitle_lines(0);
    row.set_widget_name(name);
    row
}
fn navigate(row: &adw::ActionRow, on_select: &SelectSection, section: Section) {
    row.set_activatable(true);
    let on_select = on_select.clone();
    row.connect_activated(move |_| {
        let select = on_select.borrow().clone();
        select(section);
    });
}

impl Overview {
    pub fn new(texts: Texts) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 18);
        root.set_margin_top(12);
        root.set_margin_bottom(24);
        root.set_margin_start(12);
        root.set_margin_end(12);
        let group = |title: &str, name: &str| {
            let group = adw::PreferencesGroup::builder().title(title).build();
            group.set_widget_name(name);
            root.append(&group);
            group
        };
        let attention = group(texts.ux.attention, "overview-attention");
        attention.set_visible(false);
        let notices = list();
        attention.add(&notices);
        let technical = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .build();
        let technical_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        technical_box.append(&technical);
        let copy = gtk::Button::with_label(texts.copy_error);
        copy.set_halign(gtk::Align::Start);
        let details = technical.clone();
        copy.connect_clicked(move |_| details.clipboard().set_text(&details.text()));
        technical_box.append(&copy);
        let expander = gtk::Expander::builder()
            .label(texts.technical_details)
            .child(&technical_box)
            .build();
        expander.set_widget_name("overview-technical");
        attention.add(&expander);
        let reapply = gtk::Button::with_label(texts.ux.reapply);
        reapply.set_widget_name("overview-reapply");
        reapply.set_valign(gtk::Align::Center);
        crate::ui::form::wrap_button(&reapply);
        let reapply_row = row(texts.ux.saved_unapplied, "", "overview-saved-unapplied");
        reapply_row.add_suffix(&reapply);
        attention.add(&reapply_row);
        let quotas = list();
        attention.add(&quotas);
        let traffic_group = group(texts.dashboard.routing_summary, "overview-traffic");
        let summary = list();
        traffic_group.add(&summary);
        let traffic = list();
        let details_content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let evidence = gtk::Label::builder()
            .label(texts.primary.local_evidence)
            .xalign(0.0)
            .wrap(true)
            .build();
        evidence.add_css_class("dim-label");
        details_content.append(&evidence);
        details_content.append(&traffic);
        let routing_details = gtk::Expander::builder()
            .label(texts.dashboard.routing_details)
            .child(&details_content)
            .build();
        routing_details.set_widget_name("overview-routing-details");
        traffic_group.add(&routing_details);
        let usage = OverviewUsage::new(texts);
        root.append(&usage.group);
        let connections = group(texts.ux.connections, "overview-connections");
        let lanes = list();
        connections.add(&lanes);
        let counts = group(texts.dashboard.quick_actions, "overview-shortcuts");
        let rules = row("", "", "overview-rules");
        counts.add(&rules);
        let apps = row("", "", "overview-applications");
        counts.add(&apps);
        let on_select: SelectSection = Rc::new(RefCell::new(Rc::new(|_| {})));
        navigate(&rules, &on_select, Section::Rules);
        navigate(&apps, &on_select, Section::Applications);
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        footer.set_widget_name("overview-footer");
        let refresh = gtk::Button::with_label(texts.ux.refresh);
        refresh.set_widget_name("overview-refresh");
        let help = gtk::Button::with_label(texts.ux.help);
        help.set_widget_name("overview-help");
        footer.append(&refresh);
        footer.append(&help);
        root.append(&footer);
        let help_text = gtk::Label::builder()
            .label(texts.ux.snapshot_help)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .visible(false)
            .build();
        help_text.set_widget_name("overview-help-text");
        help_text.add_css_class("dim-label");
        root.append(&help_text);
        help.connect_clicked(move |_| help_text.set_visible(!help_text.is_visible()));
        Self {
            root,
            reapply,
            refresh,
            attention,
            notices,
            quotas,
            reapply_row,
            traffic,
            summary,
            usage,
            lanes,
            rules,
            apps,
            counts,
            technical,
            expander,
            on_select,
            rendered: RefCell::new(String::new()),
            texts,
        }
    }
    pub fn set_on_select(&self, on_select: Rc<dyn Fn(Section)>) {
        *self.on_select.borrow_mut() = on_select;
    }
    pub fn add_quick_application(&self, button: &gtk::Button) {
        self.counts.add(button);
    }
    pub fn update(&self, profile: &Profile, session: &UiSession) {
        let texts = self.texts;
        let attention = attention_items(profile, session, texts);
        let traffic: Vec<(String, String, &'static str)> = traffic_rows(profile, session, texts)
            .into_iter()
            .map(|row| (row.title, row.subtitle, row.name))
            .collect();
        let status = session
            .snapshot_known()
            .then(|| session.confirmed_status())
            .flatten();
        let lanes: Vec<(String, String, String)> = profile
            .lanes
            .iter()
            .map(|lane| {
                let health = status
                    .and_then(|s| s.health.lanes.get(&lane.id))
                    .map(|h| health_label(*h, texts))
                    .unwrap_or(texts.ux.unknown);
                let mut subtitle = format!("{} · {health}", lane.interface.as_str());
                for role in lane_roles(profile, status, &lane.id, texts) {
                    subtitle.push_str(&format!(" · {role}"));
                }
                (
                    lane.name.clone(),
                    subtitle,
                    format!("overview-lane-{}", lane.id.as_str()),
                )
            })
            .collect();
        let items: Vec<(String, AttentionAction)> = attention
            .items
            .iter()
            .map(|item| (item.text.clone(), item.action))
            .collect();
        // Rows are rebuilt only when their content changes: a usage or status
        // poll must not reset keyboard focus on an activatable row.
        let key = format!("{items:?}{:?}{traffic:?}{lanes:?}", attention.technical);
        if *self.rendered.borrow() != key {
            *self.rendered.borrow_mut() = key;
            clear(&self.notices);
            clear(&self.quotas);
            let mut reapply = false;
            for (text, action) in &items {
                match action {
                    AttentionAction::Reapply => reapply = true,
                    AttentionAction::Connections => {
                        let row = row(text, "", "overview-notice");
                        navigate(&row, &self.on_select, Section::Connections);
                        self.quotas.append(&row);
                    }
                    AttentionAction::None => self.notices.append(&row(text, "", "overview-notice")),
                }
            }
            self.reapply_row.set_visible(reapply);
            self.reapply.set_visible(reapply);
            self.notices
                .set_visible(self.notices.first_child().is_some());
            self.quotas.set_visible(self.quotas.first_child().is_some());
            self.technical.set_label(&attention.technical.join("\n\n"));
            self.expander.set_visible(!attention.technical.is_empty());
            self.attention.set_visible(!items.is_empty());
            clear(&self.traffic);
            clear(&self.summary);
            for (title, subtitle, name) in &traffic {
                if matches!(*name, "overview-applied-scope" | "overview-backup") {
                    let mut detail = subtitle.clone();
                    if *name == "overview-applied-scope"
                        && status.is_some_and(|s| s.applied_profile_id.is_some())
                    {
                        let exits = traffic
                            .iter()
                            .filter(|(_, _, name)| {
                                matches!(*name, "overview-ipv4" | "overview-ipv6")
                            })
                            .map(|(title, value, _)| format!("{title}: {value}"))
                            .collect::<Vec<_>>()
                            .join(" · ");
                        if !detail.is_empty() {
                            detail.push('\n');
                        }
                        detail.push_str(&exits);
                    }
                    self.summary.append(&row(title, &detail, name));
                } else {
                    self.traffic.append(&row(title, subtitle, name));
                }
            }
            clear(&self.lanes);
            for (title, subtitle, name) in &lanes {
                let row = row(title, subtitle, name);
                navigate(&row, &self.on_select, Section::Connections);
                self.lanes.append(&row);
            }
            if profile.lanes.is_empty() {
                self.lanes
                    .append(&row(texts.ux.no_connections, "", "overview-empty"));
            }
        }
        self.reapply
            .set_sensitive(session.config_writable() && !session.is_busy());
        self.refresh.set_sensitive(!session.is_busy());
        self.usage.update(profile, session);
        let (_, rules, apps) = profile_counts(profile);
        self.rules
            .set_title(&format!("{} · {rules}", texts.ux.rules));
        self.apps
            .set_title(&format!("{} · {apps}", texts.ux.applications));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use routelane::domain::{
        ApplicationSelector, Lane, LaneFailurePolicy, Rule, RuleAction, RuleSource, RuleTarget,
    };
    #[test]
    fn counts_all_connections_and_separates_apps_from_target_rules() {
        let mut profile = Profile {
            routing: Default::default(),
            id: "work".into(),
            name: "Work".into(),
            enabled: false,
            lanes: vec![],
            rules: vec![],
        };
        for id in ["wifi", "vinn"] {
            profile.lanes.push(Lane {
                id: id.into(),
                name: id.into(),
                interface: id.into(),
                fallback_lane: None,
                failure_policy: LaneFailurePolicy::Block,
            });
        }
        profile.rules.push(Rule {
            id: "block".into(),
            enabled: true,
            priority: 10,
            source: RuleSource::Any,
            target: RuleTarget::Any,
            action: RuleAction::Block,
        });
        profile.rules.push(Rule {
            id: "browser".into(),
            enabled: true,
            priority: 20,
            source: RuleSource::Application(ApplicationSelector {
                desktop_id: None,
                executable_path: "/usr/bin/browser".into(),
                include_children: true,
                launch: None,
            }),
            target: RuleTarget::Any,
            action: RuleAction::Route("vinn".into()),
        });
        assert_eq!(profile_counts(&profile), (2, 1, 1));
    }
    use crate::{models::DaemonToUi, ui::i18n::LanguageTexts};
    use routelane::{
        backend::{FamilyPair, PrimaryDecision, RuntimeGrant, RuntimeStatus},
        config::Language,
        domain::UnmatchedTraffic,
        health::HealthReport,
        ipc::{ApplyProfileResult, DaemonRequest, DaemonResponse, DaemonResult},
        usage::{
            quota::LaneQuota,
            service::{LaneUsage, QuotaStatus, UsageReport},
            LocalDate, UsageTotals,
        },
    };

    fn profile() -> Profile {
        Profile {
            id: "work".into(),
            name: "Work".into(),
            enabled: true,
            routing: routelane::domain::RoutingPolicy {
                primary: PrimarySelection::Lane("ethernet".into()),
                backups: vec!["vinn".into()],
                ..Default::default()
            },
            lanes: [("ethernet", "Ethernet"), ("vinn", "VINN")]
                .into_iter()
                .map(|(id, name)| Lane {
                    id: id.into(),
                    name: name.into(),
                    interface: id.into(),
                    fallback_lane: None,
                    failure_policy: LaneFailurePolicy::Block,
                })
                .collect(),
            rules: vec![],
        }
    }
    #[test]
    fn overview_usage_never_relabels_old_interface_data_as_the_selected_connection() {
        let profile = profile();
        let report = UsageReport {
            lanes: vec![LaneUsage {
                lane_id: "ethernet".into(),
                interface: "old-interface".into(),
                available: true,
                today: UsageTotals::default(),
                quota: None,
                daily: vec![],
                rate: None,
                recent_rates: vec![],
            }],
        };
        assert!(selected_usage(&profile, &report, "ethernet").is_none());
        let mut current = report;
        current.lanes[0].interface = "ethernet".into();
        assert!(selected_usage(&profile, &current, "ethernet").is_some());
        assert!(selected_usage(&profile, &current, "vinn").is_none());
        assert!(selected_usage(&profile, &current, "removed-lane").is_none());
    }
    fn sent(effects: Vec<crate::ui::session::UiEffect>) -> String {
        effects
            .into_iter()
            .find_map(|effect| match effect {
                crate::ui::session::UiEffect::Send(request) => Some(request.request_id),
                _ => None,
            })
            .expect("a request")
    }
    /// Establishes a locally confirmed apply, including independent family exits and scope.
    pub(crate) fn applied_session(profile: &Profile) -> UiSession {
        let mut session = UiSession::new(profile.clone(), true);
        session.handle_daemon(DaemonToUi::ConnectionEstablished { epoch: 1 });
        let mut status = DaemonStatus {
            generation: 0,
            event_sequence: 0,
            health: HealthReport::inactive(),
            desired_profile_id: None,
            applied_profile_id: None,
            last_error: None,
            primary: RuntimeStatus::default(),
        };
        session.handle_daemon(DaemonToUi::Snapshot {
            epoch: 1,
            status: status.clone(),
            profiles: vec![profile.clone()],
        });
        let id = sent(
            session
                .begin_request(
                    DaemonRequest::ApplyProfile(profile.clone()),
                    Some(profile.clone()),
                )
                .unwrap(),
        );
        let hash = routelane::backend::policy_hash(
            profile,
            &routelane::backend::ResolutionSnapshot::empty(),
        );
        session.handle_daemon(DaemonToUi::Response {
            epoch: 1,
            response: DaemonResponse::success(
                id,
                DaemonResult::Applied(ApplyProfileResult {
                    generation: 1,
                    plan_hash: hash,
                }),
            ),
        });
        status.generation = 1;
        status.event_sequence = 1;
        status.desired_profile_id = Some(profile.id.clone());
        status.applied_profile_id = Some(profile.id.clone());
        status.health = HealthReport::from_parts(
            HealthState::Healthy,
            profile
                .lanes
                .iter()
                .map(|l| (l.id.clone(), HealthState::Healthy))
                .collect(),
            vec![],
        );
        status.primary = RuntimeStatus {
            grant: Some(RuntimeGrant {
                owner_uid: 1000,
                boot_id: "boot".into(),
                network_namespace: 1,
                policy_hash: hash,
                interfaces: Default::default(),
            }),
            generation: 1,
            phase: RuntimePhase::Armed,
            unmatched_traffic: Some(profile.routing.unmatched_traffic),
            decision: Some(PrimaryDecision {
                preferred: FamilyPair {
                    v4: profile.lanes.first().map(|l| l.id.clone()),
                    v6: None,
                },
                effective: FamilyPair {
                    v4: profile
                        .lanes
                        .first()
                        .map_or(FamilyExit::Blocked, |l| FamilyExit::Lane(l.id.clone())),
                    v6: FamilyExit::Blocked,
                },
            }),
            ..RuntimeStatus::default()
        };
        session.handle_daemon(DaemonToUi::Snapshot {
            epoch: 1,
            status,
            profiles: vec![profile.clone()],
        });
        assert!(!session.needs_reapply(profile));
        session
    }
    #[test]
    fn lane_roles_distinguish_saved_primary_backup_and_verified_effective_exit() {
        let profile = profile();
        let texts = Language::En.texts();
        let session = applied_session(&profile);
        assert_eq!(
            lane_roles(
                &profile,
                session.confirmed_status(),
                &"ethernet".into(),
                texts
            ),
            ["Primary", "In use now"]
        );
        assert_eq!(
            lane_roles(&profile, session.confirmed_status(), &"vinn".into(), texts),
            ["Backup"]
        );
        assert_eq!(
            lane_roles(&profile, None, &"ethernet".into(), texts),
            ["Primary"]
        );
    }
    #[test]
    fn healthy_applied_profile_has_no_attention_but_saved_changes_need_reapply() {
        let mut profile = profile();
        let session = applied_session(&profile);
        assert!(attention_items(&profile, &session, Language::En.texts())
            .items
            .is_empty());
        profile.routing.unmatched_traffic = UnmatchedTraffic::UsePrimary;
        let attention = attention_items(&profile, &session, Language::En.texts());
        assert_eq!(attention.items.len(), 1);
        assert_eq!(attention.items[0].action, AttentionAction::Reapply);
    }
    #[test]
    fn traffic_keeps_family_exits_and_saved_applied_scopes_separate() {
        let mut profile = profile();
        let session = applied_session(&profile);
        profile.routing.unmatched_traffic = UnmatchedTraffic::UsePrimary;
        let texts = Language::En.texts();
        let rows = traffic_rows(&profile, &session, texts);
        assert_eq!(
            rows.iter()
                .find(|r| r.name == "overview-ipv4")
                .unwrap()
                .subtitle,
            "Ethernet"
        );
        assert_eq!(
            rows.iter()
                .find(|r| r.name == "overview-ipv6")
                .unwrap()
                .subtitle,
            texts.primary.blocked
        );
        assert_eq!(
            rows.iter()
                .find(|r| r.name == "overview-saved-scope")
                .unwrap()
                .subtitle,
            texts.primary.scope_use_primary
        );
        let applied = rows
            .iter()
            .find(|r| r.name == "overview-applied-scope")
            .unwrap();
        assert!(applied.title.contains(texts.primary.scope_selective));
        assert!(applied.subtitle.contains(crate::ui::window::scope_line(
            UnmatchedTraffic::PreserveSystem,
            texts.primary
        )));
        assert!(applied.subtitle.contains(texts.ux.saved_unapplied));
    }
    #[test]
    fn unknown_snapshot_hides_effective_exits_roles_and_health_diagnostics() {
        let profile = profile();
        let mut session = applied_session(&profile);
        let texts = Language::En.texts();
        let mut status = session.confirmed_status().unwrap().clone();
        status.event_sequence += 1;
        status.health = HealthReport::failure(
            HealthState::Degraded,
            DiagnosticCode::LaneInterfaceMissing,
            DiagnosticSubject::Lane("vinn".into()),
            "missing interface",
            "usb0 missing",
            None,
        );
        session.handle_daemon(DaemonToUi::Snapshot {
            epoch: 1,
            status,
            profiles: vec![profile.clone()],
        });
        let attention = attention_items(&profile, &session, texts);
        assert_eq!(
            attention.items[0].text,
            format!("VINN: {}", texts.ux.missing_interface)
        );
        assert!(attention.technical[0].contains("usb0 missing"));
        session.handle_daemon(DaemonToUi::DaemonUnavailable {
            epoch: 1,
            request_id: None,
            error: routelane::ipc::IpcError::new("OFFLINE", "offline"),
        });
        let texts = Language::En.texts();
        let rows = traffic_rows(&profile, &session, texts);
        assert!(rows
            .iter()
            .find(|r| r.name == "overview-status")
            .unwrap()
            .subtitle
            .contains(texts.ux.last_known));
        for name in ["overview-ipv4", "overview-ipv6"] {
            assert_eq!(
                rows.iter().find(|r| r.name == name).unwrap().subtitle,
                texts.ux.unknown
            );
        }
        let attention = attention_items(&profile, &session, texts);
        assert_eq!(attention.items.len(), 1);
        assert_eq!(attention.items[0].text, texts.ux.request_failed);
        assert_eq!(attention.technical, ["OFFLINE: offline"]);
        assert!(!attention
            .items
            .iter()
            .any(|item| item.action == AttentionAction::Reapply));
        let known_status = session
            .snapshot_known()
            .then(|| session.confirmed_status())
            .flatten();
        assert_eq!(
            lane_roles(&profile, known_status, &"ethernet".into(), texts),
            ["Primary"]
        );
    }
    #[test]
    fn reauthorization_attention_requires_a_known_snapshot() {
        let profile = profile();
        let mut session = applied_session(&profile);
        let mut status = session.confirmed_status().unwrap().clone();
        status.event_sequence += 1;
        status.primary.phase = RuntimePhase::NeedsReauthorization;
        session.handle_daemon(DaemonToUi::Snapshot {
            epoch: 1,
            status,
            profiles: vec![profile.clone()],
        });
        let texts = Language::Tr.texts();
        assert!(attention_items(&profile, &session, texts)
            .items
            .iter()
            .any(|item| item.text == texts.primary.needs_reauthorization));
    }
    #[test]
    fn enabled_quotas_warn_at_threshold_with_turkish_suffixes_and_english_copy() {
        let profile = profile();
        let mut session = applied_session(&profile);
        for (used, tr, en) in [
            (79, None, None),
            (
                80,
                Some("VINN: kotanın %80'i kullanıldı"),
                Some("VINN: 80% of quota used"),
            ),
            (
                90,
                Some("VINN: kotanın %90'ı kullanıldı"),
                Some("VINN: 90% of quota used"),
            ),
            (
                100,
                Some("VINN: kotanın %100'ü kullanıldı"),
                Some("VINN: 100% of quota used"),
            ),
        ] {
            let id = sent(
                session
                    .begin_request(DaemonRequest::GetUsage { fast_lane: None }, None)
                    .unwrap(),
            );
            session.handle_daemon(DaemonToUi::Response {
                epoch: 1,
                response: DaemonResponse::success(
                    id,
                    DaemonResult::Usage(UsageReport {
                        lanes: vec![LaneUsage {
                            lane_id: "vinn".into(),
                            interface: "usb0".into(),
                            available: true,
                            today: UsageTotals::default(),
                            daily: vec![],
                            rate: None,
                            recent_rates: vec![],
                            quota: Some(QuotaStatus {
                                settings: LaneQuota {
                                    enabled: true,
                                    limit_bytes: 100,
                                    reset_day: 1,
                                    warn_percent: 80,
                                },
                                period_start: LocalDate::parse("2026-11-01").unwrap(),
                                next_reset: LocalDate::parse("2026-12-01").unwrap(),
                                used_bytes: used,
                                measured: UsageTotals::default(),
                            }),
                        }],
                    }),
                ),
            });
            for (language, expected) in [(Language::Tr, tr), (Language::En, en)] {
                let attention = attention_items(&profile, &session, language.texts());
                assert_eq!(
                    attention.items.first().map(|item| item.text.as_str()),
                    expected
                );
                if expected.is_some() {
                    assert_eq!(attention.items[0].action, AttentionAction::Connections);
                }
            }
        }
        let id = sent(
            session
                .begin_request(DaemonRequest::GetUsage { fast_lane: None }, None)
                .unwrap(),
        );
        let mut report = session.usage().unwrap().clone();
        report.lanes[0].quota.as_mut().unwrap().settings.enabled = false;
        session.handle_daemon(DaemonToUi::Response {
            epoch: 1,
            response: DaemonResponse::success(id, DaemonResult::Usage(report)),
        });
        assert!(attention_items(&profile, &session, Language::En.texts())
            .items
            .is_empty());
    }
}
