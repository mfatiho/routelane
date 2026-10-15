# Primary Connection and Automatic Failover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Kullanıcının seçimi: main üzerinde native/inline; alt ajan ve ayrı worktree yok.

**Goal:** Sistem varsayılanını başlangıç ana hattı yap; onaylı manuel seçim, özel yardımcı hat kuralları ve UI'dan bağımsız otomatik geçiş sağla.

**Architecture:** Domain/config tercihi, aile bazlı saf seçim makinesi ve planner birbirinden ayrılır. Root helper ilk Polkit onayında sınırlı politikayı journal'a bağlar; kernel olaylarında yalnız bu politikayı yeniden derler. Daemon ve GTK doğrulanmış etkin çıkışı gösterir; kalıcı OS routing/DNS ayarları değişmez.

**Tech Stack:** Mevcut Rust/Tokio/serde/zbus/libc/ipnet; GTK4 v4_4, libadwaita v1_1; iproute2, nftables, systemd/Polkit. Yeni Rust bağımlılığı yok.

**Spec:** `docs/superpowers/specs/2026-10-14-primary-connection-failover-design.md`

**Durum (15 Ekim 2026):** Task 1–12 main üzerinde uygulandı. Dağıtım matrisi ve NOT VERIFIED satırları `docs/primary-failover-verification.md`; tüm satırlar PASS olmadan yayın kapısı açılmaz.

## Global Constraints

- Tercih edilen ana bağlantı, sistem varsayılanı ve etkin çıkış ayrı kavramlardır; `LaneId("default")` rol değildir.
- Varsayılan `FollowSystemDefault`; manuel seçimde uyarı + açık onay, iptalde sıfır kalıcı/IPC/kernel mutasyonu.
- Otomatik geçiş kesinti anında onay istemez; ana hatta dönüş **5 saniye** kesintisiz yerel hazır olma sonrasıdır.
- Yalnız yeni bağlantılar yeniden seçilir; mevcut yardımcı hat oturumları zorla kapatılmaz. Kopan uplink'te TCP/QUIC kesintisiz taşınma garantisi verilmez.
- IPv4/IPv6 ayrı; onaylı kullanılabilir çıkış yoksa ilgili yönetilen internet trafiği engellenir. Block kararları korunur.
- Local servis/LAN, DHCP/DNS ve yabancı firewall/route kaynakları korunur; global NAT, firewall flush, kalıcı metric/DNS ve global sysctl değişikliği yoktur.
- Kernel link/carrier/address/default-route olayları kullanılır; harici IP servisi, HTTPS inceleme ve sürekli internet probu yoktur.
- Ubuntu, Debian, Fedora ilk sürümden resmî destek kapsamındadır; doğrulanmamış distro veya root gate tamamlandı sayılmaz.
- Yeni config şeması zorunludur; eski uygulama yeni semantiği sessizce yok sayamaz. Load hiçbir zaman activate etmez.
- Inspect salt okunur kalır; otomasyon Polkit keep süresine veya arka planda interaktif ApplyPlan çağrılarına dayanmaz.
- Domain DNS/IP cache/DoH/CDN ve Snap/Flatpak sınırları bu özellik tarafından çözülmüş gösterilmez.
- Önceden dirty reliability değişiklikleri korunur. Her commit öncesi sadece ilgili hunks/files stage edilir; `git add .`, reset/stash/restore yoktur.
- Canlı host aktivasyonu, servis değişikliği ve paket kurulumu bu planın kod geliştirme izni değildir; yalnız ayrıca yetkilendirilen izole test ortamlarında yapılır.

## Review Focus

1. USB çıkarılıp aynı arayüz adı farklı ifindex ile geri gelirse eski yetki yeni aygıta geçmemeli — Task 2/8.
2. UI kaydetmişken helper reddederse, veya onay sırasında profil değişirse, tercih ile etkin çıkış karıştırılmamalı — Task 10.
3. Bir aile hazırken diğerinin route'u kaybolursa kontrolsüz IPv6/IPv4 kaçışı olmamalı — Task 3/4/7.
4. RouteLane'in kendi route yazmaları olay fırtınası yaratırsa tekrarlanan apply/nesil veya busy-loop olmamalı — Task 8.
5. Journal yazımı/cleanup yarıda kalır veya başka yönetici arayüz ayarını değiştirirse yabancı durum geri yazılmamalı — Task 6/8.

## Dosya ve sorumluluk haritası

| Dosya | Sorumluluk |
| --- | --- |
| `src/domain/routing.rs` (yeni), `profile.rs`, `mod.rs` | Ana tercih ve yardımcı sıra doğrulaması |
| `src/config/v2.rs` (yeni), `src/config.rs` | Dondurulmuş V2 DTO, açık V3 migrasyonu/atomik kayıt |
| `src/backend/topology.rs` (yeni), `plan.rs`, `daemon/service.rs` | Kernel kanıtını ayrıştırma, OS varsayılanını ayırma |
| `src/backend/primary.rs` (yeni) | Saf aile bazlı failover/failback durum makinesi |
| `src/backend/planner.rs`, `plan.rs`, `policy_route.rs` | Etkin ana çıkış + kurallar + kaynak/dönüş planı |
| `src/backend/nft_document.rs` (yeni), `nftables.rs`, `privileged.rs` | Ortak nft belgesi ve text/JSON eşdeğerliği |
| `src/backend/interface_settings.rs` (yeni), `privileged.rs`, `transaction.rs`, `inspect.rs` | Dar ayar journal'ı, exact ownership, rollback |
| `src/backend/runtime.rs`, `runtime_monitor.rs` (yeni), `routelane_helper.rs` | Root-owned yetki kapsamı, olay izleme ve seri mutasyon |
| `src/ipc/protocol.rs`, `daemon/state.rs`, `service.rs`, `routelane_daemon.rs` | Sürümlü gerçek durum ve yeniden bağlanma |
| `src/ui/primary.rs` (yeni), `connections.rs`, `overview.rs`, `session.rs`, `controller.rs`, `window.rs`, `i18n.rs`, `tray.rs` | Seçim/onay/etkin çıkış/bildirim |
| `tests/primary_*_contract.rs` (yeni), mevcut contract'lar, `tests/root/primary_failover.rs` (yeni) | Rootless, GTK, namespace ve gerçek yetki kabulü |
| `packaging/rpm/` (yeni), mevcut deb/data dosyaları, `docs/primary-failover-verification.md` (yeni) | Üç distro paket/servis kabulü ve kanıt kaydı |

