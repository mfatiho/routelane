# Practical RouteLane UI/UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Execution is native/inline on main; do not dispatch subagents or create a worktree.

**Goal:** Deliver the approved four-section GTK interface with compact lists,
clear setup guidance and trustworthy, correlated operation feedback.

**Architecture:** Keep domain, persistence format, daemon wire protocol and
privileged backend unchanged. Introduce a pure UI session model and preserve
request/epoch identity across the desktop bridge. Compose focused GTK pages and
editors around one controller that owns persistence and mutation submission.

**Tech Stack:** Rust, existing GTK4 v4_4 / Libadwaita v1_1 features, GLib,
async-channel, Tokio; existing Broadway/private D-Bus GTK smoke harness.

**Spec:** `docs/superpowers/specs/2026-10-13-practical-ui-ux-design.md`.

**Execution:** Preserve the user's native/inline, main-branch preference.
Plan review is required before implementation. No independent reviewer agent
is authorized; perform inline reviews and report this limit honestly.

**Status:** Complete and self-reviewed; awaiting user review before execution.

## Global Constraints

- Routing planlayıcısı, helper'ın yetki sınırı, daemon IPC wire sözleşmesi ve ConfigV2 biçimi değişmez.
- Yeni bağımlılık, GTK/Libadwaita sürüm yükseltmesi, kernel değişikliği veya otomatik ağ onarımı eklenmez.
- Mevcut GTK4 v4_4 ve Libadwaita v1_1 API sınırı korunur.
- Başlangıç pencere boyutu 900×720 mantıksal pikseldir.
- 560×600 mantıksal pikselde ve yüzde 200 metin ölçeğinde temel işlemler erişilebilir kalmalıdır.
- Yerleşimde 6/12/24 piksel boşluk ritmi; tipografi title/body/caption rolleriyle tutarlı.
- Bütün kullanıcı metinleri i18n üzerinden Türkçe/İngilizce gelir; enum debug adı görünmez.
- Kural Ekle/Uygulama Ekle boş, dolu ve filtreli listelerin üstünde kalır.
- Mevcut IPv4/IPv6, Route/Direct/Block, Block aktif/pasif ve öncelik semantiği korunur.
- GUI sudo çalıştırmaz; onay ve helper sahiplik kontrolleri korunur.
- Ubuntu/Debian/Fedora destek kapsamı azaltılmaz; yerel build tüm dağıtımların doğrulaması değildir.
- All shell commands start with `rtk`; use `apply_patch` for local source/document edits.
- Never install the package, mutate live routing or invoke privileged recovery as part of a UI smoke test.

## Review Focus

1. Reconnection with matching profile IDs but different saved/applied contents: do not enable launch based on IDs alone; Task 1 pins unknown revision handling.
2. Dialog open while its lane/application disappears or another snapshot arrives: preserve identity, reject stale submit, never choose index zero as a replacement; Tasks 5/7 pin this.
3. Filtered rules with equal priorities and hidden application rules: a move must change the intended real order without dropping unrelated entries; Task 6 pins this.
4. Save failure while routing is active or unrelated status arrives during save/apply: keep safe deactivation available, never release the mutation gate early; Tasks 1/4 pin this.
5. Long translated labels, missing desktop metadata and focus after list refresh: no overflow, incorrect name fallback or keyboard trap; Tasks 7/8 pin this.

## File map and common interfaces

Existing edits from the reliability audit are present in the worktree. Read
the current files rather than restoring HEAD. Do not stage entire overlapping
files blindly, reset, stash away or discard these earlier changes.

