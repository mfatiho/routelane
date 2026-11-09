//! Root-owned runtime authority for automatic primary/backup failover.
//!
//! One interactive authorization arms a [`RuntimeGrant`] bound to the caller,
//! boot, network namespace, policy and the exact interfaces observed at that
//! moment. Kernel events then only recompile that stored policy against fresh,
//! grant-filtered topology; they can never add a lane, rule, UID or device.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;

use serde::{Deserialize, Serialize};

use super::{
    InterfaceIdentity, KernelPlan, PlanHash, PrimaryDecision, PrivilegedError, ResolutionSnapshot,
    TopologySnapshot,
};
use crate::domain::{LaneId, Profile, UnmatchedTraffic};

/// GUI/daemon/helper runtime protocol. Older peers fail closed.
/// 3: the routing mode (`UnmatchedTraffic`) is part of the granted policy.
pub const RUNTIME_PROTOCOL_VERSION: u32 = 3;
/// Upper bound for an activation request (profile plus resolutions).
pub const MAX_ACTIVATION_JSON_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeActivation {
    pub protocol_version: u32,
    pub profile: Profile,
    pub resolutions: ResolutionSnapshot,
    pub expected_generation: u64,
}

/// Authority granted by one interactive authorization. Never supplied by a
/// client: the helper derives every field from the bus caller and the kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeGrant {
    pub owner_uid: u32,
    pub boot_id: String,
    pub network_namespace: u64,
    pub policy_hash: PlanHash,
    pub interfaces: BTreeMap<LaneId, InterfaceIdentity>,
}

/// Everything needed to recompile the authorized policy; journaled with the
/// plan so a restart can resume without trusting user configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRecord {
    pub grant: RuntimeGrant,
    pub profile: Profile,
    pub resolutions: ResolutionSnapshot,
    pub decision: PrimaryDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimePhase {
    Inactive,
    Armed,
    Switching,
    Degraded,
    NeedsReauthorization,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub grant: Option<RuntimeGrant>,
    pub generation: u64,
    pub decision: Option<PrimaryDecision>,
    pub phase: RuntimePhase,
    pub last_error: Option<String>,
    /// Time until the runtime must be re-evaluated (a pending failback, or a
    /// held-back retry of a failed transition), from the last refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failback_in_ms: Option<u64>,
    /// The routing mode the helper armed, from its own journaled profile.
    /// `None` for an inactive runtime or older ownership: never a claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmatched_traffic: Option<UnmatchedTraffic>,
}

impl Default for RuntimeStatus {
    fn default() -> Self {
        Self::inactive(0)
    }
}

impl RuntimeStatus {
    pub fn inactive(generation: u64) -> Self {
        Self {
            grant: None,
            generation,
            decision: None,
            phase: RuntimePhase::Inactive,
            last_error: None,
            failback_in_ms: None,
            unmatched_traffic: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeApplyResponse {
    pub status: RuntimeStatus,
    pub plan: KernelPlan,
    /// Single-use token for restoring the previous runtime record.
    pub rollback_receipt: String,
}

/// The helper-owned prior state a single-use restore token may bring back.
/// Bound to the caller and to the exact commit it was issued for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRestoreReceipt {
    pub token_hash: String,
    pub owner_uid: u32,
    pub issued_for_generation: u64,
    pub issued_for_plan_hash: PlanHash,
    pub previous_plan: Option<KernelPlan>,
    pub previous_record: Option<RuntimeRecord>,
}

impl RuntimeRestoreReceipt {
    pub fn token_hash(token: &str) -> String {
        blake3::hash(token.as_bytes()).to_hex().to_string()
    }
}

/// Policy identity: rules, routing, fallbacks, application identity and the
/// initial resolution snapshot. A different value needs a new authorization.
pub fn policy_hash(profile: &Profile, resolutions: &ResolutionSnapshot) -> PlanHash {
    let canonical = serde_json::to_vec(&(profile, resolutions))
        .expect("serializing a profile and its resolutions cannot fail");
    PlanHash::of_bytes(&canonical)
}

/// Grant-filtered topology: an interface whose identity differs from the one
/// authorized for its lane is treated as absent, so neither the selector nor
/// the planner can adopt a recreated or renamed device.
pub fn restrict_to_grant(topology: &TopologySnapshot, grant: &RuntimeGrant) -> TopologySnapshot {
    topology.restricted_to(grant.interfaces.values())
}

/// Kernel facts a grant is bound to, and the source of restore tokens.
pub trait RuntimeEnvironment: Send {
    fn boot_id(&mut self) -> Result<String, PrivilegedError>;
    fn network_namespace(&mut self) -> Result<u64, PrivilegedError>;
    fn new_token(&mut self) -> Result<String, PrivilegedError>;
}

#[derive(Debug, Default)]
pub struct SystemRuntimeEnvironment;

impl RuntimeEnvironment for SystemRuntimeEnvironment {
    fn boot_id(&mut self) -> Result<String, PrivilegedError> {
        let text = fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(|error| PrivilegedError::new(format!("read boot id: {error}")))?;
        let text = text.trim();
        if text.len() != 36
            || !text
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            return Err(PrivilegedError::new("kernel boot id is malformed"));
        }
        Ok(text.to_owned())
    }

    fn network_namespace(&mut self) -> Result<u64, PrivilegedError> {
        fs::metadata("/proc/self/ns/net")
            .map(|metadata| metadata.ino())
            .map_err(|error| PrivilegedError::new(format!("read network namespace: {error}")))
    }

    fn new_token(&mut self) -> Result<String, PrivilegedError> {
        let mut bytes = [0; 32];
        fs::File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut bytes))
            .map_err(|error| {
                PrivilegedError::new(format!("read restore token entropy: {error}"))
            })?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}