Araştırma dayanağı: [kernel karar notu](../../research/primary-failover-kernel.md).
Bu alt sistemler tek davranışın bağımlı parçalarıdır; ayrı bağımsız ürünler gibi
yayınlanmaz. Task 7'nin kernel kapısı geçmeden runtime otomasyonu açılmaz.

## Ortak sözleşme kararları

- `PrimarySelection::{FollowSystemDefault, Lane(LaneId)}`;
  `RoutingPolicy { primary: PrimarySelection, backups: Vec<LaneId>, automatic_failover: bool }`.
  `Profile.routing: RoutingPolicy` V3'te zorunludur. İlk migrasyon: system-default,
  boş yardımcı sıra, otomatik geçiş açık; ilk aktivasyon öncesi UI uygun tek
  yardımcıyı önerir, sıralı kapsamı kullanıcıya gösterip kaydeder/onaylatır.
  FollowSystemDefault çözülünce o lane yardımcı adaylardan çıkarılır; değişim
  kalıcı tercihi yeniden yazmaz. Profilde olmayan bir OS çıkışı otomatik yetki kazanmaz.
- `InterfaceIdentity { ifindex: u32, name: String, kind: String, hardware_address: Option<String> }`.
  Runtime kimliği aynı boot/network-namespace içinde tüm alanlarla eşleşir;
  kayıp/yeniden yaratılmış veya yeniden adlandırılmış aygıt rearm gerektirir.
  V3'te kayıtlı explicit LaneId kayıpsa başka lane'e otomatik çevrilmez.
- `FamilyPair<T> { v4: T, v6: T }`; `FamilyExit::{Lane(LaneId), Blocked}`;
  `PrimaryDecision { preferred: FamilyPair<Option<LaneId>>, effective: FamilyPair<FamilyExit> }`.
  Runtime UI durumu ayrıca geçiş nedeni ve bekleyen geri dönüş deadline'ını taşır.
- `PrimarySelector::update(&mut self, policy: &RoutingPolicy, topology: &TopologySnapshot, now: Duration) -> PrimaryDecision`.
  `now` monotonic'tir; ilk seçim beklemez, aktif çıkış kaybı hemen seçim üretir,
  geri dönüş 5000 ms kesintisiz hazır olmayı ister. Mevcut çıkış yoksa hazır ana
  hattı bekletmez. `automatic_failover=false` iken çıkış kaybı Blocked üretir.
- `compile_profile_with_primary(&Profile, &TopologySnapshot, &ResolutionSnapshot, &PrimaryDecision, u64) -> Result<KernelPlan, PlanError>`.
  Mevcut `compile_profile` ilk seçim wrapper'ı olarak kalır; runtime yalnız
  explicit decision'lı API'yi çağırır. Plan hash yeni semantik alanları içerir,
  generation içermez. Lane mark/table/priority atamaları aynı yetki süresince
  sabittir; failover listeyi yeniden numaralandırmaz.
- `RuntimeActivation { protocol_version: u32, profile: Profile, resolutions: ResolutionSnapshot, expected_generation: u64 }`;
  `RuntimeGrant { owner_uid: u32, boot_id: String, network_namespace: u64, policy_hash: PlanHash, interfaces: BTreeMap<LaneId, InterfaceIdentity> }`.
  Policy hash profilin kuralları, routing, fallback, uygulama kimliği ve ilk
  resolution snapshot'ını kapsar. Grant client'tan alınmaz; caller UID bus'tan
  gelir. DNS kapsamı otomatik genişletilmez; mevcut resolution yenilemesi yeni
  yetkili apply üzerinden olur. Grant deaktivasyon/politika değişimiyle iptal,
  reboot'ta geçersizdir; Polkit cache ömründen bağımsızdır.
  `RuntimeRecord { grant: RuntimeGrant, profile: Profile, resolutions: ResolutionSnapshot, decision: PrimaryDecision }`
  journal'da birlikte tutulur; hash tek başına yeniden derleme için yeterli değildir.
  Restart'ta bekleyen failback süresi yeniden başlar; süreç-local Instant journal'a yazılmaz.
