use crate::ui::i18n::LanguageTexts;
use gtk4::{self as gtk, prelude::*};
use routelane::{
    config::Language,
    usage::{
        calendar::{local_date, local_midnight},
        service::{LaneUsage, QuotaStatus},
        LocalDate,
    },
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

fn decimal(value: f64, language: Language) -> String {
    let value = format!("{value:.1}");
    if language == Language::Tr {
        value.replace('.', ",")
    } else {
        value
    }
}
/// GTK 0.5 uses Inhibit for output and Option<Result<f64, ()>> for input.
pub fn localize_spin_button(spin: &gtk::SpinButton, language: Language) {
    fn output(spin: &gtk::SpinButton, language: Language) {
        let value = format!("{:.*}", spin.digits() as usize, spin.value());
        spin.set_text(&if language == Language::Tr {
            value.replace('.', ",")
        } else {
            value
        });
    }
    // Let both separators reach our input handler, regardless of the process locale.
    spin.set_numeric(false);
    spin.connect_output(move |spin| {
        output(spin, language);
        glib::signal::Inhibit(true)
    });
    spin.connect_input(|spin| {
        Some(
            spin.text()
                .trim()
                .replace(',', ".")
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .ok_or(()),
        )
    });
    output(spin, language);
}

pub fn format_gb(bytes: u64, language: Language) -> String {
    decimal(bytes as f64 / 1e9, language)
}
pub fn format_rate(bytes_per_second: u64, language: Language) -> String {
    if bytes_per_second >= 1_000_000 {
        format!("{} MB/s", decimal(bytes_per_second as f64 / 1e6, language))
    } else {
        format!("{} kB/s", decimal(bytes_per_second as f64 / 1e3, language))
    }
}
pub fn format_limit(bytes: u64, language: Language) -> String {
    let value = format_gb(bytes, language);
    value
        .trim_end_matches(if language == Language::Tr { ",0" } else { ".0" })
        .to_owned()
}
pub fn format_bytes(bytes: u64, language: Language) -> String {
    if bytes >= 1_000_000_000 {
        format!("{} GB", format_gb(bytes, language))
    } else if bytes >= 1_000_000 {
        format!("{} MB", format_limit(bytes.saturating_mul(1000), language))
    } else {
        format!(
            "{} kB",
            format_limit(bytes.saturating_mul(1_000_000), language)
        )
    }
}
pub fn reset_date(date: LocalDate, language: Language) -> String {
    format!(
        "{} {}",
        date.day,
        language.texts().usage.reset_months[usize::from(date.month.saturating_sub(1)).min(11)]
    )
}
/// Calendar-day counting at noon avoids DST days being 23 or 25 hours long.
pub fn daily_limit(quota: &QuotaStatus, today: LocalDate) -> f64 {
    let mut date = today;
    let mut days = 0;
    while date < quota.next_reset && days < 32 {
        days += 1;
        date = local_date(local_midnight(date) + 36 * 3600);
    }
    quota.settings.limit_bytes.saturating_sub(quota.used_bytes) as f64 / f64::from(days.max(1))
}

pub struct QuotaBar {
    pub area: gtk::DrawingArea,
}
impl QuotaBar {
    pub fn new(quota: &QuotaStatus, language: Language) -> Self {
        let area = gtk::DrawingArea::new();
        area.set_content_width(120);
        area.set_content_height(12);
        area.set_hexpand(true);
        let fraction = quota.used_bytes as f64 / quota.settings.limit_bytes.max(1) as f64;
        let warn = f64::from(quota.settings.warn_percent) / 100.0;
        let label = language
            .texts()
            .usage
            .quota_badge
            .replace("{percent}", &(fraction * 100.0).round().to_string());
        area.update_property(&[gtk::accessible::Property::Label(&label)]);
        area.set_draw_func(move |_, cr: &gtk::cairo::Context, width, height| {
            cr.set_source_rgb(0.78, 0.80, 0.82);
            cr.rectangle(0.0, 0.0, width as f64, height as f64);
            let _ = cr.fill();
            if fraction >= 1.0 {
                cr.set_source_rgb(0.88, 0.18, 0.18);
            } else {
                cr.set_source_rgb(0.20, 0.50, 0.85);
            }
            cr.rectangle(
                0.0,
                0.0,
                width as f64 * fraction.clamp(0.0, 1.0),
                height as f64,
            );
            let _ = cr.fill();
            cr.set_source_rgb(0.20, 0.20, 0.20);
            cr.set_line_width(2.0);
            cr.move_to(width as f64 * warn, 0.0);
            cr.line_to(width as f64 * warn, height as f64);
            let _ = cr.stroke();
        });
        Self { area }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChartMode {
    Period,
    LastSevenDays,
    LiveRate,
}
pub struct UsageChart {
    pub area: gtk::DrawingArea,
    data: Rc<RefCell<Option<LaneUsage>>>,
    mode: Rc<Cell<ChartMode>>,
    language: Language,
}
impl UsageChart {
    pub fn new(language: Language) -> Self {
        let area = gtk::DrawingArea::new();
        area.set_content_width(320);
        area.set_content_height(200);
        area.set_hexpand(true);
        let data = Rc::new(RefCell::new(None::<LaneUsage>));
        let mode = Rc::new(Cell::new(ChartMode::Period));
        let chart = Self {
            area,
            data,
            mode,
            language,
        };
        let data = chart.data.clone();
        let mode = chart.mode.clone();
        chart
            .area
            .set_draw_func(move |_, cr: &gtk::cairo::Context, width, height| {
                let data = data.borrow();
                let Some(lane) = data.as_ref() else {
                    return;
                };
                let width = (width - 24).max(1) as f64;
                let height = (height - 32).max(1) as f64;
                cr.translate(12.0, 8.0);
                cr.set_source_rgb(0.5, 0.5, 0.5);
                cr.move_to(0.0, height);
                cr.line_to(width, height);
                let _ = cr.stroke();
                if mode.get() == ChartMode::LiveRate {
                    let max = lane
                        .recent_rates
                        .iter()
                        .map(|v| v.rx_per_second.max(v.tx_per_second))
                        .max()
                        .unwrap_or(1)
                        .max(1) as f64;
                    for download in [true, false] {
                        if download {
                            cr.set_source_rgb(0.2, 0.5, 0.85);
                        } else {
                            cr.set_source_rgb(0.2, 0.7, 0.4);
                        }
                        cr.set_line_width(2.0);
                        for (i, value) in lane.recent_rates.iter().enumerate() {
                            let value = if download {
                                value.rx_per_second
                            } else {
                                value.tx_per_second
                            };
                            let x = i as f64 / 59.0 * width;
                            let y = height - value as f64 / max * height;
                            if i == 0 {
                                cr.move_to(x, y);
                            } else {
                                cr.line_to(x, y);
                            }
                        }
                        let _ = cr.stroke();
                    }
                    if let Some(rate) = &lane.rate {
                        cr.move_to(0.0, height + 18.0);
                        let _ = cr.show_text(&format!(
                            "↓ {}  ↑ {}",
                            format_rate(rate.rx_per_second, language),
                            format_rate(rate.tx_per_second, language)
                        ));
                    }
                    return;
                }
                let daily = if mode.get() == ChartMode::LastSevenDays {
                    &lane.daily[lane.daily.len().saturating_sub(7)..]
                } else {
                    &lane.daily[..]
                };
                let today = lane.daily.last().map(|(date, _)| *date);
                let limit = lane
                    .quota
                    .as_ref()
                    .zip(today)
                    .map(|(quota, today)| daily_limit(quota, today));
                let max = daily
                    .iter()
                    .map(|(_, v)| v.rx.saturating_add(v.tx) as f64)
                    .fold(limit.unwrap_or(1.0).max(1.0), f64::max);
                let step = width / daily.len().max(1) as f64;
                for (i, (date, value)) in daily.iter().enumerate() {
                    let total = value.rx.saturating_add(value.tx) as f64;
                    if limit.is_some_and(|limit| total > limit) {
                        cr.set_source_rgb(0.9, 0.5, 0.12);
                    } else {
                        cr.set_source_rgb(0.2, 0.5, 0.85);
                    }
                    let h = total / max * height;
                    cr.rectangle(i as f64 * step, height - h, (step - 3.0).max(1.0), h);
                    let _ = cr.fill();
                    cr.set_source_rgb(0.5, 0.5, 0.5);
                    cr.move_to(i as f64 * step, height + 18.0);
                    let _ = cr.show_text(&date.day.to_string());
                }
                if let Some(limit) = limit {
                    cr.set_source_rgb(0.8, 0.25, 0.2);
                    cr.set_dash(&[5.0, 4.0], 0.0);
                    let y = height - limit / max * height;
                    cr.move_to(0.0, y);
                    cr.line_to(width, y);
                    let _ = cr.stroke();
                }
            });
        chart.update_label();
        chart
    }
    pub fn update(&self, lane: Option<LaneUsage>) {
        *self.data.borrow_mut() = lane;
        self.update_label();
        self.area.queue_draw();
    }
    pub fn set_mode(&self, mode: ChartMode) {
        self.mode.set(mode);
        self.update_label();
        self.area.queue_draw();
    }
    fn update_label(&self) {
        let texts = self.language.texts().usage;
        let data = self.data.borrow();
        let peak = data.as_ref().and_then(|lane| {
            let values = if self.mode.get() == ChartMode::LastSevenDays {
                &lane.daily[lane.daily.len().saturating_sub(7)..]
            } else {
                &lane.daily[..]
            };
            values.iter().max_by_key(|(_, v)| v.rx.saturating_add(v.tx))
        });
        let label = if self.mode.get() == ChartMode::LiveRate {
            texts.rate_chart_label.to_owned()
        } else {
            texts
                .chart_label
                .replace(
                    "{day}",
                    &peak.map_or_else(|| "—".into(), |(d, _)| d.to_string()),
                )
                .replace(
                    "{value}",
                    &format_gb(
                        peak.map_or(0, |(_, v)| v.rx.saturating_add(v.tx)),
                        self.language,
                    ),
                )
        };
        self.area
            .update_property(&[gtk::accessible::Property::Label(&label)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use routelane::config::Language;
    #[test]
    fn gigabytes_use_one_decimal_and_locale_separator() {
        assert_eq!(format_gb(12_400_000_000, Language::Tr), "12,4");
        assert_eq!(format_gb(12_400_000_000, Language::En), "12.4");
        assert_eq!(format_rate(1_800_000, Language::Tr), "1,8 MB/s");
    }
    #[test]
    fn daily_limit_line_is_remaining_over_remaining_days() {
        use routelane::usage::{quota::LaneQuota, service::QuotaStatus, LocalDate, UsageTotals};
        let q = QuotaStatus {
            settings: LaneQuota {
                enabled: true,
                limit_bytes: 20_000_000_000,
                reset_day: 1,
                warn_percent: 80,
            },
            period_start: LocalDate::parse("2026-11-01").unwrap(),
            next_reset: LocalDate::parse("2026-12-01").unwrap(),
            used_bytes: 12_400_000_000,
            measured: UsageTotals {
                rx: 11_100_000_000,
                tx: 1_300_000_000,
            },
        };
        let today = LocalDate::parse("2026-11-09").unwrap();
        assert!((daily_limit(&q, today) / 1e9 - 0.345).abs() < 0.001);
        let full = QuotaStatus {
            used_bytes: 21_000_000_000,
            ..q
        };
        assert_eq!(daily_limit(&full, today), 0.0);
    }
}
