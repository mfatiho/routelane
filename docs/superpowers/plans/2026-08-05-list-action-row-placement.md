# List Action Row Placement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep routing-rule and application add actions in fixed rows above their dynamic list entries.

**Architecture:** Introduce a small pure ordering contract for fixed versus dynamic group children, then wrap each add button in a dedicated GTK action row inserted before any dynamic row. Existing rebuild functions continue tracking and removing dynamic rows only.

**Tech Stack:** Rust 2021, GTK4, libadwaita, Cargo tests and Clippy.

## Global Constraints

- Preserve existing dialogs, click handlers, translations, sensitivity and keyboard behavior.
- Add no dependency and no custom theme.
- Rebuilding lists must not remove, duplicate or move add actions.

---

### Task 1: Fixed-first list ordering contract

**Files:**
- Modify: `src/ui/window.rs`
- Test: `src/ui/window.rs`

**Interfaces:**
- Produces private `ListChildKind::{FixedAction, DynamicRow}`.
- Produces private `list_child_order(ListChildKind) -> u8` returning `0` for the fixed action and `1` for dynamic rows.

- [ ] **Step 1: Write the failing ordering test**

```rust
#[test]
fn list_action_is_ordered_before_empty_or_populated_dynamic_rows() {
    assert!(
        list_child_order(ListChildKind::FixedAction)
            < list_child_order(ListChildKind::DynamicRow)
    );
}
```

- [ ] **Step 2: Verify RED**

Run: `rtk cargo test list_action_is_ordered_before_empty_or_populated_dynamic_rows`

Expected: compilation fails because the enum and ordering function do not exist.

- [ ] **Step 3: Implement the minimal ordering contract**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListChildKind {
    FixedAction,
    DynamicRow,
}

fn list_child_order(kind: ListChildKind) -> u8 {
    match kind {
        ListChildKind::FixedAction => 0,
        ListChildKind::DynamicRow => 1,
    }
}
```

- [ ] **Step 4: Verify GREEN**

Run: `rtk cargo test list_action_is_ordered_before_empty_or_populated_dynamic_rows`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
rtk git add src/ui/window.rs
rtk git commit -m "test: define fixed list action ordering"
```

---

### Task 2: Dedicated action rows above dynamic entries

**Files:**
- Modify: `src/ui/window.rs`
- Test: `src/ui/window.rs`

**Interfaces:**
- Consumes the fixed-first contract from Task 1.
- Preserves existing `rebuild_rule_rows` and `rebuild_application_rows` dynamic-row vectors.

- [ ] **Step 1: Add a failing pure construction assertion**

Add `ordered_list_child_kinds(dynamic_count: usize) -> Vec<ListChildKind>` to the test import before implementing it, and test literal results:

```rust
#[test]
fn rebuilding_a_list_keeps_exactly_one_action_before_dynamic_rows() {
    assert_eq!(
        ordered_list_child_kinds(0),
        vec![ListChildKind::FixedAction]
    );
    assert_eq!(
        ordered_list_child_kinds(2),
        vec![
            ListChildKind::FixedAction,
            ListChildKind::DynamicRow,
            ListChildKind::DynamicRow,
        ]
    );
}
```

- [ ] **Step 2: Verify RED**

Run: `rtk cargo test rebuilding_a_list_keeps_exactly_one_action_before_dynamic_rows`

Expected: compilation fails because `ordered_list_child_kinds` does not exist.

- [ ] **Step 3: Implement the helper and GTK action rows**

Implement the helper by starting with one `FixedAction`, appending `dynamic_count` `DynamicRow` values, and sorting with `list_child_order`.

Create each fixed action container as an `adw::ActionRow`, place the existing button inside a left-aligned `gtk::Box` with 6 px spacing and 6 px vertical margins, and call `group.add(&action_row)` before any `rebuild_*` call. Prefix button labels with the Unicode plus sign: `+ {texts.add_rule}` and `+ {texts.add_application}`. Keep the existing button variables and click handlers unchanged.

Do not add action rows to `RuleListContext.rows` or `ApplicationListContext.rows`; those collections remain dynamic-only.

- [ ] **Step 4: Run UI tests and static verification**

Run: `rtk cargo test ui::`

Expected: PASS.

Run: `rtk cargo fmt --all -- --check`

Expected: exit 0.

Run: `rtk cargo clippy --all-targets --all-features -- -D warnings`

Expected: exit 0 with no warnings.

- [ ] **Step 5: Run the full suite**

Run: `rtk cargo test`

Expected: all tests pass with zero failures.

- [ ] **Step 6: Commit**

```bash
rtk git add src/ui/window.rs
rtk git commit -m "feat: keep list add actions above entries"
```