- GUI/daemon IPC **2**; yeni runtime D-Bus arayüzü `io.github.routelane.Helper2`.
  `ActivateRuntime(json) -> json`, `Inspect() -> json`, `Cleanup() -> json`.
  Activate yanıtı `RuntimeApplyResponse { status: RuntimeStatus, plan: KernelPlan, rollback_receipt: String }`;
  `RestoreRuntime(receipt: str) -> json` yalnız helper'ın root-owned önceki
  record'u için verdiği tek kullanımlık receipt'i kabul eder; keyfi eski plan
  veya grant client'tan alınmaz. Receipt owner UID/current commit'e bağlıdır;
  yeni nesil/politika varsa eski receipt onu geri alamaz. Restore mevcut Polkit
  yetkilendirmesini korur, otomatik failover yolu değildir.
  Otomatik plan kabul eden D-Bus metodu yoktur. Helper1 eski plan mutasyonları
  uyumsuzluk hatası verir; doğrulanabilir eski journal cleanup/recovery yolu
  korunur. Mevcut application-cgroup prepare/adopt metotları ve yetki kontrolleri
  korunur; uygulama başlatma özelliği kaldırılmaz. Eski client/helper karışımı
  fail-closed sürüm hatasıdır.
- `RuntimeStatus { grant: Option<RuntimeGrant>, generation: u64, decision: Option<PrimaryDecision>, phase: RuntimePhase, last_error: Option<String> }`;
  `RuntimePhase::{Inactive, Armed, Switching, Degraded, NeedsReauthorization}`.
  `InspectResponse.runtime: RuntimeStatus`; daemon bu bilgiyle ilgili applied
  plan/hash ve kernel snapshot'ını birlikte doğrular; salt sağlık göstergesi
  etkin profil kanıtı değildir.

### Task 1: Domain politikası ve kayıpsız V3 migrasyonu

**Files:** create `src/domain/routing.rs`, `src/config/v2.rs`, `tests/primary_config_contract.rs`; modify `src/domain/{mod,profile}.rs`, `src/config.rs`, `src/bin/routelanectl.rs`, `src/ui/{session,controller,window}.rs` and existing Profile fixtures.
**Interfaces:** consumes existing `validate_profile`; produces the common routing types, `ConfigV3`, `ConfigDocument::{V3,V2,Legacy}` and `into_current() -> anyhow::Result<ConfigV3>`.

- [ ] Write table-driven tests `v2_migration_preserves_ids_language_and_launcher_arguments`, `legacy_migration_never_activates`, `invalid_primary_and_duplicate_backups_are_rejected`, `v3_cannot_be_read_as_v2`, `save_failure_preserves_original_bytes`.
  Assertions: migrated routing equals system-default/[]/true; profiles disabled;
  rules, names, IDs and launcher argument vectors equal input; duplicate/unknown
  backup and explicit-primary-in-backups rejected; original file unchanged on failure.
  Pin with `assert_eq!(migrated.schema_version, 3)` and
  `assert!(migrated.profiles.iter().all(|p| !p.enabled))`.
- [ ] RED: `rtk cargo test --test primary_config_contract`; expect missing V3/policy API, then the new assertions failing. Do not count unrelated build failures as RED.
- [ ] Implement required routing field; freeze old Profile shape inside V2 DTO
  rather than allowing serde defaults to reinterpret old/new schemas. `CURRENT_SCHEMA_VERSION=3`;
  versions 2/legacy migrate explicitly, unknown versions fail. Update named config
  consumers and all Profile literals found with `rtk proxy rg -n 'Profile \{|ConfigV2' src tests`.
  Preserve atomic/private-file validation. Record Direct's changed meaning as
  migration information displayed before first activation, not auto-activation.
- [ ] GREEN: repeat contract; `rtk cargo test --all-targets`; all non-root tests pass with only the existing explicit GTK ignore.
- [ ] Commit scoped changes: `feat(config): add explicit primary routing policy and v3 migration`.

### Task 2: Güvenilir topology ve OS varsayılanı

**Files:** create `src/backend/topology.rs`, `tests/primary_topology_contract.rs`; modify `src/backend/{mod,plan,inspect}.rs`, `src/daemon/service.rs`.
**Interfaces:** produces `TopologyInput<'a> { links: &'a [u8], addresses: &'a [u8], routes_v4: &'a [u8], routes_v6: &'a [u8], rules_v4: &'a [u8], rules_v6: &'a [u8] }`, `parse_topology(TopologyInput<'_>) -> Result<TopologySnapshot, TopologyError>` and `capture_system_topology() -> Result<TopologySnapshot, TopologyError>`; snapshot exposes per-family OS-default, interface identity, usable source addresses and connected prefixes.

- [ ] Add fixtures/tests `owned_defaults_never_become_system_default`,
  `metric_and_family_select_os_default`, `carrier_route_or_address_loss_is_unavailable`,
  `tentative_ipv6_is_not_ready`, `same_name_new_ifindex_is_not_same_identity`.
  Assert VINN metric101 beats Ethernet102 despite reversed JSON order; owned
  table20000 is excluded; address/route absence cannot count as healthy.
  Fixture assertions include `assert_ne!(before_identity, recreated_identity)`;
  selected system-default ID must equal VINN's ID, never the owned-table first entry.
- [ ] RED: `rtk cargo test --test primary_topology_contract`; expect new parser/API assertions fail.
- [ ] Parse typed ip JSON (`-j -N`); filter RouteLane tuples and identify main-table
  default by metric per family. Respect carrier/flags and address readiness;
  accept gatewayless point-to-point routes with correct evidence. Equal-metric
  different exits, ECMP, or nontrivial foreign policy routing that prevents a
  unique OS-default choice produce an explicit ambiguous-selection error, not
  arbitrary order. Offer manual primary selection in that state. Do not enumerate
  only RouteLane's tables to infer source topology. Wrap blocking capture off GTK/Tokio workers.
