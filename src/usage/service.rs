//! User-session sampling and reports. The injected clock returns Unix **seconds**.
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use super::calendar::{days_in_month, local_date, next_reset, period_start, LocalDate};
use super::counters::InterfaceCounters;
use super::events::{redact_addresses, EventCategory, EventKind, EventStore, UsageEvent};
use super::quota::{LaneQuota, QuotaSettings};
use super::store::{QuotaCrossing, UsageStore};
use super::UsageTotals;
use crate::backend::{FamilyExit, RuntimePhase, RuntimeStatus};
use crate::daemon::DaemonError;
use crate::domain::{Lane, LaneId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReport {
    pub lanes: Vec<LaneUsage>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneUsage {
    pub lane_id: LaneId,
    pub interface: String,
    pub available: bool,
    pub today: UsageTotals,
    pub quota: Option<QuotaStatus>,
    pub daily: Vec<(LocalDate, UsageTotals)>,
    pub rate: Option<UsageRate>,
    pub recent_rates: Vec<UsageRate>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaStatus {
    pub settings: LaneQuota,
    pub period_start: LocalDate,
    pub next_reset: LocalDate,
    pub used_bytes: u64,
    pub measured: UsageTotals,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRate {
    pub rx_per_second: u64,
    pub tx_per_second: u64,
}

#[derive(Default)]
struct Rates {
    carry: UsageTotals,
    last: Option<i64>,
    recent: VecDeque<(i64, UsageRate)>,
}
struct State {
    store: UsageStore,
    last_save: Option<i64>,
    settings: QuotaSettings,
    events: EventStore,
    fast_requested: Option<i64>,
    rates: BTreeMap<LaneId, Rates>,
    mark: Option<FailoverMark>,
}
pub struct UsageService {
    state: Mutex<State>,
    settings_path: PathBuf,
    counters: Arc<dyn InterfaceCounters>,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    sampling: Notify,
}

impl UsageService {
    pub fn new(
        store: UsageStore,
        settings_path: PathBuf,
        events: EventStore,
        counters: Arc<dyn InterfaceCounters>,
        clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        let (settings, note) = match QuotaSettings::load(&settings_path) {
            Ok(settings) => (settings, None),
            Err(error) => {
                log::warn!("load quota settings: {error}");
                (
                    QuotaSettings::default(),
                    Some(EventKind::StoreRecovered {
                        file: "usage-settings.json".into(),
                    }),
                )
            }
        };
        let service = Self {
            state: Mutex::new(State {
                store,
                last_save: None,
                settings,
                events,
                fast_requested: None,
                rates: BTreeMap::new(),
                mark: None,
            }),
            settings_path,
            counters,
            clock,
            sampling: Notify::new(),
        };
        if let Some(note) = note {
            if let Err(error) = service.record(note) {
                log::warn!("record settings recovery: {error}");
            }
        }
        service
    }

    pub fn tick(&self, lanes: &[Lane]) -> Vec<(LaneId, QuotaCrossing)> {
        let now = (self.clock)();
        let today = local_date(now);
        let mut state = self.state.lock().unwrap();
        let mut crossings = Vec::new();
        // Removing a lane never removes its persisted counters or settings.
        state
            .rates
            .retain(|id, _| lanes.iter().any(|lane| lane.id == *id));
        for lane in lanes {
            if let Some(sample) = self.counters.sample(lane.interface.as_str()) {
                let delta = state.store.record_sample(&lane.id, today, sample);
                let rates = state.rates.entry(lane.id.clone()).or_default();
                rates.carry = rates.carry.add(delta);
                if let Some(elapsed) = rates
                    .last
                    .map(|last| now.saturating_sub(last))
                    .filter(|elapsed| *elapsed > 0)
                {
                    rates.recent.push_back((
                        now,
                        UsageRate {
                            rx_per_second: rates.carry.rx / elapsed as u64,
                            tx_per_second: rates.carry.tx / elapsed as u64,
                        },
                    ));
                    rates.carry = UsageTotals::default();
                    rates.last = Some(now);
                } else if rates.last.is_none() {
                    rates.last = Some(now);
                    rates.carry = UsageTotals::default();
                }
            } else {
                state.rates.remove(&lane.id);
            }
            if let Some(quota) = state.settings.lanes.get(&lane.id).cloned() {
                crossings.extend(
                    state
                        .store
                        .crossings(&lane.id, &quota, today)
                        .into_iter()
                        .map(|crossing| (lane.id.clone(), crossing)),
                );
            }
        }
        for rates in state.rates.values_mut() {
            rates.recent.retain(|(at, _)| *at > now.saturating_sub(60));
        }
        if state.store.is_dirty()
            && (!crossings.is_empty()
                || state
                    .last_save
                    .is_none_or(|last| now.saturating_sub(last) >= 30))
        {
            // A failed attempt also restarts the 30 s cadence, so a full or
            // read-only disk is retried (and logged) twice a minute, not every second.
            state.last_save = Some(now);
            if let Err(error) = state.store.save() {
                log::warn!("save usage samples: {error}");
            }
        }
        crossings
    }

    /// Persist unsaved samples now; errors leave the store dirty for a retry.
    pub fn flush(&self) -> Result<(), DaemonError> {
        let mut state = self.state.lock().unwrap();
        if state.store.is_dirty() {
            state.store.save().map_err(storage_error)?;
            state.last_save = Some((self.clock)());
        }
        Ok(())
    }

    pub fn report(&self, lanes: &[Lane], fast_lane: Option<LaneId>) -> UsageReport {
        let now = (self.clock)();
        let today = local_date(now);
        let mut state = self.state.lock().unwrap();
        if fast_lane.is_some_and(|id| lanes.iter().any(|lane| lane.id == id)) {
            let was_fast = state
                .fast_requested
                .is_some_and(|at| (0..=5).contains(&now.saturating_sub(at)));
            state.fast_requested = Some(now);
            if !was_fast {
                self.sampling.notify_one();
            }
        }
        UsageReport {
            lanes: lanes
                .iter()
                .map(|lane| {
                    let quota = state
                        .settings
                        .lanes
                        .get(&lane.id)
                        .filter(|quota| quota.enabled)
                        .map(|settings| {
                            let start = period_start(today, settings.reset_day);
                            QuotaStatus {
                                settings: settings.clone(),
                                period_start: start,
                                next_reset: next_reset(today, settings.reset_day),
                                used_bytes: state.store.used_bytes(&lane.id, settings, today),
                                measured: state.store.period_measured(&lane.id, start, today),
                            }
                        });
                    let start = quota.as_ref().map(|q| q.period_start).unwrap_or_else(|| {
                        let mut date = today;
                        for _ in 0..6 {
                            date = previous_day(date);
                        }
                        date
                    });
                    let mut daily = Vec::new();
                    let mut date = start;
                    while date <= today {
                        daily.push((date, state.store.day(&lane.id, date)));
                        date = following_day(date);
                    }
                    let available = self.counters.sample(lane.interface.as_str()).is_some();
                    let recent_rates: Vec<_> = state
                        .rates
                        .get(&lane.id)
                        .into_iter()
                        .flat_map(|rates| rates.recent.iter())
                        .filter(|(at, _)| available && *at > now.saturating_sub(60))
                        .map(|(_, rate)| rate.clone())
                        .collect();
                    LaneUsage {
                        lane_id: lane.id.clone(),
                        interface: lane.interface.as_str().to_owned(),
                        available,
                        today: state.store.day(&lane.id, today),
                        quota,
                        daily,
                        rate: recent_rates.last().cloned(),
                        recent_rates,
                    }
                })
                .collect(),
        }
    }

    pub fn set_quota(&self, lane: &LaneId, quota: Option<LaneQuota>) -> Result<(), DaemonError> {
        if let Some(quota) = &quota {
            quota
                .validate()
                .map_err(|error| DaemonError::validation(error.to_string()))?;
        }
        let mut state = self.state.lock().unwrap();
        let mut settings = state.settings.clone();
        match quota {
            Some(quota) => {
                settings.lanes.insert(lane.clone(), quota);
            }
            None => {
                settings.lanes.remove(lane);
            }
        }
        settings.save(&self.settings_path).map_err(storage_error)?;
        state.settings = settings;
        Ok(())
    }

    pub fn correct(
        &self,
        lane: &LaneId,
        used_bytes: u64,
    ) -> Result<Vec<(LaneId, QuotaCrossing)>, DaemonError> {
        let mut state = self.state.lock().unwrap();
        let quota = state
            .settings
            .lanes
            .get(lane)
            .filter(|quota| quota.enabled)
            .cloned()
            .ok_or_else(|| {
                DaemonError::validation("configure an enabled quota before correcting usage")
            })?;
        let today = local_date((self.clock)());
        // UsageStore represents the correction as i64; reject unrepresentable offsets.
        let measured = state
            .store
            .period_measured(lane, period_start(today, quota.reset_day), today)
            .total();
        i64::try_from(i128::from(used_bytes) - i128::from(measured))
            .map_err(|_| DaemonError::validation("usage correction is too large"))?;
        let mut store = state.store.clone();
        store.set_used(lane, &quota, today, used_bytes);
        let crossings = store.crossings(lane, &quota, today);
        store.save().map_err(storage_error)?;
        state.store = store;
        state.last_save = Some((self.clock)());
        Ok(crossings
            .into_iter()
            .map(|crossing| (lane.clone(), crossing))
            .collect())
    }

    pub fn record(&self, mut kind: EventKind) -> Result<(), DaemonError> {
        match &mut kind {
            EventKind::ApplyFailed { message, .. }
            | EventKind::RuntimeAttention {
                message: Some(message),
                ..
            } => {
                *message = redact_addresses(message);
            }
            _ => {}
        }
        let now = (self.clock)().saturating_mul(1000);
        self.state
            .lock()
            .unwrap()
            .events
            .append_at(
                UsageEvent {
                    at_unix_ms: now,
                    kind,
                },
                now,
            )
            .map_err(storage_error)
    }
    pub fn events(&self, category: EventCategory) -> Vec<UsageEvent> {
        self.state
            .lock()
            .unwrap()
            .events
            .list(category, (self.clock)().saturating_mul(1000))
    }
    pub fn clear_events(&self) -> Result<(), DaemonError> {
        self.state
            .lock()
            .unwrap()
            .events
            .clear()
            .map_err(storage_error)
    }
    pub fn interval(&self) -> Duration {
        let now = (self.clock)();
        let state = self.state.lock().unwrap();
        let fast = state
            .fast_requested
            .is_some_and(|at| (0..=5).contains(&now.saturating_sub(at)));
        let mut seconds = if fast { 1 } else { 30 };
        if state.store.is_dirty() {
            let until_save = state.last_save.map_or(1, |last| {
                30i64.saturating_sub(now.saturating_sub(last)).clamp(1, 30) as u64
            });
            seconds = seconds.min(until_save);
        }
        Duration::from_secs(seconds)
    }
    /// Wakes a sleeping sampler immediately when a detail request starts fast sampling.
    pub async fn sampling_requested(&self) {
        self.sampling.notified().await;
    }
    pub fn runtime_change(&self, previous: &RuntimeStatus, next: &RuntimeStatus) -> Vec<EventKind> {
        let now = (self.clock)();
        let mut state = self.state.lock().unwrap();
        let total = state
            .mark
            .as_ref()
            .map(|mark| state.store.lifetime_bytes(&mark.backup_lane));
        let events = runtime_events(previous, next, state.mark.as_ref(), now, total);
        for event in &events {
            match event {
                EventKind::Failover { to, .. } => {
                    if state
                        .mark
                        .as_ref()
                        .is_none_or(|mark| mark.backup_lane != *to)
                    {
                        state.mark = Some(FailoverMark {
                            started_unix: now,
                            backup_lane: to.clone(),
                            backup_total_at_start: state.store.lifetime_bytes(to),
                        });
                    }
                }
                EventKind::Failback { .. } => state.mark = None,
                _ => {}
            }
        }
        if next.phase == RuntimePhase::Inactive {
            state.mark = None;
        }
        events
    }
    pub fn reset_runtime(&self) {
        self.state.lock().unwrap().mark = None;
    }
}
fn storage_error(error: anyhow::Error) -> DaemonError {
    DaemonError::new("DAEMON_USAGE_STORAGE_FAILED", error.to_string())
}
fn previous_day(date: LocalDate) -> LocalDate {
    if date.day > 1 {
        LocalDate {
            day: date.day - 1,
            ..date
        }
    } else {
        let (year, month) = if date.month == 1 {
            (date.year - 1, 12)
        } else {
            (date.year, date.month - 1)
        };
        LocalDate {
            year,
            month,
            day: days_in_month(year, month),
        }
    }
}
fn following_day(date: LocalDate) -> LocalDate {
    if date.day < days_in_month(date.year, date.month) {
        LocalDate {
            day: date.day + 1,
            ..date
        }
    } else if date.month < 12 {
        LocalDate {
            month: date.month + 1,
            day: 1,
            ..date
        }
    } else {
        LocalDate {
            year: date.year + 1,
            month: 1,
            day: 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FailoverMark {
    pub started_unix: i64,
    pub backup_lane: LaneId,
    pub backup_total_at_start: u64,
}

pub fn runtime_events(
    previous: &RuntimeStatus,
    next: &RuntimeStatus,
    mark: Option<&FailoverMark>,
    now_unix: i64,
    backup_total_now: Option<u64>,
) -> Vec<EventKind> {
    let mut events = Vec::new();
    if let (Some(before), Some(after)) = (&previous.decision, &next.decision) {
        for (old, new, preferred, family) in [
            (
                &before.effective.v4,
                &after.effective.v4,
                &after.preferred.v4,
                "IPv4",
            ),
            (
                &before.effective.v6,
                &after.effective.v6,
                &after.preferred.v6,
                "IPv6",
            ),
        ] {
            if matches!(old, FamilyExit::Lane(_)) && matches!(new, FamilyExit::Blocked) {
                if let Some(EventKind::Blocked { families }) = events
                    .iter_mut()
                    .find(|event| matches!(event, EventKind::Blocked { .. }))
                {
                    families.push(family.into());
                } else {
                    events.push(EventKind::Blocked {
                        families: vec![family.into()],
                    });
                }
            }
            let FamilyExit::Lane(to) = new else { continue };
            if old == new {
                continue;
            }
            if preferred.as_ref() == Some(to) {
                // A return from a backup, not initial activation or recovery from blocking.
                if !matches!(old, FamilyExit::Lane(from) if Some(from) != preferred.as_ref()) {
                    continue;
                }
                if !events.iter().any(
                    |event| matches!(event, EventKind::Failback { to: lane, .. } if lane == to),
                ) {
                    events.push(EventKind::Failback {
                        to: to.clone(),
                        backup_seconds: mark.map_or(0, |mark| {
                            now_unix.saturating_sub(mark.started_unix).max(0) as u64
                        }),
                        backup_bytes: mark.and_then(|mark| {
                            backup_total_now
                                .map(|total| total.saturating_sub(mark.backup_total_at_start))
                        }),
                    });
                }
            } else {
                let from = match old {
                    FamilyExit::Lane(from) => Some(from.clone()),
                    FamilyExit::Blocked => preferred.clone(),
                };
                let existing = events.iter_mut().find(|event| {
                    matches!(event, EventKind::Failover { from: source, to: lane, .. }
                        if lane == to && source == &from)
                });
                if let Some(EventKind::Failover { families, .. }) = existing {
                    families.push(family.into());
                } else {
                    events.push(EventKind::Failover {
                        from,
                        to: to.clone(),
                        families: vec![family.into()],
                    });
                }
            }
        }
    }
    if previous.phase != next.phase
        && matches!(
            next.phase,
            RuntimePhase::NeedsReauthorization | RuntimePhase::Degraded
        )
    {
        events.push(EventKind::RuntimeAttention {
            phase: format!("{:?}", next.phase),
            message: next.last_error.clone(),
        });
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{FamilyPair, PrimaryDecision};
    fn status(id: &str) -> RuntimeStatus {
        RuntimeStatus {
            phase: RuntimePhase::Armed,
            decision: Some(PrimaryDecision {
                preferred: FamilyPair {
                    v4: Some("ethernet".into()),
                    v6: Some("ethernet".into()),
                },
                effective: FamilyPair {
                    v4: FamilyExit::Lane(id.into()),
                    v6: FamilyExit::Lane(id.into()),
                },
            }),
            ..Default::default()
        }
    }
    #[test]
    fn losing_every_approved_exit_is_recorded_once() {
        let primary = status("ethernet");
        let mut blocked = primary.clone();
        blocked.decision.as_mut().unwrap().effective = FamilyPair {
            v4: FamilyExit::Blocked,
            v6: FamilyExit::Blocked,
        };
        assert_eq!(
            runtime_events(&primary, &blocked, None, 100, None),
            vec![EventKind::Blocked {
                families: vec!["IPv4".into(), "IPv6".into()]
            }]
        );
        assert!(runtime_events(&blocked, &blocked, None, 100, None).is_empty());
        let mut split = primary.clone();
        split.decision.as_mut().unwrap().effective.v6 = FamilyExit::Blocked;
        assert_eq!(
            runtime_events(&primary, &split, None, 100, None),
            vec![EventKind::Blocked {
                families: vec!["IPv6".into()]
            }]
        );
        assert_eq!(
            runtime_events(&split, &blocked, None, 100, None),
            vec![EventKind::Blocked {
                families: vec!["IPv4".into()]
            }]
        );
        assert!(runtime_events(&RuntimeStatus::default(), &blocked, None, 100, None).is_empty());
    }

    #[test]
    fn runtime_transitions_produce_one_event_each() {
        let primary = status("ethernet");
        let backup = status("vinn");
        assert_eq!(
            runtime_events(&primary, &backup, None, 100, None),
            vec![EventKind::Failover {
                from: Some("ethernet".into()),
                to: "vinn".into(),
                families: vec!["IPv4".into(), "IPv6".into()]
            }]
        );
        assert!(runtime_events(&backup, &backup, None, 100, None).is_empty());
        let mark = FailoverMark {
            started_unix: 100,
            backup_lane: "vinn".into(),
            backup_total_at_start: 900_000_000,
        };
        assert_eq!(
            runtime_events(&backup, &primary, Some(&mark), 520, Some(1_310_000_000)),
            vec![EventKind::Failback {
                to: "ethernet".into(),
                backup_seconds: 420,
                backup_bytes: Some(410_000_000)
            }]
        );
        assert_eq!(
            runtime_events(&backup, &primary, None, 520, None),
            vec![EventKind::Failback {
                to: "ethernet".into(),
                backup_seconds: 0,
                backup_bytes: None
            }]
        );
        let degraded = RuntimeStatus {
            phase: RuntimePhase::Degraded,
            last_error: Some("monitor stopped".into()),
            ..primary.clone()
        };
        assert_eq!(
            runtime_events(&primary, &degraded, None, 520, None),
            vec![EventKind::RuntimeAttention {
                phase: "Degraded".into(),
                message: Some("monitor stopped".into())
            }]
        );
        assert!(runtime_events(&degraded, &degraded, None, 520, None).is_empty());
        let reauthorize = RuntimeStatus {
            phase: RuntimePhase::NeedsReauthorization,
            ..primary.clone()
        };
        assert!(
            matches!(runtime_events(&primary, &reauthorize, None, 520, None).as_slice(), [EventKind::RuntimeAttention { phase, .. }] if phase == "NeedsReauthorization")
        );
    }
}
