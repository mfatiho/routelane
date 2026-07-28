mod inspect;
mod nftables;
mod plan;
mod planner;
mod policy_route;

pub use inspect::{
    detect_resource_conflicts, KernelSnapshot, ResourceConflict, SnapshotInput, SnapshotParseError,
    SnapshotSource,
};
pub use nftables::{render_ruleset, NftRenderError, TableMode};
pub use plan::{
    CompiledAction, CompiledRule, CompiledTarget, KernelPlan, LaneAvailability, LanePlan, PlanHash,
    ResolutionSnapshot, TopologySnapshot, MARK_BASE, MARK_MAX, RULE_PRIORITY_BASE,
    RULE_PRIORITY_MAX, TABLE_BASE, TABLE_MAX,
};
pub use planner::{compile_profile, Capability, PlanError};
pub use policy_route::{build_policy_commands, CommandSpec, IpFamily, Program};
