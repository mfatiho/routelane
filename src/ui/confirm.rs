use std::{cell::Cell, rc::Rc};

use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};

use crate::ui::i18n::Texts;

pub fn confirm_destructive_action(
    parent: &impl IsA<gtk::Window>,
    texts: Texts,
    heading: &str,
    body: &str,
    on_confirm: Rc<dyn Fn()>,
) {
    confirm_destructive_action_with_label(
        parent,
        texts,
        heading,
        body,
        texts.delete,
        on_confirm,
        Rc::new(|| {}),
    );
}

pub fn confirm_destructive_action_with_label(
    parent: &impl IsA<gtk::Window>,
    texts: Texts,
    heading: &str,
    body: &str,
    confirm_label: &str,
    on_confirm: Rc<dyn Fn()>,
    on_cancel: Rc<dyn Fn()>,
) {
    let dialog = adw::Window::builder()
        .title(heading)
        .transient_for(parent)
        .modal(true)
        .default_width(360)
        .build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    let body_label = gtk::Label::builder()
        .label(body)
        .wrap(true)
        .xalign(0.0)
        .build();

    let button_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let cancel = gtk::Button::with_label(texts.cancel);
    let delete = gtk::Button::with_label(confirm_label);
    delete.add_css_class("destructive-action");
    button_row.append(&cancel);
    button_row.append(&delete);

    content.append(&body_label);
    content.append(&button_row);
    dialog.set_content(Some(&content));
    dialog.set_default_widget(Some(&cancel));

    let resolved = Rc::new(Cell::new(false));
    let previous = gtk::prelude::GtkWindowExt::focus(parent).map(|widget| widget.downgrade());
    dialog.connect_close_request({
        let resolved = resolved.clone();
        move |_| {
            if !resolved.replace(true) {
                on_cancel();
            }
            if let Some(widget) = previous.as_ref().and_then(|widget| widget.upgrade()) {
                if widget.is_sensitive() {
                    widget.grab_focus();
                }
            }
            glib::signal::Inhibit(false)
        }
    });
    cancel.connect_clicked({
        let dialog = dialog.downgrade();
        move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        }
    });
    delete.connect_clicked({
        let dialog = dialog.downgrade();
        move |_| {
            if !resolved.replace(true) {
                on_confirm();
            }
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        }
    });
    let keys = gtk::EventControllerKey::new();
    let close = dialog.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            if let Some(dialog) = close.upgrade() {
                dialog.close();
            }
            glib::signal::Inhibit(true)
        } else {
            glib::signal::Inhibit(false)
        }
    });
    dialog.add_controller(&keys);

    dialog.present();
    cancel.grab_focus();
}
