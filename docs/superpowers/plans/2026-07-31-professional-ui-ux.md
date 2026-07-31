# Professional UI/UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a single-window, guided GTK experience for setup, routing,
rule creation and Polkit-backed authorization.

**Architecture:** Routing remains in the existing daemon/helper boundary. A
pure UI experience model decides setup, primary action and authorization state;
`window.rs` renders it with Libadwaita widgets. `main.rs` reuses an existing
application window rather than creating another one on activation.

**Tech Stack:** Rust, GTK4, Libadwaita, async-channel, Tokio, system D-Bus,
Polkit, existing `ConfigV2` persistence and daemon IPC.

## Global Constraints

- GUI never runs `sudo`, stores credentials or bypasses system D-Bus + Polkit.
- Keep helper action `io.github.routelane.apply` and `auth_admin_keep`; the
  helper's `CheckAuthorization(..., 1, ...)` requests interaction.
- Preserve `ConfigV2`, profiles, rules, lanes, helper validation and IPC.
- Add all visible copy in both Turkish and English `Texts` variants.
- Primary controls must be textual, focusable and accessible; colour is never
  the sole status signal.
- Follow TDD: observe each behavior test RED before production code.
- Every shell command begins with `rtk`.

---

## File Structure

- Create `src/ui/experience.rs`: pure setup, activation and authorization
  decisions with narrow unit tests.
- Modify `src/ui/mod.rs`: export `experience`.
- Modify `src/main.rs`: single-window activation.
- Modify `src/ui/i18n.rs`: bilingual dashboard/authentication copy.
- Modify `src/ui/window.rs`: dashboard, guided setup, rule dialog and pending
  mutation presentation; retain existing persistence and IPC ordering.
- Modify `README.md`: first-run, Polkit and single-instance guidance.

## Status (2026-07-31, follow-up review)

Tasks 1, 2 and 4 shipped and hold up under `cargo fmt --check`, `cargo test`,
and `clippy -D warnings`: commits `a60ba4f`, `76417e0`, `5cc3278`, `e23a9cd`,
`8938c69`, plus `ca3fe74` (this session), which fixed a gap Task 4 required
but the earlier commits missed — the primary button and header switch could
each fire a second `ApplyProfile`/`Deactivate` before the daemon confirmed
the first one, because nothing disabled them at click time, only on the
next daemon event. `mutation_pending` is now set and both controls disabled
synchronously in the click/switch handlers, not just after the fact.

Task 3 shipped its state machine and copy but not its central deliverable:
the plan called for replacing the always-visible inline add row with an
`adw::Window`/`adw::MessageDialog`. What exists instead is
`PrimaryAction::OpenRuleDialog` calling `target.grab_focus()` on the same
old inline row (`window.rs` add_row, ~lines 348-370). Section order also
does not match spec §2: the health group renders above the rules group and
there is no active-lane summary row. See Task 6 below for the remaining
scope. Task 5 (README + release/deb build + final verification) was never
started.

### Task 1: Pure UI experience model

**Files:**
- Create: `src/ui/experience.rs`
- Modify: `src/ui/mod.rs`
- Test: `src/ui/experience.rs`

**Interfaces:**
- Consumes: `Profile`, `HealthState`.
- Produces: `SetupStep`, `PrimaryAction`, `AuthorizationPresentation`,
  `DashboardState`, `dashboard_state`, `ActivationAction`, `activation_action`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn dashboard_guides_lane_then_rule_then_enable() {
    assert_eq!(dashboard_state(&empty_profile(), HealthState::Inactive, false).setup_step,
        Some(SetupStep::AddLane));
    assert_eq!(dashboard_state(&profile_with_lane(), HealthState::Inactive, false).setup_step,
        Some(SetupStep::AddRule));
    assert_eq!(dashboard_state(&profile_with_lane_and_rule(), HealthState::Inactive, false).primary_action,
        PrimaryAction::EnableRouting);
}

#[test]
fn pending_mutation_waits_for_authorization_and_blocks_repeat_action() {
    let state = dashboard_state(&profile_with_lane_and_rule(), HealthState::Inactive, true);
    assert_eq!(state.authorization, AuthorizationPresentation::Waiting);
    assert!(!state.primary_action_enabled);
}

