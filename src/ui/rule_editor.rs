use crate::ui::{form::Form, i18n::Texts, rule_row::ReorderDirection};
use gtk4::{self as gtk, prelude::*};
use libadwaita as adw;
use routelane::domain::{Lane, Profile, Rule, RuleAction, RuleId, RuleSource, RuleTarget};
use std::{cell::RefCell, rc::Rc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetInputError {
    Empty,
    Invalid,
    Unsupported,
}

pub fn parse_rule_target(input: &str) -> Result<RuleTarget, TargetInputError> {
    let value = input.trim();
    if value.is_empty() {
        return Err(TargetInputError::Empty);
    }
    if value == "*" {
        return Ok(RuleTarget::Any);
    }
    if value.contains('*') {
        return Err(TargetInputError::Unsupported);
    }
    if let Ok(address) = value.parse() {
        return Ok(RuleTarget::Ip(address));
    }
    if let Ok(network) = value.parse() {
        return Ok(RuleTarget::Cidr(network));
    }
    let domain = value.strip_suffix('.').unwrap_or(value);
    if domain.len() > 253
        || domain.is_empty()
        || domain.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        || domain.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Err(TargetInputError::Invalid);
    }
    Ok(RuleTarget::Domain(value.to_ascii_lowercase()))
}

#[derive(Debug)]
pub struct RuleDraft {
    pub target: String,
    pub action: RuleAction,
}
#[derive(Debug, PartialEq, Eq)]
pub enum RuleDraftError {
    Target(TargetInputError),
    MissingLane,
}

pub fn validate_rule_draft(
    profile: &Profile,
    draft: &RuleDraft,
) -> Result<(RuleTarget, RuleAction), RuleDraftError> {
    let target = parse_rule_target(&draft.target).map_err(RuleDraftError::Target)?;
    if let RuleAction::Route(id) = &draft.action {
        if !profile.lanes.iter().any(|lane| &lane.id == id) {
            return Err(RuleDraftError::MissingLane);
        }
    }
    Ok((target, draft.action.clone()))
}

pub fn target_text(target: &RuleTarget) -> String {
    match target {
        RuleTarget::Any => "*".into(),
        RuleTarget::Domain(value) | RuleTarget::WildcardDomain(value) => value.clone(),
        RuleTarget::Ip(value) => value.to_string(),
        RuleTarget::Cidr(value) => value.to_string(),
        RuleTarget::ServicePack(value) => format!("{value:?}"),
    }
}

pub fn action_text(action: &RuleAction, lanes: &[Lane], texts: Texts) -> String {
    match action {
        RuleAction::Direct => texts.ux.using_system.into(),
        RuleAction::Block => texts.ux.blocked.into(),
        RuleAction::Route(id) => lanes
            .iter()
            .find(|lane| &lane.id == id)
            .map(|lane| format!("{}: {}", texts.ux.connection, lane.name))
            .unwrap_or_else(|| texts.ux.missing_interface.into()),
    }
}

pub fn rule_matches_query(rule: &Rule, lanes: &[Lane], query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || target_text(&rule.target).to_lowercase().contains(&query)
        || matches!(&rule.action, RuleAction::Route(id) if lanes.iter().any(|lane| &lane.id == id && lane.name.to_lowercase().contains(&query)))
}

pub fn move_rule(profile: &mut Profile, id: &RuleId, direction: ReorderDirection) -> bool {
    let mut ordered = profile
        .rules
        .iter()
        .map(|rule| (rule.id.clone(), rule.priority))
        .collect::<Vec<_>>();
    ordered.sort_by(|(a, ap), (b, bp)| (ap, a.as_str()).cmp(&(bp, b.as_str())));
    let Some(index) = ordered.iter().position(|(rule_id, _)| rule_id == id) else {
        return false;
    };
    let neighbor = match direction {
        ReorderDirection::Up => index.checked_sub(1),
        ReorderDirection::Down => index.checked_add(1).filter(|i| *i < ordered.len()),
    };
    let Some(neighbor) = neighbor else {
        return false;
    };
    ordered.swap(index, neighbor);
    for (index, (id, _)) in ordered.iter().enumerate() {
        if let Some(rule) = profile.rules.iter_mut().find(|rule| &rule.id == id) {
            rule.priority = u32::try_from(index + 1)
                .expect("profile rule limit")
                .checked_mul(10)
                .expect("profile priority limit");
        }
    }
    true
}