| Path | Responsibility |
| --- | --- |
| `src/models.rs` | UI-only request identity and epoch-tagged bridge messages |
| `src/main.rs` | Forward existing daemon responses intact; reconnect/subscription boundary |
| `src/ui/session.rs` (new) | Pure persistence/operation/revision session state |
| `src/ui/controller.rs` (new) | Dispatch session effects to existing channels/workers |
| `src/ui/shell.rs` (new) | Four-page stack, permanent status area, navigation/actions |
| `src/ui/experience.rs` | Pure dashboard/setup/action decisions from confirmed session state |
| `src/ui/overview.rs` (new) | Counts, all-lane summary, health/error presentation |
| `src/ui/connections.rs` (new) | Lane list, explicit-save editor and deletion confirmation |
| `src/ui/rule_editor.rs` (new) | Target/action/lane form, local validation, edit submission |
| `src/ui/application_editor.rs` (new) | Searchable async desktop selector and explicit-save app editor |
| `src/ui/desktop_catalog.rs` (new) | Ephemeral names/icons/support hints; asynchronous discovery |
| `src/ui/list_page.rs` (new) | Fixed add/search controls, dynamic rows and preserved view state |
| `src/ui/rule_row.rs`, `application_row.rs` | Compact rows with contextual menus |
| `src/ui/settings.rs` | General preferences and existing confirmed language-change behavior |
| `src/ui/i18n.rs` | Localized human-facing copy and accessible names |
| `src/ui/window.rs` | Build controller/pages; GTK integration tests; remove superseded inline builders |
| `src/ui/mod.rs` | Module declarations and existing experience contract tests |

New widget modules are private to the binary. Do not turn them into a generic
framework. Existing domain `Profile`, `Rule`, `Lane` and identifiers are the
shared data types. All page callbacks pass stable IDs or complete validated
draft values, not dropdown/list indices.

### Identity and revision decisions

- `UiRequest { request_id: String, request: DaemonRequest }` is UI-only;
  `UiRequest::new(request: DaemonRequest) -> Self` allocates a UUID once.
- Use that exact ID in the existing `DaemonEnvelope`. Do not regenerate it in
  the bridge, and do not change `IPC_PROTOCOL_VERSION` or serialized methods.
- Replace flattened results with `DaemonToUi::Response { epoch: u64,
  response: DaemonResponse }`. Initial refresh is `Snapshot { epoch: u64,
  status: DaemonStatus, profiles: Vec<Profile> }` after both reads succeed.
- Other bridge messages are `ConnectionEstablished { epoch }`,
  `Event { epoch, event: DaemonEvent }`, and `DaemonUnavailable { epoch,
  request_id: Option<String>, error: IpcError }`.
- `ListProfiles` currently returns desired profiles, not a proof of applied
  contents. A fresh GUI/reconnection may know the applied ID/generation but
  not the applied profile revision. Mark revision alignment unknown, keep
  deactivation available, disable app launch/adoption, and offer explicit
  Reapply Saved Changes. Never reapply silently to prove alignment.
- A successful own Apply/SetRuleEnabled response binds its submitted candidate
  snapshot to its resulting generation. Launch/adopt requires that snapshot
  to match the current saved profile and the latest confirmed generation.
  Normalize only `Profile.enabled` for content comparison; conservatively
  treat other differences as needing reapplication.
- Mutating timeout/transport loss leaves outcome unknown. Request read-only
  status/profile refresh, label the last known state, and never resend the
  failed mutation automatically. If refresh remains Applying/unavailable,
  conflicting actions remain disabled. A fresh non-Applying snapshot permits
  explicit user actions but does not assert saved/applied content equality.

### Compilable transition between tasks

In Task 1 retain today's flattened message enum as `LegacyDaemonToUi` and
alias it to `DaemonToUi` in the existing main/window imports. Add the new
correlated `DaemonToUi` alongside it for session tests; leave the old bridge
and `UiToDaemon::Request(DaemonRequest)` behavior intact in this task.
In Task 2 switch the bridge to the new messages and tracked requests. Update
window channel signatures to receive new `DaemonToUi`, then temporarily
adapt each message through `legacy_payloads(message: DaemonToUi)
-> Vec<LegacyDaemonToUi>` before the existing render handlers. Snapshot maps
to status/profiles; response to exactly its former payload or error; epoch,
event and disconnect retain their old render meanings. This is an explicitly
temporary rendering adapter, not the final operation-state implementation.
Task 4 replaces these render handlers with controller/session projections,
then deletes LegacyDaemonToUi and legacy_payloads. No intermediate task should
leave the application uncompilable or send both legacy and new mutations.

## Task 1: Pure UI session and action eligibility

