use std::cell::RefCell;
use std::rc::Rc;

use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};
use routelane::domain::{LaneId, ProfileId, Rule, RuleAction, RuleId};
use routelane::ipc::DaemonRequest;

use crate::ui::confirm::confirm_destructive_action;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleEdit {
    Definition,
    Enabled(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReorderDirection {
    Up,
    Down,
}

pub struct RuleRowContext {
    pub lanes: Vec<(LaneId, String)>,
    pub texts: Texts,
    pub parent: adw::ApplicationWindow,
    pub on_change: Rc<dyn Fn(Rule, RuleEdit)>,
    pub on_delete: Rc<dyn Fn(RuleId)>,
    pub on_reorder: Rc<dyn Fn(RuleId, ReorderDirection)>,
}

pub fn create_rule_row(rule: &Rule, context: RuleRowContext) -> adw::ActionRow {
    let RuleRowContext {
        lanes,
        texts,
        parent,
        on_change,
        on_delete,
        on_reorder,
    } = context;
    let row = adw::ActionRow::new();
    row.set_title(&target_label(rule));

    let enabled = gtk::Switch::builder()
        .active(rule.enabled)
        .valign(gtk::Align::Center)
        .tooltip_text(texts.rule_enabled)
        .build();
    let move_up = gtk::Button::builder()
        .icon_name("go-up-symbolic")
        .tooltip_text(texts.move_rule_up)
        .valign(gtk::Align::Center)
        .build();
    move_up.add_css_class("flat");
    let move_down = gtk::Button::builder()
        .icon_name("go-down-symbolic")
        .tooltip_text(texts.move_rule_down)
        .valign(gtk::Align::Center)
        .build();
    move_down.add_css_class("flat");

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
    row.add_suffix(&move_up);
    row.add_suffix(&move_down);
    row.add_suffix(&action);
    row.add_suffix(&lane_select);
    row.add_suffix(&delete);
    row.set_subtitle(&action_label(&rule.action, &lanes));
    let rule_state = Rc::new(RefCell::new(rule.clone()));

    {
        let changed = rule_state.clone();
        let on_change = on_change.clone();
        enabled.connect_state_set(move |_, state| {
            changed.borrow_mut().enabled = state;
            on_change(changed.borrow().clone(), RuleEdit::Enabled(state));
            glib::signal::Inhibit(false)
        });
    }
    {
        let rule_id = rule.id.clone();
        let on_reorder = on_reorder.clone();
        move_up.connect_clicked(move |_| {
            on_reorder(rule_id.clone(), ReorderDirection::Up);
        });
    }
    {
        let rule_id = rule.id.clone();
        let on_reorder = on_reorder.clone();
        move_down.connect_clicked(move |_| {
            on_reorder(rule_id.clone(), ReorderDirection::Down);
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
            on_change(changed.borrow().clone(), RuleEdit::Definition);
        });
    }
    {
        let changed = rule_state;
        let lanes = lanes.to_vec();
        let on_change = on_change.clone();
        lane_select.connect_selected_notify(move |dropdown| {
            if let Some((lane_id, _)) = lanes.get(dropdown.selected() as usize) {
                changed.borrow_mut().action = RuleAction::Route(lane_id.clone());
                on_change(changed.borrow().clone(), RuleEdit::Definition);
            }
        });
    }
    {
        let rule_id = rule.id.clone();
        let row = row.clone();
        let body = format!(
            "{}{}{}",
            texts.confirm_delete_rule_prefix,
            target_label(rule),
            texts.confirm_delete_rule_suffix
        );
        delete.connect_clicked(move |_| {
            let rule_id = rule_id.clone();
            let row = row.clone();
            let on_delete = on_delete.clone();
            confirm_destructive_action(
                &parent,
                texts,
                texts.confirm_delete_rule_heading,
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

fn action_label(action: &RuleAction, lanes: &[(LaneId, String)]) -> String {
    match action {
        RuleAction::Route(lane_id) => lanes
            .iter()
            .find(|(id, _)| id == lane_id)
            .map(|(_, name)| format!("Route: {name}"))
            .unwrap_or_else(|| "Route".to_owned()),
        RuleAction::Direct => "Direct".to_owned(),
        RuleAction::Block => "Block".to_owned(),
    }
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
