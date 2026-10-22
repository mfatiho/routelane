# Selective Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Kullanıcının korunacak yürütme tercihi: main üzerinde native/inline; yeni worktree ve alt ajan yok.

**Goal:** Yalnız açık RouteLane kurallarının trafiğini yönet; diğer yeni bağlantıların sistem rotasını, işaretlerini ve kaynak adresini koru.

**Architecture:** Profilin açık kapsam seçimi planner, policy hash, helper grant'i ve GTK durumuna taşınır. Ortak nft predicate üreticisi normal kurallar ile kapsamı daraltılmış transaction guard'ını besler. Eski journal V2 dondurulmuş DTO'larla cleanup-only okunur; modern activation temiz bir V3 journal üzerinden başlar.

**Tech Stack:** Mevcut Rust 2021, serde, Tokio, zbus, ipnet, GTK4/Libadwaita; iproute2, nftables, systemd/Polkit. Yeni bağımlılık yok.

**Spec:** `docs/superpowers/specs/2026-10-22-selective-routing-design.md`.

**Durum:** Yazılı tasarım 22 Ekim'de kullanıcı tarafından “Devam et” ile onaylandı. Bu plan inceleme bekliyor; hiçbir görev uygulanmadı. Canlı lab erişimi düzeltilmiş sayılmaz.

## Global Constraints

- Yeni varsayılan `PreserveSystem`; ayrı onaylı seçenek `UsePrimary`. Eşleşmeyen **yeni** trafik seçici modda işaretlenmez, NAT'lanmaz, primary'ye gönderilmez veya genel reject ile engellenmez.
- Kullanıcı config ve desired-profile şeması **4**, daemon IPC ve runtime activation protokolü **3**, yeni journal sürümü **3** olur. Eksik/bilinmeyen mod ve sürüm fail-closed reddedilir.
- `Direct` halen **Ana hattı kullan** anlamındadır; yeni System-bypass eylemi eklenmez. Route/Block ilk eşleşme sırası ile Block/Direct/Inactive lane hata politikaları korunur.
- Açık kuralı eşleşmeyen foreign-mark/forwarded trafik korunur. Eşleşen foreign-mark Route/Direct çatışması işaret silinmeden yalnız kendi kapsamını reddeder; açık Block uygulanır.
- Inspect salt okunurdur. Yalnız doğrulanmış RouteLane kaynakları değişebilir; main/default/local tabloları, yabancı NFT/VPN/Docker, DNS, genel sysctl ve global conntrack/firewall state değişmez.
- Eski journal modern renderer ile hash'lenmez/render edilmez, otomatik yeniden silahlandırılmaz. Modern activation öncesi kullanıcı onaylı exact legacy cleanup gerekir; sahiplik kanıtı yoksa silme yoktur.
- Mod genişlemesi mevcut grant ile yapılamaz; yeni yetkilendirme gerekir. Aktif mod, kaydedilen mod ve dış ağ erişim kanıtı ayrı gösterilir.
- `10.65.201.0/24 → Ethernet` varsa açık kuraldır; silme/bypass yok, önizleme ve kullanıcı kararı vardır. Önceden mark/NAT uygulanmış socket'lerin kesintisiz taşınma garantisi yoktur.
- Ubuntu, Debian ve Fedora'nın ayrı paket/kernel/Polkit kabul kapıları korunur. Derleme veya exit 77 başarı değildir; kanıt yoksa NOT VERIFIED raporlanır.
- Önceden kirli CIDR/nesil değişiklikleri ile `Cargo.toml`/`Cargo.lock` korunur. Yalnız bu işe ait hunks stage edilir; `git add .`, reset/stash/restore yoktur. Ara sürüm host'a kurulmaz.
- Host aktivasyonu/kapatması, servis değişikliği, paket kurulumu, elle `priority 9000` istisnası ve mevcut kullanıcı lab VM'leri bu planın yürütme yetkisi değildir. Kernel testleri yalnız ayrıca yetkilendirilmiş disposable ortamda yapılır.

## Review Focus