**Files:** Create `src/ui/session.rs`; modify `src/models.rs`,
`src/ui/mod.rs`; mechanical legacy imports in `src/main.rs` and
`src/ui/window.rs`; tests in `session.rs` and the existing experience tests.

**Interfaces:**

- Produces `OperationKind::{Apply, SetRuleEnabled, Deactivate, Launch, Adopt,
  Recover, SaveOnly, ChangeLanguage}` and `OperationStage::{Saving, Sending,
  AwaitingReply, AwaitingSnapshot}`.
- Produces `AfterSave::{None, Request(DaemonRequest), QuitApplication}`.
- Produces `UiEffect::{Persist(PersistenceRequest), Send(UiRequest),
  RefreshSnapshot, QuitApplication}` and
  `UiGateError::{Busy, StateUnknown, ReadOnlyConfig, UnappliedProfile, DisabledRule}`.
- Produces `SaveState::{Clean, Saving, SavedInactive, Applying, Applied,
  SavedUnapplied, Failed}`; save/apply outcome is separate from kernel health.
- `UiSession::new(profile: Profile, config_writable: bool) -> Self`.
- `begin_save(&mut self, config: ConfigV2, profile: Profile,
  follow_up: AfterSave) -> Result<Vec<UiEffect>, UiGateError>`.
- `begin_request(&mut self, request: DaemonRequest,
  submitted_profile: Option<Profile>) -> Result<Vec<UiEffect>, UiGateError>`.
- `handle_persistence(&mut self, generation: u64,
  result: Result<(), String>) -> Vec<UiEffect>`.
- `handle_daemon(&mut self, message: DaemonToUi) -> Vec<UiEffect>`.
- Getters: `is_busy() -> bool`, `snapshot_known() -> bool`,
  `confirmed_status() -> Option<&DaemonStatus>`, `config_writable() -> bool`,
  `application_actions_enabled(&Profile, &Rule) -> bool`,
  `needs_reapply(&Profile) -> bool`, `last_error() -> Option<&IpcError>`,
  `operation() -> Option<(OperationKind, OperationStage)>`,
  `save_state() -> SaveState`.

- [ ] Write pure tests `failed_health_without_applied_plan_is_not_active`,
  `readonly_config_preserves_deactivate`, `status_does_not_complete_apply`,
  `foreign_request_or_epoch_cannot_finish_operation`,
  `same_profile_id_after_reconnect_does_not_prove_revision`, and
  `persistence_result_can_start_only_its_own_followup`.
  Assert no second begin succeeds while busy; responses with another ID do
  not clear the gate; a desired snapshot alone never enables app actions.
  Include this concrete initial-state test:

  ```rust
  #[test]
  fn initial_state_does_not_claim_confirmed_routing() {
      let profile = Profile {
          id: "work".into(), name: "Work".into(), enabled: false,
          lanes: vec![], rules: vec![],
      };
      let session = UiSession::new(profile.clone(), false);
      assert!(!session.snapshot_known());
      assert!(!session.is_busy());
      assert!(!session.config_writable());
      assert!(session.confirmed_status().is_none());
      assert!(session.needs_reapply(&profile));
  }
  ```
- [ ] Run `rtk cargo test --bin routelane ui::session -- --test-threads=1`.
  Confirm failure from the missing session behavior, not test fixture errors.
- [ ] Implement these interfaces with private pending/saved/applied snapshots,
  monotonically increasing persistence generation, and request/epoch checks.
  Saving must remain busy until its matching follow-up completes. A failed
  save produces no Send effect. Read-only refresh never becomes an Apply.
  A matching successful Apply reply may enter AwaitingSnapshot and request
  refresh; only then can a fresh matching-generation snapshot finish that
  operation. A snapshot before that reply cannot do so. Read-only requests
  never replace a pending mutation or require writable configuration.
- [ ] Add `deactivated_generation_is_not_required_to_increase`,
  `new_external_generation_invalidates_applied_revision`,
  `timeout_does_not_replay_mutation`, and
  `disabled_rule_never_enables_application_actions`; pin the expected effects.
