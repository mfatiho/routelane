use super::i18n::LanguageTexts;
use routelane::{
    config::Language,
    domain::{LaneId, RuleId},
    usage::events::{EventKind, UsageEvent},
};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct EventNames {
    pub lanes: BTreeMap<LaneId, String>,
    pub rules: BTreeMap<RuleId, String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventText {
    pub title: String,
    pub time: String,
    pub detail: Option<String>,
}
impl EventNames {
    pub fn from_profile(profile: &routelane::domain::Profile) -> Self {
        use routelane::domain::{RuleSource, RuleTarget};
        Self {
            lanes: profile
                .lanes
                .iter()
                .map(|lane| (lane.id.clone(), lane.name.clone()))
                .collect(),
            rules: profile
                .rules
                .iter()
                .map(|rule| {
                    let display = match (&rule.source, &rule.target) {
                        (RuleSource::Application(app), RuleTarget::Any) => app
                            .desktop_id
                            .clone()
                            .unwrap_or_else(|| app.executable_path.to_string_lossy().into_owned()),
                        (_, RuleTarget::Any) => rule.id.as_str().to_owned(),
                        (_, RuleTarget::ServicePack(id)) => id.as_str().to_owned(),
                        _ => super::rule_editor::target_text(&rule.target),
                    };
                    (rule.id.clone(), display)
                })
                .collect(),
        }
    }
    pub fn lane(&self, id: &LaneId) -> String {
        self.lanes
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.as_str().to_owned())
    }
}

pub fn event_text(
    event: &UsageEvent,
    names: &EventNames,
    now_unix_ms: i64,
    language: Language,
) -> EventText {
    use super::usage_widgets::{format_bytes, format_gb, format_limit};
    let texts = language.texts();
    let u = texts.usage;
    let mut detail = None;
    let title = match &event.kind {
        EventKind::ApplyFailed {
            code,
            message,
            rolled_back,
        } => {
            detail = Some(
                u.event_error_detail
                    .replace("{code}", code)
                    .replace("{message}", message)
                    .replace("{rollback}", if *rolled_back { "ok" } else { "failed" }),
            );
            if *rolled_back {
                u.event_apply_failed
            } else {
                u.event_apply_failed_no_rollback
            }
            .to_owned()
        }
        EventKind::QuotaWarning {
            lane_id,
            used,
            limit,
        } => u
            .event_quota_warning
            .replace("{lane}", &names.lane(lane_id))
            .replace("{percent}", &percent(*used, *limit).to_string())
            .replace("{used}", &format_gb(*used, language))
            .replace("{limit}", &format_limit(*limit, language)),
        EventKind::QuotaFull { lane_id, .. } => {
            u.event_quota_full.replace("{lane}", &names.lane(lane_id))
        }
        EventKind::Failover { from, to, families } => u
            .event_failover
            .replace(
                "{from}",
                &from
                    .as_ref()
                    .map(|id| names.lane(id))
                    .unwrap_or_else(|| texts.primary.system_default.to_owned()),
            )
            .replace("{to_dative}", &turkish_dative(&names.lane(to)))
            .replace("{to}", &names.lane(to))
            .replace("{families}", &families.join(u.event_and)),
        EventKind::Failback {
            to,
            backup_seconds,
            backup_bytes,
        } => u
            .event_failback
            .replace("{lane_dative}", &turkish_dative(&names.lane(to)))
            .replace("{lane}", &names.lane(to))
            .replace("{minutes}", &(backup_seconds / 60).to_string())
            .replace(
                "{bytes}",
                &backup_bytes
                    .map(|bytes| format_bytes(bytes, language))
                    .unwrap_or_else(|| u.event_unknown.to_owned()),
            ),
        EventKind::RuleApplied { rule_id } => u.event_rule_applied.replace(
            "{rule}",
            names
                .rules
                .get(rule_id)
                .map(String::as_str)
                .unwrap_or(rule_id.as_str()),
        ),
        EventKind::ProfileApplied { profile } => {
            u.event_profile_applied.replace("{profile}", profile)
        }
        EventKind::ProfileDeactivated => u.event_deactivated.to_owned(),
        EventKind::RuntimeAttention { phase, message } => {
            detail = message.clone();
            u.event_attention.replace("{phase}", phase)
        }
        EventKind::StoreRecovered { file } => u.event_recovered.replace("{file}", file),
    };
    let mut time = event_time(event.at_unix_ms, now_unix_ms, language);
    if let EventKind::ProfileApplied { profile } = &event.kind {
        time.push_str(&format!(" · {}: {profile}", u.event_profile));
    }
    EventText {
        title,
        time,
        detail,
    }
}

