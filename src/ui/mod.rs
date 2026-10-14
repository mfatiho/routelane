pub mod application_editor;
pub mod application_row;
pub mod confirm;
pub mod connections;
pub mod controller;
pub mod desktop_catalog;
pub mod experience;
pub mod form;
pub mod i18n;
pub mod list_page;
pub mod overview;
pub mod rule_editor;
pub mod rule_row;
pub mod session;
pub mod settings;
pub mod shell;
pub mod tray;
pub mod window;

#[cfg(test)]
mod tests {
    #[test]
    fn activation_reuses_an_existing_window() {
        use super::experience::{activation_action, ActivationAction};
        assert_eq!(activation_action(true), ActivationAction::PresentExisting);
        assert_eq!(activation_action(false), ActivationAction::CreateWindow);
    }
}