- [ ] Run the session tests and existing protocol/config tests. Preserve
  compilation of existing UI callers through adapters until Task 4 replaces them.
- [ ] Inline-review and checkpoint only the Task 1 changes; keep earlier
  overlapping reliability hunks outside the task commit.

## Task 2: Correlated desktop bridge

**Files:** Modify `src/main.rs`, `src/models.rs`, `src/ui/window.rs`;
tests in `main.rs` and temporary adapter tests in `window.rs`.

**Interfaces:** Consumes Task 1 `UiRequest` and epoch-tagged messages.
Keep `UiToDaemon::Request(UiRequest)` and Shutdown. Add
`UiToDaemon::RefreshSnapshot` for explicit paired read-only refresh.
Keep existing timeout policy: reads 3 s, mutations 180 s.
Add convenience `UiToDaemon::request(request: DaemonRequest) -> Self`, which
calls UiRequest::new exactly once, and update all legacy raw request sites.
`forward_response(response: DaemonResponse, epoch: u64,
sender: &Sender<DaemonToUi>)` forwards the response without changing its ID.

- [ ] Write async bridge tests `forward_response_retains_id_error_and_epoch`,
  `atomic_snapshot_requires_both_reads`, `transport_failure_identifies_request`,
  and `refresh_snapshot_sends_only_status_and_profiles`.
  Use the existing temporary Unix socket fixture; assert the exact envelope
  ID is unchanged, error/result is not flattened, and no Apply is sent.
  Pin the forwarding contract without a socket dependency:

  ```rust
  #[tokio::test]
  async fn forward_response_retains_id_error_and_epoch() {
      let (sender, receiver) = async_channel::bounded(1);
      let response = DaemonResponse::failure(
          "apply-1", IpcError::new("DAEMON_NEEDS_AUTHORIZATION", "denied"),
      );
      forward_response(response.clone(), 7, &sender).await;
      assert!(matches!(receiver.recv().await.unwrap(),
          DaemonToUi::Response { epoch: 7, response: actual }
              if actual == response));
  }
  ```
- [ ] Run `rtk cargo test --bin routelane tests:: -- --test-threads=1`; verify RED.
- [ ] Change `send_with_timeout` to consume `UiRequest`, forward whole responses,
  tag events/failures with the active epoch, and emit Snapshot only after both
  reads. Preserve subscribe-before-refresh and sequence-boundary behavior.
  On transport failure emit the correlated failure before reconnecting.
- [ ] Update existing bridge tests and request creation sites without removing
  their reconnect, ordering or long-authorization timeout assertions. Add
  `request_ids_are_not_reused_for_recovery_or_retry`.
- [ ] Run bridge tests and `rtk cargo test --test ipc_contract -- --test-threads=1`;
  all pass, with unchanged wire serialization/version assertions.
- [ ] Inline-review and checkpoint only the bridge task changes.

## Task 3: Four-section shell and localized overview

**Files:** Create `shell.rs`, `overview.rs`, `list_page.rs` under `src/ui/`;
modify `window.rs`, `experience.rs`, `i18n.rs`, `mod.rs`.

**Interfaces:**

- `Section::{Overview, Rules, Applications, Connections}`.
- `Shell::new(texts: Texts) -> Self`; exposes `root: gtk::Box`,
  `status_row: adw::ActionRow`, `primary: gtk::Button`;
  `add_page(&self, section: Section, child: &impl IsA<gtk::Widget>)`,
  `select(&self, section: Section)`, `section(&self) -> Section`.
- `ListPage::new(texts: Texts, section: Section) -> Self`; exposes
  `root: gtk::Box`, `group: adw::PreferencesGroup`, `add: gtk::Button`,
  `search: gtk::SearchEntry`; `set_empty(&self, no_items: bool, no_matches: bool)`.
- `dashboard_state(profile: &Profile, session: &UiSession) -> DashboardState`;
  replace health-as-active guessing. `PrimaryAction` gains OpenConnections,
  RefreshStatus and ReapplyRouting; retain rule/setup and enable/disable actions.
- `Overview::new(texts: Texts) -> Self`; `update(&self, profile: &Profile,
  session: &UiSession)` renders counts, every lane, localized health and last error.

