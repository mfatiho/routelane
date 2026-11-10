//! Best-effort notifications on the user's session bus; no privileged bus calls.
use super::{quota::LaneQuota, store::QuotaCrossing, LocalDate};
use crate::config::Language;
use std::collections::HashMap;

pub trait Notifier: Send + Sync {
    fn notify(&self, summary: &str, body: &str);
}

#[derive(Default)]
pub struct SessionNotifier;
impl Notifier for SessionNotifier {
    fn notify(&self, summary: &str, body: &str) {
        let summary = summary.to_owned();
        let body = body.to_owned();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            log::warn!("session notification unavailable: no async runtime");
            return;
        };
        runtime.spawn(async move {
            if let Err(error) = send(&summary, &body).await {
                log::warn!("session notification unavailable: {error}");
            }
        });
    }
}
async fn send(summary: &str, body: &str) -> zbus::Result<()> {
    let connection = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await?;
    let hints: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
    let _: u32 = proxy
        .call(
            "Notify",
            &(
                "RouteLane",
                0u32,
                "network-transmit-receive-symbolic",
                summary,
                body,
                Vec::<&str>::new(),
                hints,
                -1i32,
            ),
        )
        .await?;
    Ok(())
}

pub fn quota_notification(
    lane: &str,
    crossing: &QuotaCrossing,
    settings: &LaneQuota,
    reset: LocalDate,
    language: Language,
) -> String {
    match (language, crossing) {
        (Language::Tr, QuotaCrossing::Full { .. }) => {
            format!("{lane} kotası doldu — trafik kesilmedi; operatörünüz ek ücret alabilir.")
        }
        (Language::En, QuotaCrossing::Full { .. }) => format!(
            "{lane} quota is full — traffic was not stopped; your operator may charge extra."
        ),
        (_, QuotaCrossing::Warning { used, limit }) => {
            let left = gb(limit.saturating_sub(*used), language, true);
            let used = gb(*used, language, false);
            let limit = gb(*limit, language, true);
            let percent = settings.warn_percent;
            let month = match language {
                Language::Tr => [
                    "Ocak", "Şubat", "Mart", "Nisan", "Mayıs", "Haziran", "Temmuz", "Ağustos",
                    "Eylül", "Ekim", "Kasım", "Aralık",
                ],
                Language::En => [
                    "January",
                    "February",
                    "March",
                    "April",
                    "May",
                    "June",
                    "July",
                    "August",
                    "September",
                    "October",
                    "November",
                    "December",
                ],
            }[(reset.month - 1) as usize];
            match language {
                Language::Tr => {
                    let suffix = match reset.month {
                        1 | 2 | 3 | 5 | 8 | 12 => "ta",
                        9 | 10 => "de",
                        _ => "da",
                    };
                    format!("{lane} kotasının %{percent}'i doldu — {used} / {limit} GB · {left} GB kaldı · {} {month}'{suffix} yenilenir", reset.day)
                }
                Language::En => format!("{lane} quota is {percent}% used — {used} / {limit} GB · {left} GB left · renews on {} {month}", reset.day),
            }
        }
    }
}
fn gb(bytes: u64, language: Language, trim: bool) -> String {
    let mut value = format!("{:.1}", bytes as f64 / 1_000_000_000.0);
    if trim && value.ends_with(".0") {
        value.truncate(value.len() - 2);
    }
    if language == Language::Tr {
        value = value.replace('.', ",");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_notifications_match_both_languages() {
        let settings = LaneQuota {
            enabled: true,
            limit_bytes: 20_000_000_000,
            reset_day: 1,
            warn_percent: 80,
        };
        let reset = LocalDate::parse("2026-12-01").unwrap();
        let warning = QuotaCrossing::Warning {
            used: 16_000_000_000,
            limit: settings.limit_bytes,
        };
        let full = QuotaCrossing::Full {
            used: settings.limit_bytes,
            limit: settings.limit_bytes,
        };
        assert_eq!(
            quota_notification("VINN", &warning, &settings, reset, Language::Tr),
            "VINN kotasının %80'i doldu — 16,0 / 20 GB · 4 GB kaldı · 1 Aralık'ta yenilenir"
        );
        assert_eq!(
            quota_notification("VINN", &full, &settings, reset, Language::Tr),
            "VINN kotası doldu — trafik kesilmedi; operatörünüz ek ücret alabilir."
        );
        assert_eq!(
            quota_notification("VINN", &warning, &settings, reset, Language::En),
            "VINN quota is 80% used — 16.0 / 20 GB · 4 GB left · renews on 1 December"
        );
        assert_eq!(
            quota_notification("VINN", &full, &settings, reset, Language::En),
            "VINN quota is full — traffic was not stopped; your operator may charge extra."
        );
    }
}
