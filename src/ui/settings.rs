use std::rc::Rc;

use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};
use routelane::domain::{
    canonical_lane_name, DomainError, InterfaceSelector, Lane, LaneFailurePolicy, LaneId, Profile,
    RuleAction,
};

use crate::ui::confirm::confirm_destructive_action_with_label;
use crate::ui::i18n::Texts;
use routelane::config::Language;

pub fn add_lane(
    profile: &mut Profile,
    name: &str,
    interface: &str,
    failure_policy: LaneFailurePolicy,
) -> Result<LaneId, LaneEditError> {
    let name = canonical_lane_name(profile, None, name).map_err(LaneEditError::from)?;
    let interface = interface.trim();
    if interface.is_empty() {
        return Err(LaneEditError::EmptyInterface);
    }
    let id = LaneId::from(format!("lane-{}", uuid::Uuid::new_v4()).as_str());
    profile.lanes.push(Lane {
        id: id.clone(),
        name,
        interface: InterfaceSelector::from(interface),
        fallback_lane: None,
        failure_policy,
    });
    Ok(id)
}

pub fn rename_lane(
    profile: &mut Profile,
    lane_id: &LaneId,
    name: &str,
) -> Result<(), LaneEditError> {
    let name = canonical_lane_name(profile, Some(lane_id), name).map_err(LaneEditError::from)?;
    let lane = profile
        .lanes
        .iter_mut()
        .find(|lane| &lane.id == lane_id)
        .ok_or(LaneEditError::NotFound)?;
    lane.name = name;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneEditError {
    EmptyName,
    DuplicateName,
    EmptyInterface,
    NotFound,
}

impl From<DomainError> for LaneEditError {
    fn from(error: DomainError) -> Self {
        match error {
            DomainError::EmptyLaneName(_) => Self::EmptyName,
            DomainError::DuplicateLaneName(_) => Self::DuplicateName,
            _ => Self::NotFound,
        }
    }
}

pub fn lane_edit_error_text(error: LaneEditError, texts: Texts) -> &'static str {
    match error {
        LaneEditError::EmptyName => texts.lane_name_empty,
        LaneEditError::DuplicateName => texts.lane_name_duplicate,
        LaneEditError::EmptyInterface => texts.lane_interface_empty,
        LaneEditError::NotFound => texts.lane_not_found,
    }
}

fn language_change_requires_restart(current: Language, selected: Language) -> bool {
    current != selected
}

fn confirmed_language_change(
    current: Language,
    selected: Language,
    confirmed: bool,
) -> Option<Language> {
    (confirmed && language_change_requires_restart(current, selected)).then_some(selected)
}

fn confirm_language_restart(
    parent: &adw::PreferencesWindow,
    texts: Texts,
    on_confirm: Rc<dyn Fn()>,
) {
    confirm_destructive_action_with_label(
        parent,
        texts,
        texts.language_restart_title,
        texts.language_restart_body,
        texts.confirm,
        on_confirm,
        Rc::new(|| {}),
    );
}

pub fn routed_rule_count(profile: &Profile, lane_id: &LaneId) -> usize {
    profile
        .rules
        .iter()
        .filter(|rule| matches!(&rule.action, RuleAction::Route(target) if target == lane_id))
        .count()
}

pub fn delete_lane(profile: &mut Profile, lane_id: &LaneId) {
    profile.lanes.retain(|lane| &lane.id != lane_id);
    profile
        .rules
        .retain(|rule| !matches!(&rule.action, RuleAction::Route(target) if target == lane_id));
}

pub fn build_general_settings_dialog(
    parent: &adw::ApplicationWindow,
    texts: Texts,
    initial_language: Language,
    on_language: Rc<dyn Fn(Language)>,
) -> adw::PreferencesWindow {
    // The window is cached and presented again on the next open, so closing
    // it hides it; a destroyed window shown again stops responding to close.
    let dialog = adw::PreferencesWindow::builder()
        .title(texts.settings_title)
        .transient_for(parent)
        .modal(true)
        .hide_on_close(true)
        .build();
    let page = adw::PreferencesPage::new();
    let language_group = adw::PreferencesGroup::builder()
        .title(texts.language_title)
        .description(texts.language_description)
        .build();
    let language = adw::ComboRow::builder()
        .title(texts.language_row_title)
        .subtitle(texts.language_row_subtitle)
        .build();
    language.set_widget_name("language-choice");
    language.set_model(Some(&gtk::StringList::new(&["Türkçe", "English"])));
    language.set_selected(initial_language.index());
    let confirmed_language = Rc::new(std::cell::Cell::new(initial_language));
    let reverting_language = Rc::new(std::cell::Cell::new(false));
    language.connect_selected_notify({
        let dialog = dialog.downgrade();
        let confirmed_language = confirmed_language.clone();
        let reverting_language = reverting_language.clone();
        move |combo| {
            if reverting_language.get() {
                return;
            }
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            let current = confirmed_language.get();
            let selected = Language::from_index(combo.selected());
            if !language_change_requires_restart(current, selected) {
                return;
            }
            reverting_language.set(true);
            combo.set_selected(current.index());
            reverting_language.set(false);

            let confirmed_language = confirmed_language.clone();
            let on_language = on_language.clone();
            confirm_language_restart(
                &dialog,
                texts,
                Rc::new(move || {
                    if let Some(language) = confirmed_language_change(current, selected, true) {
                        confirmed_language.set(language);
                        on_language(language);
                    }
                }),
            );
        }
    });
    language_group.add(&language);
    page.add(&language_group);
    let about = adw::PreferencesGroup::builder()
        .title(texts.about_title)
        .build();
    let version = adw::ActionRow::builder()
        .title(texts.version_title)
        .subtitle(app_version())
        .build();
    version.set_widget_name("app-version");
    about.add(&version);
    page.add(&about);
    dialog.add(&page);
    dialog
}