- [ ] Add pure tests for Direct/Block without lanes, disabled/unsupported-only rules,
  active plan with missing local setup, and all-lane counts excluding app
  rules from target-rule counts. Add GTK smoke assertions for exactly four
  named sections and one global routing control.
  In the isolated GTK shell test, create a page for every Section, then assert:

  ```rust
  assert_eq!(shell.section(), Section::Overview);
  for section in [Section::Rules, Section::Applications, Section::Connections] {
      shell.select(section);
      assert_eq!(shell.section(), section);
  }
  assert_eq!(window.default_width(), 900);
  assert_eq!(window.default_height(), 720);
  ```
- [ ] Run `rtk cargo test --bin routelane ui::experience -- --test-threads=1`
  and `rtk proxy bash scripts/run-gtk-smoke.sh`; verify RED for real behavior.
- [ ] Build the stack using existing GTK APIs, persistent status area and
  fixed list controls. Set 900×720 default size; remove the duplicate header
  switch. For narrow width, stack the status/button vertically; navigation
  scrolls horizontally rather than forcing window growth.
- [ ] Extend both language packs for section names, operation stages, empty
  states, reapply/unknown states and menu labels. Render DiagnosticCode and
  subject into localized summary/action; preserve raw details in expanders.
  Map Healthy to `Yerel yönlendirme kuralları doğrulandı` /
  `Local routing rules verified`, never website availability.
- [ ] Run new pure/GTK tests and the existing experience contract tests after
  updating their expectations to the approved new primary-action contract.
- [ ] Inline-review and checkpoint the shell/overview change.

## Task 4: One controller for save/apply/results/recovery

**Files:** Create `src/ui/controller.rs`; modify `window.rs`, `session.rs`,
`models.rs`; tests in `controller.rs`, `window.rs` and existing bridge tests.

**Interfaces:** `UiController::new(session: UiSession,
sender: Sender<UiToDaemon>, persistence: Option<PersistenceWorker>) -> Rc<Self>`;
`save(config: ConfigV2, profile: Profile, follow_up: AfterSave) -> bool`;
`request(request: DaemonRequest, submitted_profile: Option<Profile>) -> bool`;
`receive(message: DaemonToUi)`; `session() -> Ref<'_, UiSession>`;
`refresh()`; `connect_changed(callback: Rc<dyn Fn()>)`. Returned bool means accepted locally,
not persisted/applied. Controller executes UiEffects, then notifies renderers.

- [ ] Add tests `save_then_apply_holds_one_gate`,
  `save_failure_sends_no_apply_and_still_allows_deactivate`,
  `interleaved_status_does_not_reenable_row_actions`, and
  `unrelated_error_does_not_trigger_recovery`.
  Use memory channels/temp config; assert request count, type and identity.
  For the interleaved-status scenario, consume the initial outgoing Apply
  from the request receiver, inject a newer ordinary status, and before the
  matching mutation response assert:

  ```rust
  assert!(controller.session().is_busy());
  assert!(!controller.request(DaemonRequest::Deactivate, None));
  assert!(requests.try_recv().is_err());
  ```
- [ ] Run controller/session tests and GTK smoke; verify RED.
- [ ] Route primary action, every row edit/delete/reorder, connection save,
  app launch/adoption and language save through the controller. Remove the
  independent mutation_pending/routing_active guesses and scattered direct
  mutation sends. Keep explicit read-only refresh paths available.
- [ ] Keep orphan recovery associated with its failed Apply ID and candidate.
  Confirmation cancel sends nothing; confirmation sends one Recover;
  successful result permits exactly one new Apply ID; second failure does
  not prompt another recovery cycle. Language change still saves only after
  confirmation and quits only after its own successful persistence result.
- [ ] Add `recovery_cancel_is_readonly`, `recovery_retry_is_bounded`,
  `language_cancel_does_not_persist`, and
  `snapshot_preserves_local_failed_apply_draft`. Partial daemon Profiles
  upsert by ID; do not delete absent local profiles or overwrite pending/failed
  local edits with an unrelated stale desired snapshot.
