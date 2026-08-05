# Orphaned Resource Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user explicitly approve safe cleanup of independently verified orphaned RouteLane resources and automatically retry the original profile activation once.

**Architecture:** Add a dedicated, authorized recovery operation from GTK through daemon IPC and the helper D-Bus boundary. The privileged backend captures and validates a closed `OrphanedOwnership` model, journals that exact model before mutation, removes only those resources, verifies their absence, and then allows the daemon to reset reconciliation. The UI recognizes a stable error code, asks for confirmation, sends one recovery request, and resends the preserved activation only after recovery succeeds.

**Tech Stack:** Rust 2021, Tokio, serde/serde_json, zbus, GTK4/libadwaita, PolicyKit, nftables/iproute2, cgroup v2, Cargo tests and Clippy.

## Global Constraints

- Preserve normal reconciliation and `Cleanup` fail-closed behavior.
- Perform no privileged mutation before explicit user approval.
- Remove only resources whose RouteLane ownership is independently verified.
- Reject ambiguous or mixed ownership before the first mutation.
- Retry the original activation exactly once after successful recovery.
- Add no new runtime dependency.
- Preserve Ubuntu, Debian, and Fedora support.
- Keep the unrelated pending change in `src/application/desktop.rs` intact and out of recovery-specific commits.

## File Structure

- `src/backend/privileged.rs`: orphan ownership model, persistent recovery journal, exact validation, cleanup and verification.
- `src/backend/cgroup.rs`: bounded inspection and cleanup of the RouteLane application cgroup subtree.
- `src/backend/mod.rs`: exports recovery response types.
- `src/bin/routelane_helper.rs`: authorized D-Bus and root-integration recovery entry points.
- `src/ipc/protocol.rs`, `src/ipc/mod.rs`: stable daemon recovery request/result contract.
- `src/daemon/service.rs`: helper client method, recoverable reconciliation code, recovery gate reset.
- `src/models.rs`, `src/main.rs`: bridge recovery responses from daemon to GTK.
- `src/ui/experience.rs`: pure recovery decision/state transitions.
- `src/ui/confirm.rs`: reusable confirmation dialog with custom destructive label.
- `src/ui/i18n.rs`: Turkish and English recovery copy.
- `src/ui/window.rs`: confirmation, progress, recovery request and one-shot activation retry.
- `tests/ipc_contract.rs`: daemon/helper recovery behavior.
- `tests/root/namespace_routing.rs`, `scripts/run-root-integration.sh`: real namespace recovery coverage.

---

### Task 1: Stable daemon IPC recovery contract

**Files:**
- Modify: `src/ipc/protocol.rs`
- Modify: `src/ipc/mod.rs`
- Test: `src/ipc/protocol.rs`

**Interfaces:**
- Produces: `DaemonRequest::RecoverOrphanedResources`.
- Produces: `DaemonResult::OrphanedResourcesRecovered(OrphanRecoveryResult)`.
- Produces: `pub struct OrphanRecoveryResult { pub cleaned: bool }`.

- [ ] **Step 1: Write the failing serialization test**

Add a protocol round-trip test that uses literal JSON and proves the wire names:

```rust
#[test]
fn orphan_recovery_request_and_result_have_stable_wire_names() {
    let request = DaemonEnvelope::new("recover-1", DaemonRequest::RecoverOrphanedResources);
    let encoded = serde_json::to_value(&request).unwrap();
    assert_eq!(encoded["request"]["method"], "RecoverOrphanedResources");

    let result = DaemonResult::OrphanedResourcesRecovered(OrphanRecoveryResult {
        cleaned: true,
    });
    let encoded = serde_json::to_value(result).unwrap();
    assert_eq!(encoded["kind"], "OrphanedResourcesRecovered");
    assert_eq!(encoded["value"]["cleaned"], true);
}
```

- [ ] **Step 2: Run the test and verify RED**

Run: `rtk cargo test orphan_recovery_request_and_result_have_stable_wire_names`

Expected: compilation fails because the request, result, and result type do not exist.

- [ ] **Step 3: Add the minimal protocol types**

