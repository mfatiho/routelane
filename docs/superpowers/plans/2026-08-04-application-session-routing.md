# Application Session Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow a user to select a desktop application, launch it through RouteLane or adopt an existing matching process, and route all of that application's traffic through its selected lane.

**Architecture:** Reuse `RuleSource::Application` with `RuleTarget::Any` and `RuleAction::Route`. The daemon owns desktop-entry discovery and user-process lifecycle; the privileged helper owns a per-user delegated cgroup subtree and validates all PID adoption requests. The kernel plan compiles application sources into cgroup selectors that nftables evaluates before destination rules.

**Tech Stack:** Rust, GTK4/Libadwaita, Tokio, zbus system D-Bus, cgroup v2, nftables, iproute2, systemd user sessions.

## Global Constraints

- Do not inspect, decrypt, proxy, or alter HTTPS traffic.
- Preserve existing domain, IP/CIDR, Direct, and Block rules.
- Supported first-release distributions are Ubuntu, Debian, and Fedora with cgroup v2 and nftables `socket cgroupv2` support.
- Application rules must target `Any` and use `Route(existing LaneId)`; reject all other combinations.
- Application cgroup marks have precedence over destination marks.
- The helper may touch only `/sys/fs/cgroup/routelane.apps`; it must validate caller UID, PID start time, and `/proc/PID/exe` before adopting a process.
- Do not add a runtime dependency merely for desktop entry parsing or process inspection.

## File Structure

- Create: `src/application/mod.rs` — desktop-entry, command, process, and cgroup-safe value types.
- Create: `src/application/desktop.rs` — XDG desktop-entry discovery and safe `Exec` parsing.
- Create: `src/application/process.rs` — `/proc` snapshots and executable/start-time matching.
- Create: `src/backend/cgroup.rs` — constrained cgroup preparation, delegation, PID validation, adoption, and cleanup.
- Modify: `src/domain/rule.rs`, `src/domain/profile.rs` — application-rule validation and IDs.
- Modify: `src/backend/plan.rs`, `src/backend/planner.rs`, `src/backend/nftables.rs` — compile and render cgroup source matches before destination rules.
- Modify: `src/backend/privileged.rs`, `src/bin/routelane_helper.rs` — privileged cgroup operations and D-Bus methods.
- Modify: `src/daemon/service.rs`, `src/ipc/protocol.rs`, `src/main.rs` — launch/adopt IPC and daemon orchestration.
- Modify: `src/ui/window.rs`, `src/ui/rule_row.rs`, `src/ui/i18n.rs` — application picker, lane selection, launch/adopt actions, and accessible status copy.

---

### Task 1: Validate application rules in the domain model

**Files:**
- Modify: `src/domain/rule.rs`
- Modify: `src/domain/profile.rs`

**Consumes:** Existing `ApplicationSelector`, `RuleSource`, `RuleTarget`, and `RuleAction`.

**Produces:** `validate_profile` rejects invalid application rule combinations with a specific `DomainError`.

- [ ] **Step 1: Write failing validation tests**

```rust
#[test]
fn application_rule_requires_any_target_and_route_action() {
    let mut profile = fixture_profile_with_lane();
    profile.rules.push(application_rule(
        RuleTarget::Domain("example.com".into()),
        RuleAction::Route(LaneId::from("vinn")),
    ));
    assert!(matches!(validate_profile(&profile), Err(DomainError::InvalidApplicationRule(_))));
}
```

- [ ] **Step 2: Run the focused test and confirm RED**

Run: `rtk cargo test domain::profile::tests::application_rule_requires_any_target_and_route_action --lib`

Expected: FAIL because `InvalidApplicationRule` and the validation rule do not exist.

- [ ] **Step 3: Implement minimal validation**

```rust
if matches!(rule.source, RuleSource::Application(_))
    && (!matches!(rule.target, RuleTarget::Any)
        || !matches!(rule.action, RuleAction::Route(_))) {
    return Err(DomainError::InvalidApplicationRule(rule.id.clone()));
}
```

Also reject an empty `ApplicationSelector::executable_path` and duplicate non-empty `desktop_id` values.

- [ ] **Step 4: Run domain tests and confirm GREEN**

Run: `rtk cargo test domain:: --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/domain/rule.rs src/domain/profile.rs
git commit -m "feat: validate application routing rules"
```

### Task 2: Add desktop-entry and process discovery primitives

**Files:**
- Create: `src/application/mod.rs`
- Create: `src/application/desktop.rs`
- Create: `src/application/process.rs`
- Modify: `src/lib.rs`

**Consumes:** A canonical executable path from `ApplicationSelector`.

**Produces:** `discover_desktop_apps() -> Vec<DesktopApp>` and `matching_processes(&ApplicationSelector) -> Vec<RunningProcess>`.

- [ ] **Step 1: Write failing unit tests using temporary XDG and proc-reader fixtures**

