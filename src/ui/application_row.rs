use std::cell::RefCell;
use std::rc::Rc;

use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};
use routelane::domain::{LaneId, ProfileId, Rule, RuleAction, RuleId, RuleSource};
use routelane::ipc::DaemonRequest;

use crate::ui::confirm::confirm_destructive_action;
use crate::ui::i18n::Texts;
use crate::ui::rule_row::RuleEdit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationAction {
    Launch,
    AdoptRunningProcesses,
}

pub fn application_action_request(
    profile_id: ProfileId,
    rule_id: RuleId,
    action: ApplicationAction,
) -> DaemonRequest {
    match action {
        ApplicationAction::Launch => DaemonRequest::LaunchApplication {
            profile_id,
            rule_id,
        },
        ApplicationAction::AdoptRunningProcesses => DaemonRequest::AdoptApplicationProcesses {
            profile_id,
            rule_id,
        },
    }
}

pub fn application_subtitle(
    lane_id: &LaneId,
    lanes: &[(LaneId, String)],
    prefix: &str,
    suffix: &str,
) -> String {
    let lane = lanes
        .iter()
        .find(|(id, _)| id == lane_id)
        .map(|(_, name)| name.as_str())
        .unwrap_or_else(|| lane_id.as_str());
    format!("{prefix}{lane}{suffix}")
}

pub struct ApplicationRowContext {
    pub profile_id: ProfileId,
    pub lanes: Vec<(LaneId, String)>,
    pub texts: Texts,
    pub parent: adw::ApplicationWindow,
    pub on_change: Rc<dyn Fn(Rule, RuleEdit)>,
    pub on_delete: Rc<dyn Fn(RuleId)>,
    pub on_request: Rc<dyn Fn(DaemonRequest)>,
    pub routing_active: bool,
}

