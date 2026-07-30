use std::cell::{Cell, RefCell};
use std::rc::Rc;

use async_channel::Sender;
use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};
use routelane::domain::{LaneId, ProfileId, Rule, RuleAction, RuleId};
use routelane::ipc::DaemonRequest;

use crate::models::UiToDaemon;
use crate::ui::i18n::Texts;

pub fn set_rule_enabled_request(
    profile_id: ProfileId,
    rule_id: RuleId,
    enabled: bool,
) -> DaemonRequest {
    DaemonRequest::SetRuleEnabled {
        profile_id,
        rule_id,
        enabled,
    }
}

pub fn create_rule_row(
    rule: &Rule,
    profile_id: ProfileId,
    lanes: &[(LaneId, String)],
    sender: Sender<UiToDaemon>,
    routing_active: Rc<Cell<bool>>,
    texts: Texts,
    on_change: Rc<dyn Fn(Rule)>,
    on_delete: Rc<dyn Fn(RuleId)>,
) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(&target_label(rule));

    let enabled = gtk::Switch::builder()
        .active(rule.enabled)
        .valign(gtk::Align::Center)
        .tooltip_text(texts.rule_enabled)
        .build();
    let priority = gtk::SpinButton::with_range(1.0, 4_294_967_295.0, 1.0);
    priority.set_value(f64::from(rule.priority));
    priority.set_width_chars(6);
    priority.set_tooltip_text(Some(texts.rule_priority));

    let actions =
        gtk::StringList::new(&[texts.action_route, texts.action_direct, texts.action_block]);
    let action = gtk::DropDown::builder()
        .model(&actions)
        .valign(gtk::Align::Center)
        .build();
    action.set_selected(match rule.action {
        RuleAction::Route(_) => 0,
        RuleAction::Direct => 1,
        RuleAction::Block => 2,
    });

    let lane_names = lanes
        .iter()
        .map(|(_, name)| name.as_str())
        .collect::<Vec<_>>();
    let lane_model = gtk::StringList::new(&lane_names);
    let lane_select = gtk::DropDown::builder()
        .model(&lane_model)
        .valign(gtk::Align::Center)
        .build();
    if let RuleAction::Route(selected) = &rule.action {
        if let Some(index) = lanes.iter().position(|(id, _)| id == selected) {
            lane_select.set_selected(index as u32);
        }
    }
    lane_select.set_visible(matches!(rule.action, RuleAction::Route(_)));

    let delete = gtk::Button::builder()
        .icon_name("list-remove-symbolic")
        .tooltip_text(texts.delete_rule_tooltip)
        .valign(gtk::Align::Center)
        .build();
    delete.add_css_class("flat");

    row.add_suffix(&enabled);
    row.add_suffix(&priority);
    row.add_suffix(&action);
    row.add_suffix(&lane_select);
    row.add_suffix(&delete);
    row.set_subtitle(&action_label(&rule.action, lanes, rule.priority));
    let rule_state = Rc::new(RefCell::new(rule.clone()));

    {
        let rule_id = rule.id.clone();
        let profile_id = profile_id.clone();
        let sender = sender.clone();
        let changed = rule_state.clone();
        let on_change = on_change.clone();
        enabled.connect_state_set(move |_, state| {
            changed.borrow_mut().enabled = state;
            on_change(changed.borrow().clone());
            if routing_active.get() {
                let request = set_rule_enabled_request(profile_id.clone(), rule_id.clone(), state);
                let sender = sender.clone();
                glib::MainContext::default().spawn_local(async move {
                    sender.send(UiToDaemon::Request(request)).await.ok();
                });
            }
            glib::signal::Inhibit(false)
        });
    }
    {
        let changed = rule_state.clone();
        let on_change = on_change.clone();
        priority.connect_value_changed(move |spin| {
            changed.borrow_mut().priority = spin.value_as_int().max(1) as u32;
            on_change(changed.borrow().clone());
        });
    }
    {
        let changed = rule_state.clone();
        let lanes = lanes.to_vec();
        let lane_select_for_action = lane_select.clone();
        let on_change = on_change.clone();
        action.connect_selected_notify(move |dropdown| {
            changed.borrow_mut().action = match dropdown.selected() {
                1 => RuleAction::Direct,
                2 => RuleAction::Block,
                _ => lanes
                    .get(lane_select_for_action.selected() as usize)
                    .map(|(id, _)| RuleAction::Route(id.clone()))
                    .unwrap_or(RuleAction::Direct),
            };
            lane_select_for_action
                .set_visible(matches!(changed.borrow().action, RuleAction::Route(_)));
            on_change(changed.borrow().clone());
        });
    }
    {
        let changed = rule_state;
        let lanes = lanes.to_vec();
        let on_change = on_change.clone();
        lane_select.connect_selected_notify(move |dropdown| {
            if let Some((lane_id, _)) = lanes.get(dropdown.selected() as usize) {
                changed.borrow_mut().action = RuleAction::Route(lane_id.clone());
                on_change(changed.borrow().clone());
            }
        });
    }
    {
        let rule_id = rule.id.clone();
        let row = row.clone();
        delete.connect_clicked(move |_| {
            on_delete(rule_id.clone());
            row.set_visible(false);
        });
    }
    row
}

fn target_label(rule: &Rule) -> String {
    match &rule.target {
        routelane::domain::RuleTarget::Any => "Any".to_owned(),
        routelane::domain::RuleTarget::Domain(value)
        | routelane::domain::RuleTarget::WildcardDomain(value) => value.clone(),
        routelane::domain::RuleTarget::Ip(value) => value.to_string(),
        routelane::domain::RuleTarget::Cidr(value) => value.to_string(),
        routelane::domain::RuleTarget::ServicePack(value) => format!("{value:?}"),
    }
}

fn action_label(action: &RuleAction, lanes: &[(LaneId, String)], priority: u32) -> String {
    let action = match action {
        RuleAction::Route(lane_id) => lanes
            .iter()
            .find(|(id, _)| id == lane_id)
            .map(|(_, name)| format!("Route: {name}"))
            .unwrap_or_else(|| "Route".to_owned()),
        RuleAction::Direct => "Direct".to_owned(),
        RuleAction::Block => "Block".to_owned(),
    };
    format!("{action} · priority {priority}")
}

#[cfg(test)]
mod tests {
    use super::set_rule_enabled_request;
    use routelane::domain::{ProfileId, RuleId};
    use routelane::ipc::DaemonRequest;

    #[test]
    fn disabling_a_block_rule_uses_mutation_without_deleting_the_rule() {
        let profile_id = ProfileId::from("default");
        let rule_id = RuleId::from("blocked-service");

        assert_eq!(
            set_rule_enabled_request(profile_id.clone(), rule_id.clone(), false),
            DaemonRequest::SetRuleEnabled {
                profile_id,
                rule_id,
                enabled: false,
            }
        );
    }
}
