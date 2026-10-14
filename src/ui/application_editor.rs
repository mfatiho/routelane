use crate::ui::{
    desktop_catalog::{load_catalog, DesktopCatalog},
    form::Form,
    i18n::Texts,
};
use gtk4::{self as gtk, prelude::*};
use libadwaita as adw;
use routelane::domain::{
    ApplicationSelector, LaneId, Profile, Rule, RuleAction, RuleId, RuleSource, RuleTarget,
};
use std::{cell::RefCell, rc::Rc};

pub fn with_application_lane(rule: &Rule, lane: LaneId) -> Rule {
    let mut candidate = rule.clone();
    candidate.action = RuleAction::Route(lane);
    candidate
}

pub fn validate_application_choice(
    profile: &Profile,
    editing: Option<&RuleId>,
    selector: &ApplicationSelector,
    lane: &LaneId,
    catalog: &DesktopCatalog,
) -> bool {
    profile.lanes.iter().any(|candidate| &candidate.id == lane)
        && editing.is_none_or(|id| profile.rules.iter().any(|rule| &rule.id == id))
        && catalog.find(selector).is_some_and(|choice| choice.unsupported_reason.is_none())
        && !profile.rules.iter().any(|rule| Some(&rule.id) != editing && matches!(&rule.source, RuleSource::Application(other) if selector.desktop_id.is_some() && other.desktop_id == selector.desktop_id))
}

pub fn open_application_editor(
    parent: &adw::ApplicationWindow,
    texts: Texts,
    profile: Rc<RefCell<Profile>>,
    editing: Option<RuleId>,
    on_save: Rc<dyn Fn(Rule) -> bool>,
) {
    open_application_editor_with_loader(
        parent,
        texts,
        profile,
        editing,
        on_save,
        Rc::new(load_catalog),
    );
}

type CatalogLoader = Rc<dyn Fn(Box<dyn FnOnce(Result<DesktopCatalog, String>)>)>;