1. DNS alias'ları/örtüşen CIDR'ler ve cgroup uygulama kuralları: normal verdict ile guard farklı kapsam yorumlamamalı — Task 4/5.
2. Eski journal'ın pending, cleanup veya interface-setting aşamasında kesilmesi: modern tipe dönüştürmeden yalnız exact sahiplik kanıtı temizlenmeli — Task 1/3.
3. VPN'in meta mark veya ct mark'ının tek başına ya da birlikte bulunması: eşleşmeyen bağlantı korunmalı, açık route çatışması başka trafiği engellememeli — Task 4/9.
4. Mod onayı beklerken profil değişmesi/helper restart'ı veya kaydetme başarılıyken apply reddi: eski önizleme yeni taslağı onaylamamalı, saved/applied ayrımı kaybolmamalı — Task 6/7/8.
5. Bir ailede lane kaybı, diğer yöneticinin OS default'unu değiştirmesi ve eski owned socket: eşleşmeyen yeni trafik OS'yi izlemeli; mevcut owned affinity ile yeni akış karıştırılmamalı — Task 4/6/9.

## Dosya ve sorumluluk haritası

| Dosyalar | Sorumluluk |
| --- | --- |
| Yeni `src/config/v3.rs`, `src/backend/legacy_journal_v2.rs`, `tests/fixtures/legacy-v2/` | Dondurulmuş V3 profil/V2 journal, eski hash ve exact cleanup kanıtı |
| `src/domain/{routing,mod}.rs`, `src/config.rs`, `src/config/v2.rs` | Zorunlu mod, ConfigV4 ve açık salt-okunur migrasyon |
| `src/backend/{plan,planner,nft_document,nftables,privileged}.rs` | Hash, seçici verdict, scoped guard, exact transaction |
| `src/backend/{runtime,topology}.rs`, `src/bin/routelane_helper.rs`, `src/daemon/{service,state}.rs`, `src/ipc/protocol.rs` | Sürümlü yetki, gözlenen ağ bilgisi, doğrulanmış uygulanan mod |
| Yeni `src/backend/routing_preview.rs`, `src/bin/routelanectl.rs` | Ortak yan etkisiz risk önizlemesi ve açık CLI onayı |
| `src/ui/{primary,connections,overview,session,controller,window,i18n}.rs` | Mod seçimi, önizleme/onay, saved/applied ve TR/EN açıklamalar |
| Yeni `tests/selective_*_contract.rs`, mevcut contract/GTK testleri | Seçici semantik, uyumluluk, hata ve UI regresyonları |
| Yeni `tests/root/selective_routing.rs`, `tests/root/mod.rs`, `tests/root/system_bus_acceptance.sh` | Gerçek paket, lab köprüsü, yetki ve yükseltme kabulü |
| Yeni `docs/selective-routing-verification.md`, README ve paket contract'ları | Açık kanıt matrisi, kullanım ve güvenli paket teslimi |

Tek davranışın bağımlı katmanlarıdır; ayrı ürünler gibi yayınlanmaz. Sıra Task 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10. Her görevin commit'i yalnız kendi hunks'larını içerir; mevcut dirty baseline sahiplenilmez. Task 9'un trafik kapısı olmadan ürünün düzeldiği veya host kurulumuna hazır olduğu iddia edilmez.

## Başlangıç kontrolü

Yürütme onayından sonra `rtk git status --short --branch`, `rtk git diff --stat`, `rtk cargo test --all-targets --no-fail-fast` çalıştır. Son tarihsel tur 451 passed/1 failed/2 ignored; bilinen bağımsız hata `ui::desktop_catalog::tests::sandbox_launcher_is_not_selectable_as_a_host_application`. Yeni koşuyu ayrı kaydet; bu hatayı sessiz skip ederek tam PASS raporlama. Kaynak düzeltmesiyle ilgisiz hatayı ayrı tut; ek başarısızlıklar varsa önce teşhis et. Plan yazılırken bu testler yeniden çalıştırılmadı.

### Task 1: Eski V2 journal ve V3 profil kanıtını dondur

**Files:** create `src/config/v3.rs`, `src/backend/legacy_journal_v2.rs`, `tests/fixtures/legacy-v2/{committed,pending,cleanup-pending}.json`, `tests/fixtures/legacy-v2/evidence.json`; modify module declarations in `src/config.rs`, `src/backend/mod.rs`; tests in the new legacy module.
**Interfaces:** produces `ProfileV3`, `RoutingPolicyV3`, `ConfigV3` wire DTO'ları ve `LegacyJournalV2::decode(json: &[u8]) -> Result<Self, PrivilegedError>`, `LegacyPlanV2::content_hash(&self) -> PlanHash`, `LegacyPlanV2::state_chain_name(&self) -> String`. Eski journal'ın runtime/restore/pending/ayar kayıtları aynı alan sırası ve atlama kurallarıyla dondurulur; current Profile/KernelPlan'a dönüşüm yoktur.

