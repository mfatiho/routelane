# List Action Row Placement Design

## Goal

Keep the “Kural Ekle” and “Uygulama Ekle” actions visibly above their
corresponding dynamic table rows so users encounter the primary action before
scanning existing entries.

## Layout

Each libadwaita preferences group keeps its title and description. Immediately
below that header, a dedicated action container appears as the first group child.
The container holds a left-aligned `+ Kural Ekle` or `+ Uygulama Ekle` button.
Dynamic rule/application rows follow beneath it.

The action container is structurally separate from the vectors that track
dynamic rows. Rebuilding a list removes and recreates only dynamic rows, so the
button remains first and does not move, duplicate, or disappear.

## Behavior and Accessibility

Existing click handlers, dialogs, sensitivity rules, and keyboard activation are
preserved. Buttons retain textual labels and receive the standard suggested
spacing without adding a dependency or custom widget theme. Empty lists still
show the action first, with no artificial empty-state element between the action
and the group description.

## Testing

A pure ordering helper defines the fixed action position before dynamic entries.
Unit tests protect the ordering contract for empty and populated lists. Existing
GTK/UI tests verify that add actions and dialogs continue to compile and behave
through their current handlers.

## Acceptance Criteria

- “Kural Ekle” remains above every routing-rule row.
- “Uygulama Ekle” remains above every application row.
- Rebuilding either list cannot remove, duplicate, or move its add action.
- Existing add-dialog behavior, sensitivity, keyboard use, and translations are
  unchanged.
