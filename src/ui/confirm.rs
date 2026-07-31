use std::rc::Rc;

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
    let delete = gtk::Button::with_label(texts.delete);
    delete.add_css_class("destructive-action");
    button_row.append(&cancel);
    button_row.append(&delete);

    content.append(&body_label);
    content.append(&button_row);
    dialog.set_content(Some(&content));
    dialog.set_default_widget(Some(&cancel));

    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| dialog.close()
    });
    delete.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            on_confirm();
            dialog.close();
        }
    });

    dialog.present();
    cancel.grab_focus();
}
