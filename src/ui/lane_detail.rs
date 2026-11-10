use super::{
    controller::UiController,
    i18n::Texts,
    quota_dialog,
    usage_widgets::{format_gb, format_limit, reset_date, ChartMode, QuotaBar, UsageChart},
};
use gtk4::{self as gtk, prelude::*};
use libadwaita::{self as adw, prelude::*};
use routelane::{domain::Lane, usage::service::LaneUsage};
use std::rc::Rc;

pub fn open(
    parent: &adw::ApplicationWindow,
    texts: Texts,
    lane: Lane,
    controller: Rc<UiController>,
    on_edit: Rc<dyn Fn()>,
) {
    let window = adw::ApplicationWindow::builder()
        .title(&lane.name)
        .transient_for(parent)
        .modal(true)
        .default_width(660)
        .default_height(620)
        .build();
    window.set_widget_name("lane-detail");
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let back = gtk::Button::with_label(&format!("‹ {}", texts.ux.connections));
    let title = gtk::Label::new(Some(&lane.name));
    title.set_hexpand(true);
    title.add_css_class("title-2");
    let edit = gtk::Button::with_label(texts.ux.edit);
    edit.connect_clicked(move |_| on_edit());
    let weak = window.downgrade();
    back.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    header.append(&back);
    header.append(&title);
    header.append(&edit);
    body.append(&header);
    let quota_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.append(&quota_box);
    let totals = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    let download = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .hexpand(true)
        .build();
    let upload = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .hexpand(true)
        .build();
    totals.append(&download);
    totals.append(&upload);
    body.append(&totals);
    body.append(
        &gtk::Label::builder()
            .label(texts.usage.daily_usage)
            .xalign(0.0)
            .build(),
    );
    let modes = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let chart = Rc::new(UsageChart::new(texts.language));
    let period = gtk::ToggleButton::with_label(texts.usage.period);
    let seven = gtk::ToggleButton::with_label(texts.usage.last_seven_days);
    let live = gtk::ToggleButton::with_label(texts.usage.live_rate);
    seven.set_group(Some(&period));
    live.set_group(Some(&period));
    period.set_active(true);
    for (button, mode) in [
        (&period, ChartMode::Period),
        (&seven, ChartMode::LastSevenDays),
        (&live, ChartMode::LiveRate),
    ] {
        let chart = chart.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                chart.set_mode(mode);
            }
        });
        modes.append(button);
    }
    body.append(&modes);
    body.append(&chart.area);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&body)
        .build();
    window.set_content(Some(&scroll));
    let lane_id = lane.id.clone();
    let rendered = std::cell::RefCell::new(None);
    let weak = window.downgrade();
    let weak_controller = Rc::downgrade(&controller);
    let update = Rc::new(move || {
        let (Some(window), Some(controller)) = (weak.upgrade(), weak_controller.upgrade()) else {
            return;
        };
        if !window.is_visible() {
            return;
        }
        let usage = controller
            .session()
            .usage()
            .and_then(|report| report.lanes.iter().find(|u| u.lane_id == lane.id))
            .cloned();
        if rendered.borrow().as_ref() == Some(&usage) {
            return;
        }
        *rendered.borrow_mut() = Some(usage.clone());
        while let Some(child) = quota_box.first_child() {
            quota_box.remove(&child);
        }
        if let Some(quota) = usage.as_ref().and_then(|u| u.quota.as_ref()) {
            let summary = texts
                .usage
                .used_summary
                .replace("{used}", &format_gb(quota.used_bytes, texts.language))
                .replace(
                    "{limit}",
                    &format_limit(quota.settings.limit_bytes, texts.language),
                );
            quota_box.append(
                &gtk::Label::builder()
                    .label(&summary)
                    .wrap(true)
                    .xalign(0.0)
                    .build(),
            );
            quota_box.append(&QuotaBar::new(quota, texts.language).area);
            let remaining = texts
                .usage
                .remaining
                .replace(
                    "{left}",
                    &format_gb(
                        quota.settings.limit_bytes.saturating_sub(quota.used_bytes),
                        texts.language,
                    ),
                )
                .replace("{date}", &reset_date(quota.next_reset, texts.language))
                .replace("{warn}", &quota.settings.warn_percent.to_string());
            quota_box.append(
                &gtk::Label::builder()
                    .label(&remaining)
                    .wrap(true)
                    .xalign(0.0)
                    .build(),
            );
        }
        if usage.as_ref().is_some_and(|u| !u.available) {
            quota_box.append(&gtk::Label::new(Some(texts.usage.not_connected)));
        }
        let quota = gtk::Button::with_label(texts.usage.quota_open);
        quota.set_widget_name("quota-open");
        quota.set_halign(gtk::Align::Start);
        let parent = window.downgrade();
        let lane = lane.clone();
        let controller = controller.clone();
        let current_usage = usage.clone();
        quota.connect_clicked(move |_| {
            if let Some(parent) = parent.upgrade() {
                quota_dialog::open(
                    &parent,
                    texts,
                    &lane,
                    current_usage.as_ref(),
                    controller.clone(),
                );
            }
        });
        quota_box.append(&quota);
        let (rx, tx) = totals_for(usage.as_ref());
        let timeframe = if usage.as_ref().is_some_and(|u| u.quota.is_some()) {
            texts.usage.period
        } else {
            texts.usage.last_seven_days
        };
        download.set_label(&format!(
            "{} ({})\n{} GB · {}",
            texts.usage.download,
            timeframe.to_lowercase(),
            format_gb(rx, texts.language),
            texts.usage.approximate
        ));
        upload.set_label(&format!(
            "{} ({})\n{} GB · {}",
            texts.usage.upload,
            timeframe.to_lowercase(),
            format_gb(tx, texts.language),
            texts.usage.approximate
        ));
        chart.update(usage);
    });
    let weak_update = Rc::downgrade(&update);
    controller.connect_changed(Rc::new(move || {
        if let Some(update) = weak_update.upgrade() {
            update();
        }
    }));
    // The close handler owns the view callback only until this detail closes.
    let keep_update = std::cell::RefCell::new(Some(update.clone()));
    window.connect_close_request(move |_| {
        keep_update.borrow_mut().take();
        glib::signal::Inhibit(false)
    });
    let keys = gtk::EventControllerKey::new();
    let weak = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            if let Some(window) = weak.upgrade() {
                window.close();
            }
            glib::signal::Inhibit(true)
        } else {
            glib::signal::Inhibit(false)
        }
    });
    window.add_controller(&keys);
    window.present();
    update();
    controller.refresh_usage(Some(lane_id.clone()));
    controller.poll_usage(&window, Some(lane_id));
}

fn totals_for(usage: Option<&LaneUsage>) -> (u64, u64) {
    usage.map_or((0, 0), |usage| {
        if let Some(quota) = &usage.quota {
            return (quota.measured.rx, quota.measured.tx);
        }
        usage
            .daily
            .iter()
            .fold((0u64, 0u64), |(rx, tx), (_, value)| {
                (rx.saturating_add(value.rx), tx.saturating_add(value.tx))
            })
    })
}