- [ ] Eski renderer/serializer değişmeden, repo kaynaklı sentetik iki-lane örneğinden committed/pending/cleanup fixture'ları ve literal hash/marker/NFT fingerprint/routing tuple kanıtını kaydet. Host'un gerçek profil/journal'ını veya sırları fixture'a alma. Eski davranış için kanıt üretme komutunu `evidence.json` yanında açıkla; bu değerler yeni uygulamadan hesaplanmış expected olmayacak.
- [ ] `legacy_v2_hash_and_marker_match_frozen_evidence`, `legacy_v2_unknown_fields_and_wrong_version_are_rejected`, `legacy_profile_does_not_gain_new_policy_fields` testlerini yaz: literal expected hash/marker ile eşitlik, serialize edilen routing'de `unmatched_traffic` yokluğu ve bozuk sürüm reddi.

  ```rust
  assert_eq!(plan.content_hash(), evidence.plan_hash); // checked-in literal evidence
  assert_eq!(plan.state_chain_name(), evidence.state_chain_name);
  assert!(serde_json::to_value(&profile.routing).unwrap().get("unmatched_traffic").is_none());
  ```

- [ ] RED: `rtk cargo test --lib legacy_journal_v2` — yeni decoder/hash davranışı yokken ilgili assertion'ların başarısızlığını gör; yalnız derlenmeme RED değildir. Ardından frozen DTO/decoder/hash'i mevcut V1 örüntüsüyle uygula; yeni renderer çağırma.
- [ ] GREEN: aynı komut + `rtk cargo test --test primary_runtime_contract`; fixture hash'lerini yeni tipe uydurmak için değiştirme.
- [ ] İncele, sadece ilgili hunks'ları stage et, `rtk git diff --cached --check`; commit: `test(compat): freeze v2 journal and v3 profile evidence`.

### Task 2: Mod ve kayıpsız ConfigV4 migrasyonu

**Files:** modify `src/domain/{routing,mod}.rs`, `src/config.rs`, `src/config/v2.rs`, `src/config/v3.rs`, current config consumers in `src/bin/routelanectl.rs` and `src/ui/{session,controller,window}.rs`; create `tests/selective_config_contract.rs`; update existing profile/routing literals found by `rtk proxy rg -n 'RoutingPolicy \{|ConfigV3' src tests`.
**Interfaces:** produces `UnmatchedTraffic::{PreserveSystem,UsePrimary}` (`Copy/Eq/Serialize/Deserialize`, default `PreserveSystem`), required `RoutingPolicy.unmatched_traffic: UnmatchedTraffic`, `ConfigV4`, `ConfigDocument::{V4,V3,V2,Legacy}`, `into_current() -> anyhow::Result<ConfigV4>`. Current schema=4; frozen ConfigV3 remains the Task 1 wire type. Existing load/save APIs keep their names but use ConfigV4.

- [ ] Tests `v3_migration_preserves_rules_and_primary_but_proposes_system_routing`, `v4_requires_explicit_unmatched_mode`, `desired_schema4_round_trip`, `migration_load_never_saves_or_activates`: assert schema=4, mode=PreserveSystem, `routing_notice_pending=true` for older input, profiles disabled, IDs/priority/actions/backups/language/launch arguments preserved. Missing mode, unknown mode and version=5 fail without file mutation; v2/legacy cases retain prior validation.

  ```rust
  assert_eq!(migrated.schema_version, 4);
  assert_eq!(migrated.profiles[0].routing.unmatched_traffic, UnmatchedTraffic::PreserveSystem);
  assert!(migrated.routing_notice_pending);
  assert!(migrated.profiles.iter().all(|p| !p.enabled));
  ```