pub fn open_rule_editor(
    parent: &adw::ApplicationWindow,
    texts: Texts,
    profile: Rc<RefCell<Profile>>,
    editing: Option<RuleId>,
    on_save: Rc<dyn Fn(Rule) -> bool>,
) {
    let original = editing.as_ref().and_then(|id| {
        profile
            .borrow()
            .rules
            .iter()
            .find(|rule| &rule.id == id)
            .cloned()
    });
    let form = Form::new(
        parent,
        if editing.is_some() {
            texts.ux.edit
        } else {
            texts.add_address_title
        },
        texts,
    );
    let target = gtk::Entry::builder()
        .activates_default(true)
        .placeholder_text(texts.address_placeholder)
        .text(
            &original
                .as_ref()
                .map(|rule| target_text(&rule.target))
                .unwrap_or_default(),
        )
        .build();
    target.set_widget_name("rule-target");
    let action =
        gtk::DropDown::from_strings(&[texts.action_route, texts.action_direct, texts.action_block]);
    action.set_widget_name("rule-action");
    action.set_selected(match original.as_ref().map(|rule| &rule.action) {
        Some(RuleAction::Direct) => 1,
        Some(RuleAction::Block) => 2,
        _ => 0,
    });
    let lanes = profile.borrow().lanes.clone();
    let names = lanes
        .iter()
        .map(|lane| lane.name.as_str())
        .collect::<Vec<_>>();
    let connection = gtk::DropDown::from_strings(&names);
    connection.set_widget_name("rule-connection");
    connection.set_selected(
        original
            .as_ref()
            .and_then(|rule| match &rule.action {
                RuleAction::Route(id) => lanes
                    .iter()
                    .position(|lane| &lane.id == id)
                    .map(|i| i as u32),
                _ => None,
            })
            .unwrap_or(gtk::INVALID_LIST_POSITION),
    );
    form.field(texts.rule_target_label, &target);
    form.field(texts.rule_action_label, &action);
    let connection_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    connection_box.append(
        &gtk::Label::builder()
            .label(texts.ux.connection)
            .xalign(0.0)
            .build(),
    );
    connection_box.append(&connection);
    connection.update_property(&[gtk::accessible::Property::Label(texts.ux.connection)]);
    connection_box.set_visible(action.selected() == 0);
    form.fields.append(&connection_box);
    action
        .connect_selected_notify(move |action| connection_box.set_visible(action.selected() == 0));
    let initial_focus = target.clone();
    let submit_form = form.clone();
    let handler = form.submit.connect_clicked(move |_| {
        if !submit_form.begin_submit() {
            return;
        }
        let current = profile.borrow().clone();
        if editing
            .as_ref()
            .is_some_and(|id| !current.rules.iter().any(|rule| &rule.id == id))
        {
            submit_form.error(texts.ux.request_failed, &target);
            return;
        }
        if let Err(error) = parse_rule_target(&target.text()) {
            submit_form.error(
                if error == TargetInputError::Unsupported {
                    texts.ux.unsupported_target
                } else {
                    texts.invalid_rule_target
                },
                &target,
            );
            return;
        }
        let selected_action = match action.selected() {
            1 => RuleAction::Direct,
            2 => RuleAction::Block,
            _ => {
                let Some(lane) = lanes.get(connection.selected() as usize) else {
                    submit_form.error(texts.select_interface, &connection);
                    return;
                };
                RuleAction::Route(lane.id.clone())
            }
        };
        match validate_rule_draft(
            &current,
            &RuleDraft {
                target: target.text().to_string(),
                action: selected_action,
            },
        ) {
            Ok((target_value, action)) => {
                let mut rule = original.clone().unwrap_or_else(|| Rule {
                    id: format!("rule-{}", uuid::Uuid::new_v4()).as_str().into(),
                    enabled: true,
                    priority: current
                        .rules
                        .iter()
                        .map(|r| r.priority)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(10),
                    source: RuleSource::Any,
                    target: RuleTarget::Any,
                    action: RuleAction::Direct,
                });
                rule.target = target_value;
                rule.action = action;
                if on_save(rule) {
                    submit_form.dialog.close();
                } else {
                    submit_form.error(texts.ux.request_failed, &target);
                }
            }
            Err(error) => submit_form.error(
                match error {
                    RuleDraftError::Target(TargetInputError::Unsupported) => {
                        texts.ux.unsupported_target
                    }
                    RuleDraftError::MissingLane => texts.ux.missing_interface,
                    _ => texts.invalid_rule_target,
                },
                &target,
            ),
        }
    });
    form.disconnect_on_close(handler);
    form.dialog.present();
    initial_focus.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> Profile {
        Profile {
            id: "work".into(),
            name: "Work".into(),
            enabled: false,
            lanes: vec![],
            rules: vec![],
        }
    }
    fn rule(id: &str, priority: u32) -> Rule {
        Rule {
            id: id.into(),
            enabled: true,
            priority,
            source: RuleSource::Any,
            target: RuleTarget::Domain(format!("{id}.example")),
            action: RuleAction::Block,
        }
    }
    #[test]
    fn validates_supported_targets_without_silently_accepting_urls() {
        assert_eq!(parse_rule_target("*"), Ok(RuleTarget::Any));
        assert_eq!(
            parse_rule_target("Claude.AI"),
            Ok(RuleTarget::Domain("claude.ai".into()))
        );
        assert_eq!(
            parse_rule_target("claude.ai."),
            Ok(RuleTarget::Domain("claude.ai.".into()))
        );
        assert_eq!(
            parse_rule_target("*.claude.ai"),
            Err(TargetInputError::Unsupported)
        );
        for invalid in [
            "https://claude.ai/path",
            "203.0.113.0/77",
            "a..example",
            "-bad.example",
            "example.com:443",
            "bad name",
            "999.999.999.999",
        ] {
            assert!(parse_rule_target(invalid).is_err(), "{invalid}");
        }
        assert_eq!(parse_rule_target(""), Err(TargetInputError::Empty));
        assert_eq!(
            parse_rule_target("2001:db8::1"),
            Ok(RuleTarget::Ip("2001:db8::1".parse().unwrap()))
        );
        assert_eq!(
            parse_rule_target("192.0.2.0/24"),
            Ok(RuleTarget::Cidr("192.0.2.0/24".parse().unwrap()))
        );
    }
    #[test]
    fn route_without_lane_does_not_become_direct() {
        let draft = RuleDraft {
            target: "claude.ai".into(),
            action: RuleAction::Route("missing".into()),
        };
        assert!(validate_rule_draft(&profile(), &draft).is_err());
    }
    #[test]
    fn equal_priority_move_changes_real_order_and_preserves_all_rules() {
        let mut p = profile();
        p.rules = vec![rule("a", 10), rule("b", 10), rule("c", 10)];
        assert!(move_rule(&mut p, &"c".into(), ReorderDirection::Up));
        p.rules.sort_by(Rule::execution_cmp);
        assert_eq!(
            p.rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["a", "c", "b"]
        );
        assert!(!move_rule(&mut p, &"a".into(), ReorderDirection::Up));
    }
    #[test]
    fn filtered_reorder_uses_full_execution_order() {
        let mut p = profile();
        p.rules = vec![rule("a", 10), rule("b", 20), rule("c", 30)];
        assert!(rule_matches_query(&p.rules[2], &[], "c.example"));
        assert!(move_rule(&mut p, &"c".into(), ReorderDirection::Up));
        p.rules.sort_by(Rule::execution_cmp);
        assert_eq!(
            p.rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["a", "c", "b"]
        );
    }

    #[test]
    fn equal_priority_move_reorders_without_dropping_application_rules() {
        let mut p = profile();
        let mut app = rule("b", 10);
        app.source = RuleSource::Application(routelane::domain::ApplicationSelector {
            desktop_id: Some("browser".into()),
            executable_path: "/usr/bin/browser".into(),
            include_children: true,
            launch: None,
        });
        p.rules = vec![rule("a", 10), app.clone(), rule("c", 10)];
        assert!(move_rule(&mut p, &"c".into(), ReorderDirection::Up));
        p.rules.sort_by(Rule::execution_cmp);
        assert_eq!(
            p.rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["a", "c", "b"]
        );
        let mut retained = p.rules[2].clone();
        retained.priority = app.priority;
        assert_eq!(retained, app);
    }
}
