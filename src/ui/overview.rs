use crate::ui::{
    i18n::Texts,
    session::{OperationKind, OperationStage, SaveState, UiSession},
};
use gtk4::{self as gtk, prelude::*};
use routelane::domain::{HealthState, Profile, RuleSource};
use routelane::health::{DiagnosticCode, DiagnosticSubject};

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

#[derive(Clone)]
pub struct Overview {
    pub root: gtk::Box,
    pub reapply: gtk::Button,
    pub refresh: gtk::Button,
    summary: gtk::Label,
    lanes: gtk::Box,
    health: gtk::Label,
    error: gtk::Label,
    technical: gtk::Label,
    expander: gtk::Expander,
    texts: Texts,
}

impl Overview {
    pub fn new(texts: Texts) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_top(12);
        root.set_margin_bottom(24);
        root.set_margin_start(12);
        root.set_margin_end(12);
        let summary = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .build();
        summary.add_css_class("heading");
        let lanes = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let health = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .build();
        let error = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .build();
        error.add_css_class("error");
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
        let reapply = gtk::Button::with_label(texts.ux.reapply);
        reapply.set_halign(gtk::Align::Start);
        let refresh = gtk::Button::with_label(texts.ux.refresh);
        refresh.set_halign(gtk::Align::Start);
        let help = gtk::Label::builder()
            .label(texts.ux.snapshot_help)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .build();
        help.add_css_class("dim-label");
        for widget in [
            summary.upcast_ref::<gtk::Widget>(),
            lanes.upcast_ref(),
            health.upcast_ref(),
            error.upcast_ref(),
            expander.upcast_ref(),
            reapply.upcast_ref(),
            refresh.upcast_ref(),
            help.upcast_ref(),
        ] {
            root.append(widget);
        }
        Self {
            root,
            reapply,
            refresh,
            summary,
            lanes,
            health,
            error,
            technical,
            expander,
            texts,
        }
    }

    pub fn update(&self, profile: &Profile, session: &UiSession) {
        let texts = self.texts;
        let (lanes, rules, apps) = profile_counts(profile);
        self.summary.set_label(&format!(
            "{lanes} {} · {rules} {} · {apps} {}",
            texts.ux.connections, texts.ux.rules, texts.ux.applications
        ));
        while let Some(child) = self.lanes.first_child() {
            self.lanes.remove(&child);
        }
        for lane in &profile.lanes {
            let state = if session.snapshot_known() {
                session
                    .confirmed_status()
                    .and_then(|s| s.health.lanes.get(&lane.id))
                    .map(|health| health_label(*health, texts))
                    .unwrap_or(texts.ux.unknown)
            } else {
                texts.ux.unknown
            };
            self.lanes.append(
                &gtk::Label::builder()
                    .label(&format!(
                        "{} — {} · {state}",
                        lane.name,
                        lane.interface.as_str()
                    ))
                    .xalign(0.0)
                    .wrap(true)
                    .wrap_mode(gtk::pango::WrapMode::WordChar)
                    .build(),
            );
        }
        let health = if session.snapshot_known() {
            session
                .confirmed_status()
                .map(|s| health_label(s.health.state, texts))
                .unwrap_or(texts.ux.checking)
        } else {
            texts.ux.unknown
        };
        let health = if !session.snapshot_known() {
            session
                .confirmed_status()
                .map(|status| {
                    format!(
                        "{health}\n{}: {}",
                        texts.ux.last_known,
                        if status.applied_profile_id.is_some() {
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
        self.health.set_label(&health);
        let mut summaries = vec![];
        let mut technical = vec![];
        if let Some(error) = session.last_error().or_else(|| {
            session
                .confirmed_status()
                .and_then(|status| status.last_error.as_ref())
        }) {
            summaries.push(
                if error.code == "UI_CONFIG_FAILED" {
                    texts.ux.config_failed
                } else if error.code == "DAEMON_NEEDS_AUTHORIZATION" {
                    texts.authorization_denied
                } else {
                    texts.ux.request_failed
                }
                .to_owned(),
            );
            technical.push(error.to_string());
        }
        if let Some(status) = session.confirmed_status() {
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
                summaries.push(format!("{subject}: {summary}"));
                technical.push(format!(
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
        }
        self.error.set_label(&summaries.join("\n"));
        self.error.set_visible(!summaries.is_empty());
        self.technical.set_label(&technical.join("\n\n"));
        self.expander.set_visible(!technical.is_empty());
        self.reapply.set_visible(
            session.snapshot_known()
                && session
                    .confirmed_status()
                    .is_some_and(|s| s.applied_profile_id.as_ref() == Some(&profile.id))
                && session.needs_reapply(profile),
        );
        self.reapply
            .set_sensitive(session.config_writable() && !session.is_busy());
    }
}

#[cfg(test)]
mod tests {
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
}
