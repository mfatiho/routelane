mod cgroup;
mod inspect;
mod nftables;
mod plan;
mod planner;
mod policy_route;
mod primary;
mod privileged;
mod topology;
mod transaction;

pub use cgroup::{
    application_cgroup_rule_ids, ApplicationCgroupManager, CgroupError, ProcessIdentity,
};
pub use inspect::{
    build_snapshot_commands, detect_resource_conflicts, detect_resource_conflicts_for_update,
    KernelSnapshot, ResourceConflict, ResourceState, SnapshotCommands, SnapshotInput,
    SnapshotParseError, SnapshotSource,
};
pub use nftables::{render_ruleset, NftRenderError, TableMode};
pub use plan::{
    CompiledAction, CompiledRule, CompiledTarget, KernelPlan, LaneAvailability, LanePlan, PlanHash,
    ResolutionSnapshot, RouteMode, MARK_BASE, MARK_MAX, RULE_PRIORITY_BASE, RULE_PRIORITY_MAX,
    TABLE_BASE, TABLE_MAX,
};
pub use planner::{compile_profile, Capability, PlanError};
pub use policy_route::{build_policy_commands, CommandSpec, IpFamily, Program};
pub use primary::{FamilyExit, FamilyPair, PrimaryDecision, PrimarySelector};
pub use privileged::{
    validate_privileged_plan, ApplyResponse, AuthorizationError, Authorizer, CleanupResponse,
    InspectResponse, OrphanRecoveryResponse, PrivilegedBackend, PrivilegedError, PrivilegedService,
    RestoreResponse, ServiceError, SystemPolkitAuthorizer, SystemPrivilegedBackend,
    ValidationError, MAX_PLAN_JSON_BYTES,
};
#[cfg(feature = "root-integration")]
pub use privileged::{CommandPrivilegedBackend, FileAppliedStateStore, ProcessCommandExecutor};
pub use topology::{
    capture_system_topology, parse_topology, InterfaceIdentity, InterfaceTopology, TopologyError,
    TopologyInput, TopologySnapshot,
};
pub use transaction::{
    apply_plan, AppliedKernelState, ApplyError, ApplyStage, KernelRunner, MutationFailure,
    RunnerError,
};