- [ ] GREEN: repeat contract + `rtk cargo test --test health_contract`; old fixtures get explicit complete evidence.
- [ ] Commit: `feat(topology): separate system defaults from owned routing state`.

### Task 3: Aile bazlı failover/failback makinesi

**Files:** create `src/backend/primary.rs`, `tests/primary_selection_contract.rs`; modify `src/backend/mod.rs`.
**Interfaces:** consumes Task 1/2; produces `FamilyPair`, `FamilyExit`, `PrimaryDecision`, `PrimarySelector` and `next_deadline() -> Option<Duration>`.

- [ ] Build deterministic fixtures with Ethernet + VINN + third unapproved link.
  Test `primary_loss_uses_approved_backup_without_changing_preference`,
  `failback_requires_5000_ms_continuous_readiness`, `v6_loss_does_not_follow_v4`,
  `no_exit_or_disabled_failover_blocks`, `system_default_outside_grant_is_not_adopted`.
  Assert at4999ms backup, at5000ms primary; one down event resets deadline;
  v4 can choose Ethernet while v6 chooses VINN/Blocked. Repeated identical input equals previous decision.
  Pin boundary with `assert_eq!(at_4999.effective.v4, FamilyExit::Lane(vinn.clone()))`
  and `assert_eq!(at_5000.effective.v4, FamilyExit::Lane(ethernet.clone()))`.
- [ ] RED: `rtk cargo test --test primary_selection_contract`; expect missing selector then behavior failures.
- [ ] Implement monotonic pure state machine. Resolve SystemDefault separately
  for each family, intersect candidates with authorized lane identities and ordered
  backups. Failure is immediate; no available exit can fall through to OS main.
  Keep preferred selection unchanged through transitions; emit deadline only for pending stable failback.
- [ ] GREEN: repeat contract with fake time, no wall-clock sleep/network requests.
- [ ] Commit: `feat(routing): add deterministic primary failover selection`.

### Task 4: Planner sözleşmesi ve kuralların korunması

**Files:** modify `src/backend/{plan,planner,policy_route}.rs`, `tests/planner_contract.rs`; create `tests/primary_planner_contract.rs`.
**Interfaces:** consumes Task 3; produces `compile_profile_with_primary` and `KernelPlan.primary: PrimaryDecision`, `.local_networks: Vec<IpNet>`; compiled rules gain `family: IpFamily`; LanePlan gains `identity: InterfaceIdentity`, `source_v4/source_v6` usable-address vectors. `IpFamily` gets serde support.

- [ ] Test `unmatched_and_direct_use_primary_but_vinn_rule_stays_vinn`,
  `block_and_lane_fallback_precedence_survive_failover`, `inactive_rule_falls_through_to_primary`,
  `missing_family_is_explicitly_blocked`, `hash_changes_with_primary_not_generation`.
  Assert sorted explicit rule order unchanged, Direct becomes family-specific
  primary Mark or Block (never legacy mark0 bypass), and no synthetic editable rule is stored.
  Pin unavailable-family result with `assert_eq!(plan.primary.effective.v6, FamilyExit::Blocked)`;
  changing generation alone must satisfy `assert_eq!(first.content_hash(), later.content_hash())`.
- [ ] RED: `rtk cargo test --test primary_planner_contract`; expect missing fields/API then failed routing assertions.
- [ ] Compile explicit fallbacks per family, then family default decisions.
  Explicit Route missing-family follows its configured fallback/failure policy,
  not an unauthorized arbitrary exit. Direct resolves via PrimaryDecision.
  Keep lane resource assignment fixed across topology changes. Retain a terminal
  unreachable route for owned marks of unavailable families, so stale marks cannot
  escape via later OS rules. Include source addresses, family and default policy in hash/validation.
- [ ] GREEN: repeat contract + `rtk cargo test --test planner_contract`; all ordered-action assertions pass.
- [ ] Commit: `feat(planner): compile explicit per-family primary exits`.

### Task 5: Ortak nft üretimi ve bağlantı bağlılığı

**Files:** create `src/backend/nft_document.rs`; modify `src/backend/{mod,nftables,privileged,policy_route}.rs`, `tests/nftables_contract.rs`, `tests/fixtures/nftables/three-lane.nft`.
**Interfaces:** produces `render_nft_document(active: Option<&KernelPlan>, pending: Option<&KernelPlan>, guard: bool) -> serde_json::Value`; text renderer and privileged JSON batches consume the same semantic definition.

- [ ] Add tests `text_and_json_render_same_primary_policy`, `existing_backup_flow_keeps_mark_after_failback`,
  `explicit_block_precedes_flow_restore`, `local_and_control_exemptions_are_narrow`,
  `nat_requires_exact_owned_mark_and_interface`, `foreign_marks_are_not_overwritten`.
  Assert ct mark save/restore, exact lane mark+oif matching, per-family blocked
  default and no forwarding/global NAT rules. Preserve anonymous counter JSON `null` regression.
  Assert rendered NAT has exact lane predicates, `assert!(!text.contains("flush ruleset"))`
  and `assert!(!text.contains("hook forward"))`; JSON counter expression equals `serde_json::json!({"counter": null})`.
- [ ] RED: `rtk cargo test --test nftables_contract`; new assertions fail against output-only current rules.
- [ ] Capture the unmodified renderer fixture for Task 7's isolated kernel RED
  comparison now, before implementation; record its source commit/hash.
