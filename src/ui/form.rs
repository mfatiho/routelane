use crate::ui::i18n::Texts;
use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};
use std::{cell::Cell, rc::Rc};

#[derive(Clone)]
pub struct Form {
    pub dialog: adw::Window,
    pub fields: gtk::Box,
    pub submit: gtk::Button,
    pub feedback: gtk::Label,
    submitting: Rc<Cell<bool>>,
}

impl Form {
    pub fn new(parent: &adw::ApplicationWindow, title: &str, texts: Texts) -> Self {
        let dialog = adw::Window::builder()
            .title(title)
            .transient_for(parent)
            .modal(true)
            .default_width(480)
            .build();
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        let fields = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let feedback = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .build();
        feedback.add_css_class("error");
        feedback.set_widget_name("form-feedback");
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label(texts.cancel);
        let submit = gtk::Button::with_label(texts.ux.save);
        submit.add_css_class("suggested-action");
        buttons.append(&cancel);
        buttons.append(&submit);
        body.append(&fields);
        body.append(&feedback);
        body.append(&buttons);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&body)
            .build();
        dialog.set_content(Some(&scroll));
        dialog.set_default_widget(Some(&submit));
        let close = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(close) = close.upgrade() {
                close.close();
            }
        });
        let keys = gtk::EventControllerKey::new();
        let close = dialog.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(close) = close.upgrade() {
                    close.close();
                }
                glib::signal::Inhibit(true)
            } else {
                glib::signal::Inhibit(false)
            }
        });
        dialog.add_controller(&keys);
        let previous = gtk::prelude::GtkWindowExt::focus(parent).map(|widget| widget.downgrade());
        dialog.connect_close_request(move |_| {
            if let Some(widget) = previous.as_ref().and_then(|widget| widget.upgrade()) {
                if widget.is_sensitive() {
                    widget.grab_focus();
                }
            }
            glib::signal::Inhibit(false)
        });
        Self {
            dialog,
            fields,
            submit,
            feedback,
            submitting: Rc::new(Cell::new(false)),
        }
    }
    pub fn field(&self, label: &str, widget: &impl IsA<gtk::Widget>) {
        let caption = gtk::Label::builder()
            .label(label)
            .xalign(0.0)
            .wrap(true)
            .build();
        caption.set_mnemonic_widget(Some(widget));
        widget.upcast_ref::<gtk::Widget>().update_relation(&[
            gtk::accessible::Relation::LabelledBy(&[caption.upcast_ref()]),
        ]);
        self.fields.append(&caption);
        self.fields.append(widget);
    }
    pub fn begin_submit(&self) -> bool {
        if !self.dialog.is_visible() || !self.submit.is_sensitive() || self.submitting.replace(true)
        {
            return false;
        }
        self.submit.set_sensitive(false);
        true
    }
    pub fn disconnect_on_close(&self, handler: glib::SignalHandlerId) {
        let button = self.submit.downgrade();
        let handler = std::cell::RefCell::new(Some(handler));
        self.dialog.connect_close_request(move |_| {
            if let (Some(button), Some(handler)) = (button.upgrade(), handler.borrow_mut().take()) {
                button.disconnect(handler);
            }
            glib::signal::Inhibit(false)
        });
    }
    pub fn error(&self, message: &str, focus: &impl IsA<gtk::Widget>) {
        self.submitting.set(false);
        self.submit.set_sensitive(true);
        self.feedback.set_label(message);
        self.feedback.set_visible(true);
        focus.grab_focus();
    }
}

pub type MenuAction = (&'static str, bool, Rc<dyn Fn()>);

pub fn action_menu(texts: Texts, actions: Vec<MenuAction>) -> gtk::MenuButton {
    let menu = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text(texts.ux.more)
        .valign(gtk::Align::Center)
        .build();
    let popover = gtk::Popover::new();
    menu.update_property(&[gtk::accessible::Property::Label(texts.ux.more)]);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    for (label, enabled, action) in actions {
        let button = gtk::Button::with_label(label);
        button.set_sensitive(enabled);
        button.add_css_class("flat");
        let weak = popover.downgrade();
        button.connect_clicked(move |_| {
            if let Some(popover) = weak.upgrade() {
                popover.popdown();
            }
            action();
        });
        content.append(&button);
    }
    popover.set_child(Some(&content));
    menu.set_popover(Some(&popover));
    menu
}

pub fn menu_action(menu: &gtk::MenuButton, label: &str) -> Option<gtk::Button> {
    let content = menu.popover()?.child()?;
    let mut child = content.first_child();
    while let Some(widget) = child {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.label().as_deref() == Some(label) {
                return Some(button.clone());
            }
        }
        child = widget.next_sibling();
    }
    None
}

pub fn wrap_button(button: &gtk::Button) {
    if let Some(label) = button
        .child()
        .and_then(|child| child.downcast::<gtk::Label>().ok())
    {
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_max_width_chars(24);
    }
}
