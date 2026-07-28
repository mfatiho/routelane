use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthState {
    Inactive,
    Applying,
    Healthy,
    Degraded,
    Failed,
    NeedsAuthorization,
}

#[cfg(test)]
mod tests {
    use super::HealthState;

    #[test]
    fn exposes_approved_health_lifecycle_states() {
        assert_eq!(
            serde_json::to_string(&HealthState::Inactive).unwrap(),
            "\"Inactive\""
        );
        assert_eq!(
            serde_json::to_string(&HealthState::Applying).unwrap(),
            "\"Applying\""
        );
        assert_eq!(
            serde_json::to_string(&HealthState::Failed).unwrap(),
            "\"Failed\""
        );
        assert_eq!(
            serde_json::to_string(&HealthState::NeedsAuthorization).unwrap(),
            "\"NeedsAuthorization\""
        );
    }
}