fn percent(used: u64, limit: u64) -> u64 {
    ((u128::from(used) * 100) / u128::from(limit.max(1))).min(u128::from(u64::MAX)) as u64
}

fn event_time(at_ms: i64, now_ms: i64, language: Language) -> String {
    use routelane::usage::calendar::local_date;
    let seconds = at_ms.div_euclid(1000);
    let date = local_date(seconds);
    let day = if date == local_date(now_ms.div_euclid(1000)) {
        language.texts().usage.event_today.to_owned()
    } else {
        format!(
            "{} {} {}",
            date.day,
            language.texts().usage.months[usize::from(date.month - 1)],
            date.year
        )
    };
    let raw = seconds as libc::time_t;
    // localtime_r writes only to our stack allocation and uses the system timezone.
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&raw, &mut local) }.is_null() {
        return day;
    }
    format!("{day} {:02}:{:02}", local.tm_hour, local.tm_min)
}

pub fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub fn quota_toast_text(
    lane_id: &LaneId,
    crossing: &routelane::usage::store::QuotaCrossing,
    names: &EventNames,
    next_reset: Option<routelane::usage::LocalDate>,
    language: Language,
) -> String {
    use super::usage_widgets::{format_gb, format_limit, reset_date};
    use routelane::usage::store::QuotaCrossing;
    let u = language.texts().usage;
    match crossing {
        QuotaCrossing::Full { .. } => u.event_quota_full.replace("{lane}", &names.lane(lane_id)),
        QuotaCrossing::Warning { used, limit } => next_reset
            .map_or(u.quota_toast_warning_no_reset, |_| u.quota_toast_warning)
            .replace("{lane}", &names.lane(lane_id))
            .replace("{percent}", &percent(*used, *limit).to_string())
            .replace("{used}", &format_gb(*used, language))
            .replace("{limit}", &format_limit(*limit, language))
            .replace(
                "{left}",
                &format_limit(limit.saturating_sub(*used), language),
            )
            .replace(
                "{date}",
                &next_reset
                    .map(|date| reset_date(date, language))
                    .unwrap_or_default(),
            ),
    }
}

use super::{controller::UiController, i18n::Texts};
use gtk4::{self as gtk, prelude::*};
use routelane::usage::events::EventCategory;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// Shared rendering keeps Overview and the full history's text identical.
pub fn event_row(text: &EventText, index: usize, expandable: bool) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_activatable(false);
    row.set_selectable(false);
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();
    let title = gtk::Label::builder()
        .label(&text.title)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build();
    let time = gtk::Label::builder().label(&text.time).xalign(0.0).build();
    time.add_css_class("dim-label");
    if let Some(detail) = text.detail.as_ref().filter(|_| expandable) {
        let heading = gtk::Box::new(gtk::Orientation::Vertical, 4);
        heading.append(&title);
        heading.append(&time);
        let detail = gtk::Label::builder()
            .label(detail)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .build();
        detail.set_widget_name(&format!("event-detail-text-{index}"));
        let expander = gtk::Expander::builder()
            .label_widget(&heading)
            .child(&detail)
            .build();
        expander.set_widget_name(&format!("event-detail-{index}"));
        body.append(&expander);
    } else {
        body.append(&title);
        body.append(&time);
    }
    row.set_child(Some(&body));
    row
}

