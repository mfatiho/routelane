mod probes;
mod report;

pub use probes::{evaluate_health, kernel_resources_match_plan};
pub use report::{
    aggregate_health, Diagnostic, DiagnosticCode, DiagnosticSubject, HealthReport, LaneHealth,
    Severity, SuggestedAction,
};