pub fn create_application_row(rule: &Rule, context: ApplicationRowContext) -> adw::ActionRow {
    let RuleSource::Application(selector) = &rule.source else {
        unreachable!("application rows require an application source");
    };
    let title = selector
        .desktop_id
        .as_deref()
        .or_else(|| {
            selector
                .executable_path
                .file_name()
                .and_then(|value| value.to_str())
        })
        .map(str::to_owned)
        .unwrap_or_else(|| selector.executable_path.to_string_lossy().into_owned());
    let row = adw::ActionRow::builder().title(&title).build();
    let selected_lane = match &rule.action {
        RuleAction::Route(lane_id) => lane_id.clone(),
        RuleAction::Direct | RuleAction::Block => context
            .lanes
            .first()
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| LaneId::from("missing")),
    };
    row.set_subtitle(&application_subtitle(
        &selected_lane,
        &context.lanes,
        context.texts.application_scope_prefix,
        context.texts.application_scope_suffix,
    ));

    let enabled = gtk::Switch::builder()
        .active(rule.enabled)
        .valign(gtk::Align::Center)
        .tooltip_text(context.texts.application_enabled)
        .build();
    let lane_names = context
        .lanes
        .iter()
        .map(|(_, name)| name.as_str())
        .collect::<Vec<_>>();
    let lane_select = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&lane_names))
        .valign(gtk::Align::Center)
        .tooltip_text(context.texts.application_lane)
        .build();
    if let Some(index) = context
        .lanes
        .iter()
        .position(|(id, _)| id == &selected_lane)
    {
        lane_select.set_selected(index as u32);
    }
    let launch = gtk::Button::builder()
        .label(context.texts.application_launch)
        .tooltip_text(context.texts.application_launch_tooltip)
        .valign(gtk::Align::Center)
        .build();
    launch.add_css_class("suggested-action");
    let adopt = gtk::Button::builder()
        .label(context.texts.application_adopt)
        .tooltip_text(context.texts.application_adopt_tooltip)
        .valign(gtk::Align::Center)
        .build();
    let delete = gtk::Button::builder()
        .icon_name("list-remove-symbolic")
        .tooltip_text(context.texts.delete_rule_tooltip)
        .valign(gtk::Align::Center)
        .build();
    let actions_enabled = context.routing_active && rule.enabled;
    launch.set_sensitive(actions_enabled);
    adopt.set_sensitive(actions_enabled);
    delete.add_css_class("flat");
    row.add_suffix(&enabled);
    row.add_suffix(&lane_select);
    row.add_suffix(&launch);
    row.add_suffix(&adopt);
    row.add_suffix(&delete);

    let state = Rc::new(RefCell::new(rule.clone()));
    {
        let state = state.clone();
        let on_change = context.on_change.clone();
        let launch = launch.clone();
        let adopt = adopt.clone();
        let routing_active = context.routing_active;
        enabled.connect_state_set(move |_, active| {
            state.borrow_mut().enabled = active;
            launch.set_sensitive(routing_active && active);
            adopt.set_sensitive(routing_active && active);
            on_change(state.borrow().clone(), RuleEdit::Enabled(active));
            glib::signal::Inhibit(false)
        });
    }
    {
        let state = state.clone();
        let lanes = context.lanes.clone();
        let row = row.clone();
        let on_change = context.on_change.clone();
        let texts = context.texts;
        lane_select.connect_selected_notify(move |dropdown| {
            if let Some((lane_id, _)) = lanes.get(dropdown.selected() as usize) {
                state.borrow_mut().action = RuleAction::Route(lane_id.clone());
                row.set_subtitle(&application_subtitle(
                    lane_id,
                    &lanes,
                    texts.application_scope_prefix,
                    texts.application_scope_suffix,
                ));
                on_change(state.borrow().clone(), RuleEdit::Definition);
            }
        });
    }
    for (button, action) in [
        (launch, ApplicationAction::Launch),
        (adopt, ApplicationAction::AdoptRunningProcesses),
    ] {
        let on_request = context.on_request.clone();
        let profile_id = context.profile_id.clone();
        let rule_id = rule.id.clone();
        button.connect_clicked(move |_| {
            on_request(application_action_request(
                profile_id.clone(),
                rule_id.clone(),
                action,
            ));
        });
    }
    {
        let on_delete = context.on_delete;
        let rule_id = rule.id.clone();
        let row = row.clone();
        let body = format!(
            "{}{}{}",
            context.texts.confirm_delete_rule_prefix,
            title,
            context.texts.confirm_delete_rule_suffix
        );
        delete.connect_clicked(move |_| {
            let on_delete = on_delete.clone();
            let rule_id = rule_id.clone();
            let row = row.clone();
            confirm_destructive_action(
                &context.parent,
                context.texts,
                context.texts.confirm_delete_rule_heading,
                &body,
                Rc::new(move || {
                    on_delete(rule_id.clone());
                    row.set_visible(false);
                }),
            );
        });
    }
    row
}

#[cfg(test)]
mod tests {
    use super::{application_action_request, application_subtitle, ApplicationAction};
    use routelane::domain::{LaneId, ProfileId, RuleId};
    use routelane::ipc::DaemonRequest;

    #[test]
    fn application_actions_keep_profile_and_rule_identity() {
        assert_eq!(
            application_action_request(
                ProfileId::from("default"),
                RuleId::from("browser"),
                ApplicationAction::Launch,
            ),
            DaemonRequest::LaunchApplication {
                profile_id: ProfileId::from("default"),
                rule_id: RuleId::from("browser"),
            }
        );
        assert_eq!(
            application_action_request(
                ProfileId::from("default"),
                RuleId::from("browser"),
                ApplicationAction::AdoptRunningProcesses,
            ),
            DaemonRequest::AdoptApplicationProcesses {
                profile_id: ProfileId::from("default"),
                rule_id: RuleId::from("browser"),
            }
        );
    }

    #[test]
    fn subtitle_names_the_lane_and_whole_application_scope() {
        assert_eq!(
            application_subtitle(
                &LaneId::from("vinn"),
                &[(LaneId::from("vinn"), "VINN".into())],
                "Bu uygulamanın tüm yeni bağlantıları ",
                " üzerinden yönlendirilir.",
            ),
            "Bu uygulamanın tüm yeni bağlantıları VINN üzerinden yönlendirilir."
        );
    }
}