- [ ] RED: `rtk cargo test --test selective_config_contract`; after introducing the type, tests must expose old migration/validation failures before bodies change.
- [ ] Implement strict V4 encoding and explicit V3/V2/legacy readers; preserve atomic 0600/no-symlink checks. Mechanical current literals explicitly choose mode; do not change Task 1 frozen DTO/hash evidence or silently deduplicate imported invalid profiles.
- [ ] GREEN: selective config + `rtk cargo test --test primary_config_contract --test duplicate_rule_contract --test cli_contract`; V3 historical fixtures stay V3, modern fixtures become explicit V4.
- [ ] Review/stage intended hunks; commit: `feat(config): add selective routing policy and v4 migration`.

### Task 3: Journal V3 ve cleanup-only V2 yükseltme kapısı

**Files:** modify `src/backend/{privileged,legacy_journal_v2}.rs`, `src/daemon/service.rs`; create `tests/selective_compat_contract.rs` where public seams suffice; private executor/store cases in `privileged.rs` tests.
**Interfaces:** Task 1 DTO consumed by `decode_journal(json: &[u8]) -> Result<StoredAppliedState, PrivilegedError>`; `StoredAppliedState` journal_version=3, `legacy_v2: Option<LegacyJournalV2>`. Add `CommandPrivilegedBackend::cleanup_legacy_v2(&mut self) -> Result<CleanupResponse, PrivilegedError>`. Legacy inspection exposes an explicit cleanup-required diagnostic; daemon maps it to `DAEMON_LEGACY_CLEANUP_REQUIRED`, not orphan recovery.

- [ ] Tests `v2_inspect_and_refresh_never_mutate`, `v2_activation_is_refused_before_kernel_commands`, `v2_exact_cleanup_preserves_foreign_sentinels`, `v2_cleanup_resumes_only_an_exact_prefix`. Include each Task 1 journal stage, changed ifindex/foreign policy tuple/NFT fingerprint, interrupted interface-setting receipt and unknown future journal. Assert no activation/refresh, no mutations from Inspect, and refusal without deletion on ambiguous evidence.

  ```rust
  assert!(mutation_commands_after_inspect.is_empty());
  assert_eq!(foreign_snapshot_after_cleanup, foreign_snapshot_before_cleanup);
  assert_eq!(activation_error.code(), "DAEMON_LEGACY_CLEANUP_REQUIRED");
  ```

- [ ] RED: `rtk cargo test --lib v2_`; wrong activation/adoption or absent compatibility path must cause failing assertions. Implement the strict legacy wrapper and ownership/cleanup path; retain V1 cleanup. Pending V2 data may only drive its frozen exact cleanup proof, never current render/compile/activation.
- [ ] Journal cleanup intent durably before deletion; revoke old runtime authority before mutations; preserve/resume the exact cleanup prefix after interruption. Do not claim schema-2 rollback succeeded by rendering schema-3. No current plan until legacy cleanup finishes; expose user-recoverable action without silently invoking it.
- [ ] GREEN: the new private tests + selective compatibility and `rtk cargo test --test ipc_contract`; repeat legacy V1 tests. Failed cleanup must retain enough evidence to retry safely and may not return Inactive.
- [ ] Review/stage intended hunks; commit: `fix(upgrade): require exact legacy cleanup before modern activation`.

### Task 4: Seçici plan, paket verdict'leri ve foreign mark sınırı

**Files:** modify `src/backend/{plan,planner,nft_document,nftables}.rs`, `tests/nftables_contract.rs`, `tests/primary_{planner,selection}_contract.rs`, `tests/health_contract.rs`; create `tests/selective_planner_contract.rs`, `tests/fixtures/health/{healthy,missing-lane}/plan-v3.json`. Preserve existing old `plan.json` evidence.
**Interfaces:** required `KernelPlan.unmatched_traffic: UnmatchedTraffic`; existing compile/render signatures unchanged. Plan hash contains the mode, not generation. Extract `fn rule_predicates(rule: &CompiledRule) -> Vec<serde_json::Value>` inside `nft_document.rs`; explicit verdict and Task 5 guard both consume it.