/// The version of this build, from Cargo.toml.
pub fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::{
        add_lane, confirmed_language_change, delete_lane, lane_edit_error_text,
        language_change_requires_restart, rename_lane, routed_rule_count, LaneEditError,
    };
    use crate::ui::i18n::LanguageTexts;
    use routelane::config::Language;
    use routelane::domain::{
        InterfaceSelector, LaneFailurePolicy, Profile, ProfileId, Rule, RuleAction, RuleId,
        RuleSource, RuleTarget,
    };

    fn profile() -> Profile {
        Profile {
            routing: Default::default(),
            id: ProfileId::from("default"),
            name: "Default".to_owned(),
            enabled: false,
            lanes: vec![],
            rules: vec![],
        }
    }

    #[test]
    fn adds_a_named_lane_for_a_selected_interface() {
        let mut profile = profile();

        let lane_id = add_lane(&mut profile, "Work VPN", "tun0", LaneFailurePolicy::Block).unwrap();

        assert_eq!(profile.lanes[0].id, lane_id);
        assert_eq!(profile.lanes[0].name, "Work VPN");
        assert_eq!(profile.lanes[0].interface, InterfaceSelector::from("tun0"));
        assert_eq!(profile.lanes[0].failure_policy, LaneFailurePolicy::Block);
    }

    #[test]
    fn routed_rule_count_only_counts_rules_routed_through_the_given_lane() {
        let mut profile = profile();
        let vpn = add_lane(&mut profile, "VPN", "tun0", LaneFailurePolicy::Inactive).unwrap();
        let direct = add_lane(&mut profile, "Direct", "eth0", LaneFailurePolicy::Inactive).unwrap();
        profile.rules.push(Rule {
            id: RuleId::from("routed-a"),
            enabled: true,
            priority: 1,
            source: RuleSource::Any,
            target: RuleTarget::Domain("a.example".to_owned()),
            action: RuleAction::Route(vpn.clone()),
        });
        profile.rules.push(Rule {
            id: RuleId::from("routed-b"),
            enabled: true,
            priority: 2,
            source: RuleSource::Any,
            target: RuleTarget::Domain("b.example".to_owned()),
            action: RuleAction::Route(vpn.clone()),
        });
        profile.rules.push(Rule {
            id: RuleId::from("blocked"),
            enabled: true,
            priority: 3,
            source: RuleSource::Any,
            target: RuleTarget::Domain("c.example".to_owned()),
            action: RuleAction::Block,
        });

        assert_eq!(routed_rule_count(&profile, &vpn), 2);
        assert_eq!(routed_rule_count(&profile, &direct), 0);
    }

    #[test]
    fn deleting_a_lane_also_deletes_rules_that_route_to_it() {
        let mut profile = profile();
        let lane_id = add_lane(&mut profile, "VPN", "tun0", LaneFailurePolicy::Inactive).unwrap();
        profile.rules.push(Rule {
            id: RuleId::from("vpn-only"),
            enabled: true,
            priority: 10,
            source: RuleSource::Any,
            target: RuleTarget::Domain("example.com".to_owned()),
            action: RuleAction::Route(lane_id.clone()),
        });

        delete_lane(&mut profile, &lane_id);

        assert!(profile.lanes.is_empty());
        assert!(profile.rules.is_empty());
    }

    #[test]
    fn lane_names_are_trimmed_and_unique_case_insensitively() {
        let mut profile = profile();
        let first = add_lane(&mut profile, " Work ", "tun0", LaneFailurePolicy::Inactive).unwrap();
        assert_eq!(profile.lanes[0].name, "Work");
        assert!(add_lane(&mut profile, "work", "tun1", LaneFailurePolicy::Inactive).is_err());
        assert!(rename_lane(&mut profile, &first, "  ").is_err());
        assert!(add_lane(&mut profile, "Wörk", "tun1", LaneFailurePolicy::Inactive).is_ok());
        assert!(add_lane(&mut profile, "wÖRK", "tun2", LaneFailurePolicy::Inactive).is_err());
    }

    #[test]
    fn lane_validation_messages_follow_the_selected_language() {
        assert_eq!(
            lane_edit_error_text(
                LaneEditError::DuplicateName,
                routelane::config::Language::Tr.texts(),
            ),
            "Rota adı benzersiz olmalıdır"
        );
        assert_eq!(
            lane_edit_error_text(
                LaneEditError::EmptyInterface,
                routelane::config::Language::En.texts(),
            ),
            "Lane interface must not be empty"
        );
    }

    #[test]
    fn language_change_requires_confirmation_only_for_a_different_language() {
        assert!(!language_change_requires_restart(
            Language::Tr,
            Language::Tr
        ));
        assert!(language_change_requires_restart(Language::Tr, Language::En));
    }

    #[test]
    fn cancelling_language_confirmation_preserves_the_previous_language() {
        assert_eq!(
            confirmed_language_change(Language::Tr, Language::En, false),
            None
        );
        assert_eq!(
            confirmed_language_change(Language::Tr, Language::En, true),
            Some(Language::En)
        );
    }
}