```rust
#[test]
fn desktop_discovery_ignores_hidden_entries_and_extracts_exec_path() {
    let apps = discover_from_paths(&[fixture_dir("applications")]).unwrap();
    assert_eq!(apps, vec![DesktopApp::new("org.example.Browser", "Browser", "/usr/bin/browser")]);
}

#[test]
fn process_matching_requires_the_same_executable_and_start_time() {
    assert_eq!(matching_from_snapshot(&selector(), &snapshot()), vec![running_process(42)]);
}
```

- [ ] **Step 2: Run focused tests and confirm RED**

Run: `rtk cargo test application:: --lib`

Expected: FAIL because the application module is absent.

- [ ] **Step 3: Implement dependency-free discovery and matching**

Implement `DesktopApp { desktop_id, display_name, executable_path }`, parse only `Type=Application`, `Hidden`, `NoDisplay`, `Name`, and `Exec`, reject shell metacharacters in executable paths, and canonicalize executable paths. Read `/proc/<pid>/exe` and `/proc/<pid>/stat` through injectable readers so production access and fixture testing share matching code.

- [ ] **Step 4: Run focused tests and confirm GREEN**

Run: `rtk cargo test application:: --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/application src/lib.rs
git commit -m "feat: discover desktop applications and processes"
```

### Task 3: Compile application sources into explicit kernel selectors

**Files:**
- Modify: `src/backend/plan.rs`
- Modify: `src/backend/planner.rs`
- Modify: `src/backend/nftables.rs`

**Consumes:** A validated application rule and resolved lane mark.

**Produces:** `CompiledTarget::Cgroup(String)` and an nft rule rendered before all destination rules.

- [ ] **Step 1: Write failing planner and renderer tests**

```rust
#[test]
fn application_rule_compiles_to_its_lane_mark() {
    let plan = compile_profile(&application_profile(), &topology(), &ResolutionSnapshot::empty(), 7).unwrap();
    assert!(matches!(plan.rules[0].target, CompiledTarget::Cgroup(_)));
    assert_eq!(plan.rules[0].action, CompiledAction::Mark(MARK_BASE));
}

#[test]
fn renderer_places_cgroup_mark_before_destination_mark() {
    let ruleset = render_ruleset(&plan_with_application_and_domain_rules(), TableMode::Create).unwrap();
    assert!(ruleset.find("socket cgroupv2").unwrap() < ruleset.find("ip daddr").unwrap());
}
```

- [ ] **Step 2: Run focused tests and confirm RED**

Run: `rtk cargo test backend::planner::tests backend::nftables::tests --lib`

Expected: FAIL because compiled sources do not yet exist.

- [ ] **Step 3: Implement source-aware compilation and rendering**

Add `CompiledTarget::Cgroup(String)` to `CompiledRule`. Derive the relative path only from a validated rule ID, render the nftables `socket cgroupv2` expression using an escaped fixed RouteLane subtree path, then append `meta mark set ... counter return`. Keep the existing destination target output byte-for-byte equivalent.

- [ ] **Step 4: Run focused tests and confirm GREEN**

Run: `rtk cargo test backend::planner::tests backend::nftables::tests --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/backend/plan.rs src/backend/planner.rs src/backend/nftables.rs
git commit -m "feat: compile application cgroup routing rules"
```

### Task 4: Implement constrained privileged cgroup operations

**Files:**
- Create: `src/backend/cgroup.rs`
- Modify: `src/backend/privileged.rs`

**Consumes:** `KernelPlan`, caller UID, application rule ID, PID, process start time, and canonical executable path.

**Produces:** `prepare_application_cgroups`, `adopt_application_process`, and `cleanup_application_cgroups` with typed errors.

- [ ] **Step 1: Write failing filesystem-backed unit tests**

```rust
#[test]
fn adoption_rejects_pid_when_executable_or_start_time_differs() {
    let error = adopt_with_fixture(42, 101, "/usr/bin/browser").unwrap_err();
    assert!(matches!(error, CgroupError::ProcessIdentityMismatch));
}

#[test]
fn cleanup_never_removes_non_routelane_cgroups() {
    assert!(cleanup_with_fixture("../../user.slice").is_err());
}
```

- [ ] **Step 2: Run focused tests and confirm RED**

Run: `rtk cargo test backend::cgroup::tests --lib`

Expected: FAIL because the cgroup module is absent.

- [ ] **Step 3: Implement the helper-only cgroup boundary**

Use a configurable filesystem root only in tests and `/sys/fs/cgroup/routelane.apps` in production. Create rule-ID child directories with `create_dir`, set ownership to the D-Bus caller UID for launch delegation, read process identity before writing its PID to `cgroup.procs`, and remove only empty, validated child directories. Never accept an arbitrary cgroup path.

- [ ] **Step 4: Run focused tests and confirm GREEN**

Run: `rtk cargo test backend::cgroup::tests --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/backend/cgroup.rs src/backend/privileged.rs
git commit -m "feat: manage application routing cgroups safely"
```

### Task 5: Expose launch preparation and existing-process adoption over IPC

**Files:**
- Modify: `src/bin/routelane_helper.rs`
- Modify: `src/daemon/service.rs`
- Modify: `src/ipc/protocol.rs`

