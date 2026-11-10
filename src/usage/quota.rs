use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::domain::LaneId;

pub const QUOTA_SETTINGS_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneQuota {
    pub enabled: bool,
    pub limit_bytes: u64,
    pub reset_day: u8,
    pub warn_percent: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuotaError {
    LimitTooSmall,
    InvalidResetDay,
    InvalidWarnPercent,
}

impl std::fmt::Display for QuotaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::LimitTooSmall => "quota limit must be at least 100000000 bytes",
            Self::InvalidResetDay => "reset day must be between 1 and 31",
            Self::InvalidWarnPercent => "warning percent must be between 1 and 99",
        })
    }
}
impl std::error::Error for QuotaError {}

impl LaneQuota {
    pub fn validate(&self) -> Result<(), QuotaError> {
        if self.limit_bytes < 100_000_000 {
            return Err(QuotaError::LimitTooSmall);
        }
        if !(1..=31).contains(&self.reset_day) {
            return Err(QuotaError::InvalidResetDay);
        }
        if !(1..=99).contains(&self.warn_percent) {
            return Err(QuotaError::InvalidWarnPercent);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaSettings {
    pub version: u32,
    pub lanes: BTreeMap<LaneId, LaneQuota>,
}

impl Default for QuotaSettings {
    fn default() -> Self {
        Self {
            version: QUOTA_SETTINGS_VERSION,
            lanes: BTreeMap::new(),
        }
    }
}

impl QuotaSettings {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(error.into()),
        };
        let settings: Self = serde_json::from_slice(&bytes)?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        self.validate()?;
        super::atomic_save(path.as_ref(), &serde_json::to_vec_pretty(self)?)
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == QUOTA_SETTINGS_VERSION,
            "unsupported quota settings version {}",
            self.version
        );
        for quota in self.lanes.values() {
            quota.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_validation_rejects_out_of_range_values() {
        let ok = LaneQuota {
            enabled: true,
            limit_bytes: 20_000_000_000,
            reset_day: 1,
            warn_percent: 80,
        };
        assert!(ok.validate().is_ok());
        for bad in [
            LaneQuota { reset_day: 0, ..ok },
            LaneQuota {
                reset_day: 32,
                ..ok
            },
            LaneQuota {
                warn_percent: 0,
                ..ok
            },
            LaneQuota {
                warn_percent: 100,
                ..ok
            },
            LaneQuota {
                limit_bytes: 0,
                ..ok
            },
            LaneQuota {
                limit_bytes: 99_999_999,
                ..ok
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        assert!(LaneQuota {
            limit_bytes: 100_000_000,
            reset_day: 31,
            warn_percent: 99,
            ..ok
        }
        .validate()
        .is_ok());
    }
}
