# Partial Cleanup Reconciliation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow normal cleanup to finish when live RouteLane routing resources are an exact subset of the validated journaled plan.

**Architecture:** Replace cleanup command-prefix fingerprint membership with explicit set-subset ownership validation while retaining exact journal and nftables fingerprint requirements. Continue using the existing idempotent restore-to-empty transaction.

**Tech Stack:** Rust 2021, serde, nftables/iproute2 backend tests, Cargo and Clippy.

## Global Constraints

- Require a validated active or cleanup-pending journal.
- Require the exact committed nftables marker and fingerprint.
- Accept only missing resources; reject added or modified resources before mutation.
- Preserve cleanup resumption and orphan recovery behavior.

---

### Task 1: Test partial exact-subset cleanup

**Files:**
- Modify: `src/backend/privileged.rs`
- Test: `src/backend/privileged.rs`

**Interfaces:**
- Consumes existing `RoutingOwnership { routes, rules }` and `plan_routing_ownership`.
- Produces private `routing_ownership_is_subset(live, expected) -> bool`.

- [ ] **Step 1: Write the failing regression test**

Apply `fixture_valid_plan()` with `FakeCommandExecutor`, set only
`FakeKernelState::has_route = false`, then call normal `cleanup()` on the same
backend. Assert `cleaned == true` and that rule, route and table flags are false.

- [ ] **Step 2: Verify RED**

Run: `rtk cargo test cleanup_accepts_exact_rules_after_routes_were_already_removed`

Expected: FAIL with `cleanup journal does not match an exact RouteLane cleanup command prefix`.

- [ ] **Step 3: Implement explicit subset validation**

```rust
fn routing_ownership_is_subset(live: &RoutingOwnership, expected: &RoutingOwnership) -> bool {
    live.routes.is_subset(&expected.routes) && live.rules.is_subset(&expected.rules)
}
```

In `verify_cleanup_table_ownership`, retain exact equality for normal pre-cleanup
validation when `allow_command_prefix == false`. When it is true, compare the
live ownership with `plan_routing_ownership(active.plan())` using the helper.
Remove `cleanup_routing_fingerprints` and `push_routing_fingerprint` only after
all call sites are gone.

- [ ] **Step 4: Verify GREEN and safety regressions**

Run: `rtk cargo test cleanup_accepts_exact_rules_after_routes_were_already_removed`

Expected: PASS.

Run: `rtk cargo test cleanup_ backend::privileged`

Expected: all cleanup, foreign-resource, fingerprint and restart tests pass.

- [ ] **Step 5: Commit**

```bash
rtk git add src/backend/privileged.rs
rtk git commit -m "fix: reconcile exact partial cleanup state"
```

---

### Task 2: Full verification

**Files:**
- Verify: `src/backend/privileged.rs`

- [ ] **Step 1: Run format and Clippy**

Run: `rtk cargo fmt --all -- --check`

Expected: exit 0.

Run: `rtk cargo clippy --all-targets --all-features -- -D warnings`

Expected: no warnings.

- [ ] **Step 2: Run the full suite serially**

Run: `rtk cargo test -- --test-threads=1`

Expected: all tests pass; serial execution avoids the known sandbox Unix-socket parallel-test restriction.

- [ ] **Step 3: Build release binaries and inspect the tree**

Run: `rtk cargo build --release --bins`

Expected: exit 0.

Run: `rtk git diff --check`

Expected: exit 0.