- [ ] Generate own `route/output -150`, `filter/prerouting -150` reply-mark
  restore and `nat/postrouting 100` source correction; NAT applies only locally
  generated managed flows, exact owned ct/meta mark and egress identity. No
  blanket masquerade. Preserve established authorized flows during automatic
  transitions; reject flows whose marked family/lane is no longer available.
  Do not rewrite existing ct tuples or flush conntrack.
- [ ] Define ordering explicitly: loopback/local-service and necessary DHCP,
  IPv6 ND/RA + connected-link multicast control exclusions; ordered explicit
  rule verdict lookup; explicit Block wins, otherwise restore an existing owned
  flow; new explicit Route/Direct; unmatched connected LAN bypass; family default.
  Thus explicit local-target Block/Route can apply without diverting DHCP/ND.
  “Block wins” means the first matching explicit rule's verdict, not promoting
  every later Block above an earlier Route. This preserves execution_cmp order.
  LAN includes actual connected prefixes, not blanket RFC1918 exemptions.
  Local stub DNS remains local; remote DNS follows normal target/primary policy,
  unless its server is on a preserved connected LAN. No resolver config changes.
  Do not silently present foreign-marked/VPN-controlled internet as managed;
  incompatible foreign policy selectors produce an actionable activation
  conflict rather than mark overwrite or an undocumented bypass guarantee.
- [ ] GREEN: repeat nft contract + `rtk cargo test --test planner_contract`; JSON and text fixtures agree structurally.
- [ ] Commit: `feat(nftables): add scoped flow affinity and source correction`.

### Task 6: Journal, exact ownership ve dar arayüz ayarı

**Files:** create `src/backend/interface_settings.rs`, `tests/primary_transaction_contract.rs`; modify `src/backend/{mod,privileged,transaction,inspect}.rs`, `tests/transaction_contract.rs`, `data/routelane-helper.service`.
**Interfaces:** produces `InterfaceSettingReceipt { identity: InterfaceIdentity, previous: u8, applied: u8 }`; extends versioned `StoredAppliedState` with receipts; includes new nft objects/routes in exact fingerprints and cleanup prefixes. Task 8 adds RuntimeRecord.

- [ ] Test `new_nft_chains_are_verified_not_just_table_markers`, `pending_commit_failure_restores_exact_previous_state`,
  `interface_setting_is_journaled_before_write`, `foreign_setting_change_is_not_restored`,
  `old_journal_can_clean_but_cannot_arm_runtime`, `unavailable_mark_has_terminal_route`.
  Assert failed journal fsync means zero setting writes; cleanup after another
  administrator changed the value reports divergence without overwriting it.
  Mock write log pins journal failure with `assert_eq!(setting_writes, 0)`;
  post-cleanup foreign value must satisfy `assert_eq!(current_setting, administrator_setting)`.
- [ ] RED: `rtk cargo test --test primary_transaction_contract`; expect ownership/journal tests fail.
- [ ] Version journal explicitly. Extend validation, guard, expected-document,
  pending/committed fingerprint, rollback, interrupted cleanup and orphan
  recognition together. Unknown chains or reserved tuples remain ambiguous;
  do not broaden recovery to deleting anything named routelane. Guard must cover
  new unmatched internet during partial activation while preserving local control.
  Freeze V1 journal/KernelPlan DTO and matching legacy ownership verifier for
  cleanup only; do not deserialize old state with invented primary defaults.
  Task 8 adds optional RuntimeRecord and a bounded previous-record restore
  receipt to the same versioned journal, updating MAX_STATE_BYTES explicitly.
- [ ] Implement fixed-purpose setting API, no arbitrary sysctl path/value IPC:
  `ensure_mark_aware_rpf(&InterfaceIdentity) -> Result<Option<InterfaceSettingReceipt>, PrivilegedError>`
  and `restore_mark_aware_rpf(&InterfaceSettingReceipt) -> Result<(), PrivilegedError>`.
  Write only selected interface `src_valid_mark=1` when effective value needs it;
  preserve rp_filter/all/default values. Validate identity immediately before
  opening/writing and cleanup. Journal intent before write; compare-and-restore
  applied value only. Keep ProtectKernelTunables=yes; add only the necessary
  `/proc/sys/net/ipv4/conf` sandbox exception, with helper whitelist excluding
  all/default and every other tunable. Actual service test is mandatory in Task 12.
- [ ] GREEN: repeat new contract + `rtk cargo test --test transaction_contract` + existing privileged unit tests via `rtk cargo test --lib backend::privileged`.
- [ ] Commit only feature hunks overlapping dirty baseline: `feat(backend): journal exact primary routing ownership and rollback`.

### Task 7: Gerçek kernel tracer-bullet kapısı

**Files:** create `tests/root/primary_failover.rs`; modify `tests/root/{mod,namespace_routing}.rs`, `scripts/run-root-integration.sh`.
**Interfaces:** consumes Tasks 2–6; isolated root harness supplies two uplinks,
  simulated remote TCP/UDP endpoints and per-egress source/counter evidence.

- [ ] Add real tests `primary_and_rule_paths_have_correct_source_and_return`,
  `backup_tcp_session_survives_new_flow_failback`, `ipv6_missing_family_has_no_escape`,
  `strict_and_loose_rpf_accept_only_correct_paths`, `local_dhcp_dns_and_foreign_resources_survive_cleanup`.
  Assert response reaches original client socket, remote observes chosen
  interface source; long-lived backup TCP continues while new TCP uses primary;
  UDP request/reply (QUIC-like transport, not a claimed full QUIC test) uses chosen exit.
  Endpoint evidence pins `assert_eq!(observed_source, vinn_source)` for a VINN
  rule and `assert_eq!(reply_payload, sent_payload)` on the original client socket.
