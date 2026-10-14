use std::collections::{BTreeMap, BTreeSet, VecDeque};

use routelane::backend::{
    apply_plan, AppliedKernelState, ApplyError, ApplyStage, CompiledAction, CompiledRule,
    CompiledTarget, KernelPlan, KernelRunner, KernelSnapshot, LaneAvailability, LanePlan,
    ResourceConflict, RouteMode, RunnerError, SnapshotInput,
};
use routelane::domain::{LaneFailurePolicy, LaneId, ProfileId, RuleId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailurePoint {
    Snapshot,
    InstallGuard,
    PrepareRoutes,
    ActivateNft,
    Verify,
    RemoveOldGeneration,
    RemoveGuard,
    Restore,
}

#[derive(Debug)]
struct InMemoryKernel {
    route_generations: BTreeMap<u64, KernelPlan>,
    active_nft: Option<KernelPlan>,
    temporary_guard: bool,
    snapshot_override: Option<KernelSnapshot>,
    snapshot_taken: bool,
    failures: VecDeque<FailurePoint>,
    mutations: usize,
}

impl InMemoryKernel {
    fn empty() -> Self {
        Self {
            route_generations: BTreeMap::new(),
            active_nft: None,
            temporary_guard: false,
            snapshot_override: None,
            snapshot_taken: false,
            failures: VecDeque::new(),
            mutations: 0,
        }
    }

    fn from_plan(plan: &KernelPlan) -> Self {
        let mut kernel = Self::empty();
        kernel
            .route_generations
            .insert(plan.generation, plan.clone());
        kernel.active_nft = Some(plan.clone());
        kernel
    }

    fn from_applied(state: AppliedKernelState) -> Self {
        Self::from_plan(state.plan())
    }

    fn with_snapshot(mut self, snapshot: KernelSnapshot) -> Self {
        self.snapshot_override = Some(snapshot);
        self
    }

    fn fail_at(mut self, point: FailurePoint) -> Self {
        self.failures.push_back(point);
        self
    }

    fn fail_in_order(mut self, points: impl IntoIterator<Item = FailurePoint>) -> Self {
        self.failures.extend(points);
        self
    }

    fn applied_state(&self) -> Option<AppliedKernelState> {
        self.active_nft.as_ref().map(AppliedKernelState::from_plan)
    }

    fn applied_generation(&self) -> Option<u64> {
        self.active_nft.as_ref().map(|plan| plan.generation)
    }

    fn kernel_plan_hash(&self) -> Option<routelane::backend::PlanHash> {
        self.active_nft.as_ref().map(KernelPlan::content_hash)
    }

    fn route_tables(&self) -> BTreeSet<u32> {
        self.route_generations
            .values()
            .flat_map(|plan| plan.lanes.iter().map(|lane| lane.table))
            .collect()
    }

    fn route_generations(&self) -> BTreeSet<u64> {
        self.route_generations.keys().copied().collect()
    }

    fn has_routelane_table(&self) -> bool {
        self.active_nft.is_some()
    }

    fn has_temporary_guard(&self) -> bool {
        self.temporary_guard
    }

    fn mutation_count(&self) -> usize {
        self.mutations
    }

    fn fail_if_scheduled(&mut self, point: FailurePoint) -> Result<(), RunnerError> {
        if self.failures.front() == Some(&point) {
            self.failures.pop_front();
            return Err(RunnerError::new(format!("injected failure at {point:?}")));
        }
        Ok(())
    }

    fn record_mutation(&mut self) {
        self.mutations += 1;
    }

    fn live_snapshot(&self) -> KernelSnapshot {
        let mut ipv4_routes = Vec::new();
        let mut ipv6_routes = Vec::new();
        let mut ipv4_rules = Vec::new();
        let mut ipv6_rules = Vec::new();

        for lane in self
            .route_generations
            .values()
            .flat_map(|plan| &plan.lanes)
            .filter(|lane| lane.availability == LaneAvailability::Available)
        {
            if let Some(route) = lane.route_v4 {
                let mut value = serde_json::json!({
                    "dst": "default",
                    "dev": lane.interface,
                    "table": lane.table,
                    "flags": [],
                });
                if let RouteMode::Via(gateway) = route {
                    value["gateway"] = serde_json::json!(gateway);
                }
                ipv4_routes.push(value);
                ipv4_rules.push(serde_json::json!({
                    "priority": lane.rule_priority,
                    "src": "all",
                    "fwmark": format!("{:#010x}", lane.mark),
                    "table": lane.table,
                }));
            }
            if let Some(route) = lane.route_v6 {
                let mut value = serde_json::json!({
                    "dst": "default",
                    "dev": lane.interface,
                    "table": lane.table,
                    "metric": 1024,
                    "flags": [],
                    "pref": "medium",
                });
                if let RouteMode::Via(gateway) = route {
                    value["gateway"] = serde_json::json!(gateway);
                }
                ipv6_routes.push(value);
                ipv6_rules.push(serde_json::json!({
                    "priority": lane.rule_priority,
                    "src": "all",
                    "fwmark": format!("{:#010x}", lane.mark),
                    "table": lane.table,
                }));
            }
        }

        let nftables = if self.active_nft.is_some() {
            vec![serde_json::json!({
                "table": {"family": "inet", "name": "routelane"}
            })]
        } else {
            vec![]
        };
        let ipv4_routes = serde_json::to_vec(&ipv4_routes).unwrap();
        let ipv6_routes = serde_json::to_vec(&ipv6_routes).unwrap();
        let ipv4_rules = serde_json::to_vec(&ipv4_rules).unwrap();
        let ipv6_rules = serde_json::to_vec(&ipv6_rules).unwrap();
        let nft = serde_json::to_vec(&serde_json::json!({"nftables": nftables})).unwrap();

        KernelSnapshot::parse(SnapshotInput {
            ipv4_routes: &ipv4_routes,
            ipv6_routes: &ipv6_routes,
            ipv4_rules: &ipv4_rules,
            ipv6_rules: &ipv6_rules,
            nft: &nft,
        })
        .unwrap()
    }
}

impl KernelRunner for InMemoryKernel {
    fn snapshot(&mut self) -> Result<KernelSnapshot, RunnerError> {
        self.fail_if_scheduled(FailurePoint::Snapshot)?;
        self.snapshot_taken = true;
        Ok(self
            .snapshot_override
            .clone()
            .unwrap_or_else(|| self.live_snapshot()))
    }

    fn install_guard(&mut self, _next: &KernelPlan) -> Result<(), RunnerError> {
        if !self.snapshot_taken {
            return Err(RunnerError::new("guard installed before snapshot"));
        }
        self.record_mutation();
        self.temporary_guard = true;
        self.fail_if_scheduled(FailurePoint::InstallGuard)
    }

    fn prepare_routes(&mut self, next: &KernelPlan) -> Result<(), RunnerError> {
        if !self.temporary_guard {
            return Err(RunnerError::new("routes prepared without guard"));
        }
        self.record_mutation();
        self.route_generations.insert(next.generation, next.clone());
        self.fail_if_scheduled(FailurePoint::PrepareRoutes)
    }

    fn activate_nft(&mut self, next: &KernelPlan) -> Result<(), RunnerError> {
        if !self.temporary_guard || !self.route_generations.contains_key(&next.generation) {
            return Err(RunnerError::new(
                "nft activated before guarded routes were prepared",
            ));
        }
        self.record_mutation();
        self.fail_if_scheduled(FailurePoint::ActivateNft)?;
        self.active_nft = Some(next.clone());
        Ok(())
    }

    fn verify(&mut self, next: &KernelPlan) -> Result<(), RunnerError> {
        if !self.temporary_guard
            || self.active_nft.as_ref().map(|plan| plan.generation) != Some(next.generation)
        {
            return Err(RunnerError::new(
                "new generation verified before activation",
            ));
        }
        self.fail_if_scheduled(FailurePoint::Verify)
    }

    fn remove_generation(&mut self, generation: u64) -> Result<(), RunnerError> {
        if !self.temporary_guard
            || self.active_nft.as_ref().map(|plan| plan.generation) == Some(generation)
        {
            return Err(RunnerError::new(
                "old generation removed before guarded activation",
            ));
        }
        self.record_mutation();
        self.route_generations.remove(&generation);
        self.fail_if_scheduled(FailurePoint::RemoveOldGeneration)
    }

    fn remove_guard(&mut self) -> Result<(), RunnerError> {
        self.record_mutation();
        self.temporary_guard = false;
        self.fail_if_scheduled(FailurePoint::RemoveGuard)
    }

    fn restore(&mut self, state: Option<&AppliedKernelState>) -> Result<(), RunnerError> {
        self.record_mutation();
        self.route_generations.clear();
        self.active_nft = None;
        if let Some(state) = state {
            self.route_generations
                .insert(state.generation, state.plan().clone());
            self.active_nft = Some(state.plan().clone());
        }
        self.fail_if_scheduled(FailurePoint::Restore)
    }
}

fn fixture_kernel_plan(generation: u64) -> KernelPlan {
    KernelPlan {
        primary: Default::default(),
        local_networks: vec![],
        generation,
        profile_id: ProfileId::from("default"),
        lanes: vec![LanePlan {
            identity: None,
            source_v4: vec![],
            source_v6: vec![],
            id: LaneId::from("privacy"),
            interface: "wg0".to_owned(),
            route_v4: Some(RouteMode::Via("10.0.0.1".parse().unwrap())),
            route_v6: None,
            availability: LaneAvailability::Available,
            table: 20_000,
            mark: 0x524c0001,
            rule_priority: 10_000,
            failure_policy: LaneFailurePolicy::Inactive,
        }],
        rules: vec![CompiledRule {
            family: routelane::backend::IpFamily::V4,
            id: RuleId::from("block-example"),
            priority: 10,
            target: CompiledTarget::Network("203.0.113.0/24".parse().unwrap()),
            action: CompiledAction::Block,
        }],
    }
}

fn fixture_applied_state(generation: u64) -> AppliedKernelState {
    AppliedKernelState::from_plan(&fixture_kernel_plan(generation))
}

fn snapshot_with(ipv4_routes: &[u8], ipv4_rules: &[u8]) -> KernelSnapshot {
    KernelSnapshot::parse(SnapshotInput {
        ipv4_routes,
        ipv6_routes: b"[]",
        ipv4_rules,
        ipv6_rules: b"[]",
        nft: br#"{"nftables":[]}"#,
    })
    .unwrap()
}

fn conflicting_snapshot() -> KernelSnapshot {
    snapshot_with(
        br#"[{"dst":"default","gateway":"192.0.2.1","dev":"eth9","table":20000,"flags":[]}]"#,
        b"[]",
    )
}

#[test]
fn repeated_generation_and_hash_is_zero_mutation_idempotent() {
    let plan = fixture_kernel_plan(9);
    let mut runner = InMemoryKernel::from_plan(&plan);
    let current = runner.applied_state().unwrap();

    let reapplied = apply_plan(&mut runner, Some(&current), &plan).unwrap();

    assert_eq!(reapplied, current);
    assert_eq!(runner.mutation_count(), 0);
    assert!(!runner.snapshot_taken);
}

#[test]
fn same_generation_with_different_hash_is_an_explicit_conflict() {
    let old = fixture_applied_state(9);
    let mut next = fixture_kernel_plan(9);
    next.profile_id = ProfileId::from("different");
    let mut runner = InMemoryKernel::from_applied(old.clone());

    let error = apply_plan(&mut runner, Some(&old), &next).unwrap_err();

    assert!(matches!(
        error,
        ApplyError::GenerationConflict { generation: 9, .. }
    ));
    assert_eq!(error.code(), "GENERATION_CONFLICT");
    assert_eq!(runner.applied_generation(), Some(9));
    assert_eq!(runner.kernel_plan_hash(), Some(old.plan_hash));
    assert_eq!(runner.mutation_count(), 0);
    assert!(!runner.snapshot_taken);
}

#[test]
fn lower_generation_is_stale_without_kernel_inspection_or_mutation() {
    let old = fixture_applied_state(9);
    let next = fixture_kernel_plan(8);
    let mut runner = InMemoryKernel::from_applied(old.clone());

    let error = apply_plan(&mut runner, Some(&old), &next).unwrap_err();

    assert_eq!(
        error,
        ApplyError::StaleGeneration {
            current: 9,
            requested: 8,
        }
    );
    assert_eq!(error.code(), "STALE_GENERATION");
    assert_eq!(runner.mutation_count(), 0);
    assert!(!runner.snapshot_taken);
}

#[test]
fn higher_generation_applies_even_when_content_hash_is_unchanged() {
    let old = fixture_applied_state(4);
    let next = fixture_kernel_plan(5);
    assert_eq!(old.plan_hash, next.content_hash());
    let mut runner = InMemoryKernel::from_applied(old.clone());

    let applied = apply_plan(&mut runner, Some(&old), &next).unwrap();

    assert_eq!(applied, AppliedKernelState::from_plan(&next));
    assert_eq!(runner.applied_generation(), Some(5));
    assert_eq!(runner.kernel_plan_hash(), Some(next.content_hash()));
    assert_eq!(runner.route_generations(), BTreeSet::from([5]));
    assert!(!runner.has_temporary_guard());
    assert_eq!(runner.mutation_count(), 5);
}

#[test]
fn higher_generation_replaces_exact_owned_route_and_rule_slot() {
    let old = fixture_applied_state(4);
    let mut next = fixture_kernel_plan(5);
    next.lanes[0].interface = "wg1".to_owned();
    next.lanes[0].route_v4 = Some(RouteMode::Via("10.0.0.2".parse().unwrap()));
    next.lanes[0].mark = 0x524c0002;
    assert_ne!(old.plan_hash, next.content_hash());
    let mut runner = InMemoryKernel::from_applied(old.clone());

    let applied = apply_plan(&mut runner, Some(&old), &next).unwrap();

    assert_eq!(applied, AppliedKernelState::from_plan(&next));
    assert_eq!(runner.applied_generation(), Some(5));
    assert_eq!(runner.kernel_plan_hash(), Some(next.content_hash()));
    assert_eq!(runner.route_generations(), BTreeSet::from([5]));
    assert!(!runner.has_temporary_guard());
    assert_eq!(runner.mutation_count(), 5);
}

#[test]
fn gatewayless_route_update_rolls_back_to_previous_gateway_route_on_failure() {
    let old = fixture_applied_state(4);
    let mut next = fixture_kernel_plan(5);
    next.lanes[0].route_v4 = Some(RouteMode::Device);
    let mut runner = InMemoryKernel::from_applied(old.clone()).fail_at(FailurePoint::ActivateNft);

    let error = apply_plan(&mut runner, Some(&old), &next).unwrap_err();

    assert!(matches!(
        error,
        ApplyError::Mutation(ref failure)
            if failure.stage == ApplyStage::ActivateNft
                && failure.rollback_error.is_none()
    ));
    assert_eq!(runner.applied_state(), Some(old));
    assert!(!runner.has_temporary_guard());
}

#[test]
fn foreign_route_alongside_exact_current_owned_route_conflicts_without_mutation() {
    let old = fixture_applied_state(4);
    let mut next = fixture_kernel_plan(5);
    next.lanes[0].route_v4 = Some(RouteMode::Via("10.0.0.2".parse().unwrap()));
    let snapshot = snapshot_with(
        br#"[
            {"dst":"default","gateway":"10.0.0.1","dev":"wg0","table":20000,"flags":[]},
            {"dst":"192.0.2.0/24","dev":"wg0","table":20000,"flags":[]}
        ]"#,
        br#"[{"priority":10000,"src":"all","fwmark":"0x524c0001","table":20000}]"#,
    );
    let mut runner = InMemoryKernel::from_applied(old.clone()).with_snapshot(snapshot);

    let error = apply_plan(&mut runner, Some(&old), &next).unwrap_err();

    assert_eq!(
        error,
        ApplyError::ResourceConflict(ResourceConflict::RoutingTableInUse(20_000))
    );
    assert_eq!(runner.applied_generation(), Some(4));
    assert_eq!(runner.kernel_plan_hash(), Some(old.plan_hash));
    assert_eq!(runner.mutation_count(), 0);
    assert!(!runner.has_temporary_guard());
}

