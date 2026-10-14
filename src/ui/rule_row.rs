use std::rc::Rc;

use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};
use routelane::domain::{ProfileId, Rule, RuleId};
use routelane::ipc::DaemonRequest;

use crate::ui::i18n::Texts;

#[derive(Clone)]
pub struct CompactRuleRow {
    pub row: adw::ActionRow,
    pub enabled: gtk::Switch,
    pub menu: gtk::MenuButton,
}

pub fn create_compact_rule_row(
    rule: &Rule,
    title: &str,
    subtitle: &str,
    texts: Texts,
    on_toggle: Rc<dyn Fn(RuleId, bool)>,
    actions: Vec<crate::ui::form::MenuAction>,
) -> CompactRuleRow {
    let row = adw::ActionRow::builder()
        .title(&glib::markup_escape_text(title))
        .subtitle(&glib::markup_escape_text(subtitle))
        .build();
    row.set_widget_name(&format!("rule-{}", rule.id.as_str()));
    row.set_title_lines(2);
    row.set_subtitle_lines(0);
    let enabled = gtk::Switch::builder()
        .active(rule.enabled)
        .valign(gtk::Align::Center)
        .tooltip_text(texts.rule_enabled)
        .build();
    let id = rule.id.clone();
    enabled.set_widget_name("rule-enable");
    enabled.update_property(&[
        gtk::accessible::Property::Label(texts.rule_enabled),
        gtk::accessible::Property::Description(title),
    ]);
    enabled.connect_state_set(move |_, state| {
        on_toggle(id.clone(), state);
        glib::signal::Inhibit(false)
    });
    row.add_suffix(&enabled);
    let menu = crate::ui::form::action_menu(texts, actions);
    menu.set_widget_name("rule-menu");
    menu.update_property(&[gtk::accessible::Property::Description(title)]);
    row.add_suffix(&menu);
    CompactRuleRow { row, enabled, menu }
}

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
pub enum ReorderDirection {
    Up,
    Down,
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