- [ ] RED: run extended root suite against pre-feature behavior inside authorized
  disposable VM; confirm actual wrong source/egress/session assertions fail.
  Capture the existing renderer's legacy fixture before Task 5 changes it;
  use it only for baseline RED evidence, then test the new real backend for GREEN.
  Rootless permission failure/exit77 is **not** expected RED evidence.
- [ ] Extend harness only with registry-owned namespaces/veth and services bound
  inside them. Include IPv6-only/dual-stack, address removal, default-route loss,
  non-OS-default uplink, both rp_filter modes and all-links-down. Update isolated
  helper CLI test harness to exercise the same new backend, not a hand-coded
  replacement ruleset. Never flush host routing/firewall/conntrack.
- [ ] GREEN: `rtk proxy bash scripts/run-root-integration.sh` as root **inside
  authorized isolated VM**; all namespace tests pass and cleanup leaves foreign
  sentinel resources identical. Failure blocks runtime enablement; do not work
  around it with global sysctl/NAT, transparent proxy or disabled IPv6.
- [ ] Commit: `test(kernel): prove primary routing source and session safety`.

### Task 8: Sınırlı root runtime ve olay tabanlı otomasyon

**Files:** create `src/backend/{runtime,runtime_monitor}.rs`, `tests/primary_runtime_contract.rs`; modify `src/backend/{mod,privileged}.rs`, `src/bin/routelane_helper.rs`, `data/io.github.routelane.Helper.conf`.
**Interfaces:** consumes Tasks 1–7; produces common runtime types;
  `PrivilegedBackend::activate_runtime(&RuntimeActivation, u32, &TopologySnapshot, Duration) -> Result<RuntimeApplyResponse, PrivilegedError>`;
  `PrivilegedBackend::refresh_runtime(&TopologySnapshot, Duration) -> Result<RuntimeStatus, PrivilegedError>`.
  `PrivilegedBackend::restore_runtime(&str, u32) -> Result<RuntimeStatus, PrivilegedError>` restores only an exact receipt-owned previous record.
  Core activation authorizes real D-Bus caller; refresh has no client-callable D-Bus equivalent.

- [ ] Test `expired_polkit_cache_does_not_block_authorized_failover`, `grant_cannot_add_lane_rule_or_uid`,
  `same_name_new_ifindex_needs_rearm`, `self_generated_route_events_do_not_reapply`,
  `deactivate_cancels_queued_refresh`, `restart_requires_exact_grant_journal_and_kernel_match`,
  `journal_failure_does_not_publish_new_decision`. Use fake topology/time/backend
  and a counting authorizer; assert exactly one activation authorization, zero
  automatic authorizations, unchanged policy hash, one commit per content change.
  Assert `assert_eq!(authorization_calls, 1)` after activation plus outage plus
  failback; 100 duplicate monitor events satisfy `assert_eq!(applies_after, applies_before)`.
  Add `restore_receipt_cannot_roll_back_newer_transition_or_expand_scope` and
  `application_prepare_and_adopt_remain_authorized` as compatibility regressions.
- [ ] RED: `rtk cargo test --test primary_runtime_contract`; expect missing runtime API then bounded-authority assertions fail.
- [ ] Helper2 ActivateRuntime validates size, schema2, profile/resolution bounds,
  expected generation and caller UID; captures topology itself, creates grant,
  compiles plan and commits grant+plan atomically in the same journal. Prepare
  only authorized app cgroups. Manual change obtains new Polkit authorization;
  failed activation retains old grant/plan. No credentials supplied in JSON.
  Issue bounded single-use restore receipt with the prior RuntimeRecord;
  restore checks bus UID, authorization and exact current commit before touching
  resources. Crash/failed verification keeps the previous record recoverable.
- [ ] Keep one serialized backend mutation lock for ordinary operations and
  runtime refresh; root generation is authoritative. `refresh_runtime` derives
  candidate only from stored profile/resolutions/grant and fresh kernel evidence.
  Root-owned state has bounded sizes, no symlink/group-write acceptance. Resume
  helper restart only on matching boot/netns/identities/grant/hash/exact kernel;
  otherwise freeze mutation, report NeedsReauthorization and offer safe cleanup.
- [ ] Implement `ip monitor link address route` via existing typed process
  execution, absolute binary and no shell; messages trigger fresh snapshots,
  not trusted policy commands. Drain bounded buffers, coalesce refreshes,
  exclude owned-table feedback by content comparison; subscribe **before**
  initial snapshot. EOF/overflow triggers bounded restart+resnapshot and visible
  Degraded state, not polling spin. Use only one-shot 5s failback timer.
  Cleanup revokes grant before queued updates can run; cancellation prevents resurrection.
- [ ] GREEN: repeat runtime contract; all helper authorization/cleanup regressions pass with `rtk cargo test --all-targets`.
- [ ] Commit: `feat(helper): run consent-scoped automatic link failover`.

### Task 9: Daemon IPC, restart ve gerçek etkin çıkış