#[test]
fn foreign_rule_alongside_exact_current_owned_rule_conflicts_without_mutation() {
    let old = fixture_applied_state(4);
    let mut next = fixture_kernel_plan(5);
    next.lanes[0].mark = 0x524c0002;
    let snapshot = snapshot_with(
        br#"[{"dst":"default","gateway":"10.0.0.1","dev":"wg0","table":20000,"flags":[]}]"#,
        br#"[
            {"priority":10000,"src":"all","fwmark":"0x524c0001","table":20000},
            {"priority":10000,"src":"all","fwmark":"0x0000002a","table":20000}
        ]"#,
    );
    let mut runner = InMemoryKernel::from_applied(old.clone()).with_snapshot(snapshot);

    let error = apply_plan(&mut runner, Some(&old), &next).unwrap_err();

    assert_eq!(
        error,
        ApplyError::ResourceConflict(ResourceConflict::RulePriorityInUse(10_000))
    );
    assert_eq!(runner.applied_generation(), Some(4));
    assert_eq!(runner.kernel_plan_hash(), Some(old.plan_hash));
    assert_eq!(runner.mutation_count(), 0);
    assert!(!runner.has_temporary_guard());
}

#[test]
fn snapshot_failure_precedes_all_mutation() {
    let next = fixture_kernel_plan(1);
    let mut runner = InMemoryKernel::empty().fail_at(FailurePoint::Snapshot);

    let error = apply_plan(&mut runner, None, &next).unwrap_err();

    assert_eq!(error.code(), "KERNEL_SNAPSHOT_FAILED");
    assert_eq!(runner.mutation_count(), 0);
    assert_eq!(runner.applied_generation(), None);
    assert!(!runner.has_temporary_guard());
}