- [ ] Run full rootless tests, inspect all Request construction sites for
  bypasses, and checkpoint this controller integration only.

## Task 5: Connections page and explicit-save forms

**Files:** Create `src/ui/connections.rs`; modify `settings.rs`, `window.rs`,
`i18n.rs`, `mod.rs`; tests in connections/settings and GTK smoke.

**Interfaces:**

- Move existing lane edit helpers into `connections.rs` without changing
  ID/name/fallback semantics. `LaneDraft { name: String, interface: String,
  failure_policy: LaneFailurePolicy }`.
- `validate_lane_draft(profile: &Profile, lane_id: Option<&LaneId>,
  draft: &LaneDraft, interfaces: &[String]) -> Result<LaneDraft, LaneEditError>`.
- `lane_delete_impact(profile: &Profile, lane_id: &LaneId) -> (usize, usize)`
  returns target-rule count, application-rule count.
- `ConnectionsPage::new(parent: &adw::ApplicationWindow, texts: Texts,
  profile: Rc<RefCell<Profile>>, on_save: Rc<dyn Fn(Profile)>) -> Self`;
  `update(&self, editing_enabled: bool)`, `open_add(&self)`.

- [ ] Write tests `missing_interface_is_not_replaced_by_first_entry`,
  `duplicate_or_empty_lane_name_keeps_draft`,
  `lane_delete_counts_target_and_application_rules`,
  `cancel_lane_edit_changes_nothing`. Add a GTK dialog test that changing
  fields sends no request until explicit valid Save.
- [ ] Run connections/settings tests and GTK smoke; verify RED.
- [ ] Move lane UI out of Preferences into its own compact list. Separate
  draft from live profile; select by interface string and LaneId, never a
  saved dropdown index. Missing interface stays visibly unselected; Save
  validates again against the current profile and refreshed interface list.
- [ ] Use vertical labeled forms, localized failure-policy explanations and
  existing destructive confirmation. Preserve fallback fields on editing.
  After Save submit exactly once through Task 4; after Cancel return focus.
  Keep Preferences focused on language and existing general settings.
- [ ] Add `lane_disappearing_during_dialog_rejects_submit` and
  `missing_saved_interface_can_be_replaced_only_explicitly` and
  `deleting_fallback_target_never_persists_invalid_profile`. Validate the
  candidate profile before committing deletion; if another lane references
  the deleted lane as fallback, show a localized error and persist nothing
  rather than silently rewriting fallback configuration. Run pure/GTK
  tests plus existing lane deletion and language confirmation regressions.
- [ ] Inline-review and checkpoint the connection-page change.

## Task 6: Compact searchable rules and a complete editor

**Files:** Create `src/ui/rule_editor.rs`; modify `rule_row.rs`, `list_page.rs`,
`window.rs`, `i18n.rs`, `mod.rs`; tests in rule editor/row/window.

**Interfaces:**

- `parse_rule_target(input: &str) -> Result<RuleTarget, TargetInputError>` with
  Empty, Invalid and Unsupported variants. Preserve `*` as Any, valid IP/CIDR,
  ASCII domain/hostname (lowercase, optional terminal dot preserved); reject
  URLs, embedded ports/paths/whitespace, invalid label boundaries and wildcard.
- `RuleDraft { target: String, action: RuleAction }`;
  `validate_rule_draft(profile: &Profile, draft: &RuleDraft)
  -> Result<(RuleTarget, RuleAction), RuleDraftError>`.
- `open_rule_editor(parent: &adw::ApplicationWindow, texts: Texts,
  profile: Rc<RefCell<Profile>>, editing: Option<RuleId>,
  on_save: Rc<dyn Fn(Rule)>)`.
- `rule_matches_query(rule: &Rule, lanes: &[Lane], query: &str) -> bool`;
  `move_rule(profile: &mut Profile, id: &RuleId,
  direction: ReorderDirection) -> bool`.