- [ ] Extend existing packet evaluator with a real test packet cgroup value (instead of always `not-selected`); do not expose test APIs in production. Test `selective_unmatched_dual_stack_preserves_packet`, `selective_foreign_mark_is_only_rejected_on_an_explicit_route`, `explicit_rule_order_and_cgroup_scope_are_preserved`, `mode_changes_plan_and_policy_hashes`. For lab/public/remote-private packets assert `(mark,ct_mark,translated)==(0,0,false)` and no reject/drop across output + egress_guard + source_nat; repeat foreign pairs `(42,0),(0,42),(42,43)` unchanged when unmatched.
- [ ] Pin explicit matching Route/Direct foreign-mark rejection without changing marks; Block reject; earlier Route before later Block; CIDR overlap, DNS alias and selected/unselected cgroup packets. New unmatched IPv6 must pass even if all RouteLane lanes lack IPv6; an explicit protected IPv6 match still follows its Block/fallback/Inactive policy.

  ```rust
  assert_eq!((packet.mark, packet.ct_mark, packet.translated), (0, 0, false));
  assert!(!matches!(verdict, "reject" | "drop"));
  assert_eq!((foreign.mark, foreign.ct_mark), (42, 43));
  assert_ne!(selective.content_hash(), use_primary.content_hash());
  ```

- [ ] RED: `rtk cargo test --test nftables_contract --test selective_planner_contract`; observe existing default marking/global foreign rejection fail new assertions. Implement selective final `return`, per-matched-rule conflict handling and mode hashing. UsePrimary fixtures explicitly choose UsePrimary and retain primary/failover semantics.
- [ ] GREEN: repeat + planner/primary planner/duplicate/health contracts. Assert existing unmarked established/reply traffic stays unmanaged; owned backup sessions keep their lane after failback. Source proof, ifindex+name checks, terminal routes and scoped NAT remain intact.
- [ ] Review/stage intended hunks; commit: `feat(routing): preserve unmatched traffic in selective mode`.

### Task 5: Scoped guard ve transaction hata regresyonları

**Files:** modify `src/backend/{nft_document,privileged,transaction}.rs`, `tests/nftables_contract.rs`, `tests/primary_transaction_contract.rs`; create `tests/selective_transaction_contract.rs`.
**Interfaces:** existing `render_nft_document(active: Option<&KernelPlan>, pending: Option<&KernelPlan>, guard: bool) -> Value` consumes Task 4 predicates; current `PendingExpectations` fingerprints model the same scoped guard. No parallel predicate interpreter or global temporary firewall is introduced.

- [ ] Tests `selective_guard_only_covers_active_pending_union`, `empty_selective_guard_does_not_reject_unmatched`, `cgroup_guard_uses_the_same_match_scope`, `rollback_never_blocks_unmatched_packets`. Apply exact packet evaluator to guard and inject a failure at every `ApplyStage` using existing fake executor/store; assert unmanaged lab/public/foreign packets never reject, managed scope cannot leak, previous modern policy/hash restored.

  ```rust
  assert!(!matches!(unmanaged_guard_verdict, "reject" | "drop"));
  assert_eq!(managed_guard_verdict, "reject");
  assert_eq!(committed_hash_after_rollback, previous_committed_hash);
  ```

- [ ] RED: `rtk cargo test --test nftables_contract --test selective_transaction_contract`; global guard must fail the unmatched assertions. Implement active/pending predicate union plus owned-flow protection; UsePrimary adds the broad scope it explicitly owns. Preserve narrow control exemptions and rule order; compare text/JSON semantic output.
- [ ] Test interruption/restart with all modern pending fingerprints, guard removal and no-op repeated apply; assert no owned tuple duplication and no foreign mutation. Same transition replay must not generate endless refresh work.
- [ ] GREEN: repeat + `rtk cargo test --test transaction_contract --test primary_transaction_contract`; retained legacy cleanup fingerprints from Task 1 must not change.
- [ ] Review/stage intended hunks; commit: `fix(transaction): scope selective guards to managed traffic`.

### Task 6: Protokol 3, yetki ve gerçek uygulanan mod

**Files:** modify `src/backend/{runtime,privileged}.rs`, `src/bin/{routelane_helper,routelane_daemon}.rs`, `src/daemon/{service,state}.rs`, `src/ipc/protocol.rs`, existing IPC/runtime/daemon contracts.
**Interfaces:** `IPC_PROTOCOL_VERSION=3`, `RUNTIME_PROTOCOL_VERSION=3`. Add read-only Helper2 `GetRuntimeProtocol() -> u32` and `PrivilegedHelperClient::runtime_protocol_version(&self) -> Result<u32, DaemonError>`; preflight before saving desired state or changing kernel. `RuntimeStatus.unmatched_traffic: Option<UnmatchedTraffic>` is Some only for verified modern runtime, None for inactive/legacy; helper derives it from its stored profile, not GUI claims.