Add the unit variant to `DaemonRequest`, the payload variant to `DaemonResult`, and:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanRecoveryResult {
    pub cleaned: bool,
}
```

Re-export `OrphanRecoveryResult` from `src/ipc/mod.rs`.

- [ ] **Step 4: Run the focused test and protocol suite**

Run: `rtk cargo test orphan_recovery_request_and_result_have_stable_wire_names`

Expected: PASS.

Run: `rtk cargo test ipc`

Expected: PASS with no serialization regression.

- [ ] **Step 5: Commit the contract**

```bash
rtk git add src/ipc/protocol.rs src/ipc/mod.rs
rtk git commit -m "feat: add orphan recovery IPC contract"
```

---

### Task 2: Exact privileged orphan ownership model and journal

**Files:**
- Modify: `src/backend/privileged.rs`
- Modify: `src/backend/cgroup.rs`
- Modify: `src/backend/mod.rs`
- Test: `src/backend/privileged.rs`
- Test: `src/backend/cgroup.rs`

**Interfaces:**
- Produces: `pub struct OrphanRecoveryResponse { pub cleaned: bool, pub diagnostics: Vec<String> }`.
- Produces: `PrivilegedBackend::recover_orphaned(&mut self) -> Result<OrphanRecoveryResponse, PrivilegedError>`.
- Produces private serializable `OrphanedOwnership { nft_fingerprint, routing }` stored in `StoredAppliedState::orphan_cleanup`.
- Produces `ApplicationCgroupManager::verified_orphan_directories() -> Result<Vec<PathBuf>, CgroupError>` and `remove_verified_orphan_directories(&[PathBuf]) -> Result<(), CgroupError>` for helper-level preflight and cleanup.
- Consumes existing `capture_snapshot`, `routing_ownership_fingerprint`, and nft normalization/fingerprinting primitives.

- [ ] **Step 1: Write backend tests for accepted and rejected ownership**

Create command-executor fixtures with literal nft/rule/route snapshots. The accepted fixture must contain:

```rust
let orphan = OrphanedOwnership {
    nft_fingerprint: fixture_routelane_nft_fingerprint(),
    routing: RoutingOwnership {
        rules: BTreeSet::from([
            (IpFamily::V4, 10_000, 20_000, 0x524c_0001),
            (IpFamily::V6, 10_000, 20_000, 0x524c_0001),
        ]),
        routes: fixture_lane_routes(20_000),
    },
};
```

Test observable behavior:

```rust
#[test]
fn recovery_journals_exact_verified_orphans_before_cleanup() {
    let (executor, store) = orphan_fixture();
    let mut backend = CommandPrivilegedBackend::with_store(executor, store.clone());

    assert!(backend.recover_orphaned().unwrap().cleaned);
    assert!(store.load().unwrap().is_none());
    assert!(!backend.inspect().unwrap().routelane_table_present);
}

#[test]
fn recovery_rejects_a_routelane_named_table_without_a_valid_state_marker() {
    let (executor, store) = markerless_orphan_fixture();
    let commands_before = executor.commands();
    let mut backend = CommandPrivilegedBackend::with_store(executor, store);

    let error = backend.recover_orphaned().unwrap_err();
    assert!(error.to_string().contains("ownership marker"));
    assert_eq!(backend.executor.commands(), commands_before);
}

#[test]
fn recovery_rejects_mixed_reserved_and_unattributed_routing_resources() {
    let mut fixture = orphan_fixture();
    fixture.add_unattributed_rule(10_001, 20_001, 7);

    let error = fixture.backend().recover_orphaned().unwrap_err();
    assert!(error.to_string().contains("ambiguous RouteLane ownership"));
    assert!(!fixture.mutating_command_was_run());
}
```

The production change each test catches is respectively: missing journaled cleanup, name-only table deletion, and partial cleanup of mixed ownership.

- [ ] **Step 2: Run tests and verify RED**

Run: `rtk cargo test recovery_`

Expected: compilation fails because `OrphanedOwnership`, the journal field, and `recover_orphaned` do not exist.

- [ ] **Step 3: Add the ownership types and store migration default**

Add serde-defaulted journal state so older state files still decode:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct OrphanedOwnership {
    nft_fingerprint: String,
    routing: RoutingOwnership,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredAppliedState {
    // existing fields remain unchanged
    #[serde(default, skip_serializing_if = "Option::is_none")]
    orphan_cleanup: Option<OrphanedOwnership>,
}
```

Update every `StoredAppliedState` constructor/literal to set `orphan_cleanup: None`. Reject records that combine `orphan_cleanup` with active, pending, cleanup-pending, or restore-receipt state.

- [ ] **Step 4: Implement validate-before-mutate orphan capture**

Add a private `capture_verified_orphaned_ownership` that:

1. rejects recovery if the state store has a validated active or pending plan;
2. reads and normalizes the complete `inet/routelane` table;
3. requires exactly one `state_g<generation>_<16 lowercase hex>` chain and the standard RouteLane base chains/rules, rejecting unknown chains or expressions;
4. captures the exact normalized nft fingerprint;
5. captures policy rules/routes and accepts only self-consistent lane tuples in planner-reserved table/priority/mark ranges;
6. rejects any reserved-looking rule/route that does not belong to a complete tuple;
7. returns ambiguous-ownership detail without running mutation commands when any check fails.

Keep range checks in shared private predicates based on the existing planner constants; do not duplicate numeric bounds.

- [ ] **Step 5: Implement journaled exact cleanup and resume**

Extend `ensure_reconciled` with an `orphan_cleanup` branch. It must recapture live ownership and require equality with the journal before each deletion. Delete the exact recorded policy rules/routes and only the fingerprint-matching nft table. Persist the remaining exact set after every successful sub-step, then verify all recorded resources are absent and clear the store.

Expose:

```rust
fn recover_orphaned(&mut self) -> Result<OrphanRecoveryResponse, PrivilegedError> {
    self.ensure_reconciled()?;
    let ownership = capture_verified_orphaned_ownership(&mut self.executor)?;
    if ownership.is_empty() {
        return Ok(OrphanRecoveryResponse { cleaned: false, diagnostics: vec![] });
    }
    self.store.save(&StoredAppliedState::orphan_cleanup(ownership.clone()))?;
    resume_orphan_cleanup(&mut self.executor, &mut self.store, ownership)?;
    Ok(OrphanRecoveryResponse { cleaned: true, diagnostics: vec![] })
}
```

- [ ] **Step 6: Add interruption-resume and cgroup boundary tests**

Add tests proving a failure after policy-rule deletion retains the reduced journal and a restarted backend resumes it. In `cgroup.rs`, use a temporary real directory fixture to prove `verified_orphan_directories` returns only empty direct children beneath the configured RouteLane root; a symlink, nested directory, unsafe rule ID, or non-empty `cgroup.procs` must return an error before removal. Test that `remove_verified_orphan_directories` rejects every path not returned by the same preflight.

- [ ] **Step 7: Run backend tests and format**

Run: `rtk cargo test backend::privileged`

Expected: PASS.

Run: `rtk cargo test backend::cgroup`

Expected: PASS.

Run: `rtk cargo fmt --all -- --check`

Expected: exit 0.

- [ ] **Step 8: Commit the backend safety boundary**

```bash
rtk git add src/backend/privileged.rs src/backend/cgroup.rs src/backend/mod.rs
rtk git commit -m "feat: safely recover verified orphaned resources"
```

---

### Task 3: Authorized helper recovery boundary

**Files:**
- Modify: `src/bin/routelane_helper.rs`
- Modify: `tests/root/namespace_routing.rs`
- Modify: `scripts/run-root-integration.sh`
- Test: `src/backend/privileged.rs`

**Interfaces:**
- Consumes: `PrivilegedBackend::recover_orphaned` and `OrphanRecoveryResponse` from Task 2.
- Produces: D-Bus method `RecoverOrphanedResources() -> String` containing the JSON response.
- Produces: `RootIntegrationRequest::RecoverOrphanedResources` for the existing root harness.
- Consumes: helper-owned `ApplicationCgroupManager` orphan preflight/removal methods from Task 2.

- [ ] **Step 1: Write the authorization test**

Extend the existing recording backend and authorizer tests:

```rust
#[tokio::test]
async fn orphan_recovery_requires_mutation_authorization() {
    let service = PrivilegedService::new(DenyingAuthorizer, RecordingBackend::default());

    let error = service.recover_orphaned_resources(Some(":1.42")).await.unwrap_err();

    assert!(matches!(error, ServiceError::Authorization(_)));
    assert_eq!(service.backend_calls(), 0);
}
```

- [ ] **Step 2: Run the test and verify RED**

Run: `rtk cargo test orphan_recovery_requires_mutation_authorization`

Expected: compilation fails because the service method does not exist.

- [ ] **Step 3: Add service and D-Bus methods**

Add:

```rust
pub async fn recover_orphaned_resources(
    &self,
    sender: Option<&str>,
) -> Result<OrphanRecoveryResponse, ServiceError> {
    self.authorize(sender).await?;
    self.run_backend(PrivilegedBackend::recover_orphaned).await
}
```

