use crate::ui::{i18n::Texts, shell::Section};
use gtk4::{self as gtk, prelude::*};
use libadwaita as adw;
use routelane::domain::RuleId;

#[derive(Clone)]
pub struct ListViewState {
    pub query: String,
    pub scroll: f64,
    pub focused_rule: Option<RuleId>,
    control: Option<String>,
    neighbors: Vec<RuleId>,
}

fn descendants(widget: &gtk::Widget, action: &mut impl FnMut(&gtk::Widget)) {
    action(widget);
    let mut child = widget.first_child();
    while let Some(current) = child {
        descendants(&current, action);
        child = current.next_sibling();
    }
}

impl ListViewState {
    pub fn capture(root: &gtk::Widget) -> Self {
        let mut query = String::new();
        let mut focused_rule = None;
        let mut control = None;
        let mut order = vec![];
        let focus = root
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok())
            .and_then(|window| gtk::prelude::GtkWindowExt::focus(&window));
        descendants(root, &mut |widget| {
            if let Some(search) = widget.downcast_ref::<gtk::SearchEntry>() {
                query = search.text().to_string();
            }
            if let Some(id) = widget
                .widget_name()
                .strip_prefix("rule-")
                .filter(|_| widget.is::<adw::ActionRow>())
            {
                let id = RuleId::from(id);
                order.push(id.clone());
                if focus
                    .as_ref()
                    .is_some_and(|focus| focus == widget || focus.is_ancestor(widget))
                {
                    focused_rule = Some(id);
                    control = focus.as_ref().map(|focus| focus.widget_name().to_string());
                }
            }
        });
        let neighbors = focused_rule
            .as_ref()
            .and_then(|id| order.iter().position(|other| other == id))
            .map(|index| {
                order[index..]
                    .iter()
                    .chain(order[..index].iter().rev())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let scroll = root
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_then(|w| w.downcast::<gtk::ScrolledWindow>().ok())
            .map(|w| w.vadjustment().value())
            .unwrap_or(0.0);
        Self {
            query,
            scroll,
            focused_rule,
            control,
            neighbors,
        }
    }
}

#[derive(Clone)]
pub struct ListPage {
    pub root: gtk::Box,
    pub group: adw::PreferencesGroup,
    pub add: gtk::Button,
    pub search: gtk::SearchEntry,
    pub feedback: gtk::Label,
    empty: gtk::Label,
    texts: Texts,
    section: Section,
}

impl ListPage {
    pub fn remember_view(&self) -> ListViewState {
        ListViewState::capture(self.root.upcast_ref())
    }

    pub fn restore_view(&self, state: &ListViewState) {
        if self.search.text().as_str() != state.query {
            self.search.set_text(&state.query);
        }
        if state.focused_rule.is_some() {
            let mut target = None;
            for id in &state.neighbors {
                descendants(self.root.upcast_ref(), &mut |widget| {
                    if widget.is::<adw::ActionRow>()
                        && widget.widget_name().as_str() == format!("rule-{}", id.as_str())
                    {
                        descendants(widget, &mut |child| {
                            if target.is_none()
                                && child.widget_name().as_str()
                                    == state.control.as_deref().unwrap_or("rule-enable")
                            {
                                target = Some(child.clone());
                            }
                        });
                    }
                });
                if target.is_some() {
                    break;
                }
            }
            target
                .unwrap_or_else(|| self.add.clone().upcast())
                .grab_focus();
        }
        if let Some(scroll) = self
            .root
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_then(|w| w.downcast::<gtk::ScrolledWindow>().ok())
        {
            let adjustment = scroll.vadjustment();
            let value = state.scroll;
            glib::idle_add_local_once(move || {
                adjustment
                    .set_value(value.min((adjustment.upper() - adjustment.page_size()).max(0.0)))
            });
        }
    }
    pub fn new(texts: Texts, section: Section) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_top(12);
        root.set_margin_bottom(24);
        root.set_margin_start(12);
        root.set_margin_end(12);
        let add = gtk::Button::with_label(match section {
            Section::Applications => texts.add_application,
            Section::Connections => texts.ux.add_connection,
            Section::Events => texts.ux.events,
            _ => texts.add_rule,
        });
        add.set_visible(section != Section::Events);
        add.set_halign(gtk::Align::Start);
        crate::ui::form::wrap_button(&add);
        add.add_css_class("suggested-action");
        let search = gtk::SearchEntry::builder()
            .placeholder_text(texts.ux.search)
            .hexpand(true)
            .build();
        search.set_widget_name(&format!("{}-search", section.name()));
        search.set_visible(matches!(section, Section::Rules | Section::Applications));
        root.append(&add);
        root.append(&search);
        let feedback = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .build();
        feedback.add_css_class("error");
        root.append(&feedback);
        let empty = gtk::Label::builder().wrap(true).xalign(0.0).build();
        empty.add_css_class("dim-label");
        root.append(&empty);
        let group = adw::PreferencesGroup::new();
        root.append(&group);
        Self {
            root,
            group,
            add,
            search,
            feedback,
            empty,
            texts,
            section,
        }
    }
    pub fn set_empty(&self, no_items: bool, no_matches: bool) {
        self.empty.set_visible(no_items || no_matches);
        self.empty.set_label(if no_matches && !no_items {
            self.texts.ux.no_matches
        } else {
            match self.section {
                Section::Applications => self.texts.ux.no_apps,
                Section::Connections => self.texts.ux.no_connections,
                Section::Events => self.texts.ux.events,
                _ => self.texts.ux.no_rules,
            }
        });
    }
}