**Files:** modify `src/ipc/{protocol,client,server,mod}.rs`, `src/daemon/{state,service,mod}.rs`, `src/bin/{routelane_daemon,routelanectl}.rs`, `tests/{ipc,cli,health}_contract.rs`; create `tests/primary_daemon_contract.rs`.
**Interfaces:** consumes Helper2 `InspectResponse.runtime`; produces IPC2
  `DaemonStatus.primary: RuntimeStatus`, `DaemonEvent::PrimaryChanged { sequence: u64, generation: u64, primary: RuntimeStatus }`.
  `PrivilegedHelperClient::activate_runtime(&RuntimeActivation) -> Result<RuntimeApplyResponse, DaemonError>`
  and `restore_runtime(&str) -> Result<RuntimeStatus, DaemonError>` replace plan-based apply/restore in the new path.

- [ ] Test `helper_transition_updates_status_without_gui`, `daemon_restart_reconciles_valid_runtime`,
  `old_protocol_is_rejected_before_mutation`, `new_helper_old_client_mismatch_is_actionable`,
  `stale_helper_generation_cannot_replace_new_status`, `transition_failure_is_not_healthy`.
  Assert applied/desired preference distinction, ordered event sequence, correct
  effective v4/v6 and exact applied-plan hash verification before reporting Armed.
  Pin `assert_eq!(status.primary.generation, status.generation)` for a verified
  status and `assert_ne!(failed_status.primary.phase, RuntimePhase::Armed)`.
- [ ] RED: `rtk cargo test --test primary_daemon_contract`; expect missing IPC2 runtime state then reconciliation assertions fail.
- [ ] Replace ApplyProfile's helper ApplyPlan call with ActivateRuntime request.
  Keep desired-config save before apply and saved/applied failure distinction.
  Accept helper-produced plan/generation, not a daemon preview compiled from a
  potentially older topology. Verify returned policy hash matches requested
  profile/resolutions and pair plan/hash with Inspect. A legitimate newer
  automatic generation of the same grant is refreshed/verified, not treated
  as a response mismatch. On real candidate failure use RestoreRuntime receipt;
  a newer commit makes old restore ineligible and must not be overwritten.
  Daemon subscribes to Helper2 `RuntimeChanged(status_json)` signal, then takes
  initial Inspect snapshot to close subscribe/read race. Reconnect on helper
  owner changes; periodic refresh is not the failover engine. Daemon closed or
  restarted does not revoke a valid root runtime. Inspect failures/ambiguous
  journals show degraded/error, never reconstruct a trusted grant from user config.
  CLI prints preferred/effective per family and local-signal limitation.
- [ ] GREEN: new contract + `rtk cargo test --test ipc_contract` + `rtk cargo test --test cli_contract` + `rtk cargo test --test health_contract`.
- [ ] Commit scoped service hunks: `feat(daemon): expose verified primary runtime through ipc v2`.

### Task 10: GTK seçim, onay ve durum sunumu

**Files:** create `src/ui/primary.rs`; modify `src/ui/{mod,connections,overview,session,controller,window,i18n,tray,confirm}.rs` and GTK smoke tests.
**Interfaces:** consumes Task 9; produces `PrimaryDraft { profile_id: ProfileId, primary: PrimarySelection, backups: Vec<LaneId>, automatic_failover: bool }`, `validate_primary_draft(&Profile, &PrimaryDraft) -> Result<Profile, PrimaryEditError>`; existing UiController.save/UiSession correlation remains authoritative.

- [ ] Write pure tests `cancel_escape_close_emit_no_effect`, `double_confirmation_saves_once`,
  `stale_primary_draft_is_revalidated`, `saved_primary_is_not_effective_after_apply_failure`,
  `lane_delete_revalidates_primary_and_backup_references`. Assert no Save/IPC
  effect before confirm; confirm once → existing persistence path → active-only
  apply → correlated snapshot; failure leaves prior effective status visible.
  Pin cancellation with `assert!(effects.is_empty())`; repeated confirm yields
  `assert_eq!(persistence_request_count, 1)` and failure keeps prior effective LaneId.
- [ ] RED: `rtk cargo test --bin routelane primary_`; expect missing draft/confirmation behavior tests fail.
- [ ] Add accessible “Ana bağlantı / Sistem varsayılanı (arayüz)” selector,
  visible ordered backup controls, auto-failover switch; two lanes propose the
  other link, not LaneId/default heuristics. Refresh available choices async;
  stale editor references must not silently switch targets. Deleting referenced
  lane requires explicit revised policy/confirmation; cannot save dangling IDs.
- [ ] Reuse existing modal and save/apply gate; cancel is default, Escape/X
  cancel. Use spec's exact manual-change warning. First activation shows
  complete allowed lane order, mobile-data cost, Direct migration meaning and
  narrow per-interface src_valid_mark effect/restoration. Denying consent does
  not save or activate. Keyboard focus/labels and loading/error/retry states
  use current GTK/adwaita floors; no new toolkit dependency.
- [ ] Overview shows preferred primary, effective v4/v6 exit, backup use and
  “yerel bağlantı kanıtı; internet erişimi garantisi değildir”. Automatic events
  notify once per state transition; never pop a consent dialog during outage.
  UI closed needs no notification agent for routing to continue; reopen shows
  current verified state. Preserve rule/application editing, language restart
  consent, singleton behavior and recovery confirmations.
- [ ] GREEN: `rtk cargo test --bin routelane`; `rtk proxy bash scripts/run-gtk-smoke.sh` passes including selector keyboard/confirm/cancel/reopen scenarios with fatal warnings enabled.
- [ ] Commit: `feat(ui): add confirmed primary selection and clear failover status`.

### Task 11: Runtime uçtan uca güvenlik kabulü