**Consumes:** Task 2 process snapshots and Task 4 typed cgroup operations.

**Produces:** `DaemonRequest::LaunchApplication` and `DaemonRequest::AdoptApplicationProcesses` plus result/status values.

- [ ] **Step 1: Write failing protocol and daemon tests**

```rust
#[test]
fn launch_request_serializes_only_a_profile_and_rule_id() {
    let request = DaemonRequest::LaunchApplication { profile_id: id("default"), rule_id: id("browser") };
    assert!(serde_json::to_string(&request).unwrap().contains("LaunchApplication"));
}

#[tokio::test]
async fn adoption_sends_verified_process_identity_to_helper() {
    let result = service.adopt_application_processes(id("default"), id("browser")).await.unwrap();
    assert_eq!(result.adopted_count, 1);
}
```

- [ ] **Step 2: Run focused tests and confirm RED**

Run: `rtk cargo test ipc:: --lib`

Then run: `rtk cargo test daemon::service::tests --lib`

Expected: FAIL because these requests and results do not exist.

- [ ] **Step 3: Implement narrow D-Bus and daemon operations**

Add helper methods that receive only plan-owned rule IDs plus typed PID identity fields; authorize mutations with existing Polkit checks. For launch, daemon resolves the configured desktop command, requests a delegated cgroup for the active caller, starts a small RouteLane-owned launcher that joins the cgroup before `exec`, and returns the spawned PID. For adoption, daemon discovers matches, submits each identity to helper, and returns adopted/skipped counts without exposing raw helper errors to the UI.

- [ ] **Step 4: Run focused tests and confirm GREEN**

Run: `rtk cargo test ipc:: --lib`

Then run: `rtk cargo test daemon::service::tests --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/routelane_helper.rs src/daemon/service.rs src/ipc/protocol.rs
git commit -m "feat: launch and adopt application routing sessions"
```

### Task 6: Add the accessible GTK application-policy UI

**Files:**
- Modify: `src/ui/window.rs`
- Modify: `src/ui/rule_row.rs`
- Modify: `src/ui/i18n.rs`

**Consumes:** `DesktopApp`, application-source rules, and Task 5 daemon requests.

**Produces:** Application picker, lane selector, enable switch, launch/adopt actions, and persistent status copy in Turkish and English.

- [ ] **Step 1: Write failing UI contract tests**

```rust
#[test]
fn application_rule_row_exposes_launch_and_adopt_actions() {
    let presentation = application_rule_presentation(&application_rule());
    assert!(presentation.launch_accessible_label.contains("Browser"));
    assert!(presentation.adopt_visible);
}
```

- [ ] **Step 2: Run focused tests and confirm RED**

Run: `rtk cargo test ui:: --lib`

Expected: FAIL because application-rule presentation is absent.

- [ ] **Step 3: Implement minimal GTK flow**

Show desktop applications by display name and allow a validated executable-path fallback. Persist an application source rule with `Any` target and selected route lane. Display the exact lane effect, disable actions while IPC is pending, expose launch/adopt buttons with accessible labels, and show success, no-process, incompatible-cgroup, authorization, and failure states in the row.

- [ ] **Step 4: Run focused tests and confirm GREEN**

Run: `rtk cargo test ui:: --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/ui/window.rs src/ui/rule_row.rs src/ui/i18n.rs
git commit -m "feat: add application lane selection UI"
```

### Task 7: Verify platform behavior and document user-facing semantics

**Files:**
- Modify: `docs/commercial-roadmap.md`
- Modify: `README.md`
- Modify: `tests/root/namespace_routing.rs`

**Consumes:** All preceding tasks.

**Produces:** A root integration test proving cgroup traffic selects the expected lane mark and concise support documentation.

- [ ] **Step 1: Write the failing root integration assertion**

```rust
#[test]
fn application_cgroup_rule_is_present_before_destination_rules() {
    let ruleset = apply_fixture_application_plan();
    assert_rule_precedes(&ruleset, "socket cgroupv2", "ip daddr");
}
```

- [ ] **Step 2: Run the integration test and confirm RED**

Run: `rtk cargo test --features root-integration root_integration::application_cgroup_rule_is_present_before_destination_rules -- --nocapture`

Expected: FAIL until the helper and renderer are connected end-to-end.

- [ ] **Step 3: Complete only missing integration wiring and documentation**

Document that application routing covers all traffic from the selected app, including browser tabs and third-party verification endpoints; describe launch, adoption, supported distributions, Flatpak/Snap limitation, and lane-failure behavior. Do not claim third-party dependency inference.

- [ ] **Step 4: Run release-quality verification**

Run: `rtk cargo fmt --check`

Then run: `rtk cargo test`

Then run: `rtk cargo clippy --all-targets -- -D warnings`

Then run: `rtk cargo build --release`

Expected: all commands PASS with no warnings.

- [ ] **Step 5: Commit**

```bash
git add docs/commercial-roadmap.md README.md tests/root/namespace_routing.rs src
git commit -m "docs: explain application session routing"
```
