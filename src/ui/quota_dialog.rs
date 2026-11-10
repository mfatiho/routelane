use super::{controller::UiController, form::Form, i18n::Texts};
use gtk4::{self as gtk, prelude::*};
use libadwaita as adw;
use routelane::{
    domain::Lane,
    usage::{quota::LaneQuota, service::LaneUsage},
};
use std::rc::Rc;

pub fn open(
    parent: &adw::ApplicationWindow,
    texts: Texts,
    lane: &Lane,
    usage: Option<&LaneUsage>,
    controller: Rc<UiController>,
) {
    let form = Form::new(parent, texts.usage.quota_open, texts);
    let quota = usage.and_then(|usage| usage.quota.as_ref());
    let enabled = gtk::Switch::builder()
        .active(quota.is_some_and(|q| q.settings.enabled))
        .halign(gtk::Align::Start)
        .build();
    enabled.set_widget_name("quota-enabled");
    form.field(texts.usage.quota_enabled, &enabled);
    let spin = |name: &str, label: &str, min, max, step, value, digits| {
        let spin = gtk::SpinButton::with_range(min, max, step);
        spin.set_digits(digits);
        spin.set_value(value);
        spin.set_widget_name(name);
        form.field(label, &spin);
        spin
    };
    let limit = spin(
        "quota-limit",
        texts.usage.quota_limit,
        0.1,
        u64::MAX as f64 / 1e9,
        0.1,
        quota.map_or(20.0, |q| q.settings.limit_bytes as f64 / 1e9),
        1,
    );
    let reset = spin(
        "quota-reset-day",
        texts.usage.quota_reset_day,
        1.0,
        31.0,
        1.0,
        quota.map_or(1.0, |q| f64::from(q.settings.reset_day)),
        0,
    );
    let warn = spin(
        "quota-warn",
        texts.usage.quota_warn,
        1.0,
        99.0,
        1.0,
        quota.map_or(80.0, |q| f64::from(q.settings.warn_percent)),
        0,
    );
    let original_used = quota.map_or(0, |q| q.used_bytes);
    let used = spin(
        "quota-used",
        texts.usage.quota_used,
        0.0,
        u64::MAX as f64 / 1e9,
        0.1,
        original_used as f64 / 1e9,
        1,
    );
    // Preserve exact daemon bytes when the one-decimal display was not edited.
    let original_used_text = used.text().to_string();
    let sensitivity = {
        let fields = [limit.clone(), reset.clone(), warn.clone(), used.clone()];
        move |active| {
            for field in &fields {
                field.set_sensitive(active);
            }
        }
    };
    sensitivity(enabled.is_active());
    enabled.connect_active_notify(move |enabled| sensitivity(enabled.is_active()));
    if std::path::Path::new("/sys/class/net")
        .join(lane.interface.as_str())
        .join("wireless")
        .is_dir()
    {
        form.fields.append(
            &gtk::Label::builder()
                .label(texts.usage.wifi_note)
                .wrap(true)
                .xalign(0.0)
                .build(),
        );
    }
    form.fields.append(
        &gtk::Label::builder()
            .label(texts.usage.quota_note)
            .wrap(true)
            .xalign(0.0)
            .build(),
    );
    form.submit.set_widget_name("quota-save");
    let lane_id = lane.id.clone();
    let form_copy = form.clone();
    let handler = form.submit.connect_clicked(move |_| {
        if !form_copy.begin_submit() {
            return;
        }
        let parse = |field: &gtk::SpinButton| field.text().trim().replace(',', ".").parse::<f64>().ok().filter(|v| v.is_finite());
        let values = [parse(&limit), parse(&reset), parse(&warn), parse(&used)];
        if enabled.is_active() && !matches!(values, [Some(l), Some(r), Some(w), Some(u)]
            if l >= 0.1 && l < u64::MAX as f64 / 1e9 && (1.0..=31.0).contains(&r) && r.fract() == 0.0 && (1.0..=99.0).contains(&w) && w.fract() == 0.0 && u >= 0.0 && u < u64::MAX as f64 / 1e9) {
            form_copy.error(texts.usage.quota_invalid, &limit);
            return;
        }
        let quota = LaneQuota {
            enabled: true,
            limit_bytes: (values[0].unwrap_or(20.0) * 1e9).round() as u64,
            reset_day: values[1].unwrap_or(1.0) as u8,
            warn_percent: values[2].unwrap_or(80.0) as u8,
        };
        if enabled.is_active() && quota.validate().is_err() {
            form_copy.error(texts.usage.quota_invalid, &limit);
            return;
        }
        let new_used = (values[3].unwrap_or(0.0) * 1e9).round() as u64;
        let correction = enabled.is_active() && used.text().as_str() != original_used_text && new_used != original_used;
        if !controller.save_quota(lane_id.clone(), enabled.is_active().then_some(quota)) {
            form_copy.error(texts.ux.request_failed, &limit);
            return;
        }
        if correction {
            controller.correct_usage(lane_id.clone(), new_used);
        }
        form_copy.dialog.close();
    });
    form.disconnect_on_close(handler);
    form.dialog.present();
}
