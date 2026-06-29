use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::{atomic::Ordering, Arc, Mutex};

use async_channel::{Receiver, Sender};
use glib::signal::Inhibit;
use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};

use crate::config::{Language, PersistedState};
use crate::models::{EngineToUi, NetworkInterface, RuleTarget, UiToEngine};
use crate::ui::rule_row::create_rule_row;
use crate::ui::settings::build_settings_dialog;
use crate::ui::tray::{spawn_tray, TrayCommand, APP_ICON_NAME};

const TOAST_TIMEOUT_SHORT: u32 = 4;
const TOAST_TIMEOUT_LONG: u32 = 7;

pub fn build_window(
    app: &adw::Application,
    ui_sender: Sender<UiToEngine>,
    engine_receiver: Receiver<EngineToUi>,
) {
    gtk::Window::set_default_icon_name(APP_ICON_NAME);
    let initial_language = PersistedState::load().language;
    let current_language = Rc::new(RefCell::new(initial_language));
    let tray_language = Arc::new(Mutex::new(initial_language));
    let texts = initial_language.texts();

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("RouteLane")
        .icon_name(APP_ICON_NAME)
        .default_width(480)
        .default_height(640)
        .build();

    let (tray_sender, tray_receiver) =
        glib::MainContext::channel::<TrayCommand>(glib::PRIORITY_DEFAULT);
    let tray_available = spawn_tray(tray_sender, tray_language.clone());

    {
        let window = window.clone();
        let app = app.clone();
        tray_receiver.attach(None, move |command| {
            match command {
                TrayCommand::ShowWindow => {
                    window.present();
                }
                TrayCommand::Quit => {
                    app.quit();
                }
            }
            glib::Continue(true)
        });
    }

    {
        let window = window.clone();
        let tray_available = tray_available.clone();
        window.connect_close_request(move |window| {
            if tray_available.load(Ordering::Relaxed) {
                window.hide();
                Inhibit(true)
            } else {
                Inhibit(false)
            }
        });
    }

    // ── HeaderBar ─────────────────────────────────────────────────────────────
    let header = adw::HeaderBar::new();

    let window_title = adw::WindowTitle::builder()
        .title("RouteLane")
        .subtitle(texts.inactive)
        .build();
    header.set_title_widget(Some(&window_title));

    // Servis switch + etiket
    let routing_label = gtk::Label::builder()
        .label(texts.routing_label)
        .valign(gtk::Align::Center)
        .build();
    let service_switch = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .tooltip_text(texts.service_tooltip)
        .build();
    let switch_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .valign(gtk::Align::Center)
        .build();
    switch_box.append(&routing_label);
    switch_box.append(&service_switch);

    // Tercihler butonu
    let prefs_btn = gtk::Button::builder()
        .icon_name("preferences-system-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text(texts.settings_title)
        .build();
    prefs_btn.add_css_class("flat");

    // Menü butonu (⋮) — popover içinde Reset
    let reset_popover_btn = gtk::Button::builder().label(texts.menu_reset).build();
    reset_popover_btn.add_css_class("destructive-action");
    reset_popover_btn.add_css_class("flat");

    let reset_popover_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    reset_popover_box.append(&reset_popover_btn);

    let reset_popover = gtk::Popover::new();
    reset_popover.set_child(Some(&reset_popover_box));

    let menu_btn = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .valign(gtk::Align::Center)
        .popover(&reset_popover)
        .build();

    header.pack_end(&menu_btn);
    header.pack_end(&prefs_btn);
    header.pack_end(&switch_box);

    // ── Toast overlay + içerik ────────────────────────────────────────────────
    let toast_overlay = adw::ToastOverlay::new();

    let prefs_page = adw::PreferencesPage::new();
    toast_overlay.set_child(Some(&prefs_page));

    // ── Settings diyaloğu ────────────────────────────────────────────────────
    let sw = build_settings_dialog(&window, texts, initial_language.index());
    let default_info_row = sw.default_info_row.clone();
    let alt_combo = sw.alt_combo.clone();
    let language_combo = sw.language_combo.clone();
    let iface_group = sw.iface_group.clone();
    let language_group = sw.language_group.clone();

    // ── Yönlendirme Kuralları ─────────────────────────────────────────────────
    let rules_group = adw::PreferencesGroup::builder()
        .title(texts.rules_title)
        .description(texts.rules_description)
        .build();

    // Kural ekleme satırı
    let add_row = adw::ActionRow::builder()
        .title(texts.add_address_title)
        .build();

    let target_entry = gtk::Entry::builder()
        .placeholder_text(texts.address_placeholder)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();

    let add_btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text(texts.add_rule_tooltip)
        .build();
    add_btn.add_css_class("flat");
    add_btn.add_css_class("circular");

    add_row.add_suffix(&target_entry);
    add_row.add_suffix(&add_btn);
    add_row.set_activatable_widget(Some(&add_btn));
    rules_group.add(&add_row);

    // Boş durum satırı — kural yokken görünür
    let empty_icon = gtk::Image::from_icon_name("list-add-symbolic");
    empty_icon.set_pixel_size(32);
    empty_icon.set_opacity(0.35);

    let empty_row = adw::ActionRow::new();
    empty_row.set_title(texts.no_rules_title);
    empty_row.set_subtitle(texts.no_rules_subtitle);
    empty_row.set_sensitive(false);
    empty_row.add_prefix(&empty_icon);
    rules_group.add(&empty_row);

    prefs_page.add(&rules_group);

    // ── Rota Testi ───────────────────────────────────────────────────────────
    let test_group = adw::PreferencesGroup::builder()
        .title(texts.route_test_title)
        .description(texts.route_test_description)
        .build();

    let test_row = adw::ActionRow::builder()
        .title(texts.test_address_title)
        .build();

    let test_entry = gtk::Entry::builder()
        .placeholder_text(texts.test_placeholder)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();

    let test_btn = gtk::Button::builder()
        .icon_name("system-search-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text(texts.route_test_tooltip)
        .build();
    test_btn.add_css_class("flat");
    test_btn.add_css_class("circular");

    test_row.add_suffix(&test_entry);
    test_row.add_suffix(&test_btn);
    test_row.set_activatable_widget(Some(&test_btn));
    test_group.add(&test_row);

    let test_status_icon = gtk::Image::builder()
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    let test_network_row = adw::ActionRow::new();
    test_network_row.set_title(texts.used_network);
    test_network_row.set_subtitle("-");
    test_network_row.set_sensitive(false);
    test_network_row.add_suffix(&test_status_icon);
    test_group.add(&test_network_row);

    let test_expected_row = adw::ActionRow::new();
    test_expected_row.set_title(texts.expected_network);
    test_expected_row.set_subtitle("-");
    test_expected_row.set_sensitive(false);
    test_group.add(&test_expected_row);

    let test_probes_row = adw::ActionRow::new();
    test_probes_row.set_title(texts.probe_samples);
    test_probes_row.set_subtitle("-");
    test_probes_row.set_sensitive(false);
    test_group.add(&test_probes_row);

    prefs_page.add(&test_group);

    // ── Pencere düzeni ────────────────────────────────────────────────────────
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.append(&header);
    vbox.append(&toast_overlay);
    window.set_content(Some(&vbox));

    // ── Paylaşılan state ──────────────────────────────────────────────────────
    let rule_rows: Rc<RefCell<HashMap<u64, adw::ActionRow>>> =
        Rc::new(RefCell::new(HashMap::new()));

    let iface_model: Rc<RefCell<Vec<NetworkInterface>>> = Rc::new(RefCell::new(Vec::new()));

    // Kaç kural var — boş durum satırını göster/gizle için
    let rule_count: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));

    let test_has_result: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));

    // ── Sinyaller ─────────────────────────────────────────────────────────────

    // Tercihler butonu
    {
        let settings_dialog = sw.dialog.clone();
        let sender = ui_sender.clone();
        prefs_btn.connect_clicked(move |_| {
            let sender = sender.clone();
            glib::MainContext::default().spawn_local(async move {
                sender.send(UiToEngine::RefreshInterfaces).await.ok();
            });
            settings_dialog.present();
        });
    }

    // Dil seçimi
    {
        let current_language = current_language.clone();
        let tray_language = tray_language.clone();
        let window_title = window_title.clone();
        let service_switch = service_switch.clone();
        let rules_group = rules_group.clone();
        let add_row = add_row.clone();
        let target_entry = target_entry.clone();
        let add_btn = add_btn.clone();
        let empty_row = empty_row.clone();
        let test_group = test_group.clone();
        let test_row = test_row.clone();
        let test_entry = test_entry.clone();
        let test_btn = test_btn.clone();
        let test_network_row = test_network_row.clone();
        let test_expected_row = test_expected_row.clone();
        let test_probes_row = test_probes_row.clone();
        let test_has_result = test_has_result.clone();
        let iface_group = iface_group.clone();
        let language_group = language_group.clone();
        let iface_model = iface_model.clone();
        let default_info_row = default_info_row.clone();
        let alt_combo = alt_combo.clone();
        let language_combo_widget = language_combo.clone();

        language_combo.connect_selected_notify(move |combo| {
            let language = Language::from_index(combo.selected());
            if *current_language.borrow() == language {
                return;
            }

            *current_language.borrow_mut() = language;
            if let Ok(mut tray_language) = tray_language.lock() {
                *tray_language = language;
            }
            if let Err(err) = PersistedState::save_language(language) {
                log::warn!("Dil ayarı kaydedilemedi: {}", err);
            }

            let texts = language.texts();
            if !service_switch.is_active() {
                window_title.set_subtitle(texts.inactive);
            }
            service_switch.set_tooltip_text(Some(texts.service_tooltip));

            rules_group.set_title(texts.rules_title);
            rules_group.set_description(Some(texts.rules_description));
            add_row.set_title(texts.add_address_title);
            target_entry.set_placeholder_text(Some(texts.address_placeholder));
            add_btn.set_tooltip_text(Some(texts.add_rule_tooltip));
            empty_row.set_title(texts.no_rules_title);
            empty_row.set_subtitle(texts.no_rules_subtitle);

            test_group.set_title(texts.route_test_title);
            test_group.set_description(Some(texts.route_test_description));
            test_row.set_title(texts.test_address_title);
            test_entry.set_placeholder_text(Some(texts.test_placeholder));
            test_btn.set_tooltip_text(Some(texts.route_test_tooltip));
            test_network_row.set_title(texts.used_network);
            test_expected_row.set_title(texts.expected_network);
            test_probes_row.set_title(texts.probe_samples);
            if !*test_has_result.borrow() {
                test_network_row.set_subtitle("-");
                test_expected_row.set_subtitle("-");
                test_probes_row.set_subtitle("-");
            }

            // Settings diyaloğu widget'larını güncelle
            iface_group.set_title(texts.network_title);
            iface_group.set_description(Some(texts.network_description));
            default_info_row.set_title(texts.default_connection_title);
            let ifaces = iface_model.borrow();
            if let Some(def) = ifaces.iter().find(|i| i.is_default) {
                let gw = def.gateway.as_deref().unwrap_or("?");
                default_info_row.set_subtitle(&language.default_gateway_subtitle(&def.name, gw));
            } else if ifaces.is_empty() {
                default_info_row.set_subtitle(texts.detecting);
            } else {
                default_info_row.set_subtitle(texts.default_not_found);
            }
            alt_combo.set_title(texts.exception_network_title);
            if ifaces.is_empty() {
                alt_combo.set_subtitle(texts.no_interfaces);
            } else if alt_combo.selected() == gtk::INVALID_LIST_POSITION {
                alt_combo.set_subtitle(texts.exception_select_prompt);
            } else {
                alt_combo.set_subtitle(texts.exception_subtitle);
            }
            drop(ifaces);
            language_group.set_title(texts.language_title);
            language_group.set_description(Some(texts.language_description));
            language_combo_widget.set_title(texts.language_row_title);
            language_combo_widget.set_subtitle(texts.language_row_subtitle);
        });
    }

    // Sıfırla butonu — onay diyaloğu ile
    {
        let sender = ui_sender.clone();
        let window = window.clone();
        let current_language = current_language.clone();
        let reset_popover = reset_popover.clone();
        reset_popover_btn.connect_clicked(move |_| {
            reset_popover.popdown();
            let texts = current_language.borrow().texts();
            let dialog = gtk::MessageDialog::builder()
                .transient_for(&window)
                .modal(true)
                .message_type(gtk::MessageType::Warning)
                .text(texts.reset_dialog_heading)
                .secondary_text(texts.reset_dialog_body)
                .build();
            dialog.add_button(texts.reset_dialog_cancel, gtk::ResponseType::Cancel);
            let confirm_widget =
                dialog.add_button(texts.reset_dialog_confirm, gtk::ResponseType::Accept);
            if let Some(btn) = confirm_widget.downcast_ref::<gtk::Button>() {
                btn.add_css_class("destructive-action");
            }
            let sender = sender.clone();
            dialog.connect_response(move |dlg, response| {
                dlg.close();
                if response == gtk::ResponseType::Accept {
                    let sender = sender.clone();
                    glib::MainContext::default().spawn_local(async move {
                        sender.send(UiToEngine::Reset).await.ok();
                    });
                }
            });
            dialog.present();
        });
    }

    // Servis switch'i
    {
        let sender = ui_sender.clone();
        service_switch.connect_state_set(move |_sw, active| {
            let sender = sender.clone();
            glib::MainContext::default().spawn_local(async move {
                sender.send(UiToEngine::SetActive(active)).await.ok();
            });
            Inhibit(false)
        });
    }

    // İstisna ağı seçildiğinde engine'e bildir
    {
        let sender = ui_sender.clone();
        let iface_model = iface_model.clone();
        alt_combo.connect_selected_notify(move |combo| {
            if combo.selected() == gtk::INVALID_LIST_POSITION {
                return;
            }
            let idx = combo.selected() as usize;
            let model = iface_model.borrow();
            if let Some(iface) = model.get(idx) {
                let name = iface.name.clone();
                let sender = sender.clone();
                glib::MainContext::default().spawn_local(async move {
                    sender
                        .send(UiToEngine::SetAltInterface { interface: name })
                        .await
                        .ok();
                });
            }
        });
    }

    // Kural ekleme
    let add_rule = {
        let sender = ui_sender.clone();
        let target_entry = target_entry.clone();
        let alt_combo = alt_combo.clone();
        let iface_model = iface_model.clone();

        move || {
            let text = target_entry.text().trim().to_owned();
            if text.is_empty() {
                return;
            }

            let idx = alt_combo.selected() as usize;
            let model = iface_model.borrow();
            let Some(iface) = model.get(idx) else { return };
            let interface = iface.name.clone();
            drop(model);

            let Some(target) = RuleTarget::from_user_input(&text) else {
                return;
            };

            target_entry.set_text("");

            let sender = sender.clone();
            glib::MainContext::default().spawn_local(async move {
                sender
                    .send(UiToEngine::AddRule { target, interface })
                    .await
                    .ok();
            });
        }
    };

    add_btn.connect_clicked({
        let add_rule = add_rule.clone();
        move |_| add_rule()
    });
    target_entry.connect_activate(move |_| add_rule());

    // Rota testi
    let run_test = {
        let sender = ui_sender.clone();
        let test_entry = test_entry.clone();
        let test_network_row = test_network_row.clone();
        let test_expected_row = test_expected_row.clone();
        let test_probes_row = test_probes_row.clone();
        let test_status_icon = test_status_icon.clone();
        let test_has_result = test_has_result.clone();

        move || {
            let input = test_entry.text().trim().to_owned();
            if input.is_empty() {
                return;
            }
            test_network_row.set_subtitle("...");
            test_network_row.set_sensitive(false);
            test_expected_row.set_subtitle("...");
            test_expected_row.set_sensitive(false);
            test_probes_row.set_subtitle("...");
            test_probes_row.set_sensitive(false);
            test_status_icon.set_visible(false);
            *test_has_result.borrow_mut() = true;

            let sender = sender.clone();
            glib::MainContext::default().spawn_local(async move {
                sender.send(UiToEngine::TestRoute { input }).await.ok();
            });
        }
    };

    test_btn.connect_clicked({
        let run_test = run_test.clone();
        move |_| run_test()
    });
    test_entry.connect_activate(move |_| run_test());

    // ── Engine → UI güncelleme döngüsü ───────────────────────────────────────
    {
        let rule_rows = rule_rows.clone();
        let rules_group = rules_group.clone();
        let service_switch = service_switch.clone();
        let toast_overlay = toast_overlay.clone();
        let iface_model = iface_model.clone();
        let ui_sender = ui_sender.clone();
        let window_title = window_title.clone();
        let alt_combo = alt_combo.clone();
        let default_info_row = default_info_row.clone();
        let empty_row = empty_row.clone();
        let rule_count = rule_count.clone();
        let test_network_row = test_network_row.clone();
        let test_expected_row = test_expected_row.clone();
        let test_probes_row = test_probes_row.clone();
        let test_status_icon = test_status_icon.clone();
        let current_language = current_language.clone();
        let test_has_result = test_has_result.clone();

        glib::MainContext::default().spawn_local(async move {
            while let Ok(msg) = engine_receiver.recv().await {
                let language = *current_language.borrow();
                let texts = language.texts();
                match msg {
                    EngineToUi::InterfacesList(ifaces) => {
                        let previous_selection = {
                            let old_model = iface_model.borrow();
                            if alt_combo.selected() == gtk::INVALID_LIST_POSITION {
                                None
                            } else {
                                old_model
                                    .get(alt_combo.selected() as usize)
                                    .map(|iface| iface.name.clone())
                            }
                        };

                        // Varsayılan arayüzü bilgi satırına yaz
                        if let Some(def) = ifaces.iter().find(|i| i.is_default) {
                            let gw = def.gateway.as_deref().unwrap_or("?");
                            default_info_row
                                .set_subtitle(&language.default_gateway_subtitle(&def.name, gw));
                        } else {
                            default_info_row.set_subtitle(texts.default_not_found);
                        }

                        // İstisna ağı combo'sunu doldur
                        let names: Vec<String> = ifaces.iter().map(|i| i.name.clone()).collect();
                        let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
                        let model = gtk::StringList::new(&name_refs);
                        alt_combo.set_model(Some(&model));
                        alt_combo.set_sensitive(!ifaces.is_empty());
                        if ifaces.is_empty() {
                            alt_combo.set_subtitle(texts.no_interfaces);
                        } else {
                            match refreshed_interface_selection(
                                previous_selection.as_deref(),
                                &names,
                            ) {
                                Some(idx) => {
                                    alt_combo.set_selected(idx as u32);
                                    alt_combo.set_subtitle(texts.exception_subtitle);
                                }
                                None => {
                                    alt_combo.set_selected(gtk::INVALID_LIST_POSITION);
                                    alt_combo.set_subtitle(texts.exception_select_prompt);
                                    if previous_selection.is_some() {
                                        let sender = ui_sender.clone();
                                        glib::MainContext::default().spawn_local(async move {
                                            sender.send(UiToEngine::ClearAltInterface).await.ok();
                                        });
                                    }
                                }
                            }
                        }
                        *iface_model.borrow_mut() = ifaces;
                    }

                    EngineToUi::RuleAdded(rule) => {
                        let row = create_rule_row(&rule, ui_sender.clone(), texts);
                        rules_group.add(&row);
                        rule_rows.borrow_mut().insert(rule.id, row);

                        let mut count = rule_count.borrow_mut();
                        *count += 1;
                        empty_row.set_visible(*count == 0);
                    }

                    EngineToUi::RuleRemoved(id) => {
                        if let Some(row) = rule_rows.borrow_mut().remove(&id) {
                            rules_group.remove(&row);
                        }
                        let mut count = rule_count.borrow_mut();
                        *count = count.saturating_sub(1);
                        empty_row.set_visible(*count == 0);
                    }

                    EngineToUi::ResetComplete => {
                        let mut rows = rule_rows.borrow_mut();
                        for (_, row) in rows.drain() {
                            rules_group.remove(&row);
                        }
                        *rule_count.borrow_mut() = 0;
                        empty_row.set_visible(true);
                        service_switch.set_active(false);
                        window_title.set_subtitle(texts.inactive);
                    }

                    EngineToUi::Deactivated => {
                        service_switch.set_active(false);
                        window_title.set_subtitle(texts.inactive);
                    }

                    EngineToUi::Error(msg) => {
                        let toast = adw::Toast::builder()
                            .title(&msg)
                            .timeout(TOAST_TIMEOUT_SHORT)
                            .build();
                        toast_overlay.add_toast(&toast);
                    }

                    EngineToUi::StatusUpdate(s) => {
                        window_title.set_subtitle(&language.localized_status(&s));
                    }

                    EngineToUi::ActivationFailed(msg) => {
                        service_switch.set_active(false);
                        window_title.set_subtitle(texts.activation_failed);
                        let toast = adw::Toast::builder()
                            .title(&format!("{}: {}", texts.activation_failed_toast, msg))
                            .timeout(TOAST_TIMEOUT_LONG)
                            .build();
                        toast_overlay.add_toast(&toast);
                    }

                    EngineToUi::StateRestored { alt_interface } => {
                        // Kaydedilmiş arayüz seçimini geri yükle
                        if let Some(saved_iface) = alt_interface {
                            let model = iface_model.borrow();
                            if let Some((idx, _)) = model
                                .iter()
                                .enumerate()
                                .find(|(_, i)| i.name == saved_iface)
                            {
                                // set_selected'ın connect_selected_notify'ı tetiklemesine izin ver
                                // (bu yalnızca tercih kaydeder, kernel'a dokunmaz)
                                alt_combo.set_selected(idx as u32);
                                alt_combo.set_subtitle(texts.exception_subtitle);
                            } else {
                                alt_combo.set_selected(gtk::INVALID_LIST_POSITION);
                                alt_combo.set_subtitle(texts.exception_select_prompt);
                                let sender = ui_sender.clone();
                                glib::MainContext::default().spawn_local(async move {
                                    sender.send(UiToEngine::ClearAltInterface).await.ok();
                                });
                            }
                        }
                        // Kural sayısına göre boş durum satırını ayarla
                        let count = *rule_count.borrow();
                        empty_row.set_visible(count == 0);
                    }

                    EngineToUi::RouteTestComplete(report) => {
                        let routed_ifaces: Vec<String> = report
                            .probes
                            .iter()
                            .filter_map(|probe| probe.interface.clone())
                            .collect();
                        let seen_ifaces: BTreeSet<String> = routed_ifaces.iter().cloned().collect();
                        let all_match =
                            report.expected_interface.as_ref().is_some_and(|expected| {
                                !routed_ifaces.is_empty()
                                    && routed_ifaces.iter().all(|iface| iface == expected)
                            });

                        let icon_name = if all_match {
                            "emblem-ok-symbolic"
                        } else if routed_ifaces.is_empty() {
                            "dialog-error-symbolic"
                        } else {
                            "dialog-warning-symbolic"
                        };

                        let seen = if seen_ifaces.is_empty() {
                            "?".to_owned()
                        } else {
                            seen_ifaces.into_iter().collect::<Vec<_>>().join(", ")
                        };

                        let expected = report
                            .expected_interface
                            .as_deref()
                            .unwrap_or(texts.unselected);

                        let mut samples = report
                            .probes
                            .iter()
                            .take(4)
                            .map(|probe| {
                                let family = match probe.family {
                                    crate::models::IpFamily::V4 => "IPv4",
                                    crate::models::IpFamily::V6 => "IPv6",
                                };
                                let iface = probe.interface.as_deref().unwrap_or("?");
                                format!("{} {} → {}", family, probe.destination, iface)
                            })
                            .collect::<Vec<_>>()
                            .join("  ·  ");
                        if report.probes.len() > 4 {
                            samples.push_str(&language.route_sample_more(report.probes.len() - 4));
                        }
                        if samples.is_empty() {
                            samples = "?".to_owned();
                        }

                        test_network_row.set_subtitle(&seen);
                        test_network_row.set_sensitive(true);
                        test_status_icon.set_icon_name(Some(icon_name));
                        test_status_icon.set_visible(true);
                        test_expected_row.set_subtitle(expected);
                        test_expected_row.set_sensitive(true);
                        test_probes_row.set_subtitle(&samples);
                        test_probes_row.set_sensitive(true);
                        *test_has_result.borrow_mut() = true;
                    }
                }
            }
        });
    }

    window.present();
}

fn refreshed_interface_selection(previous_name: Option<&str>, names: &[String]) -> Option<usize> {
    let previous_name = previous_name?;
    names.iter().position(|name| name == previous_name)
}

#[cfg(test)]
mod tests {
    use super::refreshed_interface_selection;

    #[test]
    fn clears_selection_when_previous_interface_disappears() {
        let names = ["wlan0".to_owned(), "tun0".to_owned()];

        assert_eq!(refreshed_interface_selection(Some("old-vpn"), &names), None);
    }

    #[test]
    fn preserves_selection_when_previous_interface_still_exists() {
        let names = ["wlan0".to_owned(), "tun0".to_owned()];

        assert_eq!(refreshed_interface_selection(Some("tun0"), &names), Some(1));
    }
}
