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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub(crate) fn health_state(self) -> HealthState {
        match self {
            Self::Info => HealthState::Healthy,
            Self::Warning => HealthState::Degraded,
            Self::Error => HealthState::Failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum DiagnosticSubject {
    Lane(LaneId),
    NftablesTable,
    Policy,
    HealthInspection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestedAction {
    pub description: String,
}

impl SuggestedAction {
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub summary: String,
    pub technical_detail: String,
    pub severity: Severity,
    pub subject: DiagnosticSubject,
    pub suggested_action: Option<SuggestedAction>,
}

impl Diagnostic {
    pub fn new(
        code: DiagnosticCode,
        summary: impl Into<String>,
        technical_detail: impl Into<String>,
        severity: Severity,
        subject: DiagnosticSubject,
        suggested_action: Option<SuggestedAction>,
    ) -> Self {
        Self {
            code,
            summary: summary.into(),
            technical_detail: technical_detail.into(),
            severity,
            subject,
            suggested_action,
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
            (left.code, &left.subject, left.technical_detail.as_str()).cmp(&(
                right.code,
                &right.subject,
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
        subject: DiagnosticSubject,
        summary: impl Into<String>,
        technical_detail: impl Into<String>,
        suggested_action: Option<SuggestedAction>,
    ) -> Self {
        Self::from_parts(
            state,
            LaneHealth::new(),
            vec![Diagnostic::new(
                code,
                summary,
                technical_detail,
                if matches!(state, HealthState::Failed | HealthState::NeedsAuthorization) {
                    Severity::Error
                } else {
                    Severity::Warning
                },
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