#[test]
fn second_activation_presents_instead_of_creating() {
    assert_eq!(activation_action(true), ActivationAction::PresentExisting);
    assert_eq!(activation_action(false), ActivationAction::CreateWindow);
}
```

- [ ] **Step 2: Verify RED**

Run: `rtk cargo test --bin routelane ui::experience::tests -- --nocapture`

Expected: FAIL because the module and requested types do not exist.

- [ ] **Step 3: Implement the minimal model**

```rust
pub enum SetupStep { AddLane, AddRule }
pub enum PrimaryAction { OpenPreferences, OpenRuleDialog, EnableRouting, DisableRouting }
pub enum AuthorizationPresentation { Idle, Waiting, Denied }
pub enum ActivationAction { PresentExisting, CreateWindow }

pub fn activation_action(has_existing_window: bool) -> ActivationAction {
    if has_existing_window { ActivationAction::PresentExisting } else { ActivationAction::CreateWindow }
}
```

Implement `dashboard_state`: no lane selects `AddLane`, no rule selects
`AddRule`, active health selects `DisableRouting`, ready inactive profile
selects `EnableRouting`, and any pending mutation disables the primary action.

- [ ] **Step 4: Verify GREEN**

Run: `rtk cargo test --bin routelane ui::experience::tests -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
rtk git add src/ui/experience.rs src/ui/mod.rs
rtk git commit -m "feat: model guided routing experience"
```

### Task 2: Single instance and localized action copy

**Files:**
- Modify: `src/main.rs`
- Modify: `src/ui/i18n.rs`
- Test: `src/ui/experience.rs`

**Interfaces:**
- Consumes: `activation_action` from Task 1.
- Produces: `present_or_build_window` behavior and dashboard `Texts` fields.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn every_language_has_actionable_setup_and_auth_copy() {
    for texts in [Language::Tr.texts(), Language::En.texts()] {
        assert!(!texts.enable_routing.is_empty());
        assert!(!texts.add_first_lane.is_empty());
        assert!(!texts.authorization_waiting.is_empty());
        assert!(!texts.authorization_denied.is_empty());
    }
}
```

- [ ] **Step 2: Verify RED**

Run: `rtk cargo test --bin routelane ui::experience::tests -- --nocapture`

Expected: FAIL because the four `Texts` fields do not exist.

- [ ] **Step 3: Implement activation and copy**

Add `enable_routing`, `disable_routing`, `add_first_lane`, `add_first_rule`,
`open_preferences`, `add_rule`, `authorization_waiting`,
`authorization_denied`, and `retry_authorization` to both language variants.
Replace unconditional window creation in `connect_activate` with:

```rust
if let Some(window) = app.active_window() {
    window.present();
    return;
}
ui::window::build_window(app, sender.clone(), receiver.clone());
```

Only the no-window branch creates an `ApplicationWindow`; desktop-launch and
re-activation therefore focus the same window.

- [ ] **Step 4: Verify GREEN**

Run: `rtk cargo test --bin routelane ui::experience::tests -- --nocapture`

Run: `rtk cargo check --bin routelane`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
rtk git add src/main.rs src/ui/i18n.rs src/ui/experience.rs
rtk git commit -m "feat: keep RouteLane to one window"
```

### Task 3: Guided dashboard and explicit rule dialog

**Files:**
- Modify: `src/ui/window.rs`
- Modify: `src/ui/i18n.rs`
- Test: `src/ui/window.rs`

**Interfaces:**
- Consumes: `DashboardState`, `SetupStep`, `PrimaryAction` from Task 1.
- Produces: `setup_action`, `rule_dialog_submission`, dashboard render/update
  hooks and `open_rule_dialog`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn rule_dialog_rejects_invalid_input_without_persistence() {
    assert_eq!(rule_dialog_submission("not a valid target!"), RuleDialogSubmission::Invalid);
}

#[test]
fn rule_dialog_accepts_domain_ip_and_cidr_targets() {
    assert!(matches!(rule_dialog_submission("example.com"), RuleDialogSubmission::Valid(_)));
    assert!(matches!(rule_dialog_submission("203.0.113.8"), RuleDialogSubmission::Valid(_)));
    assert!(matches!(rule_dialog_submission("2001:db8::/32"), RuleDialogSubmission::Valid(_)));
}
```

- [ ] **Step 2: Verify RED**

Run: `rtk cargo test --bin routelane ui::window::tests -- --nocapture`

Expected: FAIL because `RuleDialogSubmission` and its parser wrapper are absent.

- [ ] **Step 3: Implement dashboard and dialog**

Replace the always-visible inline add controls with ordered sections:

