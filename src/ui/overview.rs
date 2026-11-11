use crate::ui::shell::Section;
use crate::ui::{
    i18n::Texts,
    session::{OperationKind, OperationStage, SaveState, UiSession},
};
use gtk4 as gtk;
use libadwaita::{self as adw, prelude::*};
use routelane::domain::{HealthState, Profile};
use routelane::health::{DiagnosticCode, DiagnosticSubject};
use routelane::{
    backend::{FamilyExit, RuntimePhase},
    domain::{LaneId, PrimarySelection},
};
use std::{cell::RefCell, rc::Rc};

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

#[derive(Clone)]
pub struct Overview {
    pub root: gtk::Box,
    pub reapply: gtk::Button,
    pub refresh: gtk::Button,
    attention: adw::PreferencesGroup,
    system_attention: adw::ActionRow,
    notices: gtk::ListBox,
    quotas: gtk::ListBox,
    reapply_row: adw::ActionRow,
    traffic: gtk::ListBox,
    summary: gtk::ListBox,
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
        let on_select: SelectSection = Rc::new(RefCell::new(Rc::new(|_| {})));
        let attention = group(texts.ux.attention, "overview-attention");
        attention.set_visible(false);
        let notices = list();
        attention.add(&notices);
        let system_attention = row(texts.system_primary.title, "", "overview-system-primary");
        system_attention.set_visible(false);
        navigate(&system_attention, &on_select, Section::Connections);
        attention.add(&system_attention);
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
            system_attention,
            notices,
            quotas,
            reapply_row,
            traffic,
            summary,
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
    pub fn set_system_attention(&self, message: Option<&str>) {
        self.system_attention.set_visible(message.is_some());
        self.system_attention
            .set_subtitle(&glib::markup_escape_text(message.unwrap_or("")));
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
        let items: Vec<(String, AttentionAction)> = attention
            .items
            .iter()
            .map(|item| (item.text.clone(), item.action))
            .collect();
        // Rows are rebuilt only when their content changes: a usage or status
        // poll must not reset keyboard focus on an activatable row.
        let key = format!("{items:?}{:?}{traffic:?}", attention.technical);
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
        }
        self.reapply
            .set_sensitive(session.config_writable() && !session.is_busy());
        self.refresh.set_sensitive(!session.is_busy());
        self.attention
            .set_visible(!attention.items.is_empty() || self.system_attention.get_visible());
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{models::DaemonToUi, ui::i18n::LanguageTexts};
    use routelane::domain::{Lane, LaneFailurePolicy};
    use routelane::{
        backend::{FamilyPair, PrimaryDecision, RuntimeGrant, RuntimeStatus},
        config::Language,
        domain::UnmatchedTraffic,
        health::HealthReport,
        ipc::{ApplyProfileResult, DaemonRequest, DaemonResponse, DaemonResult, DaemonStatus},
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