pub struct EventsPage {
    pub root: gtk::Box,
    pub back: gtk::Button,
    pub all: gtk::ToggleButton,
    list: gtk::ListBox,
    category: Rc<Cell<EventCategory>>,
    rendered: RefCell<Vec<EventText>>,
    texts: Texts,
}
impl EventsPage {
    pub fn new(
        parent: &libadwaita::ApplicationWindow,
        texts: Texts,
        controller: Rc<UiController>,
    ) -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        root.set_widget_name("events-page");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let back = gtk::Button::with_label(&format!("‹ {}", texts.ux.overview));
        let title = gtk::Label::builder()
            .label(texts.usage.events_title)
            .hexpand(true)
            .build();
        title.add_css_class("title-2");
        let clear = gtk::Button::with_label(texts.usage.events_clear);
        clear.set_widget_name("events-clear");
        let weak = parent.downgrade();
        let clear_controller = Rc::downgrade(&controller);
        clear.connect_clicked(move |_| {
            let (Some(parent), Some(controller)) = (weak.upgrade(), clear_controller.upgrade())
            else {
                return;
            };
            super::confirm::confirm_destructive_action_with_label(
                &parent,
                texts,
                texts.usage.events_clear_confirm,
                texts.usage.events_clear_confirm,
                texts.usage.events_clear,
                Rc::new(move || {
                    controller.clear_events();
                }),
                Rc::new(|| {}),
            );
        });
        header.append(&back);
        header.append(&title);
        header.append(&clear);
        root.append(&header);
        let filters = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let all = gtk::ToggleButton::with_label(texts.usage.events_all);
        all.set_active(true);
        let category = Rc::new(Cell::new(EventCategory::All));
        for (id, label, value) in [
            ("all", texts.usage.events_all, EventCategory::All),
            (
                "problems",
                texts.usage.events_problems,
                EventCategory::Problems,
            ),
            (
                "transitions",
                texts.usage.events_transitions,
                EventCategory::Transitions,
            ),
            ("rules", texts.usage.events_rules, EventCategory::Rules),
            ("quota", texts.usage.events_quota, EventCategory::Quota),
        ] {
            let button = if value == EventCategory::All {
                all.clone()
            } else {
                let button = gtk::ToggleButton::with_label(label);
                button.set_group(Some(&all));
                button
            };
            button.set_widget_name(&format!("events-filter-{id}"));
            let controller = Rc::downgrade(&controller);
            let category = category.clone();
            button.connect_toggled(move |button| {
                if button.is_active() {
                    category.set(value);
                    if let Some(controller) = controller.upgrade() {
                        controller.refresh_events(value);
                    }
                }
            });
            filters.append(&button);
        }
        let filter_scroll = gtk::ScrolledWindow::builder()
            .child(&filters)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .build();
        root.append(&filter_scroll);
        let list = gtk::ListBox::new();
        list.set_widget_name("events-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");
        root.append(&list);
        let footer = gtk::Label::builder()
            .label(texts.usage.events_retention)
            .xalign(0.0)
            .wrap(true)
            .build();
        footer.add_css_class("dim-label");
        root.append(&footer);
        Self {
            root,
            back,
            all,
            list,
            category,
            rendered: RefCell::new(vec![]),
            texts,
        }
    }
    pub fn category(&self) -> EventCategory {
        self.category.get()
    }

    pub fn update(&self, profile: &routelane::domain::Profile, events: &[UsageEvent]) {
        let names = EventNames::from_profile(profile);
        let now = now_unix_ms();
        let rows: Vec<_> = events
            .iter()
            .filter(|event| {
                self.category.get() == EventCategory::All
                    || event.kind.category() == self.category.get()
            })
            .map(|event| event_text(event, &names, now, self.texts.language))
            .collect();
        if *self.rendered.borrow() == rows {
            return;
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for (i, text) in rows.iter().enumerate() {
            self.list.append(&event_row(text, i, true));
        }
        *self.rendered.borrow_mut() = rows;
    }
}

