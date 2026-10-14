use std::rc::Rc;

use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};
use routelane::domain::{LaneId, ProfileId, Rule, RuleId};
use routelane::ipc::DaemonRequest;

use crate::ui::i18n::Texts;

#[derive(Clone)]
pub struct CompactApplicationRow {
    pub row: adw::ActionRow,
    pub launch: gtk::Button,
    pub enabled: gtk::Switch,
    pub menu: gtk::MenuButton,
}

pub fn create_compact_application_row(
    rule: &Rule,
    title_icon: (&str, &str),
    subtitle: &str,
    texts: Texts,
    on_toggle: Rc<dyn Fn(RuleId, bool)>,
    on_launch: Rc<dyn Fn()>,
    actions: Vec<crate::ui::form::MenuAction>,
) -> CompactApplicationRow {
    let (title, icon) = title_icon;
    let compact = crate::ui::rule_row::create_compact_rule_row(
        rule, title, subtitle, texts, on_toggle, actions,
    );
    compact.row.add_prefix(&gtk::Image::from_icon_name(icon));
    let launch = gtk::Button::with_label(texts.ux.launch_neutral);
    launch.set_widget_name("rule-launch");
    launch.set_valign(gtk::Align::Center);
    launch.connect_clicked(move |_| on_launch());
    compact.row.add_suffix(&launch);
    crate::ui::form::wrap_button(&launch);
    compact.row.remove(&compact.menu);
    compact.row.add_suffix(&compact.menu);
    CompactApplicationRow {
        row: compact.row,
        enabled: compact.enabled,
        launch,
        menu: compact.menu,
    }
}

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
