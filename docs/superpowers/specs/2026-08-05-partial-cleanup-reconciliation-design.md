# Partial Cleanup Reconciliation Design

## Problem

Normal deactivation validates live RouteLane routing resources against a list of
states produced by one exact cleanup command order. A previously interrupted or
externally completed RouteLane step can remove routes while leaving their exact
policy rules. That state is a safe subset of the validated applied plan, but it
is not a prefix of the current rule-first cleanup order, so deactivation fails
with `cleanup journal does not match an exact RouteLane cleanup command prefix`.

## Safety Contract

Normal cleanup remains available only with a validated active plan and its exact
committed nftables fingerprint. Live RouteLane routing resources are accepted
when every live route and policy rule is an exact member of the journaled plan's
ownership set. Missing journaled resources are treated as already cleaned.

Any plan-external route or rule, foreign semantics in a reserved table, changed
priority/table/mark tuple, missing ownership marker, or nftables fingerprint
mismatch aborts cleanup before mutation. The change therefore broadens accepted
absence patterns, not ownership.

## Cleanup Flow

1. Load and validate the applied or cleanup-pending journal.
2. Capture all reserved RouteLane routes and policy rules.
3. Require the live routing ownership sets to be subsets of the journaled plan
   ownership sets.
4. Require the exact journaled nftables marker and fingerprint.
5. Run the existing idempotent restore-to-empty sequence; already absent routes
   or rules remain no-ops.
6. Verify that all journaled RouteLane resources are absent and clear the state
   journal.

The user already requested deactivation, so no additional confirmation is
introduced.

## Testing

- A cleanup test applies a valid plan, removes its routes while retaining exact
  policy rules, and proves cleanup succeeds and removes the remainder.
- The existing exact-prefix, interrupted-cleanup, foreign-resource and nftables
  fingerprint tests remain passing.
- A test injects a plan-external reserved rule into a partial state and proves
  cleanup fails before mutation.

## Acceptance Criteria

- The observed `rule present / route absent` state deactivates successfully.
- Other partial subsets of the exact journaled routing ownership clean up
  idempotently.
- Supersets and modified resources remain rejected.
- Normal apply, rollback, orphan recovery, and interrupted cleanup behavior do
  not regress.