- [ ] Add table-driven target tests covering `*`, IPv4, IPv6, CIDR,
  `Claude.AI`, terminal dot, `*.claude.ai`, URL, invalid CIDR and empty input.
  Assert Route without a real lane fails rather than changing to Direct.
  Add `filtered_reorder_uses_full_execution_order` and
  `equal_priority_move_reorders_without_dropping_application_rules`.
  Exact validation assertions include:

  ```rust
  assert_eq!(parse_rule_target("*"), Ok(RuleTarget::Any));
  assert_eq!(parse_rule_target("Claude.AI"),
      Ok(RuleTarget::Domain("claude.ai".to_owned())));
  assert_eq!(parse_rule_target("claude.ai."),
      Ok(RuleTarget::Domain("claude.ai.".to_owned())));
  assert_eq!(parse_rule_target("*.claude.ai"), Err(TargetInputError::Unsupported));
  assert!(parse_rule_target("https://claude.ai/path").is_err());
  assert!(parse_rule_target("203.0.113.0/77").is_err());
  assert_eq!(parse_rule_target(""), Err(TargetInputError::Empty));
  ```
- [ ] Run rule editor/row tests; verify RED.
- [ ] Replace permanent dropdown/up/down/delete controls with a switch and
  labeled menu actions. Edit opens a vertical target/action/lane dialog;
  Cancel changes nothing, Save revalidates by ID. Disabled menus do not
  send callbacks. Keep Any/Direct/Block valid without lanes.
- [ ] Implement search without sorting or editing the underlying list. Move
  against the complete Rule::execution_cmp order, including hidden app rules.
  When swapped priorities are equal, deterministically renumber the complete
  ordered list in steps of 10; preserve IDs, all other fields and intended order.
  Maximum 10,000 rules fits u32. First/last movement returns false.
- [ ] Add real GTK tests for add-button ordering with empty/populated/filtered
  rows; delete cancel; form validation focus; Block switch once-only request;
  changing Route selection chooses its named lane, never implicitly the first.
- [ ] Run pure/GTK tests and checkpoint the rule UX change.

## Task 7: Searchable application catalog, compact rows and safe actions

**Files:** Create `src/ui/desktop_catalog.rs`, `application_editor.rs`;
modify `application_row.rs`, `window.rs`, `i18n.rs`, `mod.rs`.
Use existing `routelane::application::DesktopApp` discovery/launcher metadata;
no new persisted fields or dependencies.

**Interfaces:**

- `ApplicationChoice { app: DesktopApp, icon_name: Option<String>,
  unsupported_reason: Option<&'static str> }`; unsupported_reason is a
  stable localization key, not a daemon success guarantee.
- `DesktopCatalog::load() -> Result<Self, String>` runs on a worker thread;
  `choices(&self) -> &[ApplicationChoice]`;
  `find(&self, selector: &ApplicationSelector) -> Option<&ApplicationChoice>`.
- `application_display_name(selector: &ApplicationSelector,
  catalog: &DesktopCatalog) -> String`; fallback executable filename/path.
- `open_application_editor(parent: &adw::ApplicationWindow, texts: Texts,
  profile: Rc<RefCell<Profile>>, editing: Option<RuleId>,
  on_save: Rc<dyn Fn(Rule)>)`.
- Existing `application_action_request` remains the typed daemon request builder;
  callbacks go through Task 4 and consult Task 1 eligibility at click time.

- [ ] Add tests for display-name lookup, missing metadata fallback, canonical
  Snap/Flatpak launcher rejection, duplicate desktop IDs, and preserved
  original launcher/quoted arguments after editing only the lane.
  Add `same_profile_id_but_unapplied_edit_blocks_launch` and
  `lane_failure_does_not_claim_selected_interface_is_used`.
- [ ] Run application tests and GTK smoke; verify RED.
- [ ] Load catalog off the GTK loop and show loading/error/empty states.
  Use GTK/GIO metadata for a generic/icon-name fallback; do not load images
  or discover desktop files synchronously on each keystroke. Track a catalog
  request token; ignore completion after dialog close or a newer request.
- [ ] Build searchable program selection and vertical lane form. Preserve
  selected app/LaneId across filtering; validate existence, duplicate identity
  and lane again on Save. Unknown integration is not labeled verified;
  known unsupported sandbox launchers are disabled with localized reason.
