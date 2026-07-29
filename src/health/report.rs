use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::{HealthState, LaneId};

pub type LaneHealth = BTreeMap<LaneId, HealthState>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DiagnosticCode {
    LaneInterfaceMissing,
    LaneGatewayUnreachable,
    NftablesFeatureMissing,
    PolicyApplyRolledBack,
    RoutingResourceConflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub summary: String,
    pub technical_detail: String,
    pub severity: HealthState,
    pub subject: String,
    pub suggested_action: String,
}

impl Diagnostic {
    pub fn new(
        code: DiagnosticCode,
        summary: impl Into<String>,
        technical_detail: impl Into<String>,
        severity: HealthState,
        subject: impl Into<String>,
        suggested_action: impl Into<String>,
    ) -> Self {
        Self {
            code,
            summary: summary.into(),
            technical_detail: technical_detail.into(),
            severity,
            subject: subject.into(),
            suggested_action: suggested_action.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthReport {
    pub state: HealthState,
    pub lanes: LaneHealth,
    pub diagnostics: Vec<Diagnostic>,
}

impl HealthReport {
    pub fn inactive() -> Self {
        Self::from_parts(HealthState::Inactive, LaneHealth::new(), vec![])
    }

    pub fn applying() -> Self {
        Self::from_parts(HealthState::Applying, LaneHealth::new(), vec![])
    }

    pub fn from_parts(
        state: HealthState,
        lanes: LaneHealth,
        mut diagnostics: Vec<Diagnostic>,
    ) -> Self {
        diagnostics.sort_by(|left, right| {
            (
                left.code,
                left.subject.as_str(),
                left.technical_detail.as_str(),
            )
                .cmp(&(
                    right.code,
                    right.subject.as_str(),
                    right.technical_detail.as_str(),
                ))
        });
        Self {
            state,
            lanes,
            diagnostics,
        }
    }

    pub fn failure(
        state: HealthState,
        code: DiagnosticCode,
        subject: impl Into<String>,
        summary: impl Into<String>,
        technical_detail: impl Into<String>,
        suggested_action: impl Into<String>,
    ) -> Self {
        Self::from_parts(
            state,
            LaneHealth::new(),
            vec![Diagnostic::new(
                code,
                summary,
                technical_detail,
                state,
                subject,
                suggested_action,
            )],
        )
    }
}

pub fn aggregate_health(states: impl IntoIterator<Item = HealthState>) -> HealthState {
    states
        .into_iter()
        .max_by_key(|state| precedence(*state))
        .unwrap_or(HealthState::Inactive)
}

fn precedence(state: HealthState) -> u8 {
    match state {
        HealthState::Inactive => 0,
        HealthState::Healthy => 1,
        HealthState::Applying => 2,
        HealthState::Degraded => 3,
        HealthState::NeedsAuthorization => 4,
        HealthState::Failed => 5,
    }
}