/// A name with its Turkish dative suffix by vowel harmony: "Ethernet'e",
/// "VINN'e", "Hotspot'a", "Wi-Fi'ye", "Modem'e". Names are user-chosen, so the
/// suffix cannot be fixed in the template.
fn turkish_dative(name: &str) -> String {
    let lower = name.to_lowercase();
    let last_vowel = lower.chars().rev().find(|c| "aeıioöuü".contains(*c));
    let back = matches!(last_vowel, Some('a' | 'ı' | 'o' | 'u'));
    let ends_with_vowel = lower
        .chars()
        .rev()
        .find(|c| c.is_alphanumeric())
        .is_some_and(|c| "aeıioöuü".contains(c));
    let suffix = match (ends_with_vowel, back) {
        (true, true) => "ya",
        (true, false) => "ye",
        (false, true) => "a",
        (false, false) => "e",
    };
    format!("{name}'{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn examples() -> Vec<UsageEvent> {
        vec![
            EventKind::ApplyFailed {
                code: "DAEMON_RECONCILIATION_FAILED".into(),
                message: "failure".into(),
                rolled_back: true,
            },
            EventKind::QuotaWarning {
                lane_id: "vinn".into(),
                used: 16_000_000_000,
                limit: 20_000_000_000,
            },
            EventKind::Failover {
                from: Some("ethernet".into()),
                to: "vinn".into(),
                families: vec!["IPv4".into(), "IPv6".into()],
            },
            EventKind::Failback {
                to: "ethernet".into(),
                backup_seconds: 420,
                backup_bytes: Some(410_000_000),
            },
            EventKind::RuleApplied {
                rule_id: "chat".into(),
            },
        ]
        .into_iter()
        .map(|kind| UsageEvent {
            at_unix_ms: 1_794_304_800_000,
            kind,
        })
        .collect()
    }
    #[test]
    fn event_titles_match_the_spec() {
        let names = EventNames {
            lanes: [
                ("vinn".into(), "VINN".into()),
                ("ethernet".into(), "Ethernet".into()),
            ]
            .into(),
            rules: [("chat".into(), "chatgpt.com".into())].into(),
        };
        let tr = [
            "Yönlendirme uygulanamadı, önceki ayar geri yüklendi",
            "VINN kotasının %80'i kullanıldı — 16,0 / 20 GB",
            "Ethernet kesildi, VINN'e geçildi — IPv4 ve IPv6",
            "Ethernet'e geri dönüldü — 7 dk yedekte, 410 MB",
            "“chatgpt.com” kuralı uygulandı",
        ];
        let en = [
            "Routing could not be applied, previous settings restored",
            "VINN quota is 80% used — 16.0 / 20 GB",
            "Ethernet disconnected, switched to VINN — IPv4 and IPv6",
            "Returned to Ethernet — 7 min on backup, 410 MB",
            "Rule “chatgpt.com” applied",
        ];
        for (language, expected) in [(Language::Tr, tr), (Language::En, en)] {
            for (event, title) in examples().iter().zip(expected) {
                let text = event_text(event, &names, event.at_unix_ms, language);
                assert_eq!(text.title, title);
                assert!(text.time.starts_with(language.texts().usage.event_today));
            }
            assert_eq!(
                event_text(&examples()[0], &names, 0, language)
                    .detail
                    .as_deref(),
                Some("DAEMON_RECONCILIATION_FAILED · failure · rollback: ok")
            );
        }
    }
    #[test]
    fn other_event_kinds_and_missing_names_remain_distinct() {
        let names = EventNames::default();
        let cases = [
            (EventKind::ApplyFailed { code: "APPLY_FAILED".into(), message: "denied".into(), rolled_back: false },
             "Yönlendirme uygulanamadı, önceki ayar geri yüklenemedi", "Routing could not be applied, previous settings could not be restored", Some("APPLY_FAILED · denied · rollback: failed")),
            (EventKind::ProfileApplied { profile: "Work".into() }, "Profil uygulandı: Work", "Profile applied: Work", None),
            (EventKind::ProfileDeactivated, "Yönlendirme kapatıldı", "Routing deactivated", None),
            (EventKind::RuntimeAttention { phase: "degraded".into(), message: Some("check lane".into()) },
             "Yönlendirme dikkat gerektiriyor: degraded", "Routing needs attention: degraded", Some("check lane")),
            (EventKind::QuotaFull { lane_id: "removed-lane".into(), used: 21_000_000_000, limit: 20_000_000_000 },
             "removed-lane kotası doldu — trafik kesilmedi; operatörünüz ek ücret alabilir.", "removed-lane quota is full — traffic was not stopped; your operator may charge extra.", None),
            (EventKind::StoreRecovered { file: "events-v1.jsonl".into() },
             "Bozuk kayıt korundu, yeni depo açıldı: events-v1.jsonl", "Corrupt data preserved, new store opened: events-v1.jsonl", None),
            (EventKind::RuleApplied { rule_id: "removed-rule".into() }, "“removed-rule” kuralı uygulandı", "Rule “removed-rule” applied", None),
            (EventKind::Failover { from: None, to: "removed-lane".into(), families: vec!["IPv4".into()] },
             "Sistem varsayılanı kesildi, removed-lane'ye geçildi — IPv4", "System default disconnected, switched to removed-lane — IPv4", None),
            (EventKind::Failback { to: "removed-lane".into(), backup_seconds: 59, backup_bytes: None },
             "removed-lane'ye geri dönüldü — 0 dk yedekte, Bilinmiyor", "Returned to removed-lane — 0 min on backup, Unknown", None),
        ];
        for (kind, tr, en, detail) in cases {
            let event = UsageEvent {
                at_unix_ms: 0,
                kind,
            };
            for (language, title) in [(Language::Tr, tr), (Language::En, en)] {
                let text = event_text(&event, &names, 0, language);
                assert_eq!(text.title, title);
                assert_eq!(text.detail.as_deref(), detail);
            }
        }
    }

    #[test]
    fn timestamps_use_local_today_and_local_calendar_dates() {
        use routelane::usage::{calendar::local_midnight, LocalDate};
        let date = LocalDate::parse("2026-11-10").unwrap();
        let at = (local_midnight(date) + 11 * 3600 + 5 * 60) * 1000;
        let event = UsageEvent {
            at_unix_ms: at,
            kind: EventKind::ProfileDeactivated,
        };
        for (language, today, previous) in [
            (Language::Tr, "Bugün 11:05", "10 Kasım 2026 11:05"),
            (Language::En, "Today 11:05", "10 November 2026 11:05"),
        ] {
            assert_eq!(
                event_text(&event, &EventNames::default(), at, language).time,
                today
            );
            assert_eq!(
                event_text(&event, &EventNames::default(), at + 86_400_000, language).time,
                previous
            );
        }
    }

    #[test]
    fn profile_names_replace_ids_in_event_text() {
        use routelane::domain::{
            Lane, LaneFailurePolicy, Profile, Rule, RuleAction, RuleSource, RuleTarget,
        };
        let profile = Profile {
            id: "work".into(),
            name: "Work".into(),
            enabled: false,
            routing: Default::default(),
            lanes: vec![Lane {
                id: "vinn".into(),
                name: "VINN".into(),
                interface: "usb0".into(),
                fallback_lane: None,
                failure_policy: LaneFailurePolicy::Block,
            }],
            rules: vec![Rule {
                id: "chat".into(),
                enabled: true,
                priority: 10,
                source: RuleSource::Any,
                target: RuleTarget::Domain("chatgpt.com".into()),
                action: RuleAction::Route("vinn".into()),
            }],
        };
        let names = EventNames::from_profile(&profile);
        assert_eq!(
            event_text(&examples()[1], &names, 0, Language::Tr).title,
            "VINN kotasının %80'i kullanıldı — 16,0 / 20 GB"
        );
        assert_eq!(
            event_text(&examples()[4], &names, 0, Language::En).title,
            "Rule “chatgpt.com” applied"
        );
    }

    #[test]
    fn quota_toast_titles_match_the_spec_and_never_invent_a_reset_date() {
        use routelane::usage::{store::QuotaCrossing, LocalDate};
        let lane_id = "vinn".into();
        let names = EventNames {
            lanes: [("vinn".into(), "VINN".into())].into(),
            rules: BTreeMap::new(),
        };
        let warning = QuotaCrossing::Warning {
            used: 16_000_000_000,
            limit: 20_000_000_000,
        };
        let full = QuotaCrossing::Full {
            used: 20_000_000_000,
            limit: 20_000_000_000,
        };
        let reset = LocalDate::parse("2026-12-01");
        for (language, expected_warning, expected_full, without_reset) in [
            (
                Language::Tr,
                "VINN kotasının %80'i doldu — 16,0 / 20 GB · 4 GB kaldı · 1 Aralık'ta yenilenir",
                "VINN kotası doldu — trafik kesilmedi; operatörünüz ek ücret alabilir.",
                "VINN kotasının %80'i doldu — 16,0 / 20 GB · 4 GB kaldı",
            ),
            (
                Language::En,
                "VINN quota is 80% used — 16.0 / 20 GB · 4 GB left · renews on 1 December",
                "VINN quota is full — traffic was not stopped; your operator may charge extra.",
                "VINN quota is 80% used — 16.0 / 20 GB · 4 GB left",
            ),
        ] {
            assert_eq!(
                quota_toast_text(&lane_id, &warning, &names, reset, language),
                expected_warning
            );
            assert_eq!(
                quota_toast_text(&lane_id, &full, &names, reset, language),
                expected_full
            );
            assert_eq!(
                quota_toast_text(&lane_id, &warning, &names, None, language),
                without_reset
            );
        }
    }

    #[test]
    fn turkish_dative_follows_vowel_harmony() {
        for (name, expected) in [
            ("Ethernet", "Ethernet'e"),
            ("VINN", "VINN'e"),
            ("Hotspot", "Hotspot'a"),
            ("Wi-Fi", "Wi-Fi'ye"),
            ("Modem", "Modem'e"),
            ("Kablo", "Kablo'ya"),
        ] {
            assert_eq!(turkish_dative(name), expected);
        }
    }
}