- [ ] Tests `protocol2_peers_are_rejected_before_mutation`, `mode_change_requires_a_new_policy_grant`, `status_reports_applied_not_saved_mode`, `automatic_refresh_keeps_authorized_mode`. Assert protocol2 requests/unknown capability cause actionable mismatch with zero config/kernel mutation; grant policy hash differs across modes; same-name/new-ifindex cannot acquire authority; runtime cannot broaden PreserveSystem into UsePrimary automatically.

  ```rust
  assert_eq!(error.code(), "DAEMON_HELPER_PROTOCOL_MISMATCH");
  assert_eq!(config_saves_after_request, 0);
  assert_eq!(status.primary.unmatched_traffic, Some(UnmatchedTraffic::PreserveSystem));
  assert_ne!(selective_grant.policy_hash, primary_grant.policy_hash);
  ```

- [ ] RED: relevant `rtk cargo test --test primary_runtime_contract --test primary_daemon_contract --test ipc_contract`. Implement capability/version gates and verified mode pairing through Inspect; update all test helpers explicitly, never make their default return a fake successful capability for legacy peers.
- [ ] Preserve status event_sequence ordering, cleanup generation=0 and apply→deactivate→apply tests. Test helper restart/name-owner change and saved-mode change while helper remains Armed; applied mode must stay the helper's value. Protocol mismatch must not auto-retry or downgrade.
- [ ] GREEN: repeat IPC/runtime/daemon + `rtk cargo test --test cli_contract`; Inspect remains nonmutating and does not authorize itself.
- [ ] Review/stage intended hunks; commit: `feat(runtime): bind selective scope to protocol3 grants`.

### Task 7: Salt okunur bağlı ağ önizlemesi ve CLI onayı

**Files:** create `src/backend/routing_preview.rs`, `tests/selective_preview_contract.rs`; modify `src/backend/{mod,privileged,topology}.rs`, `src/bin/{routelane_helper,routelanectl}.rs`, `src/daemon/service.rs`, `src/ipc/protocol.rs`, `tests/cli_contract.rs`.
**Interfaces:** `RoutingWarning::{LocalNetworkRule { rule_id: RuleId, network: IpNet }, AllTrafficRule { rule_id: RuleId }}`, `RoutingPreview { policy_hash: PlanHash, warnings: Vec<RoutingWarning> }`; `routing_preview(profile: &Profile, resolutions: &ResolutionSnapshot, observed_networks: &[IpNet]) -> RoutingPreview`. `InspectResponse.observed_connected_networks: Option<Vec<IpNet>>`, populated only from fresh root-observed topology at helper boundary via existing `capture_topology`. Add `DaemonRequest::PreviewProfile(Profile)` → `DaemonResult::Preview(RoutingPreview)`; no config/save/cgroup/grant mutation. Missing observed facts yields explicit preview-unavailable, not an empty success.

- [ ] Tests `local_cidr_overlap_is_warned_without_becoming_an_exit`, `disabled_lab_rule_is_not_warned_or_deleted`, `preview_uses_fresh_root_networks`, `preview_never_saves_or_activates`. For `10.65.201.0/24 → Ethernet`, assert warning identifies that RuleId and prefix; exact/subnet/supernet/host matches, IPv6 and resolved domain prefix handled; Any/sourceAny warns whole scope. No additional lane grant and no rule edits. Unknown topology cannot be shown as safe; restriction to grant never promotes mpqemubr0 as an exit.

  ```rust
  assert!(preview.warnings.contains(&RoutingWarning::LocalNetworkRule {
      rule_id: RuleId::from("lab-route"), network: "10.65.201.0/24".parse().unwrap(),
  }));
  assert_eq!(profile_after_preview, profile_before_preview);
  assert!(!grant.interfaces.values().any(|i| i.name == "mpqemubr0"));
  ```