Before calling the backend, the helper runs `verified_orphan_directories`; this must complete successfully so ambiguous/non-empty cgroups prevent all network mutation. After backend recovery succeeds, it calls `remove_verified_orphan_directories` with exactly the preflight result. Expose the zbus method as `recover_orphaned_resources`, serialize its response exactly like `Cleanup`, and map errors through `to_fdo_error`.

- [ ] **Step 4: Extend the root integration request**

Add `RootIntegrationRequest::RecoverOrphanedResources`, dispatch it to the same service method using `TEST_SENDER`, and add a namespace test that installs a valid orphan fixture, invokes recovery, and proves the RouteLane table/rules/routes are absent while an unrelated table/rule remains.

- [ ] **Step 5: Run focused tests**

Run: `rtk cargo test orphan_recovery_requires_mutation_authorization`

Expected: PASS.

Run: `rtk cargo test --test namespace_routing --no-default-features`

Expected as non-root: the harness reports its documented root skip; no compile failure.

- [ ] **Step 6: Commit the helper boundary**

```bash
rtk git add src/bin/routelane_helper.rs tests/root/namespace_routing.rs scripts/run-root-integration.sh
rtk git commit -m "feat: authorize orphan resource recovery"
```

---

### Task 4: Daemon recovery orchestration and reconciliation reset

**Files:**
- Modify: `src/daemon/service.rs`
- Modify: `tests/ipc_contract.rs`

**Interfaces:**
- Consumes: `DaemonRequest::RecoverOrphanedResources` and `OrphanRecoveryResult` from Task 1.
- Consumes: helper D-Bus `RecoverOrphanedResources` and `OrphanRecoveryResponse` from Tasks 2–3.
- Produces stable recoverable error code `DAEMON_ORPHANED_RESOURCES`.
- Produces `DaemonService::recover_orphaned_resources() -> Result<OrphanRecoveryResult, DaemonError>`.

- [ ] **Step 1: Write daemon contract tests**

Extend the fake helper with a recovery result queue and call counter. Add:

```rust
#[tokio::test]
async fn orphan_recovery_resets_failed_reconciliation_and_allows_apply() {
    let helper = orphaned_then_clean_helper();
    let service = fixture_service(helper.clone());

    let first = service.apply_profile(fixture_profile()).await.unwrap_err();
    assert_eq!(first.code(), "DAEMON_ORPHANED_RESOURCES");

    let recovered = service.recover_orphaned_resources().await.unwrap();
    assert!(recovered.cleaned);
    assert_eq!(helper.recovery_calls(), 1);

    service.apply_profile(fixture_profile()).await.unwrap();
}

#[tokio::test]
async fn failed_orphan_recovery_does_not_reset_reconciliation() {
    let helper = ambiguous_orphan_helper();
    let service = fixture_service(helper.clone());

    service.apply_profile(fixture_profile()).await.unwrap_err();
    let error = service.recover_orphaned_resources().await.unwrap_err();

    assert_eq!(error.code(), "DAEMON_ORPHAN_RECOVERY_AMBIGUOUS");
    assert_eq!(helper.apply_calls(), 0);
}
```

- [ ] **Step 2: Run tests and verify RED**

Run: `rtk cargo test orphan_recovery_ --test ipc_contract`

Expected: compilation fails on the missing helper method and daemon recovery method.

- [ ] **Step 3: Extend `PrivilegedHelperClient` and system D-Bus client**

Add:

```rust
async fn recover_orphaned_resources(
    &self,
) -> Result<OrphanRecoveryResponse, DaemonError>;
```

The system implementation calls `RecoverOrphanedResources`, decodes JSON, and preserves authorization errors through `helper_call_error`.

- [ ] **Step 4: Return a stable recoverable reconciliation error**

Replace only the no-plan/live-resource branch with:

```rust
self.record_reconciliation_drift_with_code(
    None,
    "DAEMON_ORPHANED_RESOURCES",
    "helper reported owned resources without a validated applied plan",
).await
```

All other reconciliation mismatches keep `DAEMON_RECONCILIATION_FAILED`.

- [ ] **Step 5: Implement serialized recovery and gate reset**

Under the daemon mutation lock, require that the last reconciliation error code is `DAEMON_ORPHANED_RESOURCES`. Call helper recovery. Only after success, reset `ReconciliationGate` to default, clear cached applied state/error, and call `ensure_reconciled` once. Return `OrphanRecoveryResult { cleaned }` only if that reconciliation succeeds. Dispatch the new request in `handle`.

