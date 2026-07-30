use std::cell::RefCell;
use std::rc::Rc;

use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};
use routelane::domain::{
    canonical_lane_name, DomainError, InterfaceSelector, Lane, LaneFailurePolicy, LaneId, Profile,
    RuleAction,
};

use crate::config::Language;
use crate::ui::i18n::Texts;

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

fn lane_edit_error_text(error: LaneEditError, texts: Texts) -> &'static str {
    match error {
        LaneEditError::EmptyName => texts.lane_name_empty,
        LaneEditError::DuplicateName => texts.lane_name_duplicate,
        LaneEditError::EmptyInterface => texts.lane_interface_empty,
        LaneEditError::NotFound => texts.lane_not_found,
    }
}

fn set_lane_interface(profile: &mut Profile, lane_id: &LaneId, interface: &str) -> bool {
    if let Some(lane) = profile.lanes.iter_mut().find(|lane| &lane.id == lane_id) {
        lane.interface = InterfaceSelector::from(interface);
        true
    } else {
        false
    }
}

fn set_lane_failure_policy(
    profile: &mut Profile,
    lane_id: &LaneId,
    failure_policy: LaneFailurePolicy,
) -> bool {
    if let Some(lane) = profile.lanes.iter_mut().find(|lane| &lane.id == lane_id) {
        lane.failure_policy = failure_policy;
        true
    } else {
        false
    }
}

pub fn delete_lane(profile: &mut Profile, lane_id: &LaneId) {
    profile.lanes.retain(|lane| &lane.id != lane_id);
    profile
        .rules
        .retain(|rule| !matches!(&rule.action, RuleAction::Route(target) if target == lane_id));
}

pub fn build_settings_dialog(
    parent: &adw::ApplicationWindow,
    texts: Texts,
    profile: Rc<RefCell<Profile>>,
    on_change: Rc<dyn Fn()>,
    initial_language: Language,
    on_language: Rc<dyn Fn(Language)>,
) -> adw::PreferencesWindow {
    let dialog = adw::PreferencesWindow::builder()
        .title(texts.settings_title)
        .transient_for(parent)
        .modal(true)
        .hide_on_close(true)
        .build();
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .title(texts.lanes_title)
        .description(texts.lanes_description)
        .build();
    let interfaces = system_interfaces();

    for lane in profile.borrow().lanes.clone() {
        add_lane_editor(
            &group,
            &lane,
            &interfaces,
            profile.clone(),
            on_change.clone(),
            texts,
        );
    }

    let add = adw::ActionRow::builder().title(texts.add_lane).build();
    let name = gtk::Entry::builder()
        .placeholder_text(texts.lane_name)
        .width_chars(12)
        .build();
    let interface_names = interfaces.iter().map(String::as_str).collect::<Vec<_>>();
    let interface_model = gtk::StringList::new(&interface_names);
    let interface = gtk::DropDown::builder().model(&interface_model).build();
    let policies = gtk::StringList::new(&[
        texts.failure_block,
        texts.failure_direct,
        texts.failure_inactive,
    ]);
    let policy = gtk::DropDown::builder().model(&policies).build();
    let button = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text(texts.add_lane)
        .build();
    add.add_suffix(&name);
    add.add_suffix(&interface);
    add.add_suffix(&policy);
    add.add_suffix(&button);
    group.add(&add);

    {
        let group = group.clone();
        let profile = profile.clone();
        let interfaces = interfaces.clone();
        let add_row = add.clone();
        button.connect_clicked(move |_| {
            let Some(interface_name) = interfaces.get(interface.selected() as usize) else {
                add_row.set_subtitle(texts.select_interface);
                return;
            };
            let failure_policy = policy_from_index(policy.selected());
            let lane_result = {
                let mut profile = profile.borrow_mut();
                add_lane(
                    &mut profile,
                    name.text().as_str(),
                    interface_name,
                    failure_policy,
                )
                .map(|id| profile.lanes.iter().find(|lane| lane.id == id).cloned())
            };
            match lane_result {
                Ok(Some(lane)) => {
                    add_row.set_subtitle("");
                    name.remove_css_class("error");
                    add_lane_editor(
                        &group,
                        &lane,
                        &interfaces,
                        profile.clone(),
                        on_change.clone(),
                        texts,
                    );
                    name.set_text("");
                    on_change();
                }
                Ok(None) => {}
                Err(error) => {
                    add_row.set_subtitle(lane_edit_error_text(error, texts));
                    name.add_css_class("error");
                    name.set_text("");
                }
            }
        });
    }

    page.add(&group);
    let language_group = adw::PreferencesGroup::builder()
        .title(texts.language_title)
        .description(texts.language_description)
        .build();
    let language = adw::ComboRow::builder()
        .title(texts.language_row_title)
        .subtitle(texts.language_row_subtitle)
        .build();
    language.set_model(Some(&gtk::StringList::new(&["Türkçe", "English"])));
    language.set_selected(initial_language.index());
    language.connect_selected_notify(move |combo| {
        on_language(Language::from_index(combo.selected()));
    });
    language_group.add(&language);
    page.add(&language_group);
    dialog.add(&page);
    dialog
}

