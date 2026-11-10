use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::calendar::{period_start, LocalDate};
use super::counters::{counter_delta, InterfaceSample};
use super::quota::LaneQuota;
use super::UsageTotals;
use crate::domain::LaneId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuotaCrossing {
    Warning { used: u64, limit: u64 },
    Full { used: u64, limit: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaneUsage {
    daily: BTreeMap<LocalDate, UsageTotals>,
    last_sample: Option<InterfaceSample>,
    boot_id: String,
    correction: Option<(LocalDate, i64)>,
    fired: Option<(LocalDate, bool, bool)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageState {
    version: u32,
    lanes: BTreeMap<LaneId, LaneUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageStore {
    path: PathBuf,
    boot_id: String,
    state: UsageState,
    dirty: Cell<bool>,
}

impl UsageStore {
    pub fn open(path: impl AsRef<Path>, boot_id: &str) -> (Self, Option<String>) {
        let path = path.as_ref().to_owned();
        let mut store = Self {
            path,
            dirty: Cell::new(false),
            boot_id: boot_id.to_owned(),
            state: UsageState {
                version: 1,
                lanes: BTreeMap::new(),
            },
        };
        let bytes = match std::fs::read(&store.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (store, None),
            Err(error) => return (store, Some(format!("could not read usage state: {error}"))),
        };
        let state = serde_json::from_slice::<UsageState>(&bytes).and_then(|state| {
            if state.version == 1 {
                Ok(state)
            } else {
                Err(<serde_json::Error as serde::de::Error>::custom(
                    "unsupported usage state version",
                ))
            }
        });
        match state {
            Ok(mut state) => {
                for lane in state.lanes.values_mut() {
                    if lane.boot_id != boot_id {
                        // Keep the old boot marker until a sample is recorded, even across saves.
                        lane.last_sample = None;
                    }
                }
                store.state = state;
                (store, None)
            }
            Err(error) => {
                let preservation = super::preserve_corrupt(&store.path);
                let mut note = format!("corrupt usage state: {error}");
                match preservation {
                    Ok(path) => {
                        note.push_str(&format!("; preserved at {}", path.display()));
                        if let Err(error) = store.save() {
                            note.push_str(&format!("; could not save empty state: {error}"));
                        }
                    }
                    Err(error) => note.push_str(&format!("; could not preserve file: {error}")),
                }
                (store, Some(note))
            }
        }
    }

    fn lane_mut(&mut self, lane: &LaneId) -> &mut LaneUsage {
        self.state
            .lanes
            .entry(lane.clone())
            .or_insert_with(|| LaneUsage {
                daily: BTreeMap::new(),
                last_sample: None,
                boot_id: self.boot_id.clone(),
                correction: None,
                fired: None,
            })
    }

    pub fn record_sample(
        &mut self,
        lane: &LaneId,
        today: LocalDate,
        sample: InterfaceSample,
    ) -> UsageTotals {
        let boot_id = self.boot_id.clone();
        let lane = self.lane_mut(lane);
        let baseline_changed = lane.boot_id != boot_id
            || lane
                .last_sample
                .as_ref()
                .is_none_or(|last| last.ifindex != sample.ifindex);
        let delta = if lane.boot_id != boot_id {
            UsageTotals {
                rx: sample.rx_bytes,
                tx: sample.tx_bytes,
            }
        } else {
            counter_delta(lane.last_sample.as_ref(), &sample)
        };
        let changed = baseline_changed || delta != UsageTotals::default();
        if changed {
            let daily = lane.daily.entry(today).or_default();
            *daily = daily.add(delta);
            lane.daily
                .retain(|date, _| date.ordinal() >= today.ordinal() - 399);
        }
        lane.last_sample = Some(sample);
        lane.boot_id = boot_id;
        if changed {
            self.dirty.set(true);
        }
        delta
    }

    pub fn day(&self, lane: &LaneId, date: LocalDate) -> UsageTotals {
        self.state
            .lanes
            .get(lane)
            .and_then(|lane| lane.daily.get(&date))
            .copied()
            .unwrap_or_default()
    }

    pub fn period_measured(
        &self,
        lane: &LaneId,
        start: LocalDate,
        today: LocalDate,
    ) -> UsageTotals {
        if start > today {
            return UsageTotals::default();
        }
        self.state
            .lanes
            .get(lane)
            .map(|lane| {
                lane.daily
                    .range(start..=today)
                    .fold(UsageTotals::default(), |sum, (_, total)| sum.add(*total))
            })
            .unwrap_or_default()
    }

    pub fn used_bytes(&self, lane: &LaneId, quota: &LaneQuota, today: LocalDate) -> u64 {
        let start = period_start(today, quota.reset_day);
        let measured = self.period_measured(lane, start, today).total();
        let correction = self
            .state
            .lanes
            .get(lane)
            .and_then(|lane| lane.correction)
            .filter(|(period, _)| *period == start)
            .map(|(_, offset)| offset)
            .unwrap_or(0);
        (i128::from(measured) + i128::from(correction)).clamp(0, i128::from(u64::MAX)) as u64
    }

    pub fn set_used(
        &mut self,
        lane: &LaneId,
        quota: &LaneQuota,
        today: LocalDate,
        used_bytes: u64,
    ) {
        let start = period_start(today, quota.reset_day);
        let measured = self.period_measured(lane, start, today).total();
        let offset = (i128::from(used_bytes) - i128::from(measured))
            .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
        self.lane_mut(lane).correction = Some((start, offset));
        self.dirty.set(true);
    }

    pub fn crossings(
        &mut self,
        lane: &LaneId,
        quota: &LaneQuota,
        today: LocalDate,
    ) -> Vec<QuotaCrossing> {
        if !quota.enabled || quota.validate().is_err() {
            return Vec::new();
        }
        let start = period_start(today, quota.reset_day);
        let used = self.used_bytes(lane, quota, today);
        let fired = &mut self.lane_mut(lane).fired;
        let previous = *fired;
        if fired.map(|(period, _, _)| period) != Some(start) {
            *fired = Some((start, false, false));
        }
        let (_, warn, full) = fired.as_mut().expect("period was initialized");
        let mut crossings = Vec::new();
        if !*warn
            && u128::from(used) * 100
                >= u128::from(quota.limit_bytes) * u128::from(quota.warn_percent)
        {
            *warn = true;
            crossings.push(QuotaCrossing::Warning {
                used,
                limit: quota.limit_bytes,
            });
        }
        if !*full && used >= quota.limit_bytes {
            *full = true;
            crossings.push(QuotaCrossing::Full {
                used,
                limit: quota.limit_bytes,
            });
        }
        if *fired != previous {
            self.dirty.set(true);
        }
        crossings
    }

    pub fn lifetime_bytes(&self, lane: &LaneId) -> u64 {
        self.state
            .lanes
            .get(lane)
            .map(|lane| {
                lane.daily
                    .values()
                    .fold(0u64, |sum, total| sum.saturating_add(total.total()))
            })
            .unwrap_or(0)
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.get()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        super::atomic_save(&self.path, &serde_json::to_vec_pretty(&self.state)?)?;
        self.dirty.set(false);
        Ok(())
    }
}