- [ ] **Step 6: Run daemon tests**

Run: `rtk cargo test orphan_recovery_ --test ipc_contract`

Expected: PASS.

Run: `rtk cargo test reconciliation --test ipc_contract`

Expected: PASS with existing retry/concurrency behavior unchanged.

- [ ] **Step 7: Commit daemon orchestration**

```bash
rtk git add src/daemon/service.rs tests/ipc_contract.rs
rtk git commit -m "feat: orchestrate approved orphan recovery"
```

---

### Task 5: Pure GTK recovery state and localized confirmation

**Files:**
- Modify: `src/ui/experience.rs`
- Modify: `src/ui/confirm.rs`
- Modify: `src/ui/i18n.rs`
- Modify: `src/ui/mod.rs`

**Interfaces:**
- Produces: `RecoveryAction::{Ignore, AskForConfirmation}` for the one-shot confirmation decision.
- Produces: `orphan_recovery_action(error_code, pending_profile, retry_used)`.
- Produces localized fields `orphan_recovery_heading`, `orphan_recovery_body`, `recover_and_retry`, `orphan_recovery_progress`, and `orphan_recovery_failed`.
- Produces `confirm_destructive_action_with_label(..., confirm_label, on_confirm)` while preserving the existing delete wrapper.

- [ ] **Step 1: Write pure decision tests**

Add tests independent of GTK widgets:

```rust
#[test]
fn only_orphaned_resource_error_requests_recovery_confirmation() {
    assert_eq!(
        orphan_recovery_action("DAEMON_ORPHANED_RESOURCES", false),
        RecoveryAction::AskForConfirmation,
    );
    assert_eq!(
        orphan_recovery_action("DAEMON_RECONCILIATION_FAILED", false),
        RecoveryAction::Ignore,
    );
}

#[test]
fn automatic_retry_cannot_start_a_second_recovery_cycle() {
    assert_eq!(
        orphan_recovery_action("DAEMON_ORPHANED_RESOURCES", true),
        RecoveryAction::Ignore,
    );
}
```

- [ ] **Step 2: Run and verify RED**

Run: `rtk cargo test orphan_recovery_action`

Expected: compilation fails because the decision function and enum do not exist.

- [ ] **Step 3: Implement the minimal pure state function**

Implement exact code matching the two branches and derive `Debug, Clone, Copy, PartialEq, Eq` for the enum. Keep the pending `Profile` in `window.rs`; the pure enum does not clone domain state.

- [ ] **Step 4: Add bilingual copy contract**

Extend `LanguageTexts` and both language literals. Turkish copy must communicate: “Önceki bir RouteLane oturumundan kalan doğrulanabilir ağ kaynakları bulundu. Onaylarsanız yalnız RouteLane’e ait olduğu doğrulanan kaynaklar temizlenecek ve seçili profil yeniden etkinleştirilecek.” English copy must convey the same safety boundary. Assert every new field is non-empty for both languages.

- [ ] **Step 5: Generalize the confirmation label without breaking delete dialogs**

Add `confirm_destructive_action_with_label`. Keep `confirm_destructive_action` as a wrapper passing `texts.delete`, so rule/lane deletion call sites do not change. The recovery caller will pass `texts.recover_and_retry`.

- [ ] **Step 6: Run UI unit tests**

Run: `rtk cargo test ui::`

Expected: PASS.

- [ ] **Step 7: Commit UI state and copy**

```bash
rtk git add src/ui/experience.rs src/ui/confirm.rs src/ui/i18n.rs src/ui/mod.rs
rtk git commit -m "feat: add orphan recovery confirmation state"
```

---

### Task 6: GTK confirmation, recovery progress, and one-shot retry

**Files:**
- Modify: `src/models.rs`
- Modify: `src/main.rs`
- Modify: `src/ui/window.rs`
- Test: `src/main.rs`
- Test: `src/ui/window.rs`

**Interfaces:**
- Consumes: `DaemonResult::OrphanedResourcesRecovered` from Task 1.
- Consumes: recovery decision and localized copy from Task 5.
- Produces: `DaemonToUi::OrphanedResourcesRecovered(OrphanRecoveryResult)`.
- Maintains window state `pending_recovery_profile: Rc<RefCell<Option<Profile>>>` and `recovery_retry_used: Rc<Cell<bool>>`.