fn add_lane_editor(
    group: &adw::PreferencesGroup,
    lane: &Lane,
    interfaces: &[String],
    profile: Rc<RefCell<Profile>>,
    on_change: Rc<dyn Fn()>,
    texts: Texts,
) {
    let row = adw::ActionRow::new();
    row.set_title(&lane.name);

    let name = gtk::Entry::builder()
        .text(&lane.name)
        .width_chars(12)
        .tooltip_text(texts.lane_name)
        .build();
    let interface_names = interfaces.iter().map(String::as_str).collect::<Vec<_>>();
    let interface_model = gtk::StringList::new(&interface_names);
    let interface = gtk::DropDown::builder().model(&interface_model).build();
    if let Some(index) = interfaces
        .iter()
        .position(|candidate| candidate == lane.interface.as_str())
    {
        interface.set_selected(index as u32);
    }
    let policies = gtk::StringList::new(&[
        texts.failure_block,
        texts.failure_direct,
        texts.failure_inactive,
    ]);
    let policy = gtk::DropDown::builder().model(&policies).build();
    policy.set_selected(policy_index(&lane.failure_policy));
    let delete = gtk::Button::builder()
        .icon_name("list-remove-symbolic")
        .tooltip_text(texts.delete_lane)
        .build();
    delete.add_css_class("flat");
    row.add_suffix(&name);
    row.add_suffix(&interface);
    row.add_suffix(&policy);
    row.add_suffix(&delete);
    group.add(&row);

    {
        let lane_id = lane.id.clone();
        let profile = profile.clone();
        let on_change = on_change.clone();
        let row = row.clone();
        let resetting = Rc::new(std::cell::Cell::new(false));
        name.connect_changed(move |entry| {
            if resetting.get() {
                return;
            }
            let result = {
                let mut profile = profile.borrow_mut();
                rename_lane(&mut profile, &lane_id, entry.text().as_str()).map(|()| {
                    profile
                        .lanes
                        .iter()
                        .find(|lane| lane.id == lane_id)
                        .map(|lane| lane.name.clone())
                        .unwrap_or_default()
                })
            };
            match result {
                Ok(canonical) => {
                    row.set_subtitle("");
                    entry.remove_css_class("error");
                    if entry.text().as_str() != canonical {
                        resetting.set(true);
                        entry.set_text(&canonical);
                        resetting.set(false);
                    }
                    on_change();
                }
                Err(error) => {
                    let canonical = profile
                        .borrow()
                        .lanes
                        .iter()
                        .find(|lane| lane.id == lane_id)
                        .map(|lane| lane.name.clone())
                        .unwrap_or_default();
                    row.set_subtitle(lane_edit_error_text(error, texts));
                    entry.add_css_class("error");
                    resetting.set(true);
                    entry.set_text(&canonical);
                    resetting.set(false);
                }
            }
        });
    }
    {
        let lane_id = lane.id.clone();
        let profile = profile.clone();
        let interfaces = interfaces.to_vec();
        let on_change = on_change.clone();
        interface.connect_selected_notify(move |dropdown| {
            let Some(selected) = interfaces.get(dropdown.selected() as usize) else {
                return;
            };
            let changed = {
                let mut profile = profile.borrow_mut();
                set_lane_interface(&mut profile, &lane_id, selected)
            };
            if changed {
                on_change();
            }
        });
    }
    {
        let lane_id = lane.id.clone();
        let profile = profile.clone();
        let on_change = on_change.clone();
        policy.connect_selected_notify(move |dropdown| {
            let changed = {
                let mut profile = profile.borrow_mut();
                set_lane_failure_policy(
                    &mut profile,
                    &lane_id,
                    policy_from_index(dropdown.selected()),
                )
            };
            if changed {
                on_change();
            }
        });
    }
    {
        let lane_id = lane.id.clone();
        let profile = profile.clone();
        let group = group.clone();
        delete.connect_clicked(move |_| {
            {
                let mut profile = profile.borrow_mut();
                delete_lane(&mut profile, &lane_id);
            }
            group.remove(&row);
            on_change();
        });
    }
}

fn policy_from_index(index: u32) -> LaneFailurePolicy {
    match index {
        0 => LaneFailurePolicy::Block,
        1 => LaneFailurePolicy::Direct,
        _ => LaneFailurePolicy::Inactive,
    }
}

fn policy_index(policy: &LaneFailurePolicy) -> u32 {
    match policy {
        LaneFailurePolicy::Block => 0,
        LaneFailurePolicy::Direct => 1,
        LaneFailurePolicy::Inactive => 2,
    }
}

fn system_interfaces() -> Vec<String> {
    let mut interfaces = std::fs::read_dir("/sys/class/net")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    interfaces.sort();
    interfaces
}

#[cfg(test)]
mod tests {
    use super::{add_lane, delete_lane, lane_edit_error_text, rename_lane, LaneEditError};
    use routelane::domain::{
        InterfaceSelector, LaneFailurePolicy, Profile, ProfileId, Rule, RuleAction, RuleId,
        RuleSource, RuleTarget,
    };

    fn profile() -> Profile {
        Profile {
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
                crate::config::Language::Tr.texts(),
            ),
            "Rota adı benzersiz olmalıdır"
        );
        assert_eq!(
            lane_edit_error_text(
                LaneEditError::EmptyInterface,
                crate::config::Language::En.texts(),
            ),
            "Lane interface must not be empty"
        );
    }
}