- [ ] RED: `rtk cargo test --test selective_preview_contract`; implement pure overlap inspection with canonical prefix/family handling. Helper captures topology off the async/GTK worker and can still return inspection diagnostics if capture fails; preview then refuses confirmation. Inject observed topology at tests; never invoke host commands from a fake backend.
- [ ] CLI adds `--acknowledge-routing-impact` for migrated notice, UsePrimary or nonempty preview warnings. Without required acknowledgement, abort before ApplyProfile/save, explain risk and preserve original file; ordinary explicit V4/PreserveSystem/no-warning apply still works. `apply-profile` obtains a read-only preview first; timeout never replays mutation. Pin original CLI invocation and new flag cases in `tests/cli_contract.rs`.
- [ ] GREEN: repeat preview/CLI + primary topology + IPC contracts. Preview policy hash identifies its exact profile/resolution snapshot; UI/CLI must discard it after draft changes, not present it as ongoing connectivity proof.
- [ ] Review/stage intended hunks; commit: `feat(routing): preview local-network impact before activation`.

### Task 8: GTK seçim, onay ve doğru durum dili

**Files:** modify `src/ui/{primary,connections,overview,session,controller,window,i18n}.rs`, `src/ui/rule_editor.rs`; add behavioral tests alongside existing UI/GTK tests.
**Interfaces:** `PrimaryDraft.unmatched_traffic: UnmatchedTraffic` feeds existing `validate_primary_draft(&Profile,&PrimaryDraft) -> Result<Profile,PrimaryEditError>`. Existing save/apply confirmation state machine consumes Task 7 PreviewProfile reply; pending preview is bound to draft revision and daemon epoch. No new UI event loop or dependency.

- [ ] Tests `mode_cancel_is_side_effect_free`, `stale_preview_cannot_approve_an_edited_profile`, `saved_mode_is_not_presented_as_applied`, `double_confirmation_sends_one_apply`. Assert Cancel/Escape/window close produce zero save/apply, edited draft/helper reconnect invalidates old preview, apply failure preserves saved/applied distinction, and packet scope changes are disclosed before action.

  ```rust
  assert!(change.cancel().is_empty());
  assert_eq!(apply_requests_after_double_confirm, 1);
  assert_eq!(applied_scope_after_failed_apply, Some(UnmatchedTraffic::PreserveSystem));
  ```

- [ ] RED: `rtk cargo test --bin routelane ui::primary::tests` and `rtk cargo test --bin routelane ui::session::tests`. Add keyboard-accessible selector named `routing-scope` in Connections, choices **Yalnız kuralları yönlendir** / **Diğer trafiği de ana hatta yönlendir**; selection flows through the existing confirmation gate. Mode/disclosure text covers mobile data, foreign-mark conflict and reconnecting previously marked sessions. Migration cancellation saves/applies nothing.
- [ ] Overview displays saved and helper-verified applied scope separately; selective unmatched text is **Diğer trafik: işletim sistemi yönetiyor**. Direct is **Ana hattı kullan** / **Use the primary connection**, not system bypass. Primary/failover notifications explicitly refer to managed traffic in selective mode; local policy health never proves lab/site reachability.
- [ ] GREEN: relevant UI tests + `rtk proxy bash scripts/run-gtk-smoke.sh`. GTK smoke drives both modes, cancel/Escape, lab warning → edit/disable, stale preview and helper error; private Broadway/session bus/config only. Capture TR/EN screenshots of the changed controls for handoff; preserve version floors and accessible labels/focus.
- [ ] Review/stage intended hunks; commit: `feat(ui): expose selective scope with impact confirmation`.

### Task 9: Gerçek kernel, lab ve yükseltme kabulü

**Files:** create `tests/root/selective_routing.rs`; modify `tests/root/mod.rs`, `tests/root/primary_failover.rs`, `tests/root/system_bus_acceptance.sh`; create `docs/selective-routing-verification.md`.
**Interfaces:** existing root integration helper request protocol and namespace fixtures; new test module uses fresh bridge/lab namespace `10.65.201.0/24`, lab target `10.65.201.60`, two uplinks and a distinct foreign-mark sentinel, only inside disposable VM. Existing broad failover fixtures explicitly use UsePrimary, not the new default.