1. Status card: localized state, selected lane summary and one textual primary
   button.
2. Setup card only for `AddLane`/`AddRule`; it opens Preferences or the rule
   dialog respectively.
3. Rules group with textual **Kural ekle / Add rule** button.
4. Existing health group, keeping action summary visible and technical details
   in its expander.

Use an `adw::MessageDialog` or `adw::Window` rule dialog with target, action
and priority controls. Implement the narrow parser wrapper:

```rust
enum RuleDialogSubmission { Invalid, Valid(RuleTarget) }
fn rule_dialog_submission(value: &str) -> RuleDialogSubmission {
    parse_target(value).map(RuleDialogSubmission::Valid).unwrap_or(RuleDialogSubmission::Invalid)
}
```

Invalid text gets localized inline feedback and retains focus/text. Valid input
uses existing `save_profile` and persistence-before-apply ordering, then closes
the dialog. Do not change `apply_request`, `store_profile`, rule-row editing,
or daemon contracts.

- [ ] **Step 4: Verify GREEN**

Run: `rtk cargo test --bin routelane ui::window::tests -- --nocapture`

Expected: PASS, including existing persistence/revision/profile tests.

- [ ] **Step 5: Commit**

```bash
rtk git add src/ui/window.rs src/ui/i18n.rs
rtk git commit -m "feat: guide RouteLane setup from dashboard"
```

### Task 4: Polkit feedback and mutation safety

**Files:**
- Modify: `src/ui/experience.rs`
- Modify: `src/ui/window.rs`
- Modify: `src/ui/i18n.rs`
- Test: `src/ui/experience.rs`
- Test: `src/ui/window.rs`

**Interfaces:**
- Consumes: `DaemonToUi::{Applied, Deactivated, Error, Status, Event}` and
  `HealthState::NeedsAuthorization`.
- Produces: authorization transition helpers and shared `mutation_pending`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn authorization_denial_reenables_control_without_changing_confirmed_state() {
    let outcome = authorization_after_error(true, true);
    assert_eq!(outcome.authorization, AuthorizationPresentation::Denied);
    assert!(outcome.primary_action_enabled);
    assert!(outcome.routing_active);
}