#[test]
fn resource_conflict_precedes_all_mutation() {
    let next = fixture_kernel_plan(1);
    let mut runner = InMemoryKernel::empty().with_snapshot(conflicting_snapshot());

    let error = apply_plan(&mut runner, None, &next).unwrap_err();

    assert_eq!(
        error,
        ApplyError::ResourceConflict(ResourceConflict::RoutingTableInUse(20_000))
    );
    assert_eq!(error.code(), "ROUTING_RESOURCE_CONFLICT");
    assert_eq!(runner.mutation_count(), 0);
    assert_eq!(runner.applied_generation(), None);
    assert!(!runner.has_temporary_guard());
}

#[test]
fn keeps_previous_generation_when_new_nft_batch_fails() {
    let old = fixture_applied_state(4);
    let next = fixture_kernel_plan(5);
    let mut runner = InMemoryKernel::from_applied(old.clone()).fail_at(FailurePoint::ActivateNft);

    let error = apply_plan(&mut runner, Some(&old), &next).unwrap_err();

    assert_eq!(error.code(), "POLICY_APPLY_ROLLED_BACK");
    assert_eq!(error.stage(), Some(ApplyStage::ActivateNft));
    assert_eq!(runner.applied_generation(), Some(4));
    assert_eq!(runner.kernel_plan_hash(), Some(old.plan_hash));
    assert_eq!(runner.route_generations(), BTreeSet::from([4]));
    assert!(!runner.has_temporary_guard());
    assert_eq!(runner.mutation_count(), 5);
}

