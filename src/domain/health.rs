use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthState {
    Unknown,
    Healthy,
    Degraded,
    Unhealthy,
}
