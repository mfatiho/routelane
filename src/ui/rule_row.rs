use crate::models::{RoutingRule, UiToEngine};
use crate::ui::i18n::Texts;
use async_channel::Sender;
use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};

/// Bir yönlendirme kuralı için AdwActionRow oluşturur.
pub fn create_rule_row(
    rule: &RoutingRule,
    sender: Sender<UiToEngine>,
    texts: Texts,
) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(rule.target.display());
    let subtitle = if rule.target.is_domain() && rule.resolved_ips.is_empty() {
        texts.resolving.to_owned()
    } else {
        rule.subtitle()
    };
    row.set_subtitle(&subtitle);

    // Arayüz etiketi
    // gtk4 0.5'te css_classes builder argümanı Vec<String> alıyor;
    // add_css_class daha pratik.
    let iface_badge = gtk::Label::builder()
        .label(&rule.interface)
        .valign(gtk::Align::Center)
        .build();
    iface_badge.add_css_class("monospace");

    // Sil butonu
    let del_btn = gtk::Button::builder()
        .icon_name("list-remove-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text(texts.delete_rule_tooltip)
        .build();
    del_btn.add_css_class("flat");
    del_btn.add_css_class("circular");

    row.add_suffix(&iface_badge);
    row.add_suffix(&del_btn);

    let rule_id = rule.id;
    del_btn.connect_clicked(move |_| {
        let sender = sender.clone();
        glib::MainContext::default().spawn_local(async move {
            sender.send(UiToEngine::RemoveRule(rule_id)).await.ok();
        });
    });

    row
}
