use async_trait::async_trait;
use routelane::backend::{
    ApplyResponse, CleanupResponse, InspectResponse, KernelPlan, OrphanRecoveryResponse,
    ResolutionSnapshot, RestoreResponse, RuntimePhase, TopologySnapshot,
};
use routelane::daemon::{
    ConfigRepository, DaemonError, DaemonService, PrivilegedHelperClient, ResolutionProvider,
    TopologyProvider,
};
use routelane::domain::Profile;
use routelane::ipc::{DaemonEnvelope, DaemonRequest, DaemonResult};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct LegacyHelper {
    cleaned: Arc<Mutex<bool>>,
    fail_cleanup: bool,
}

/// A current (protocol 3) helper still holding schema-2 ownership.
#[async_trait]
impl PrivilegedHelperClient for LegacyHelper {
    async fn runtime_protocol_version(&self) -> Result<u32, DaemonError> {
        Ok(routelane::backend::RUNTIME_PROTOCOL_VERSION)
    }
    async fn apply(&self, _: &KernelPlan) -> Result<ApplyResponse, DaemonError> {
        panic!("legacy plan cannot be applied")
    }
    async fn restore(
        &self,
        _: &KernelPlan,
        _: Option<&KernelPlan>,
    ) -> Result<RestoreResponse, DaemonError> {
        panic!("legacy plan cannot be restored as modern")
    }
    async fn inspect(&self) -> Result<InspectResponse, DaemonError> {
        let mut response = InspectResponse::empty();
        if !*self.cleaned.lock().unwrap() {
            response.routelane_table_present = true;
            response.runtime.phase = RuntimePhase::NeedsReauthorization;
            response
                .diagnostics
                .push("DAEMON_LEGACY_CLEANUP_REQUIRED: schema-2 cleanup required".into());
        }
        Ok(response)
    }
    async fn cleanup(&self) -> Result<CleanupResponse, DaemonError> {
        if self.fail_cleanup {
            return Err(DaemonError::helper("injected cleanup failure"));
        }
        *self.cleaned.lock().unwrap() = true;
        Ok(CleanupResponse {
            cleaned: true,
            diagnostics: vec![],
        })
    }
    async fn recover_orphaned_resources(&self) -> Result<OrphanRecoveryResponse, DaemonError> {
        panic!("validated legacy ownership must not use orphan recovery")
    }
}

struct Providers;
#[async_trait]
impl TopologyProvider for Providers {
    async fn snapshot(&self, _: &Profile) -> Result<TopologySnapshot, DaemonError> {
        Ok(TopologySnapshot::empty())
    }
}
#[async_trait]
impl ResolutionProvider for Providers {
    async fn snapshot(&self, _: &Profile) -> Result<ResolutionSnapshot, DaemonError> {
        Ok(ResolutionSnapshot::empty())
    }
}
impl ConfigRepository for Providers {
    fn load(&self) -> Result<Option<Profile>, DaemonError> {
        Ok(None)
    }
    fn save(&self, _: Option<&Profile>) -> Result<(), DaemonError> {
        panic!("legacy status and cleanup must not save configuration")
    }
}

#[tokio::test]
async fn legacy_cleanup_is_distinct_from_orphan_recovery() {
    let helper = LegacyHelper::default();
    let service = DaemonService::new(Providers, Providers, helper.clone(), Providers).unwrap();
    let response = service
        .handle(DaemonEnvelope::new("legacy", DaemonRequest::GetStatus))
        .await;
    let Some(DaemonResult::Status(status)) = response.result else {
        panic!("status required");
    };
    assert_eq!(
        status.last_error.unwrap().code,
        "DAEMON_LEGACY_CLEANUP_REQUIRED"
    );
    assert_eq!(
        service
            .recover_orphaned_resources()
            .await
            .unwrap_err()
            .code(),
        "DAEMON_ORPHAN_RECOVERY_UNAVAILABLE"
    );
    service.deactivate().await.unwrap();
    assert!(*helper.cleaned.lock().unwrap());
    assert!(service.state().await.applied.is_none());
    assert_eq!(service.state().await.generation, 0);
}

#[tokio::test]
async fn legacy_cleanup_failure_preserves_the_conflict() {
    let helper = LegacyHelper {
        fail_cleanup: true,
        ..LegacyHelper::default()
    };
    let service = DaemonService::new(Providers, Providers, helper.clone(), Providers).unwrap();
    assert!(service.deactivate().await.is_err());
    assert!(!*helper.cleaned.lock().unwrap());
    assert!(service.state().await.last_error.is_some());
}
