# RouteLane Orphaned Resource Recovery Design

## Problem

RouteLane refuses to activate when the privileged helper observes RouteLane-owned
kernel resources but cannot load a validated applied plan from its journal. The
failure is intentional: without a validated plan, the existing cleanup operation
cannot prove that every live resource belongs to the recorded generation.

The current UI exposes `DAEMON_RECONCILIATION_FAILED` but offers no safe recovery
path. Deactivation cannot repair the condition because it runs the same
reconciliation gate before calling cleanup.

## Goal

When activation encounters the specific orphaned-resource condition, explain the
situation to the user and request explicit approval. After approval, remove only
resources whose RouteLane ownership can be independently verified, reset daemon
reconciliation state, and automatically retry the original activation.

The recovery must fail closed. Ambiguous ownership must leave the system
unchanged and produce an actionable error.

## Non-goals

- Do not weaken normal cleanup or reconciliation checks.
- Do not delete arbitrary nftables tables, policy-routing rules, routes, or
  cgroups based only on their names.
- Do not silently recover without user approval.
- Do not repair resources owned by another tool or reconstruct a missing plan.
- Do not add a general-purpose privileged reset operation.

## Approaches Considered

| Approach | Benefit | Cost or risk |
|---|---|---|
| Dedicated orphan recovery operation | Keeps normal cleanup strict and makes user intent explicit | Adds a narrow IPC/helper operation |
| Relax existing cleanup | Smaller surface change | Weakens a safety boundary for every cleanup |
| Overwrite with a new plan | Appears seamless | Can conceal conflicts and leave stale resources |

The dedicated recovery operation is selected.

## Ownership and Safety Contract

Recovery is allowed only when inspection reports no validated applied plan and
at least one live RouteLane resource. The privileged backend captures the entire
relevant live state before mutation and validates all discovered candidates.

An nftables table is recoverable only when it is the reserved `inet/routelane`
table and contains RouteLane's structurally valid ownership marker. Policy-routing
rules and routes are recoverable only when they use RouteLane's reserved identity
scheme and form a self-consistent set. Application cgroups are recoverable only
under RouteLane's delegated cgroup subtree. The backend must reject recovery if
any candidate conflicts with these invariants or if a related resource cannot be
attributed to RouteLane.

Validation completes before the first mutation. Cleanup is journaled so an
interrupted recovery can resume safely. Post-cleanup inspection must prove that
the verified orphan set is absent. The operation does not claim or remove
unrelated resources.

## IPC and Components

Add a `RecoverOrphanedResources` daemon request and a corresponding success
result. The daemon calls a dedicated privileged-helper method; it does not route
this request through normal `Cleanup`.

The helper authorizes the mutation through the existing PolicyKit path. Its
backend performs orphan inspection, ownership validation, journaled cleanup, and
post-cleanup verification as one serialized operation.

On success, the daemon clears its cached reconciliation failure and resets the
reconciliation gate. It immediately reconciles against the now-clean helper
state. Only a clean reconciliation completes the recovery request.

Errors use distinct codes for an unavailable recovery, ambiguous ownership,
authorization denial, cleanup failure, and post-cleanup verification failure.
Raw helper details remain available for diagnostics, while the UI presents
localized, actionable text.

## UI Flow

The activation controller recognizes only the orphaned-resource recovery error;
other reconciliation failures retain their existing handling.

The confirmation dialog explains that remnants of an earlier RouteLane session
were found, that only verified RouteLane resources will be removed, and that the
selected profile will then be activated again. Cancel is the default focused
action. Recovery is styled as destructive because it removes live networking
state.

After confirmation, activation controls are disabled and progress feedback says
that old RouteLane resources are being cleaned. When recovery succeeds, the UI
automatically resends the original activation request exactly once. Success uses
the normal active-state feedback. Failure re-enables controls and reports whether
authorization, ownership validation, cleanup, or verification prevented
recovery. The UI never loops or shows a second confirmation for the same retry.

## Data Flow

1. User activates a profile.
2. Daemon reconciliation detects live RouteLane resources without a validated
   applied plan and returns the specific recoverable error.
3. UI presents the confirmation dialog and preserves the original activation
   request in memory.
4. User cancels, or approves recovery.
5. On approval, daemon asks the authorized helper to recover verified orphaned
   resources.
6. Helper validates the complete candidate set, journals cleanup, removes it,
   and verifies absence.
7. Daemon resets and reruns reconciliation.
8. UI resends the preserved activation once.

## Failure Handling

- Cancellation performs no privileged mutation and does not retry activation.
- PolicyKit denial returns the UI to an idle, inactive state with authorization
  guidance.
- Ambiguous ownership performs no mutation and tells the user manual inspection
  is required.
- A cleanup interruption retains recovery journal state so the helper can safely
  resume it.
- Failed post-cleanup verification keeps activation blocked and reports the
  remaining conflict.
- A failed automatic activation retry uses the normal activation error path and
  cannot trigger another recovery cycle automatically.

## Testing

- IPC serialization and response tests cover the new request and result.
- Privileged-backend tests prove exact RouteLane orphan sets are cleaned and that
  missing markers, mixed ownership, and unrelated resources are rejected before
  mutation.
- Recovery interruption tests prove journal-based resumption.
- Daemon tests prove recovery resets the reconciliation gate only after verified
  cleanup and that subsequent activation can proceed.
- UI decision tests prove only the specific recoverable error opens the dialog,
  cancellation is inert, confirmation sends one recovery request, and success
  retries the original activation exactly once.
- Existing apply, rollback, cleanup, authorization, and root namespace tests
  remain unchanged and passing.

## Acceptance Criteria

- The orphaned-resource error produces a localized confirmation instead of only
  exposing the raw daemon message.
- No cleanup occurs before explicit user approval.
- Only independently verified RouteLane-owned resources can be removed.
- Ambiguous ownership fails without mutation.
- Successful recovery automatically retries the user's original activation once.
- Normal reconciliation and cleanup remain fail-closed.