#[test]
fn cleans_new_state_when_first_activation_health_check_fails() {
    let next = fixture_kernel_plan(1);
    let mut runner = InMemoryKernel::empty().fail_at(FailurePoint::Verify);

    let error = apply_plan(&mut runner, None, &next).unwrap_err();

    assert_eq!(error.stage(), Some(ApplyStage::Verify));
    assert_eq!(runner.applied_generation(), None);
    assert!(runner.route_tables().is_empty());
    assert!(!runner.has_routelane_table());
    assert!(!runner.has_temporary_guard());
    assert_eq!(runner.mutation_count(), 5);
}

#[test]
fn every_mutating_failure_point_restores_state_hash_and_guard() {
    let cases = [
        (FailurePoint::InstallGuard, ApplyStage::InstallGuard, 3),
        (FailurePoint::PrepareRoutes, ApplyStage::PrepareRoutes, 4),
        (FailurePoint::ActivateNft, ApplyStage::ActivateNft, 5),
        (FailurePoint::Verify, ApplyStage::Verify, 5),
        (
            FailurePoint::RemoveOldGeneration,
            ApplyStage::RemoveOldGeneration,
            6,
        ),
        (FailurePoint::RemoveGuard, ApplyStage::RemoveGuard, 7),
    ];

    for (point, stage, expected_mutations) in cases {
        let old = fixture_applied_state(2);
        let next = fixture_kernel_plan(3);
        let mut runner = InMemoryKernel::from_applied(old.clone()).fail_at(point);

        let error = apply_plan(&mut runner, Some(&old), &next).unwrap_err();

        assert_eq!(error.stage(), Some(stage), "failure at {point:?}");
        assert_eq!(runner.applied_generation(), Some(2), "failure at {point:?}");
        assert_eq!(
            runner.kernel_plan_hash(),
            Some(old.plan_hash),
            "failure at {point:?}"
        );
        assert_eq!(
            runner.route_generations(),
            BTreeSet::from([2]),
            "failure at {point:?}"
        );
        assert!(!runner.has_temporary_guard(), "failure at {point:?}");
        assert_eq!(
            runner.mutation_count(),
            expected_mutations,
            "failure at {point:?}"
        );
    }
}

#[test]
fn preserves_primary_rollback_and_guard_cleanup_errors_independently() {
    let old = fixture_applied_state(2);
    let next = fixture_kernel_plan(3);
    let mut runner = InMemoryKernel::from_applied(old.clone()).fail_in_order([
        FailurePoint::ActivateNft,
        FailurePoint::Restore,
        FailurePoint::RemoveGuard,
    ]);

    let error = apply_plan(&mut runner, Some(&old), &next).unwrap_err();

    assert_eq!(error.code(), "POLICY_ROLLBACK_FAILED");
    assert_eq!(error.stage(), Some(ApplyStage::ActivateNft));
    assert!(error
        .primary_error()
        .unwrap()
        .message()
        .contains("ActivateNft"));
    assert!(error
        .rollback_error()
        .unwrap()
        .message()
        .contains("Restore"));
    assert!(error
        .cleanup_error()
        .unwrap()
        .message()
        .contains("RemoveGuard"));
    assert!(!runner.has_temporary_guard());
    assert_eq!(runner.mutation_count(), 5);
}
