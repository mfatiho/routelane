use std::collections::BTreeMap;
use std::net::IpAddr;

use crate::backend::{
    IpFamily, KernelPlan, KernelSnapshot, LaneAvailability, LanePlan, ResourceState,
};
use crate::domain::HealthState;

use super::report::{
    aggregate_health, Diagnostic, DiagnosticCode, DiagnosticSubject, HealthReport, LaneHealth,
    Severity, SuggestedAction,
};

pub fn evaluate_health(plan: Option<&KernelPlan>, snapshot: &KernelSnapshot) -> HealthReport {
    let Some(plan) = plan else {
        return HealthReport::inactive();
    };

    let mut lanes = plan
        .lanes
        .iter()
        .map(|lane| (lane.id.clone(), HealthState::Healthy))
        .collect::<LaneHealth>();
    let mut diagnostics = Vec::new();

    if !snapshot.nft_is_owned_by(plan) {
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::NftablesFeatureMissing,
            "RouteLane nftables state is unavailable",
            format!(
                "inet/routelane does not exactly match generation {} and its plan hash",
                plan.generation
            ),
            Severity::Warning,
            DiagnosticSubject::NftablesTable,
            Some(SuggestedAction::new("Reapply the active profile.")),
        ));
        for state in lanes.values_mut() {
            *state = HealthState::Degraded;
        }
    }

    for lane in &plan.lanes {
        inspect_lane(lane, snapshot, &mut lanes, &mut diagnostics);
    }

    let state = aggregate_health(
        lanes
            .values()
            .copied()
            .chain(
                diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.severity.health_state()),
            )
            .chain(std::iter::once(HealthState::Healthy)),
    );
    HealthReport::from_parts(state, lanes, diagnostics)
}

fn inspect_lane(
    lane: &LanePlan,
    snapshot: &KernelSnapshot,
    lanes: &mut BTreeMap<crate::domain::LaneId, HealthState>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !snapshot.has_interface(&lane.interface) {
        degrade(lane, lanes);
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::LaneInterfaceMissing,
            format!("Lane {} interface is missing", lane.id.as_str()),
            format!("interface {:?} is not present", lane.interface),
            Severity::Warning,
            DiagnosticSubject::Lane(lane.id.clone()),
            Some(SuggestedAction::new(
                "Restore the interface, then reapply the active profile.",
            )),
        ));
    }

    if lane.availability == LaneAvailability::Unavailable {
        degrade(lane, lanes);
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::LaneGatewayUnreachable,
            format!(
                "Lane {} is unavailable in the active plan",
                lane.id.as_str()
            ),
            "the compiled lane availability is Unavailable",
            Severity::Warning,
            DiagnosticSubject::Lane(lane.id.clone()),
            Some(SuggestedAction::new(
                "Refresh topology and reapply the active profile.",
            )),
        ));
        return;
    }

    if lane.gateway_v4.is_none() && lane.gateway_v6.is_none() {
        degrade(lane, lanes);
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::LaneGatewayUnreachable,
            format!("Lane {} has no supported gateway", lane.id.as_str()),
            "the available lane has neither an IPv4 nor an IPv6 gateway",
            Severity::Warning,
            DiagnosticSubject::Lane(lane.id.clone()),
            Some(SuggestedAction::new(
                "Configure a supported gateway and reapply the active profile.",
            )),
        ));
        return;
    }

    if let Some(gateway) = lane.gateway_v4 {
        inspect_family(
            lane,
            snapshot,
            IpFamily::V4,
            IpAddr::V4(gateway),
            lanes,
            diagnostics,
        );
    }
    if let Some(gateway) = lane.gateway_v6 {
        inspect_family(
            lane,
            snapshot,
            IpFamily::V6,
            IpAddr::V6(gateway),
            lanes,
            diagnostics,
        );
    }
}

fn inspect_family(
    lane: &LanePlan,
    snapshot: &KernelSnapshot,
    family: IpFamily,
    gateway: IpAddr,
    lanes: &mut LaneHealth,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let family_name = match family {
        IpFamily::V4 => "IPv4",
        IpFamily::V6 => "IPv6",
    };
    if !snapshot.gateway_reachable(&lane.interface, gateway) {
        degrade(lane, lanes);
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::LaneGatewayUnreachable,
            format!(
                "Lane {} {family_name} gateway is unavailable",
                lane.id.as_str()
            ),
            format!(
                "gateway {gateway} is not reachable through interface {:?}",
                lane.interface
            ),
            Severity::Warning,
            DiagnosticSubject::Lane(lane.id.clone()),
            Some(SuggestedAction::new(
                "Restore the gateway route or select another lane.",
            )),
        ));
    }

    diagnose_resource(
        lane,
        family_name,
        "route",
        snapshot.route_state(family, lane.table, gateway, &lane.interface),
        lanes,
        diagnostics,
    );
    diagnose_resource(
        lane,
        family_name,
        "policy rule",
        snapshot.rule_state(family, lane.rule_priority, lane.table, lane.mark),
        lanes,
        diagnostics,
    );
}

fn diagnose_resource(
    lane: &LanePlan,
    family: &str,
    resource: &str,
    state: ResourceState,
    lanes: &mut LaneHealth,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let detail = match state {
        ResourceState::Exact => return,
        ResourceState::Missing => format!(
            "{family} {resource} is missing for table {} and priority {}",
            lane.table, lane.rule_priority
        ),
        ResourceState::Foreign => format!(
            "{family} {resource} conflicts with the exact RouteLane resource for table {} and priority {}",
            lane.table, lane.rule_priority
        ),
    };
    degrade(lane, lanes);
    diagnostics.push(Diagnostic::new(
        DiagnosticCode::RoutingResourceConflict,
        format!("Lane {} routing state does not match", lane.id.as_str()),
        detail,
        Severity::Warning,
        DiagnosticSubject::Lane(lane.id.clone()),
        Some(SuggestedAction::new(
            "Remove the foreign resource or reapply the active profile.",
        )),
    ));
}

fn degrade(lane: &LanePlan, lanes: &mut LaneHealth) {
    lanes.insert(lane.id.clone(), HealthState::Degraded);
}