- [ ] **Step 1: Write bridge and retry-state tests**

Add a `forward_response` test proving the recovery result maps to the dedicated UI event. Extract a pure transition helper in `window.rs` and test:

```rust
#[test]
fn successful_recovery_consumes_the_pending_profile_for_one_retry() {
    let profile = fixture_profile();
    let mut pending = Some(profile.clone());
    let request = retry_after_recovery(&mut pending, false).unwrap();

    assert_eq!(request, DaemonRequest::ApplyProfile(profile));
    assert!(pending.is_none());
    assert!(retry_after_recovery(&mut pending, true).is_none());
}
```

- [ ] **Step 2: Run and verify RED**

Run: `rtk cargo test successful_recovery_consumes_the_pending_profile_for_one_retry`

Expected: compilation fails because the bridge variant and retry helper do not exist.

- [ ] **Step 3: Bridge the daemon result**

Add the `DaemonToUi` variant and map it in `forward_response`. Update exhaustive test fixtures in `main.rs` to include `RecoverOrphanedResources` and its response.

- [ ] **Step 4: Preserve the activation candidate and show confirmation**

Whenever the UI sends `ApplyProfile`, retain that exact enabled candidate until success or a nonrecoverable failure. On `DaemonToUi::Error` with `DAEMON_ORPHANED_RESOURCES` and `recovery_retry_used == false`, clear the normal raw-error toast, release the generic mutation-pending state, and call `confirm_destructive_action_with_label`.

Cancel clears the pending recovery candidate and restores inactive controls. Confirm sets progress feedback, disables activation controls, and sends only `DaemonRequest::RecoverOrphanedResources`.

- [ ] **Step 5: Retry exactly once after recovery success**

On `DaemonToUi::OrphanedResourcesRecovered`, consume the saved candidate, set `recovery_retry_used = true`, render `orphan_recovery_progress`, and send the resulting `ApplyProfile`. On `Applied`, clear pending recovery state and reset the retry flag for the next user-initiated activation. If the retried apply fails, show normal failure feedback and clear state without reopening confirmation.

- [ ] **Step 6: Preserve accessibility and keyboard behavior**

Use Cancel as the dialog default widget and initial focus, add a destructive-action CSS class only to “Temizle ve yeniden dene”, keep labels wrapping with left alignment, and restore focus to the activation control after cancel/failure.

- [ ] **Step 7: Run focused UI and bridge tests**

Run: `rtk cargo test ui::`

Expected: PASS.

Run: `rtk cargo test main --bin routelane`

Expected: PASS.

- [ ] **Step 8: Commit the GTK flow**

```bash
rtk git add src/models.rs src/main.rs src/ui/window.rs
rtk git commit -m "feat: confirm cleanup and retry activation"
```

---

### Task 7: End-to-end verification and documentation alignment

**Files:**
- Verify: all files changed in Tasks 1–6

**Interfaces:**
- Consumes the completed recovery flow.
- Produces verification evidence and a clean, reviewable diff.

- [ ] **Step 1: Run formatting and static analysis**

Run: `rtk cargo fmt --all -- --check`

Expected: exit 0.

Run: `rtk cargo clippy --all-targets --all-features -- -D warnings`

Expected: exit 0 with no warnings.

- [ ] **Step 2: Run the complete non-root test suite**

Run: `rtk cargo test`

Expected: all suites pass with zero failures.

- [ ] **Step 3: Run the privileged namespace integration**

Run: `rtk scripts/run-root-integration.sh`

Expected: the existing apply/rollback cases and the new orphan recovery case pass as root. If escalation is unavailable, report this test as not run rather than treating the non-root skip as a pass.

- [ ] **Step 4: Audit error and localization coverage**

Run: `rtk rg -n "DAEMON_ORPHANED_RESOURCES|DAEMON_ORPHAN_RECOVERY|orphan_recovery|RecoverOrphanedResources" src tests docs/superpowers`

Expected: the recoverable code appears in daemon and UI decision logic; helper failures have distinct codes/messages; both languages contain all new fields; tests cover request, refusal, approval and one-shot retry.

- [ ] **Step 5: Inspect diff boundaries**

Run: `rtk git diff --check`

Expected: exit 0.

Run: `rtk git status --short`

Expected: only intended recovery changes plus the pre-existing unrelated `src/application/desktop.rs` change, if it has not already been committed separately.