**Files:** extend `tests/root/primary_failover.rs`, `tests/primary_runtime_contract.rs`; create `docs/primary-failover-verification.md` with test evidence and limitations.
**Interfaces:** consumes Tasks 8–10; produces end-to-end runtime acceptance evidence separate from pure kernel tracer-bullet.

- [ ] Add tests `gui_closed_link_loss_fails_over_without_authorization_prompt`,
  `runtime_rejects_replayed_generation_and_foreign_caller`, `helper_restart_keeps_or_safely_rejects_exact_state`,
  `partial_transaction_never_reports_armed`, `all_links_down_then_recovery_is_family_safe`.
  Verify physical carrier, address and route removal independently; interface
  flap resets 5s deadline. Assert no global settings or foreign tables change.
  Pin replay rejection with `assert_eq!(applied_generation_after_replay, applied_generation_before_replay)`;
  each saved foreign nft/rule/sysctl snapshot must equal its post-cleanup snapshot.
- [ ] RED: run new scenarios in authorized disposable VM/private integration
  bus; prove missing behavior before fixing. Auth-bypass root CLI is explicitly
  insufficient evidence for real D-Bus/Polkit acceptance.
- [ ] Extend fault injection for grant fsync, candidate apply, exact fingerprint
  verification and rollback failures. Retain last validated state/error; never
  launch applications, kill browser, flush conntrack, or silently retry cleanup
  outside consent. Assert UI/controller report saved-but-not-active accurately.
- [ ] GREEN: root suite passes; capture separate real system-bus Polkit test:
  activate with consent, invalidate/let expire authorization cache, close UI,
  drop main link, verify new flow on backup with no new dialog, then reject
  policy/UID/lane expansion without fresh authorization. Record exact environment,
  commands, observed source/egress and outcomes; unavailable infrastructure = NOT VERIFIED.
- [ ] Commit: `test(runtime): verify autonomous failover and bounded authority`.

### Task 12: Üç dağıtım, paket ve yayın kapısı

**Files:** modify `packaging/deb/build-deb.sh` and feature-required data files only;
  create `packaging/rpm/{routelane.spec,build-rpm.sh}`, `tests/primary_packaging_contract.rs`;
  update verification doc, README feature section and approved spec/plan status.
**Interfaces:** produces installable deb/rpm artifacts and versioned acceptance matrix; no implicit host install.

- [ ] Test `packages_ship_helper2_policy_and_consistent_versions`,
  `service_keeps_kernel_tunable_protection`, `package_upgrade_preserves_private_config_and_journal`.
  Assert GUI/daemon/helper protocol versions align, helper stays root, D-Bus
  policy allows required interface but authorization remains method-enforced;
  no broad writable kernel subtree beyond Task 6's documented exception.
  Unit content must satisfy `assert!(unit.contains("ProtectKernelTunables=yes"))`
  and `assert!(!unit.contains("ProtectKernelTunables=no"))`; package lists include the Helper2 policy/required binaries exactly once.
- [ ] RED: `rtk cargo test --test primary_packaging_contract`; expect missing rpm/Helper2 packaging assertions fail.
- [ ] Implement rpm packaging with the same owned files/Polkit/systemd/tmpfiles
  lifecycle as deb, preserving dirty reliability fixes. Debian/Ubuntu use deb;
  Fedora uses rpm. Build script selects artifacts deterministically. Do not
  claim Fedora support from a Debian build or container-only compile.
- [ ] GREEN rootless: `rtk cargo fmt --all -- --check`; `rtk cargo clippy --all-targets -- -D warnings`;
  `rtk cargo test --all-targets`; `rtk proxy bash scripts/run-gtk-smoke.sh`;
  `rtk cargo build --release --bins`; run each packaging script in its target build environment.
- [ ] On authorized clean Ubuntu/Debian/Fedora VMs record exact release/kernel,
  nft/iproute2/systemd/Polkit/GTK versions; install/upgrade/remove package,
  validate actual hardened helper can journal/write/restore only interface
  src_valid_mark, execute Tasks 7/11, test firewall coexistence (including
  Fedora firewalld/SELinux), reboot/rearm and uninstall restoration. Foreign
  firewall conflicts produce actionable error, not disabling host protection.
  All distro rows must PASS before first release; missing VM/permission is an
  explicit uncompleted gate, not skipped-success. Keep per-source build target
  dirs distinct to avoid stale Cargo artifacts from previous snapshot tests.
- [ ] Review spec acceptance1–12 against recorded evidence; leave unavailable
  accessibility/manual theme/AT and public-site DNS/CDN checks separately
  NOT VERIFIED. Commit only scoped feature changes: `build: verify primary failover on ubuntu debian and fedora`.

## Self-review and handoff

Spec coverage: terms/migration → Task1; topology/default → Task2; family/5s
selection → Task3; rule semantics → Task4; sources/flows/local controls →
Tasks5–7; bounded authorization/monitor/restarts → Task8; verified daemon →
Task9; confirmation/accessibility → Task10; runtime faults → Task11;
Ubuntu/Debian/Fedora and release gates → Task12. Review Focus1–5 each has a
named owning test. New types/protocol names above are the single interface
contract; executors must update both sides together, not invent aliases.

Bu belge uygulama için kullanıcı incelemesi bekliyor. Onaydan sonra native/main
yöntemi korunur. Her task RED → minimal uygulama → GREEN → scoped commit
akışıyla ilerler; başarısız kernel/yetki kapısı ürün davranışı daraltılarak veya
başarı varsayılarak geçilmez. Plan hazırlanırken ürün kodu/canlı ağ değişmedi.