- [ ] Add real traffic tests `selective_lab_and_unmatched_dual_stack_keep_system_exit`, `explicit_routes_use_both_lanes`, `selective_switch_failure_preserves_lab`, `foreign_vpn_and_forwarded_container_keep_state`, `v2_upgrade_requires_exact_cleanup_then_reauthorization`. Use new ICMP identifiers/TCP connections; receive-side source addresses and per-interface counters prove paths, no reliance on route-get alone.

  ```rust
  assert!(fresh_lab_ssh_succeeded);
  assert_eq!(observed_unmatched_mark, 0);
  assert_eq!(lab_received_source, "10.65.201.1".parse::<std::net::IpAddr>().unwrap());
  assert_eq!(foreign_state_after, foreign_state_before);
  ```

- [ ] Before executing, compile only: `rtk cargo test --test root --features root-integration --no-run`. Record NOT VERIFIED if no authorized disposable VM exists; do not run host root tests or reuse central/master/slave lab VMs. Inspect current root harness invocation and VM authorization before commands; upload/build artifacts only inside that test VM.
- [ ] VM RED against pre-change behavior must demonstrate a lab/unmatched/guard regression; if a matching frozen baseline cannot run, explicitly record missing kernel RED rather than claim it. Run modern tests in strict/loose reverse-path conditions, one-family loss, default route change by a separate manager and failback; unmatched new traffic follows OS, owned prior sessions remain bound.
- [ ] Verify V1/V2 legacy cleanup, V3 interrupted apply/rollback/restart, mixed daemon/helper versions, real system-bus/Polkit and owned-only removals. Snapshot foreign firewall/routes/settings before/after; no global changes. Include selected application cgroup behavior, not only IP rules.
- [ ] Record command, artifact version, environment, exit code and packet evidence by Ubuntu/Debian/Fedora row. Only actual PASS opens the corresponding gate; exit77, compilation and rootless-only evidence remain NOT VERIFIED. Commit scoped tests/evidence: `test(routing): verify selective traffic and upgrade safety in vm`.

### Task 10: Tam regresyon, paket ve devir kapısı

**Files:** modify README, `docs/selective-routing-verification.md`, `docs/handoff.md`, `tests/primary_packaging_contract.rs`; packaging scripts only if protocol/config4 breaks an existing installed-path contract. No automatic host install.
**Interfaces:** no new product APIs. Delivery names saved/applied mode, schema/protocol versions, exact verification scope and remaining risks.

- [ ] Run `rtk cargo test --all-targets --no-fail-fast`, `rtk cargo clippy --all-targets -- -D warnings`, `rtk proxy cargo fmt --all -- --check`, `rtk git diff --check`, `rtk cargo build --release --bins`, isolated GTK smoke and root no-run. Name every failure, including the pre-existing Snap test; never report a red full suite as PASS.
- [ ] Package contract asserts matching GUI/daemon/helper version set, config4/protocol3 documentation and official supported distro names. Verify development `.deb` with `rtk proxy bash packaging/deb/build-deb.sh`; inspect contents without installing. RPM/other distro gates need their actual environment; packaging success does not replace traffic/Polkit evidence.
- [ ] Self-review changed hunks against spec sections 1–10, frozen fixture hashes and all five Review Focus cases. Add missing tests before calling a gate complete; preserve unrelated dirty changes and do not claim independent review without one.
- [ ] Update README with selective default/opt-in full routing, explicit lab-rule impact, Direct meaning, old-journal cleanup/re-auth and old-socket limits. Update verification matrix and permanent handoff; no secrets or real user profiles. Commit only intended hunks: `docs(routing): document selective scope and verified upgrade gates`.
- [ ] Hand off the actual result and remaining NOT VERIFIED rows. Host package update, legacy cleanup, selected-rule editing, service restart and traffic reactivation require a separate scoped user decision; do not infer them from plan execution approval.

## Tasarım kapsamı öz-incelemesi

- Spec §1–4: Task 2/4/6; §5: Task 3/5; §6: Task 1/2/3/6/7; §7: Task 7/8/9; §8: Task 6/7/8; §9–10: Task 9/10.
- Mod, preview, hash ve runtime alan adları yukarıdaki Interfaces bloklarıyla sabittir. Frozen profile/journal tipi ile modern Profile/KernelPlan karıştırılmaz; toplu dosya taşıma veya bağımsız mimari refactor yoktur.
- Bu plan, yazılı tasarımın onayını uygulama onayı saymaz. Kullanıcı planı inceleyip onayladıktan sonra korunmuş main/inline yöntemiyle Task 1'den başlanır.