#[test]
fn accepted_apply_clears_waiting_state() {
    assert_eq!(authorization_after_applied(), AuthorizationPresentation::Idle);
}
```

- [ ] **Step 2: Verify RED**

Run: `rtk cargo test --bin routelane ui::experience::tests -- --nocapture`

Expected: FAIL because authorization transition helpers are absent.

- [ ] **Step 3: Implement explicit authorization feedback**

Before `ApplyProfile`, `Deactivate`, `SetRuleEnabled`, or an active-profile
definition apply, set `mutation_pending = true`, render
`authorization_waiting`, disable dashboard/rule actions, then send the
existing daemon request. Reject a second mutation while pending.

On `Applied`, `Deactivated` or terminal `Status`, clear pending. On an
authorization `IpcError` or `NeedsAuthorization`, clear pending, keep the last
daemon-confirmed switch state, render `authorization_denied`, and re-enable
the same primary action as retry. Do not add `sudo`, pkexec, password storage,
or a helper API; existing Polkit is the only authorization mechanism.

- [ ] **Step 4: Verify GREEN**

Run: `rtk cargo test --bin routelane ui::experience::tests -- --nocapture`

Run: `rtk cargo test --bin routelane ui::window::tests -- --nocapture`

Expected: PASS; pending mutations cannot duplicate and denial cannot falsely
change routing state.

- [ ] **Step 5: Commit**

```bash
rtk git add src/ui/experience.rs src/ui/window.rs src/ui/i18n.rs
rtk git commit -m "feat: explain privileged routing authorization"
```

### Task 5: User documentation and final verification

**Files:**
- Modify: `README.md`
- Test: all Rust targets

**Interfaces:**
- Consumes: completed Tasks 1–4.
- Produces: first-run, authorization and single-window documentation.

- [ ] **Step 1: Document user journey**

Add this concise README flow:

```markdown
1. Open RouteLane; if it is already running, its existing window is focused.
2. Choose a lane/interface in Preferences when the setup card asks for one.
3. Add a domain, IP, or CIDR rule, then select Enable routing.
4. Complete the desktop Polkit prompt when RouteLane needs to modify routing.
```

State that the application never asks for or stores a sudo password, Polkit
controls authorization lifetime, and a missing desktop Polkit agent yields
`NeedsAuthorization` rather than silent escalation.

- [ ] **Step 2: Run final verification**

Run: `rtk cargo fmt --all -- --check`

Run: `rtk cargo test --lib --bins --tests --no-fail-fast`

Run: `rtk cargo clippy --all-targets --all-features -- -D warnings`

Run: `rtk cargo build --release --bins`

Run: `rtk ./packaging/deb/build-deb.sh`

Run: `rtk git diff --check`

Expected: all PASS. If the sandbox denies Unix sockets, run the complete suite
outside that sandbox and record the result; never claim root namespace runtime
PASS without UID 0.

- [ ] **Step 3: Commit**

```bash
rtk git add README.md src/ui/experience.rs
rtk git commit -m "docs: explain guided RouteLane workflow"
```

### Task 6: Build the real rule dialog and fix dashboard section order

Do this task before Task 5's final verification step — Task 5 was never
run and should be the last thing done, after this task's changes land.

**Files:**
- Modify: `src/ui/window.rs`
- Modify: `src/ui/i18n.rs`

**Interfaces:**
- Consumes: `rule_dialog_submission`, `PrimaryAction::OpenRuleDialog`,
  `dashboard_state` (all already implemented in `src/ui/experience.rs` and
  `src/ui/window.rs` — do not change their signatures).
- Produces: an `open_rule_dialog` function that presents a modal dialog, and
  a reordered dashboard page.

- [ ] **Step 1: Replace the inline add row with a modal dialog**

The rules group currently keeps a permanently visible inline row (target
`gtk::Entry`, action `gtk::DropDown`, priority `gtk::SpinButton`, an
icon-only add `gtk::Button`) at `window.rs` around lines 348-370, and
`PrimaryAction::OpenRuleDialog` (in the `dashboard_primary` click handler)
just calls `target.grab_focus()` on that same row. This does not satisfy
spec §3's "Kural ekleme akışı" component or the plan's original Task 3
Step 3, which asked for an `adw::Window`/`adw::MessageDialog`.

Build `fn open_rule_dialog(...)` following the existing pattern in
`src/ui/settings.rs::build_settings_dialog` (same window-transient/present
style already used in this codebase). The dialog holds its own target
entry, action dropdown, and priority spin button; a primary "Add"/"Kural
ekle" button submits through the existing `rule_dialog_submission` parser.
Invalid input keeps the dialog open, adds the `error` CSS class to the
entry, and shows inline localized feedback — do not close the dialog or
call `save_profile` on invalid input. Valid input follows the exact same
`save_profile`/`append_rule`/persistence-before-apply ordering the current
inline handler uses (see the `add_rule` closure in `window.rs`), then
closes the dialog.

Remove the permanently-visible inline row from the rules group. The rules
group should show existing rule rows plus one textual, focusable **Kural
ekle / Add rule** button that calls `open_rule_dialog`.
`PrimaryAction::OpenRuleDialog` in the `dashboard_primary` click handler
calls the same `open_rule_dialog` function instead of grabbing focus.

- [ ] **Step 2: Reorder dashboard sections to match spec §2**

Current order in `build_window`: dashboard group → config error group →
health group → rules group. Spec order: status card → active-lane summary
→ rules (with prominent add) → health (technical detail in an expander,
which already exists — do not change `render_health`'s internal
structure).

Add a lane-summary row to the dashboard group (or a small group directly
below it) showing the selected lane's name, interface, and failure policy
from `profile.borrow().lanes.first()`, localized via existing `Texts`
fields (add new ones if needed, in both `Language::Tr` and `Language::En`
variants — follow the existing field-naming pattern in `src/ui/i18n.rs`).
Reorder `page.add(...)` calls so rules comes before health. Keep the
config-error group where a load error can appear before anything else, as
today.

- [ ] **Step 3: Verify**

Run: `rtk cargo test --lib --bins --tests --no-fail-fast`

Run: `rtk cargo clippy --all-targets --all-features -- -D warnings`

Run: `rtk cargo fmt --all -- --check`

Expected: all PASS, including the existing `rule_dialog_submission` and
`dashboard_state` tests, unchanged.

- [ ] **Step 4: Commit**

```bash
rtk git add src/ui/window.rs src/ui/i18n.rs
rtk git commit -m "feat: build the guided rule dialog and reorder dashboard sections"
```

Then proceed to Task 5 (README + `cargo build --release --bins` +
`./packaging/deb/build-deb.sh` + full verification), which has not been run
since this plan was written.
