use crate::ui::i18n::Texts;
use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Overview,
    Rules,
    Applications,
    Connections,
}

impl Section {
    pub fn name(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Rules => "rules",
            Self::Applications => "applications",
            Self::Connections => "connections",
        }
    }
}

#[derive(Clone)]
pub struct Shell {
    pub root: gtk::Box,
    pub status_row: adw::ActionRow,
    pub primary: gtk::Button,
    pub stack: gtk::Stack,
}

impl Shell {
    pub fn new(texts: Texts) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_top(12);
        root.set_margin_bottom(12);
        root.set_margin_start(12);
        root.set_margin_end(12);
        let status_row = adw::ActionRow::builder().title(texts.ux.checking).build();
        status_row.set_title_lines(2);
        status_row.set_subtitle_lines(0);
        let primary = gtk::Button::with_label(texts.ux.refresh);
        primary.set_widget_name("routing-primary");
        crate::ui::form::wrap_button(&primary);
        primary.add_css_class("suggested-action");
        primary.set_valign(gtk::Align::Center);
        let status_layout = gtk::Box::new(gtk::Orientation::Vertical, 6);
        primary.set_halign(gtk::Align::End);
        let status_list = gtk::ListBox::new();
        status_list.set_selection_mode(gtk::SelectionMode::None);
        status_list.add_css_class("boxed-list");
        status_row.set_activatable(false);
        status_row.set_selectable(false);
        status_row.set_focusable(false);
        status_list.append(&status_row);
        status_layout.append(&status_list);
        status_layout.append(&primary);
        root.append(&status_layout);
        let stack = gtk::Stack::builder()
            .vexpand(true)
            .hexpand(true)
            .vhomogeneous(false)
            .hhomogeneous(false)
            .build();
        stack.set_widget_name("sections");
        let nav = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let mut first: Option<gtk::ToggleButton> = None;
        for (section, label, icon) in [
            (Section::Overview, texts.ux.overview, "view-grid-symbolic"),
            (Section::Rules, texts.ux.rules, "view-list-symbolic"),
            (
                Section::Applications,
                texts.ux.applications,
                "application-x-executable-symbolic",
            ),
            (
                Section::Connections,
                texts.ux.connections,
                "network-wired-symbolic",
            ),
        ] {
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            content.append(&gtk::Image::from_icon_name(icon));
            content.append(&gtk::Label::new(Some(label)));
            let button = gtk::ToggleButton::builder()
                .child(&content)
                .tooltip_text(label)
                .build();
            button.set_widget_name(section.name());
            if let Some(first) = &first {
                button.set_group(Some(first));
            } else {
                first = Some(button.clone());
            }
            if section == Section::Overview {
                button.set_active(true);
            }
            let pages = stack.clone();
            button.connect_clicked(move |_| pages.set_visible_child_name(section.name()));
            let button_update = button.downgrade();
            stack.connect_visible_child_name_notify(move |stack| {
                if let Some(button) = button_update.upgrade() {
                    button
                        .set_active(stack.visible_child_name().as_deref() == Some(section.name()));
                }
            });
            nav.append(&button);
        }
        let navigation = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&nav)
            .build();
        root.append(&navigation);
        root.append(&stack);
        Self {
            root,
            status_row,
            primary,
            stack,
        }
    }

    pub fn add_page(&self, section: Section, child: &impl IsA<gtk::Widget>) {
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(child)
            .build();
        self.stack.add_named(&scroll, Some(section.name()));
    }
    pub fn select(&self, section: Section) {
        self.stack.set_visible_child_name(section.name());
    }
    pub fn section(&self) -> Section {
        match self.stack.visible_child_name().as_deref() {
            Some("rules") => Section::Rules,
            Some("applications") => Section::Applications,
            Some("connections") => Section::Connections,
            _ => Section::Overview,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ui::i18n::LanguageTexts;
    use routelane::config::Language;

    pub fn assert_four_sections() {
        let shell = Shell::new(Language::Tr.texts());
        for section in [
            Section::Overview,
            Section::Rules,
            Section::Applications,
            Section::Connections,
        ] {
            shell.add_page(section, &gtk::Box::new(gtk::Orientation::Vertical, 0));
        }
        assert_eq!(shell.section(), Section::Overview);
        for section in [Section::Rules, Section::Applications, Section::Connections] {
            shell.select(section);
            assert_eq!(shell.section(), section);
        }
        assert_eq!(shell.stack.pages().n_items(), 4);
        assert_eq!(shell.primary.widget_name(), "routing-primary");
    }
}
