use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};

use crate::ui::i18n::Texts;

/// Settings diyaloğundan window.rs'e dönen widget referansları.
pub struct SettingsWidgets {
    pub dialog: adw::PreferencesWindow,
    pub iface_group: adw::PreferencesGroup,
    pub default_info_row: adw::ActionRow,
    pub alt_combo: adw::ComboRow,
    pub language_group: adw::PreferencesGroup,
    pub language_combo: adw::ComboRow,
}

pub fn build_settings_dialog(
    parent: &adw::ApplicationWindow,
    texts: Texts,
    initial_language_index: u32,
) -> SettingsWidgets {
    let dialog = adw::PreferencesWindow::builder()
        .title(texts.settings_title)
        .transient_for(parent)
        .modal(true)
        .hide_on_close(true)
        .build();

    let page = adw::PreferencesPage::new();

    // ── Ağ Yapılandırması ─────────────────────────────────────────────────────
    let iface_group = adw::PreferencesGroup::builder()
        .title(texts.network_title)
        .description(texts.network_description)
        .build();

    let default_info_row = adw::ActionRow::new();
    default_info_row.set_title(texts.default_connection_title);
    default_info_row.set_subtitle(texts.detecting);
    default_info_row.set_sensitive(false);

    let alt_combo = adw::ComboRow::new();
    alt_combo.set_title(texts.exception_network_title);
    alt_combo.set_subtitle(texts.interfaces_loading);
    alt_combo.set_sensitive(false);
    let iface_expression = gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        None::<&gtk::Expression>,
        "string",
    );
    alt_combo.set_expression(Some(&iface_expression));

    iface_group.add(&default_info_row);
    iface_group.add(&alt_combo);

    // ── Dil ──────────────────────────────────────────────────────────────────
    let language_group = adw::PreferencesGroup::builder()
        .title(texts.language_title)
        .description(texts.language_description)
        .build();

    let language_combo = adw::ComboRow::new();
    language_combo.set_title(texts.language_row_title);
    language_combo.set_subtitle(texts.language_row_subtitle);
    let language_model = gtk::StringList::new(&["Türkçe", "English"]);
    language_combo.set_model(Some(&language_model));
    language_combo.set_selected(initial_language_index);
    language_group.add(&language_combo);

    page.add(&iface_group);
    page.add(&language_group);
    dialog.add(&page);

    SettingsWidgets {
        dialog,
        iface_group,
        default_info_row,
        alt_combo,
        language_group,
        language_combo,
    }
}