- [ ] Compact rows show name/icon, lane/policy summary, switch, launch and
  menu. Adoption requires scope confirmation, including other browser tabs
  and existing-connection limits. Cancel is read-only. Do not terminate apps.
  Unavailable lane uses neutral launch copy and describes configured policy.
- [ ] Add GTK tests for `catalog_result_after_cancel_is_ignored`,
  `selected_application_removed_before_save_is_rejected`,
  `filter_does_not_retarget_selected_program`, and
  `double_launch_click_submits_once`; run pure/GTK/IPC tests and checkpoint.

## Task 8: Accessibility, refresh resilience and full verification

**Files:** Modify `shell.rs`, `list_page.rs`, `window.rs`, `confirm.rs`,
`i18n.rs`, `README.md`; add `src/ui/gtk_tests.rs` for the expanded ignored
GTK tests if extracting them makes `window.rs` easier to maintain.
Keep `scripts/run-gtk-smoke.sh` and private bus setup isolated from host config.

**Interfaces:** `ListPage::remember_view() -> ListViewState` and
`restore_view(&self, state: &ListViewState)`;
`ListViewState { query: String, scroll: f64, focused_rule: Option<RuleId> }`.
Use RuleId to restore/update an existing row, not child position. If a focused
row is deleted, focus its nearest surviving neighbor, otherwise the add button.

- [ ] Add GTK tests for preservation of section/search/scroll/focus across
  profile/health refresh; a removed focused row; Ctrl+F/Ctrl+N routing by
  section; Enter invalid/valid submission and Escape; confirmation default
  focus on Cancel; every icon-only control having an accessible name.
- [ ] Run the isolated GTK test command and verify RED for these behaviors.
- [ ] Implement actions/shortcuts, explicit label associations, focus return,
  stable row updates, wrapping/ellipsizing and persistent error panel. Do not
  reset queries or destroy/recreate all rows for a health-only update.
  Remove superseded fake child-order helper assertions; keep tests that
  inspect real GTK child order instead.
- [ ] Test 560×600 and 900×720 allocations, both language packs, long lane/app
  names and 200% text scale; record manual visual/assistive checks separately
  from automated assertions. Do not claim screen-reader or theme verification
  based only on widget property tests.
- [ ] Run `rtk cargo fmt --check`,
  `rtk cargo test -- --test-threads=1`,
  `rtk cargo clippy --all-targets --all-features -- -D warnings`,
  `rtk proxy bash scripts/run-gtk-smoke.sh`,
  `rtk cargo build --release --bins`,
  `rtk proxy bash packaging/deb/build-deb.sh` and `rtk git diff --check`.
  Every required command must exit 0; an ignored/skipped test is not a pass.
  Packaging produces an artifact only, not a system installation.
- [ ] Inline-review spec acceptance items 1–14; update README's guided journey
  and test instructions. Checkpoint task changes without bundling the earlier
  reliability edits. Report exact test results, artifact path, untouched
  system state and remaining real kernel/Polkit/distribution gates.

## Coverage and execution handoff

| Spec area | Owning tasks |
| --- | --- |
| Information hierarchy, four sections, one action, overview | 1, 3, 8 |
| Setup without fake lane for Direct/Block | 1, 3, 6 |
| Rules, ordering, top add actions, forms, search | 3, 6, 8 |
| Applications, metadata, launcher preservation, scope confirmation | 1, 4, 7 |
| Connections, fallback preservation, missing interfaces, deletion impact | 5 |
| Persistence, state truth, response correlation, reconnect, bounded recovery | 1, 2, 4 |
| Human-readable health/error copy and no false traffic guarantees | 3, 4, 7 |
| Language confirmation, keyboard, focus, theme/layout constraints | 3, 5, 8 |
| No backend/wire/dependency expansion; distro/root limitations | All; final verification in 8 |

Self-review must confirm every produced type/method is defined before its
consumer, all five Review Focus cases have tests, and no task silently expands
backend support. Implementation starts only after the user reviews this plan.
Preserve native/inline execution on main; do not ask the user to pick agents again.