fn open_application_editor_with_loader(
    parent: &adw::ApplicationWindow,
    texts: Texts,
    profile: Rc<RefCell<Profile>>,
    editing: Option<RuleId>,
    on_save: Rc<dyn Fn(Rule) -> bool>,
    loader: CatalogLoader,
) {
    let original = editing.as_ref().and_then(|id| {
        profile
            .borrow()
            .rules
            .iter()
            .find(|rule| &rule.id == id)
            .cloned()
    });
    let selected = Rc::new(RefCell::new(original.as_ref().and_then(
        |rule| match &rule.source {
            RuleSource::Application(selector) => Some(selector.clone()),
            _ => None,
        },
    )));
    let form = Form::new(
        parent,
        if editing.is_some() {
            texts.ux.edit
        } else {
            texts.add_application
        },
        texts,
    );
    form.submit.set_sensitive(false);
    let search = gtk::SearchEntry::builder()
        .placeholder_text(texts.ux.search)
        .build();
    search.set_widget_name("program-search");
    let programs = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    programs.set_widget_name("program-list");
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(160)
        .max_content_height(240)
        .propagate_natural_height(true)
        .child(&programs)
        .build();
    let selection_label = gtk::Label::builder()
        .label(texts.ux.loading_apps)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build();
    selection_label.set_widget_name("program-selection");
    let lanes = profile.borrow().lanes.clone();
    let names = lanes
        .iter()
        .map(|lane| lane.name.as_str())
        .collect::<Vec<_>>();
    let connection = gtk::DropDown::from_strings(&names);
    connection.set_widget_name("program-connection");
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
    form.field(texts.ux.program, &search);
    form.fields.append(&scroll);
    form.fields.append(&selection_label);
    form.field(texts.ux.connection, &connection);
    form.fields.append(
        &gtk::Label::builder()
            .label(texts.ux.scope_warning)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .build(),
    );
    let catalog = Rc::new(RefCell::new(DesktopCatalog::default()));
    let selector_choice = selected.clone();
    let choice_catalog = catalog.clone();
    let label = selection_label.clone();
    programs.connect_row_selected(move |_, row| {
        let Some(row) = row else {
            return;
        };
        let name = row.widget_name();
        if let Some(choice) = choice_catalog
            .borrow()
            .choices()
            .iter()
            .find(|choice| choice.app.desktop_id == name.as_str())
        {
            if selector_choice
                .borrow()
                .as_ref()
                .and_then(|selector| selector.desktop_id.as_deref())
                != Some(choice.app.desktop_id.as_str())
            {
                *selector_choice.borrow_mut() = Some(ApplicationSelector {
                    desktop_id: Some(choice.app.desktop_id.clone()),
                    executable_path: choice.app.executable_path.clone(),
                    include_children: true,
                    launch: Some(choice.app.launch.clone()),
                });
            }
            label.set_label(&format!(
                "{}: {}",
                texts.ux.program, choice.app.display_name
            ));
        }
    });
    let rows = programs.clone();
    let choices = catalog.clone();
    let matches = gtk::Label::builder()
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .visible(false)
        .label(texts.ux.no_matches)
        .build();
    form.fields.append(&matches);
    search.connect_search_changed(move |search| {
        let query = search.text().to_lowercase();
        let names = choices
            .borrow()
            .choices()
            .iter()
            .filter(|choice| choice.app.display_name.to_lowercase().contains(&query))
            .map(|choice| choice.app.desktop_id.clone())
            .collect::<Vec<_>>();
        matches.set_visible(names.is_empty());
        let mut child = rows.first_child();
        while let Some(row) = child {
            row.set_visible(names.iter().any(|name| name == row.widget_name().as_str()));
            child = row.next_sibling();
        }
    });
    let form_loaded = form.clone();
    let choices = catalog.clone();
    let list = programs.clone();
    let selected_loaded = selected.clone();
    loader(Box::new(move |result| {
        if !form_loaded.dialog.is_visible() {
            return;
        }
        match result {
            Ok(catalog) => {
                *choices.borrow_mut() = catalog;
                selection_label.set_label(if choices.borrow().choices().is_empty() {
                    texts.ux.no_apps
                } else {
                    texts.ux.program
                });
                let loaded = choices.borrow().choices().to_vec();
                for choice in &loaded {
                    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                    content.set_margin_top(6);
                    content.set_margin_bottom(6);
                    content.set_margin_start(6);
                    content.set_margin_end(6);
                    content.append(&gtk::Image::from_icon_name(
                        choice
                            .icon_name
                            .as_deref()
                            .unwrap_or("application-x-executable-symbolic"),
                    ));
                    content.append(
                        &gtk::Label::builder()
                            .label(&choice.app.display_name)
                            .xalign(0.0)
                            .wrap(true)
                            .wrap_mode(gtk::pango::WrapMode::WordChar)
                            .hexpand(true)
                            .build(),
                    );
                    let row = gtk::ListBoxRow::new();
                    row.set_widget_name(&choice.app.desktop_id);
                    row.set_child(Some(&content));
                    row.set_sensitive(choice.unsupported_reason.is_none());
                    row.set_tooltip_text(
                        choice
                            .unsupported_reason
                            .map(|_| texts.ux.unsupported_sandbox),
                    );
                    list.append(&row);
                    if selected_loaded
                        .borrow()
                        .as_ref()
                        .and_then(|s| s.desktop_id.as_ref())
                        == Some(&choice.app.desktop_id)
                    {
                        list.select_row(Some(&row));
                    }
                }
                form_loaded.submit.set_sensitive(true);
            }
            Err(error) => {
                log::warn!("desktop catalog: {error}");
                selection_label.set_label(texts.ux.catalog_failed);
            }
        }
    }));
    let submit_form = form.clone();
    let initial_focus = search.clone();
    let handler = form.submit.connect_clicked(move |_| {
        if !submit_form.begin_submit() {
            return;
        }
        let Some(selector) = selected.borrow().clone() else {
            submit_form.error(texts.ux.program, &search);
            return;
        };
        let Some(lane) = lanes
            .get(connection.selected() as usize)
            .map(|lane| lane.id.clone())
        else {
            submit_form.error(texts.ux.missing_interface, &connection);
            return;
        };
        let form = submit_form.clone();
        let profile = profile.clone();
        let editing = editing.clone();
        let original = original.clone();
        let on_save = on_save.clone();
        let search = search.clone();
        loader(Box::new(move |catalog| {
            if !form.dialog.is_visible() {
                return;
            }
            let current = profile.borrow().clone();
            if !catalog.as_ref().is_ok_and(|catalog| {
                validate_application_choice(&current, editing.as_ref(), &selector, &lane, catalog)
            }) {
                form.error(texts.ux.catalog_failed, &search);
                return;
            }
            let mut rule = original.unwrap_or_else(|| Rule {
                id: format!("app-{}", uuid::Uuid::new_v4()).as_str().into(),
                enabled: true,
                priority: current
                    .rules
                    .iter()
                    .map(|rule| rule.priority)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(10),
                source: RuleSource::Any,
                target: RuleTarget::Any,
                action: RuleAction::Direct,
            });
            rule.source = RuleSource::Application(selector);
            rule = with_application_lane(&rule, lane);
            if on_save(rule) {
                form.dialog.close();
            } else {
                form.error(texts.ux.request_failed, &search);
            }
        }));
    });
    form.disconnect_on_close(handler);
    form.dialog.present();
    initial_focus.grab_focus();
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn assert_picker_behavior(parent: &adw::ApplicationWindow, texts: Texts) {
        fn find(widget: &gtk::Widget, name: &str) -> gtk::Widget {
            fn walk(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
                if widget.widget_name().as_str() == name {
                    return Some(widget.clone());
                }
                let mut child = widget.first_child();
                while let Some(current) = child {
                    if let Some(found) = walk(&current, name) {
                        return Some(found);
                    }
                    child = current.next_sibling();
                }
                None
            }
            walk(widget, name).unwrap()
        }
        fn drain() {
            let context = glib::MainContext::default();
            while context.pending() {
                context.iteration(false);
            }
        }
        fn dialog(parent: &adw::ApplicationWindow) -> gtk::Window {
            gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Window>().ok())
                .find(|w| w.is_visible() && w.transient_for().as_ref() == Some(parent.upcast_ref()))
                .unwrap()
        }
        let apps = vec![
            routelane::application::DesktopApp::new(
                "routelane-test-first",
                "First Browser",
                "/opt/first".into(),
            ),
            routelane::application::DesktopApp::new(
                "routelane-test-second",
                "Second Browser",
                "/opt/second".into(),
            ),
        ];
        let catalog = Rc::new(RefCell::new(DesktopCatalog::from_apps(apps.clone())));
        let source = RuleSource::Application(ApplicationSelector {
            desktop_id: Some("routelane-test-first".into()),
            executable_path: "/opt/first".into(),
            include_children: false,
            launch: Some(routelane::domain::ApplicationLaunch {
                executable: "/opt/original-link".into(),
                arguments: vec!["--profile".into(), "Work Profile".into()],
            }),
        });
        let original = Rule {
            id: "app".into(),
            enabled: true,
            priority: 10,
            source: source.clone(),
            target: RuleTarget::Any,
            action: RuleAction::Route("vpn".into()),
        };
        let profile = Rc::new(RefCell::new(Profile {
            id: "work".into(),
            name: "Work".into(),
            enabled: false,
            lanes: vec![routelane::domain::Lane {
                id: "vpn".into(),
                name: "VPN".into(),
                interface: "tun0".into(),
                fallback_lane: None,
                failure_policy: routelane::domain::LaneFailurePolicy::Block,
            }],
            rules: vec![original.clone()],
        }));
        let current = catalog.clone();
        let loader: CatalogLoader = Rc::new(move |callback| {
            let result = current.borrow().clone();
            glib::idle_add_local_once(move || callback(Ok(result)));
        });
        let saved = Rc::new(RefCell::new(vec![]));
        let output = saved.clone();
        open_application_editor_with_loader(
            parent,
            texts,
            profile.clone(),
            Some("app".into()),
            Rc::new(move |rule| {
                output.borrow_mut().push(rule);
                true
            }),
            loader.clone(),
        );
        drain();
        let edit = dialog(parent);
        let label = find(edit.upcast_ref(), "program-selection")
            .downcast::<gtk::Label>()
            .unwrap();
        assert!(
            label.text().contains("First Browser"),
            "the original selection is visible after catalog load"
        );
        let search = find(edit.upcast_ref(), "program-search")
            .downcast::<gtk::SearchEntry>()
            .unwrap();
        search.set_text("Second");
        search.emit_by_name::<()>("search-changed", &[]);
        drain();
        assert!(
            label.text().contains("First Browser"),
            "filtering must not select a different program"
        );
        edit.default_widget()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        drain();
        assert_eq!(saved.borrow().len(), 1);
        assert_eq!(
            saved.borrow()[0].source,
            source,
            "a lane-only edit preserves original executable, arguments and include_children"
        );
        assert_eq!(
            *profile.borrow(),
            Profile {
                rules: vec![original.clone()],
                ..profile.borrow().clone()
            },
            "the editor only submits a draft"
        );

        let output = saved.clone();
        open_application_editor_with_loader(
            parent,
            texts,
            profile.clone(),
            Some("app".into()),
            Rc::new(move |rule| {
                output.borrow_mut().push(rule);
                true
            }),
            loader.clone(),
        );
        drain();
        let edit = dialog(parent);
        *catalog.borrow_mut() = DesktopCatalog::from_apps(vec![apps[1].clone()]);
        edit.default_widget()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        drain();
        assert_eq!(
            saved.borrow().len(),
            1,
            "removed application must fail save revalidation"
        );
        assert!(edit.is_visible());
        edit.close();
        drain();

        *catalog.borrow_mut() = DesktopCatalog::from_apps(apps);
        open_application_editor_with_loader(
            parent,
            texts,
            profile,
            None,
            Rc::new(|_| panic!("cancel cannot submit")),
            loader,
        );
        let edit = dialog(parent);
        let list = find(edit.upcast_ref(), "program-list");
        edit.close();
        drain();
        assert!(
            list.first_child().is_none(),
            "catalog completion after cancellation is ignored"
        );
    }
    #[test]
    fn selected_application_removed_before_save_is_rejected_and_duplicate_identity_is_blocked() {
        let catalog = DesktopCatalog::from_apps(vec![routelane::application::DesktopApp::new(
            "routelane-test-browser",
            "Browser",
            "/opt/browser".into(),
        )]);
        let selector = ApplicationSelector {
            desktop_id: Some("routelane-test-browser".into()),
            executable_path: "/opt/browser".into(),
            include_children: true,
            launch: None,
        };
        let mut p = Profile {
            id: "work".into(),
            name: "Work".into(),
            enabled: false,
            lanes: vec![routelane::domain::Lane {
                id: "vpn".into(),
                name: "VPN".into(),
                interface: "tun0".into(),
                fallback_lane: None,
                failure_policy: routelane::domain::LaneFailurePolicy::Block,
            }],
            rules: vec![],
        };
        assert!(validate_application_choice(
            &p,
            None,
            &selector,
            &"vpn".into(),
            &catalog
        ));
        assert!(!validate_application_choice(
            &p,
            None,
            &selector,
            &"vpn".into(),
            &DesktopCatalog::default()
        ));
        p.rules.push(Rule {
            id: "browser".into(),
            enabled: true,
            priority: 10,
            source: RuleSource::Application(selector.clone()),
            target: RuleTarget::Any,
            action: RuleAction::Route("vpn".into()),
        });
        assert!(!validate_application_choice(
            &p,
            None,
            &selector,
            &"vpn".into(),
            &catalog
        ));
        assert!(validate_application_choice(
            &p,
            Some(&"browser".into()),
            &selector,
            &"vpn".into(),
            &catalog
        ));
        p.rules.clear();
        assert!(!validate_application_choice(
            &p,
            Some(&"browser".into()),
            &selector,
            &"vpn".into(),
            &catalog
        ));
    }
    #[test]
    fn lane_only_edit_preserves_original_launcher_and_arguments() {
        let original = Rule {
            id: "browser".into(),
            enabled: true,
            priority: 10,
            source: RuleSource::Application(ApplicationSelector {
                desktop_id: Some("browser".into()),
                executable_path: "/opt/browser-real".into(),
                include_children: true,
                launch: Some(routelane::domain::ApplicationLaunch {
                    executable: "/opt/browser-link".into(),
                    arguments: vec!["--profile".into(), "Work Profile".into()],
                }),
            }),
            target: RuleTarget::Any,
            action: RuleAction::Route("wifi".into()),
        };
        let changed = with_application_lane(&original, "vinn".into());
        assert_eq!(changed.source, original.source);
        assert_eq!(changed.id, original.id);
        assert_eq!(changed.action, RuleAction::Route("vinn".into()));
    }
}
