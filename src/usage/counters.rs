use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::UsageTotals;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceSample {
    pub ifindex: u32,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

pub trait InterfaceCounters: Send + Sync {
    fn sample(&self, interface: &str) -> Option<InterfaceSample>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SysfsCounters {
    pub root: PathBuf,
}

impl Default for SysfsCounters {
    fn default() -> Self {
        Self {
            root: PathBuf::from("/sys/class/net"),
        }
    }
}

impl InterfaceCounters for SysfsCounters {
    fn sample(&self, interface: &str) -> Option<InterfaceSample> {
        if interface.is_empty()
            || interface.len() > 15
            || matches!(interface, "." | "..")
            || !interface
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
        {
            return None;
        }
        let path = self.root.join(interface);
        Some(InterfaceSample {
            ifindex: std::fs::read_to_string(path.join("ifindex"))
                .ok()?
                .trim()
                .parse()
                .ok()?,
            rx_bytes: std::fs::read_to_string(path.join("statistics/rx_bytes"))
                .ok()?
                .trim()
                .parse()
                .ok()?,
            tx_bytes: std::fs::read_to_string(path.join("statistics/tx_bytes"))
                .ok()?
                .trim()
                .parse()
                .ok()?,
        })
    }
}

pub fn counter_delta(previous: Option<&InterfaceSample>, current: &InterfaceSample) -> UsageTotals {
    let Some(previous) = previous else {
        return UsageTotals::default();
    };
    if previous.ifindex != current.ifindex
        || current.rx_bytes < previous.rx_bytes
        || current.tx_bytes < previous.tx_bytes
    {
        UsageTotals {
            rx: current.rx_bytes,
            tx: current.tx_bytes,
        }
    } else {
        UsageTotals {
            rx: current.rx_bytes - previous.rx_bytes,
            tx: current.tx_bytes - previous.tx_bytes,
        }
    }
}
