use std::fmt;

use super::{
    detect_resource_conflicts, detect_resource_conflicts_for_update, KernelPlan, KernelSnapshot,
    PlanHash, ResourceConflict,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedKernelState {
    pub generation: u64,
    pub plan_hash: PlanHash,
    plan: KernelPlan,
}

impl AppliedKernelState {
    pub fn from_plan(plan: &KernelPlan) -> Self {
        Self {
            generation: plan.generation,
            plan_hash: plan.content_hash(),
            plan: plan.clone(),
        }
    }

    pub fn plan(&self) -> &KernelPlan {
        &self.plan
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerError {
    message: String,
}

impl RunnerError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for RunnerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RunnerError {}

pub trait KernelRunner {
    fn snapshot(&mut self) -> Result<KernelSnapshot, RunnerError>;
    fn install_guard(&mut self, next: &KernelPlan) -> Result<(), RunnerError>;
    fn prepare_routes(&mut self, next: &KernelPlan) -> Result<(), RunnerError>;
    fn activate_nft(&mut self, next: &KernelPlan) -> Result<(), RunnerError>;
    fn verify(&mut self, next: &KernelPlan) -> Result<(), RunnerError>;
    fn remove_generation(&mut self, generation: u64) -> Result<(), RunnerError>;
    fn remove_guard(&mut self) -> Result<(), RunnerError>;
    fn restore(&mut self, state: Option<&AppliedKernelState>) -> Result<(), RunnerError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyStage {
    InstallGuard,
    PrepareRoutes,
    ActivateNft,
    Verify,
    RemoveOldGeneration,
    RemoveGuard,
}

impl fmt::Display for ApplyStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::InstallGuard => "install guard",
            Self::PrepareRoutes => "prepare routes",
            Self::ActivateNft => "activate nftables",
            Self::Verify => "verify generation",
            Self::RemoveOldGeneration => "remove old generation",
            Self::RemoveGuard => "remove guard",
        };
        formatter.write_str(label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationFailure {
    pub stage: ApplyStage,
    pub primary_error: RunnerError,
    pub rollback_error: Option<RunnerError>,
    pub cleanup_error: Option<RunnerError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    StaleGeneration {
        current: u64,
        requested: u64,
    },
    GenerationConflict {
        generation: u64,
        current_hash: PlanHash,
        requested_hash: PlanHash,
    },
    Snapshot(RunnerError),
    ResourceConflict(ResourceConflict),
    Mutation(MutationFailure),
}

impl ApplyError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::StaleGeneration { .. } => "STALE_GENERATION",
            Self::GenerationConflict { .. } => "GENERATION_CONFLICT",
            Self::Snapshot(_) => "KERNEL_SNAPSHOT_FAILED",
            Self::ResourceConflict(_) => "ROUTING_RESOURCE_CONFLICT",
            Self::Mutation(failure)
                if failure.rollback_error.is_some() || failure.cleanup_error.is_some() =>
            {
                "POLICY_ROLLBACK_FAILED"
            }
            Self::Mutation(_) => "POLICY_APPLY_ROLLED_BACK",
        }
    }

    pub fn stage(&self) -> Option<ApplyStage> {
        match self {
            Self::Mutation(failure) => Some(failure.stage),
            _ => None,
        }
    }

    pub fn primary_error(&self) -> Option<&RunnerError> {
        match self {
            Self::Snapshot(error) => Some(error),
            Self::Mutation(failure) => Some(&failure.primary_error),
            _ => None,
        }
    }

    pub fn rollback_error(&self) -> Option<&RunnerError> {
        match self {
            Self::Mutation(failure) => failure.rollback_error.as_ref(),
            _ => None,
        }
    }

    pub fn cleanup_error(&self) -> Option<&RunnerError> {
        match self {
            Self::Mutation(failure) => failure.cleanup_error.as_ref(),
            _ => None,
        }
    }
}

impl fmt::Display for ApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleGeneration { current, requested } => write!(
                formatter,
                "requested generation {requested} is older than applied generation {current}"
            ),
            Self::GenerationConflict { generation, .. } => {
                write!(
                    formatter,
                    "generation {generation} has different policy content"
                )
            }
            Self::Snapshot(error) => write!(formatter, "kernel snapshot failed: {error}"),
            Self::ResourceConflict(conflict) => {
                write!(formatter, "routing resource conflict: {conflict:?}")
            }
            Self::Mutation(failure) => {
                write!(
                    formatter,
                    "{} failed: {}",
                    failure.stage, failure.primary_error
                )?;
                if let Some(error) = &failure.rollback_error {
                    write!(formatter, "; restore failed: {error}")?;
                }
                if let Some(error) = &failure.cleanup_error {
                    write!(formatter, "; guard cleanup failed: {error}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ApplyError {}

pub fn apply_plan(
    runner: &mut impl KernelRunner,
    current: Option<&AppliedKernelState>,
    next: &KernelPlan,
) -> Result<AppliedKernelState, ApplyError> {
    let requested_hash = next.content_hash();

    if let Some(current) = current {
        if next.generation < current.generation {
            return Err(ApplyError::StaleGeneration {
                current: current.generation,
                requested: next.generation,
            });
        }
        if next.generation == current.generation {
            return if requested_hash == current.plan_hash {
                Ok(current.clone())
            } else {
                Err(ApplyError::GenerationConflict {
                    generation: next.generation,
                    current_hash: current.plan_hash,
                    requested_hash,
                })
            };
        }
    }

    transition_plan(runner, current, next)
}

pub(crate) fn restore_plan(
    runner: &mut impl KernelRunner,
    current: &AppliedKernelState,
    previous: &KernelPlan,
) -> Result<AppliedKernelState, ApplyError> {
    transition_plan(runner, Some(current), previous)
}

fn transition_plan(
    runner: &mut impl KernelRunner,
    current: Option<&AppliedKernelState>,
    next: &KernelPlan,
) -> Result<AppliedKernelState, ApplyError> {
    let snapshot = runner.snapshot().map_err(ApplyError::Snapshot)?;
    match current {
        Some(current) => detect_resource_conflicts_for_update(next, current.plan(), &snapshot),
        None => detect_resource_conflicts(next, &snapshot),
    }
    .map_err(ApplyError::ResourceConflict)?;

    run_mutating_stage(runner, current, ApplyStage::InstallGuard, |runner| {
        runner.install_guard(next)
    })?;
    run_mutating_stage(runner, current, ApplyStage::PrepareRoutes, |runner| {
        runner.prepare_routes(next)
    })?;
    run_mutating_stage(runner, current, ApplyStage::ActivateNft, |runner| {
        runner.activate_nft(next)
    })?;
    run_mutating_stage(runner, current, ApplyStage::Verify, |runner| {
        runner.verify(next)
    })?;
    if let Some(current) = current {
        run_mutating_stage(
            runner,
            Some(current),
            ApplyStage::RemoveOldGeneration,
            |runner| runner.remove_generation(current.generation),
        )?;
    }
    run_mutating_stage(runner, current, ApplyStage::RemoveGuard, |runner| {
        runner.remove_guard()
    })?;

    Ok(AppliedKernelState::from_plan(next))
}

fn run_mutating_stage(
    runner: &mut impl KernelRunner,
    current: Option<&AppliedKernelState>,
    stage: ApplyStage,
    action: impl FnOnce(&mut dyn KernelRunner) -> Result<(), RunnerError>,
) -> Result<(), ApplyError> {
    if let Err(primary_error) = action(runner) {
        let rollback_error = runner.restore(current).err();
        let cleanup_error = runner.remove_guard().err();
        return Err(ApplyError::Mutation(MutationFailure {
            stage,
            primary_error,
            rollback_error,
            cleanup_error,
        }));
    }
    Ok(())
}
