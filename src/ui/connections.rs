use crate::ui::{
    confirm::confirm_destructive_action,
    form::Form,
    i18n::Texts,
    list_page::ListPage,
    settings::{add_lane, delete_lane, lane_edit_error_text, rename_lane, routed_rule_count},
    shell::Section,
};
use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};
use routelane::domain::{
    canonical_lane_name, validate_profile, Lane, LaneFailurePolicy, LaneId, Profile, RuleAction,
    RuleSource,
};
use std::{cell::RefCell, rc::Rc};

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

#[derive(Clone)]
pub struct ConnectionsPage {
    pub list: ListPage,
    profile: Rc<RefCell<Profile>>,
    parent: glib::WeakRef<adw::ApplicationWindow>,
    texts: Texts,
    on_save: Rc<dyn Fn(Profile) -> bool>,
    rows: Rc<RefCell<Vec<adw::ActionRow>>>,
    rendered: Rc<RefCell<Option<Vec<Lane>>>>,
}

impl ConnectionsPage {
    pub fn new(
        parent: &adw::ApplicationWindow,
        texts: Texts,
        profile: Rc<RefCell<Profile>>,
        on_save: Rc<dyn Fn(Profile) -> bool>,
    ) -> Self {
        let page = Self {
            list: ListPage::new(texts, Section::Connections),
            profile,
            parent: parent.downgrade(),
            texts,
            on_save,
            rows: Rc::new(RefCell::new(vec![])),
            rendered: Rc::new(RefCell::new(None)),
        };
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
    pub fn update(&self, editing_enabled: bool) {
        self.list.add.set_sensitive(editing_enabled);
        self.list.group.set_sensitive(editing_enabled);
        let profile = self.profile.borrow().clone();
        if self.rendered.borrow().as_ref() == Some(&profile.lanes) {
            return;
        }
        *self.rendered.borrow_mut() = Some(profile.lanes.clone());
        for row in self.rows.borrow_mut().drain(..) {
            self.list.group.remove(&row);
        }
        self.list.set_empty(profile.lanes.is_empty(), false);
        for lane in &profile.lanes {
            let policy = match lane.failure_policy {
                LaneFailurePolicy::Block => self.texts.ux.fail_block,
                LaneFailurePolicy::Direct => self.texts.ux.fail_direct,
                LaneFailurePolicy::Inactive => self.texts.ux.fail_inactive,
            };
            let row = adw::ActionRow::builder()
                .title(&glib::markup_escape_text(&lane.name))
                .subtitle(&glib::markup_escape_text(&format!(
                    "{} · {policy}",
                    lane.interface.as_str()
                )))
                .build();
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
    }
}

#[cfg(test)]
mod tests {
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
