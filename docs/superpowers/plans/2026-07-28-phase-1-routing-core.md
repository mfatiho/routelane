# RouteLane Faz 1 Routing Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** nftables tabanlı en az üç lane'i, Route/Direct/Block eylemlerini,
generation tabanlı rollback'i ve Bağlantı Sağlığı doğrulamasını mevcut GTK
uygulamasına güvenli biçimde eklemek.

**Architecture:** Platformdan bağımsız domain modelleri ve deterministik planner
ortak Rust library'de yaşar. Normal kullanıcı daemon'ı desired/applied state'i
yönetir; system D-Bus ile aktive edilen dar yetkili helper, yapılandırılmış
`KernelPlan` nesnesini doğrular ve kernel'a uygular. GUI daemon'ın Unix socket
API'sini kullanır; mevcut tek süreçli engine ancak yeni yol doğrulandıktan sonra
kaldırılır.

**Tech Stack:** Rust 2021, Tokio, serde/serde_json, ipnet, uuid, blake3, zbus,
GTK4, Libadwaita, nftables, iproute2, systemd user service, system D-Bus,
Polkit.

## Global Constraints

- Normal `cargo test` root yetkisi istemeyecek.
- Resmî platform tabanı Ubuntu 22.04/24.04/26.04, Debian 12/13 ve Fedora
  43/44'tür.
- Resmî mimariler x86_64 ve aarch64'tür.
- Faz 1'de üretilen `.deb` geliştirme doğrulama artefact'ıdır; Ubuntu,
  Debian ve Fedora'nın tamamındaki x86_64/aarch64 matrisi Faz 5'te yeşil
  olmadan ilk resmî sürüm yayımlanmayacaktır.
- nftables isim alanı yalnızca `table inet routelane` olacaktır.
- Routing table aralığı `20000..=20099` olacaktır.
- fwmark aralığı `0x524c0001..=0x524c0064` olacaktır.
- Policy rule priority aralığı `10000..=10099` olacaktır.
- Profil başına en fazla 100 lane, 10.000 kural ve set başına 65.536 dinamik
  eleman kabul edilecektir.
- Küçük kural priority değeri önce çalışacak; eşitlikte `RuleId` artan sırası
  kullanılacaktır.
- Helper ham shell veya kullanıcıdan gelen ham nftables metni kabul etmeyecek.
- Her production davranışı önce beklenen nedenle başarısız olan testle
  geliştirilecektir.
- Mevcut kullanıcı değişiklikleri korunacak; rota testi silme değişikliği
  Bağlantı Sağlığı ile değiştirilecektir.

---

## Dosya yapısı

Bu faz sonunda aşağıdaki yeni dosyalar bulunacaktır:

```text
src/
  lib.rs
  domain/
    mod.rs
    rule.rs
    profile.rs
    health.rs
  backend/
    mod.rs
    plan.rs
    planner.rs
    nftables.rs
    policy_route.rs
    transaction.rs
    inspect.rs
  ipc/
    mod.rs
    protocol.rs
    framing.rs
    client.rs
    server.rs
  daemon/
    mod.rs
    state.rs
    service.rs
  health/
    mod.rs
    probes.rs
    report.rs
  bin/
    routelane_daemon.rs
    routelanectl.rs
tests/
  planner_contract.rs
  nftables_contract.rs
  transaction_contract.rs
  ipc_contract.rs
  health_contract.rs
  root/
    namespace_routing.rs
data/
  io.github.routelane.Helper.service
  routelane-daemon.service
  routelane-helper.service
  io.github.routelane.policy
```

Mevcut `src/routing/resolver.rs`, `src/ui/`, `src/config.rs` ve GTK binary'si
korunacak, yeni library API'sine aşamalı geçirilecektir.

---

### Task 1: Ortak library ve domain modelleri

**Files:**

- Create: `src/lib.rs`
- Create: `src/domain/mod.rs`
- Create: `src/domain/rule.rs`
- Create: `src/domain/profile.rs`
- Create: `src/domain/health.rs`
- Modify: `Cargo.toml`
- Test: `src/domain/rule.rs`
- Test: `src/domain/profile.rs`

**Interfaces:**

- Produces: `Rule`, `RuleSource`, `RuleTarget`, `RuleAction`, `Profile`, `Lane`,
  `LaneFailurePolicy`, `HealthState`, strongly typed IDs and
  `validate_profile(&Profile) -> Result<(), DomainError>`.
- Consumes: Yalnızca serde, uuid ve ipnet.

- [ ] **Step 1: Yeni domain modelinin başarısız validation testlerini yaz**

`src/domain/profile.rs` test modülüne şu davranışları ekle:

```rust
#[test]
fn rejects_route_rule_that_references_missing_lane() {
    let profile = fixture_profile(
        vec![fixture_lane("work", "wg0")],
        vec![fixture_rule(
            "rule-a",
            10,
            RuleTarget::Cidr("203.0.113.0/24".parse().unwrap()),
            RuleAction::Route(LaneId::from("missing")),
        )],
    );

    assert_eq!(
        validate_profile(&profile),
        Err(DomainError::UnknownLane(LaneId::from("missing")))
    );
}

#[test]
fn rejects_more_than_one_hundred_lanes() {
    let lanes = (0..101)
        .map(|n| fixture_lane(&format!("lane-{n}"), &format!("tun{n}")))
        .collect();
    let profile = fixture_profile(lanes, vec![]);

    assert_eq!(
        validate_profile(&profile),
        Err(DomainError::TooManyLanes { actual: 101, maximum: 100 })
    );
}

#[test]
fn rejects_duplicate_rule_ids() {
    let lane = fixture_lane("work", "wg0");
    let rule = fixture_rule(
        "duplicate",
        10,
        RuleTarget::Ip("203.0.113.7".parse().unwrap()),
        RuleAction::Direct,
    );
    let profile = fixture_profile(vec![lane], vec![rule.clone(), rule]);

    assert_eq!(
        validate_profile(&profile),
        Err(DomainError::DuplicateRuleId(RuleId::from("duplicate")))
    );
}
```

- [ ] **Step 2: Testlerin model bulunmadığı için başarısız olduğunu doğrula**

Run:

```bash
rtk cargo test domain::profile::tests --lib
```

Expected: `Profile`, `validate_profile` veya ilgili type'lar tanımlı olmadığı
için compile failure.

- [ ] **Step 3: Cargo bağımlılıklarını ve library kökünü ekle**

`Cargo.toml`:

```toml
[lib]
name = "routelane"
path = "src/lib.rs"

[dependencies]
ipnet = { version = "2", features = ["serde"] }
uuid = { version = "1", features = ["v4", "serde"] }
blake3 = "1"
```

`src/lib.rs`:

```rust
pub mod backend;
pub mod daemon;
pub mod domain;
pub mod health;
pub mod ipc;
```

Bu adımda henüz oluşturulmayan modüller için boş `mod.rs` dosyaları ekleme;
yalnızca `pub mod domain;` ile başla. Sonraki task'lar kendi modüllerini
`lib.rs` içine ekleyecek.

- [ ] **Step 4: Domain modellerini ve validation'ı minimal biçimde uygula**

ID newtype'ları aynı macro veya tekrar eden açık struct'larla şu sözleşmeyi
sağlamalı:

```rust
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LaneId(String);

impl From<&str> for LaneId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}
```

`Rule`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: RuleId,
    pub enabled: bool,
    pub priority: u32,
    pub source: RuleSource,
    pub target: RuleTarget,
    pub action: RuleAction,
}
```

`validate_profile` şu sırada kontrol yapmalı:

1. Lane sayısı `<= 100`
2. Kural sayısı `<= 10_000`
3. Lane ID'leri benzersiz
4. Kural ID'leri benzersiz
5. `Route` eylemlerinin lane referansları mevcut
6. `fallback_lane` referansları mevcut, kendi kendine değil ve fallback
   grafiğinde döngü yok

- [ ] **Step 5: Domain testlerini geçir**

Run:

```bash
rtk cargo test domain:: --lib
```

Expected: Tüm domain testleri PASS.

- [ ] **Step 6: Tüm mevcut testlerin gerilemediğini doğrula**

Run:

```bash
rtk cargo test
```

Expected: Mevcut 11 test ve yeni domain testleri PASS.

- [ ] **Step 7: Commit**

```bash
rtk git add Cargo.toml Cargo.lock src/lib.rs src/domain
rtk git commit -m "feat: add routing domain model"
```

---

### Task 2: Deterministik KernelPlan planner

**Files:**

- Modify: `src/lib.rs`
- Create: `src/backend/mod.rs`
- Create: `src/backend/plan.rs`
- Create: `src/backend/planner.rs`
- Test: `tests/planner_contract.rs`

**Interfaces:**

- Consumes: `Profile`, aktif `Rule` nesneleri, `TopologySnapshot` ve exact-domain
  kuralları için `ResolutionSnapshot`.
- Produces:
  `compile_profile(profile: &Profile, topology: &TopologySnapshot,
  resolutions: &ResolutionSnapshot, generation: u64)
  -> Result<KernelPlan, PlanError>`.
- Produces: `KernelPlan::content_hash() -> PlanHash`.

- [ ] **Step 1: Üç lane ve deterministik sıra için başarısız contract testi yaz**

`tests/planner_contract.rs`:

```rust
#[test]
fn compiles_three_lanes_into_reserved_resources_and_stable_rule_order() {
    let profile = profile_with_three_lanes_and_unsorted_rules();
    let topology = topology_for(&[
        ("wg-work", "10.0.0.1"),
        ("tun-privacy", "10.8.0.1"),
        ("eth0", "192.0.2.1"),
    ]);
    let resolutions = ResolutionSnapshot::empty();

    let plan = compile_profile(&profile, &topology, &resolutions, 7).unwrap();

    assert_eq!(plan.generation, 7);
    assert_eq!(
        plan.lanes
            .iter()
            .map(|lane| (&lane.id, lane.table, lane.mark, lane.rule_priority))
            .collect::<Vec<_>>(),
        vec![
            (&LaneId::from("direct"), 20_000, 0x524c0001, 10_000),
            (&LaneId::from("privacy"), 20_001, 0x524c0002, 10_001),
            (&LaneId::from("work"), 20_002, 0x524c0003, 10_002),
        ]
    );
    assert_eq!(
        plan.rules.iter().map(|rule| rule.id.clone()).collect::<Vec<_>>(),
        vec![
            RuleId::from("priority-10-a"),
            RuleId::from("priority-10-b"),
            RuleId::from("priority-20"),
        ]
    );
}

#[test]
fn ignores_disabled_block_rule_without_removing_it_from_profile() {
    let mut profile = profile_with_block_rule();
    profile.rules[0].enabled = false;

    let plan = compile_profile(
        &profile,
        &topology_for(&[]),
        &ResolutionSnapshot::empty(),
        1,
    ).unwrap();

    assert!(plan.rules.is_empty());
    assert!(matches!(profile.rules[0].action, RuleAction::Block));
}

#[test]
fn identical_input_produces_identical_hash() {
    let profile = profile_with_three_lanes_and_unsorted_rules();
    let topology = topology_for(&[
        ("wg-work", "10.0.0.1"),
        ("tun-privacy", "10.8.0.1"),
        ("eth0", "192.0.2.1"),
    ]);
    let resolutions = ResolutionSnapshot::empty();

    let first = compile_profile(&profile, &topology, &resolutions, 42).unwrap();
    let second = compile_profile(&profile, &topology, &resolutions, 42).unwrap();

    assert_eq!(first, second);
    assert_eq!(first.content_hash(), second.content_hash());
}

#[test]
fn content_hash_represents_policy_not_generation_counter() {
    let profile = profile_with_three_lanes_and_unsorted_rules();
    let topology = topology_for(&[
        ("wg-work", "10.0.0.1"),
        ("tun-privacy", "10.8.0.1"),
        ("eth0", "192.0.2.1"),
    ]);
    let resolutions = ResolutionSnapshot::empty();

    let first = compile_profile(&profile, &topology, &resolutions, 41).unwrap();
    let second = compile_profile(&profile, &topology, &resolutions, 42).unwrap();

    assert_ne!(first.generation, second.generation);
    assert_eq!(first.content_hash(), second.content_hash());
}
```

- [ ] **Step 2: Planner testinin eksik API nedeniyle başarısız olduğunu doğrula**

Run:

```bash
rtk cargo test --test planner_contract
```

Expected: `routelane::backend` veya `compile_profile` bulunmadığı için compile
failure.

- [ ] **Step 3: KernelPlan yapılarını ekle**

`src/backend/plan.rs`:

```rust
pub const TABLE_BASE: u32 = 20_000;
pub const TABLE_MAX: u32 = 20_099;
pub const MARK_BASE: u32 = 0x524c0001;
pub const MARK_MAX: u32 = 0x524c0064;
pub const RULE_PRIORITY_BASE: u32 = 10_000;
pub const RULE_PRIORITY_MAX: u32 = 10_099;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelPlan {
    pub generation: u64,
    pub profile_id: ProfileId,
    pub lanes: Vec<LanePlan>,
    pub rules: Vec<CompiledRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanePlan {
    pub id: LaneId,
    pub interface: String,
    pub gateway_v4: Option<Ipv4Addr>,
    pub gateway_v6: Option<Ipv6Addr>,
    pub availability: LaneAvailability,
    pub table: u32,
    pub mark: u32,
    pub rule_priority: u32,
    pub failure_policy: LaneFailurePolicy,
}
```

`LaneAvailability`, `Available` veya `Unavailable` değeridir. Lane slotu
topology'den bağımsız olarak korunur; böylece bir interface kaybolduğunda diğer
lane'lerin table/mark atamaları değişmez.

`CompiledRule` yalnızca kernel'ın eşleyebileceği normalize hedefler taşımalı.
Faz 1'de `Ip`, `Cidr` ve önceden çözülmüş domain IP'leri
`CompiledTarget::Network(IpNet)` biçiminde derlenir.

`ResolutionSnapshot`, `RuleId -> Vec<IpNet>` eşlemesidir. Exact-domain kuralı
için snapshot kaydı yoksa planner `PlanError::DomainResolutionUnavailable`
üretir; kayıt mevcut fakat boşsa geçici olarak adres üretmeyen etkin bir kural
olarak derlenir. `WildcardDomain`, `ServicePack` ve `Application` kaynakları
ileriki fazların capability'si olduğundan etkin olduklarında açık
`PlanError::UnsupportedCapability` döndürür. Disabled kural bu kontrollerden
önce elenir.

- [ ] **Step 4: Planner'ı deterministik sıralama ve kaynak atamasıyla uygula**

`compile_profile`:

1. `validate_profile` çağırır.
2. Lane'leri `LaneId` artan sırada sıralar.
3. Slot `0..lane_count` üzerinden table, mark ve policy priority atar.
4. Arayüz topology kaydı yoksa lane'i `Unavailable` işaretler; table/mark
   slotunu korur fakat o lane için route/policy command üretilmez.
5. Yalnızca enabled kuralları alır.
6. Kuralları `(priority, RuleId)` sırasına dizer.
7. Route referansını aşağıdaki lane-kayıp algoritmasıyla normalize eder.
8. `Ip`/`Cidr` hedeflerini doğrudan, `Domain` hedeflerini
   `ResolutionSnapshot` üzerinden normalize eder.

Lane-kayıp algoritması:

1. Hedef lane `Available` ise kendi mark'ını kullan.
2. Değilse fallback zincirindeki ilk `Available` lane mark'ını kullan.
3. Kullanılabilir fallback yoksa asıl lane'in `failure_policy` değerini uygula:
   `Block` → `CompiledAction::Block`, `Direct` →
   `CompiledAction::Direct`, `Inactive` → bu route kuralını plandan çıkar.

Bu dönüşüm kullanıcı kuralını değiştirmez; health raporu eksik asıl lane'i
`Degraded` göstermeye devam eder.

`content_hash` için generation alanı hariç planın canonical
`serde_json::to_vec` çıktısını Blake3 ile hash'le; hash alanını planın içinde
saklama. Transaction kimliği `(generation, content_hash)` ikilisidir.

- [ ] **Step 5: Planner testlerini geçir**

Run:

```bash
rtk cargo test --test planner_contract
```

Expected: 4 tests PASS.

- [ ] **Step 6: Planner sınır testlerini ekle ve geçir**

Şu iki davranış için ayrı test ekle:

```rust
#[test]
fn rejects_lane_when_reserved_table_capacity_would_overflow() {
    let profile = profile_with_lane_count(101);
    let error = compile_profile(
        &profile,
        &topology_for_generated_lanes(101),
        &ResolutionSnapshot::empty(),
        1,
    ).unwrap_err();
    assert_eq!(error, PlanError::Domain(DomainError::TooManyLanes {
        actual: 101,
        maximum: 100,
    }));
}

#[test]
fn route_rule_uses_the_mark_assigned_to_its_lane() {
    let plan = compile_profile(
        &profile_routing_to("work"),
        &topology_for(&[("wg0", "10.0.0.1")]),
        &ResolutionSnapshot::empty(),
        1,
    ).unwrap();
    assert_eq!(plan.rules[0].action, CompiledAction::Mark(0x524c0001));
}

#[test]
fn exact_domain_uses_only_addresses_from_resolution_snapshot() {
    let profile = profile_with_domain_rule("api.example.com");
    let resolutions = ResolutionSnapshot::from_rule(
        RuleId::from("domain-rule"),
        vec!["203.0.113.8/32".parse().unwrap()],
    );

    let plan = compile_profile(
        &profile,
        &topology_for(&[]),
        &resolutions,
        1,
    ).unwrap();

    assert_eq!(
        plan.rules[0].target,
        CompiledTarget::Network("203.0.113.8/32".parse().unwrap())
    );
}

#[test]
fn unavailable_lane_uses_fallback_then_failure_policy() {
    let profile = profile_with_unavailable_primary_and_fallback();
    let topology = topology_for(&[("wg-fallback", "10.8.0.1")]);

    let with_fallback = compile_profile(
        &profile,
        &topology,
        &ResolutionSnapshot::empty(),
        1,
    ).unwrap();
    assert_eq!(
        with_fallback.rules[0].action,
        CompiledAction::Mark(mark_for_lane(&with_fallback, "fallback"))
    );

    let without_fallback = compile_profile(
        &profile,
        &topology_for(&[]),
        &ResolutionSnapshot::empty(),
        2,
    ).unwrap();
    assert_eq!(without_fallback.rules[0].action, CompiledAction::Block);
}
```

Run:

```bash
rtk cargo test --test planner_contract
```

Expected: 8 tests PASS.

- [ ] **Step 7: Commit**

```bash
rtk git add src/lib.rs src/backend tests/planner_contract.rs
rtk git commit -m "feat: compile profiles into kernel plans"
```

---

### Task 3: nftables batch renderer

**Files:**

- Modify: `src/backend/mod.rs`
- Create: `src/backend/nftables.rs`
- Test: `tests/nftables_contract.rs`
- Test fixture: `tests/fixtures/nftables/three-lane.nft`

**Interfaces:**

- Consumes: Validated `KernelPlan`.
- Produces:
  `render_ruleset(plan: &KernelPlan, mode: TableMode)
  -> Result<String, NftRenderError>`.
- Produces: `TableMode::Create` ve `TableMode::Replace`.

- [ ] **Step 1: Route, Direct ve Block boundary çıktısı için başarısız test yaz**

`tests/nftables_contract.rs`:

```rust
#[test]
fn renders_route_direct_and_enabled_block_in_priority_order() {
    let plan = fixture_kernel_plan_with_all_actions();

    let rendered = render_ruleset(&plan, TableMode::Create).unwrap();

    assert_eq!(
        rendered,
        include_str!("fixtures/nftables/three-lane.nft")
    );
}

#[test]
fn replace_batch_deletes_and_recreates_only_routelane_table() {
    let rendered = render_ruleset(
        &fixture_kernel_plan_with_all_actions(),
        TableMode::Replace,
    ).unwrap();

    assert!(rendered.starts_with(
        "delete table inet routelane\nadd table inet routelane\n"
    ));
    assert!(!rendered.contains("flush ruleset"));
}
```

Fixture, elle doğrulanmış şu iskeleti tam olarak içermeli:

```nft
add table inet routelane
add chain inet routelane output { type route hook output priority mangle; policy accept; }
add rule inet routelane output ip daddr 203.0.113.0/24 meta mark set 0x524c0002 counter
add rule inet routelane output ip daddr 198.51.100.7 meta mark set 0x00000000 return
add rule inet routelane output ip6 daddr 2001:db8:dead::/48 reject counter
```

Fixture'deki sıra test planındaki `(priority, RuleId)` sırasıyla aynı olmalı.

- [ ] **Step 2: Renderer olmadığı için testin başarısız olduğunu doğrula**

Run:

```bash
rtk cargo test --test nftables_contract
```

Expected: `render_ruleset` bulunmadığı için compile failure.

- [ ] **Step 3: nftables renderer'ı uygula**

Renderer:

- Yalnızca sabit `inet routelane` tablo adını üretir.
- IPv4 için `ip daddr`, IPv6 için `ip6 daddr` kullanır.
- `Route` için `meta mark set 0x... counter`.
- `Direct` için önce RouteLane mark'ını sıfırlar, sonra `return`.
- `Block` için `reject counter`.
- Disabled kurallar planner'da elendiği için renderer disabled kavramı bilmez.
- Kullanıcı girdisini nft string'ine doğrudan yerleştirmez; interface ve
  network değerleri typed değerlerden formatlanır.
- `TableMode::Replace`, yalnızca RouteLane tablosunu delete/add eder.

- [ ] **Step 4: Renderer boundary testlerini geçir**

Run:

```bash
rtk cargo test --test nftables_contract
```

Expected: 2 tests PASS.

- [ ] **Step 5: Kullanıcı girdisinin nft token'ına dönüşmediğini test et**

`nft --check` de netlink/kernel erişimi isteyebildiği için normal test setinde
çağrılmaz. Bunun yerine renderer yalnızca typed alanlardan üretim yaptığını saf
testle kanıtlar:

```rust
#[test]
fn rejects_interface_value_that_cannot_be_a_kernel_interface() {
    let mut plan = fixture_kernel_plan_with_all_actions();
    plan.lanes[0].interface = "wg0\nflush ruleset".into();

    assert_eq!(
        render_ruleset(&plan, TableMode::Create),
        Err(NftRenderError::InvalidInterfaceName)
    );
}
```

Run:

```bash
rtk cargo test --test nftables_contract
```

Expected: 3 tests PASS. Gerçek `nft` parser ve kernel davranışı Task 10'daki
root namespace testinde doğrulanır.

- [ ] **Step 6: Commit**

```bash
rtk git add src/backend/nftables.rs src/backend/mod.rs tests/nftables_contract.rs tests/fixtures
rtk git commit -m "feat: render routelane nftables rules"
```

---

### Task 4: Policy route planı ve kaynak çakışması

**Files:**

- Modify: `src/backend/mod.rs`
- Create: `src/backend/policy_route.rs`
- Create: `src/backend/inspect.rs`
- Test: `src/backend/policy_route.rs`
- Test: `src/backend/inspect.rs`

**Interfaces:**

- Consumes: `KernelPlan`, `KernelSnapshot`.
- Produces:
  `build_policy_commands(plan: &KernelPlan) -> Vec<CommandSpec>`.
- Produces:
  `detect_resource_conflicts(plan: &KernelPlan, snapshot: &KernelSnapshot)
  -> Result<(), ResourceConflict>`.

- [ ] **Step 1: Elle türetilmiş command ve conflict testlerini yaz**

```rust
#[test]
fn builds_v4_and_v6_routes_before_policy_rules() {
    let commands = build_policy_commands(&fixture_dual_stack_plan());
    assert_eq!(
        commands,
        vec![
            CommandSpec::ip4(["route", "replace", "default", "via", "10.0.0.1",
                "dev", "wg0", "table", "20000"]),
            CommandSpec::ip6(["route", "replace", "default", "via", "2001:db8::1",
                "dev", "wg0", "table", "20000"]),
            CommandSpec::ip4(["rule", "add", "fwmark", "0x524c0001",
                "lookup", "20000", "priority", "10000"]),
            CommandSpec::ip6(["rule", "add", "fwmark", "0x524c0001",
                "lookup", "20000", "priority", "10000"]),
        ]
    );
}

#[test]
fn refuses_table_owned_by_non_routelane_state() {
    let plan = fixture_single_lane_plan();
    let snapshot = KernelSnapshot::with_foreign_route(20_000, "eth9");

    assert_eq!(
        detect_resource_conflicts(&plan, &snapshot),
        Err(ResourceConflict::RoutingTableInUse(20_000))
    );
}
```

- [ ] **Step 2: Eksik API nedeniyle RED durumunu doğrula**

Run:

```bash
rtk cargo test backend::policy_route::tests --lib
rtk cargo test backend::inspect::tests --lib
```

Expected: module/function missing compile failure.

- [ ] **Step 3: `CommandSpec` ve policy command builder'ı uygula**

`CommandSpec`, programı enum ile sınırlar:

```rust
pub enum Program {
    Ip,
    Nft,
}

pub struct CommandSpec {
    pub program: Program,
    pub family: Option<IpFamily>,
    pub args: Vec<String>,
    pub stdin: Option<Vec<u8>>,
}
```

Arbitrary executable path kabul edilmez. IPv4 ve IPv6 gateway'i olan her lane
için önce route, sonra policy rule üretilir.

- [ ] **Step 4: JSON snapshot parser ve conflict detection'ı uygula**

`KernelSnapshot` şu gerçek çıktılardan parse edilir:

- `ip -j -4 route show table all`
- `ip -j -6 route show table all`
- `ip -j -4 rule show`
- `ip -j -6 rule show`
- `nft -j list table inet routelane`

Yalnızca planın talep ettiği table/priority aralıklarında yabancı state
çakışma sayılır.

- [ ] **Step 5: Policy ve inspect testlerini geçir**

Run:

```bash
rtk cargo test backend::policy_route::tests --lib
rtk cargo test backend::inspect::tests --lib
```

Expected: Tüm yeni testler PASS.

- [ ] **Step 6: Commit**

```bash
rtk git add src/backend/policy_route.rs src/backend/inspect.rs src/backend/mod.rs
rtk git commit -m "feat: plan policy routing resources"
```

---

### Task 5: Transaction ve rollback motoru

**Files:**

- Modify: `src/backend/mod.rs`
- Create: `src/backend/transaction.rs`
- Test: `tests/transaction_contract.rs`

**Interfaces:**

- Consumes: `KernelPlan`, önceki `AppliedKernelState`, `KernelRunner`.
- Produces:
  `apply_plan(runner: &mut impl KernelRunner, current: Option<&AppliedKernelState>,
  next: &KernelPlan) -> Result<AppliedKernelState, ApplyError>`.
- Produces: `KernelRunner` production ve in-memory test uygulamaları.

- [ ] **Step 1: Rollback davranışını gerçek in-memory state ile test et**

`tests/transaction_contract.rs`:

```rust
#[test]
fn keeps_previous_generation_when_new_nft_batch_fails() {
    let old = fixture_applied_state(4);
    let next = fixture_kernel_plan(5);
    let mut runner = InMemoryKernel::from_applied(old.clone())
        .fail_at(FailurePoint::ActivateNft);

    let error = apply_plan(&mut runner, Some(&old), &next).unwrap_err();

    assert_eq!(error.code(), "POLICY_APPLY_ROLLED_BACK");
    assert_eq!(runner.applied_generation(), Some(4));
    assert_eq!(runner.kernel_plan_hash(), Some(old.plan_hash));
    assert!(!runner.has_temporary_guard());
}

#[test]
fn cleans_new_state_when_first_activation_health_check_fails() {
    let next = fixture_kernel_plan(1);
    let mut runner = InMemoryKernel::empty()
        .fail_at(FailurePoint::Verify);

    apply_plan(&mut runner, None, &next).unwrap_err();

    assert_eq!(runner.applied_generation(), None);
    assert!(runner.route_tables().is_empty());
    assert!(!runner.has_routelane_table());
}

#[test]
fn repeated_generation_is_idempotent() {
    let plan = fixture_kernel_plan(9);
    let mut runner = InMemoryKernel::from_plan(&plan);
    let current = runner.applied_state().unwrap();

    let reapplied = apply_plan(&mut runner, Some(&current), &plan).unwrap();

    assert_eq!(reapplied.generation, 9);
    assert_eq!(runner.mutation_count(), 0);
}
```

- [ ] **Step 2: Transaction API eksik olduğu için RED durumunu doğrula**

Run:

```bash
rtk cargo test --test transaction_contract
```

Expected: `apply_plan`/`KernelRunner` missing compile failure.

- [ ] **Step 3: KernelRunner sözleşmesini uygula**

```rust
pub trait KernelRunner {
    fn snapshot(&mut self) -> Result<KernelSnapshot, RunnerError>;
    fn install_guard(&mut self, next: &KernelPlan) -> Result<(), RunnerError>;
    fn prepare_routes(&mut self, next: &KernelPlan) -> Result<(), RunnerError>;
    fn activate_nft(&mut self, next: &KernelPlan) -> Result<(), RunnerError>;
    fn verify(&mut self, next: &KernelPlan) -> Result<(), RunnerError>;
    fn remove_generation(&mut self, generation: u64) -> Result<(), RunnerError>;
    fn remove_guard(&mut self) -> Result<(), RunnerError>;
    fn restore(&mut self, state: Option<&AppliedKernelState>) -> Result<(), RunnerError>;
}
```

Production runner yalnızca `CommandSpec` çalıştırır; in-memory runner ayrı test
utility'sinde gerçek state geçişlerini simüle eder.

- [ ] **Step 4: Transaction state machine'i uygula**

Sıra:

1. Aynı generation ve hash ise mutation yapmadan dön.
2. Eski generation'dan düşük generation ise `StaleGeneration`.
3. Snapshot ve resource conflict kontrolü.
4. Guard.
5. Routes/policy rules.
6. nftables batch.
7. Verify.
8. Eski generation cleanup.
9. Guard cleanup.

Adım 4 sonrasındaki her hata `restore(current)` ve `remove_guard()` çağırır.
Rollback hatası ana hata içinde ayrı `rollback_error` alanıyla korunur.

- [ ] **Step 5: Transaction contract testlerini geçir**

Run:

```bash
rtk cargo test --test transaction_contract
```

Expected: 3 tests PASS.

- [ ] **Step 6: Her failure point için table-driven fault testini ekle**

```rust
#[test]
fn every_mutating_failure_point_restores_previous_generation() {
    for point in [
        FailurePoint::InstallGuard,
        FailurePoint::PrepareRoutes,
        FailurePoint::ActivateNft,
        FailurePoint::Verify,
        FailurePoint::RemoveOldGeneration,
    ] {
        let old = fixture_applied_state(2);
        let next = fixture_kernel_plan(3);
        let mut runner = InMemoryKernel::from_applied(old.clone()).fail_at(point);
        apply_plan(&mut runner, Some(&old), &next).unwrap_err();
        assert_eq!(runner.applied_generation(), Some(2), "failure at {point:?}");
    }
}
```

Run:

```bash
rtk cargo test --test transaction_contract
```

Expected: 4 tests PASS.

- [ ] **Step 7: Commit**

```bash
rtk git add src/backend/transaction.rs src/backend/mod.rs tests/transaction_contract.rs
rtk git commit -m "feat: apply kernel plans with rollback"
```

---

### Task 6: Helper validation ve system D-Bus servisi

**Files:**

- Modify: `Cargo.toml`
- Modify: `src/bin/routelane_helper.rs`
- Modify: `src/routing/executor.rs`
- Create: `src/backend/privileged.rs`
- Create: `data/io.github.routelane.Helper.service`
- Modify: `data/io.github.routelane.policy`
- Test: `src/backend/privileged.rs`

**Interfaces:**

- Consumes: D-Bus `ApplyPlan(plan_json)` ve `Inspect()`.
- Produces: `ApplyResponse { generation, plan_hash, diagnostics }`.
- Produces:
  `validate_privileged_plan(plan: &KernelPlan) -> Result<(), ValidationError>`.

- [ ] **Step 1: Helper sınırlarının başarısız security testlerini yaz**

```rust
#[test]
fn rejects_mark_outside_routelane_range() {
    let mut plan = fixture_valid_plan();
    plan.lanes[0].mark = 0x1;
    assert_eq!(
        validate_privileged_plan(&plan),
        Err(ValidationError::MarkOutOfRange(0x1))
    );
}

#[test]
fn rejects_table_outside_routelane_range() {
    let mut plan = fixture_valid_plan();
    plan.lanes[0].table = 254;
    assert_eq!(
        validate_privileged_plan(&plan),
        Err(ValidationError::TableOutOfRange(254))
    );
}

#[test]
fn rejects_duplicate_marks_even_when_each_mark_is_in_range() {
    let mut plan = fixture_two_lane_plan();
    plan.lanes[1].mark = plan.lanes[0].mark;
    assert_eq!(
        validate_privileged_plan(&plan),
        Err(ValidationError::DuplicateMark(0x524c0001))
    );
}
```

- [ ] **Step 2: Validation eksikliği nedeniyle RED durumunu doğrula**

Run:

```bash
rtk cargo test backend::privileged::tests --lib
```

Expected: module/function missing compile failure.

- [ ] **Step 3: zbus bağımlılığı ve structured validator ekle**

`Cargo.toml`:

```toml
zbus = { version = "4", features = ["tokio"] }
```

Validation:

- Generation sıfır olamaz.
- Table, mark ve priority sınırları sabit aralıkta olmalı.
- Table, mark, priority ve LaneId benzersiz olmalı.
- Arayüz adı boş olamaz, 15 byte'ı aşamaz ve NUL içeremez.
- Plan rule sayısı 10.000'i aşamaz.
- `RuleAction::Route` yalnızca plandaki mark'a referans verebilir.
- Nftables table adı kullanıcı girdisinden gelmez.

- [ ] **Step 4: Helper'ı D-Bus API'sine geçir**

`routelane-helper` Tokio runtime ile system bus'a aşağıdaki sabit adla
kaydolmalı. Argümansız çalışma D-Bus servis modudur:

```text
Bus name: io.github.routelane.Helper
Object:   /io/github/routelane/Helper
Iface:    io.github.routelane.Helper1
```

Metotlar:

```rust
async fn apply_plan(&self, plan_json: &str) -> zbus::fdo::Result<String>;
async fn inspect(&self) -> zbus::fdo::Result<String>;
async fn cleanup(&self) -> zbus::fdo::Result<String>;
```

Her metot sender unique name ve UID bilgisini D-Bus'tan alır, Polkit
`io.github.routelane.apply` action'ını kontrol eder. `plan_json` önce
`KernelPlan` olarak deserialize edilir, sonra structured validator'dan geçer.

- [ ] **Step 5: D-Bus activation ve Polkit dosyalarını ekle**

`data/io.github.routelane.Helper.service`:

```ini
[D-BUS Service]
Name=io.github.routelane.Helper
Exec=/usr/lib/routelane/routelane-helper
User=root
SystemdService=routelane-helper.service
```

Polkit action varsayılanı:

```xml
<defaults>
  <allow_any>no</allow_any>
  <allow_inactive>no</allow_inactive>
  <allow_active>auth_admin_keep</allow_active>
</defaults>
```

- [ ] **Step 6: Helper validation testlerini ve binary build'ini geçir**

Run:

```bash
rtk cargo test backend::privileged::tests --lib
rtk cargo build --bin routelane-helper
```

Expected: Validation testleri PASS; helper binary build exit 0.

- [ ] **Step 7: Geçiş sürümünde legacy çağrıyı açık moda sınırla**

Task 9 tamamlanana kadar mevcut GTK engine'inin ara commit'lerde çalışması için
helper yalnızca açık `--legacy-stdin` argümanıyla eski JSON sözleşmesini kabul
eder. `src/routing/executor.rs`, `pkexec ... routelane-helper --legacy-stdin`
çağrısına geçirilir; argümansız helper yalnızca D-Bus servisidir. Bu mod
paketlenmiş servis veya daemon tarafından hiçbir zaman kullanılmaz.

Task 9'da GTK daemon'a taşındıktan sonra `--legacy-stdin`, eski `HelperOp`
tipleri ve pkexec executor aynı commit'te tamamen kaldırılır. Böylece her ara
commit derlenebilir kalırken Faz 1 çıktısında eski ayrıcalıklı yüzey bulunmaz.

- [ ] **Step 8: Tüm testleri geçir**

Run:

```bash
rtk cargo test
rtk cargo build --bins
```

Expected: Testler ve tüm binary build'leri exit 0.

- [ ] **Step 9: Commit**

```bash
rtk git add Cargo.toml Cargo.lock src/bin/routelane_helper.rs src/routing/executor.rs src/backend/privileged.rs data
rtk git commit -m "feat: expose validated helper over system dbus"
```

---

### Task 7: Daemon state ve Unix socket IPC

**Files:**

- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Create: `src/ipc/mod.rs`
- Create: `src/ipc/protocol.rs`
- Create: `src/ipc/framing.rs`
- Create: `src/ipc/client.rs`
- Create: `src/ipc/server.rs`
- Create: `src/daemon/mod.rs`
- Create: `src/daemon/state.rs`
- Create: `src/daemon/service.rs`
- Create: `src/bin/routelane_daemon.rs`
- Test: `tests/ipc_contract.rs`

**Interfaces:**

- Produces: `DaemonRequest`, `DaemonResponse`, `DaemonEvent`.
- Produces: `DaemonState::apply_profile(profile)`.
- Produces: Unix socket server/client.
- Consumes: `compile_profile`, `TopologyProvider`, `ResolutionProvider`,
  privileged helper client ve config repository.

- [ ] **Step 1: Protokol sürümü ve aynı kullanıcı socket testi yaz**

`tests/ipc_contract.rs`:

```rust
#[tokio::test]
async fn rejects_unsupported_protocol_version_without_mutating_state() {
    let harness = DaemonHarness::start().await;
    let response = harness.send(DaemonEnvelope {
        protocol_version: 999,
        request_id: "request-1".into(),
        request: DaemonRequest::GetStatus,
    }).await;

    assert_eq!(
        response.error.unwrap().code,
        "IPC_UNSUPPORTED_PROTOCOL_VERSION"
    );
    assert_eq!(harness.state().generation, 0);
}

#[tokio::test]
async fn apply_profile_returns_generation_and_publishes_event() {
    let harness = DaemonHarness::start().await;
    let mut events = harness.subscribe().await;

    let response = harness.apply(fixture_profile()).await.unwrap();
    let event = events.recv().await.unwrap();

    assert_eq!(response.generation, 1);
    assert_eq!(event, DaemonEvent::StateChanged {
        generation: 1,
        health: HealthState::Healthy,
    });
}
```

- [ ] **Step 2: IPC API eksik olduğu için RED durumunu doğrula**

Run:

```bash
rtk cargo test --test ipc_contract
```

Expected: IPC/daemon module missing compile failure.

- [ ] **Step 3: Sürümlü protocol ve framing'i uygula**

```rust
pub const IPC_PROTOCOL_VERSION: u32 = 1;

pub struct DaemonEnvelope {
    pub protocol_version: u32,
    pub request_id: String,
    pub request: DaemonRequest,
}

pub enum DaemonRequest {
    GetStatus,
    ListProfiles,
    ApplyProfile(Profile),
    SetRuleEnabled { profile_id: ProfileId, rule_id: RuleId, enabled: bool },
    Deactivate,
    Subscribe,
}
```

Framing: 4-byte big-endian payload length + UTF-8 JSON. Maksimum frame
`1_048_576` byte; daha büyük frame `IPC_FRAME_TOO_LARGE` üretir.

- [ ] **Step 4: DaemonState desired/applied generation modelini uygula**

`DaemonState`:

```rust
pub struct DaemonState {
    pub desired: Option<Profile>,
    pub applied: Option<AppliedKernelState>,
    pub generation: u64,
    pub health: HealthReport,
}
```

`DaemonService` test edilebilir bağımlılıklarla kurulur:

```rust
pub struct DaemonService<T, R, H, C>
where
    T: TopologyProvider,
    R: ResolutionProvider,
    H: PrivilegedHelperClient,
    C: ConfigRepository,
{
    state: DaemonState,
    topology: T,
    resolutions: R,
    helper: H,
    config: C,
}
```

Mutation sırasında:

1. Profile validate.
2. Desired state'i config repository'ye atomik kaydet.
3. Generation artır.
4. Topology ve exact-domain resolution snapshot'larını al.
5. Planner.
6. Helper apply.
7. Başarılıysa applied state ve event.
8. Hatalıysa desired korunur, applied eski kalır, Failed/Degraded event.

- [ ] **Step 5: Unix socket server ve client'ı uygula**

Socket yolu:

```rust
pub fn socket_path() -> Result<PathBuf, IpcError> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or(IpcError::MissingRuntimeDirectory)?;
    Ok(PathBuf::from(runtime).join("routelane/routelane.sock"))
}
```

Parent mode `0700`, socket mode `0600`. Server peer UID'yi `SO_PEERCRED` ile
kontrol eder; daemon UID'sinden farklı peer reddedilir.

- [ ] **Step 6: IPC contract testlerini geçir**

Run:

```bash
rtk cargo test --test ipc_contract
```

Expected: 2 tests PASS.

- [ ] **Step 7: Daemon binary build ve manuel smoke**

Run:

```bash
rtk cargo build --bin routelane-daemon
```

Expected: exit 0.

- [ ] **Step 8: Commit**

```bash
rtk git add Cargo.toml Cargo.lock src/lib.rs src/ipc src/daemon src/bin/routelane_daemon.rs tests/ipc_contract.rs
rtk git commit -m "feat: add local routing daemon ipc"
```

---

### Task 8: Bağlantı Sağlığı inspector ve rapor

**Files:**

- Modify: `src/lib.rs`
- Create: `src/health/mod.rs`
- Create: `src/health/probes.rs`
- Create: `src/health/report.rs`
- Test: `tests/health_contract.rs`
- Test fixture: `tests/fixtures/health/healthy/`
- Test fixture: `tests/fixtures/health/missing-lane/`

**Interfaces:**

- Consumes: `KernelPlan`, `KernelSnapshot`, interface/gateway snapshot.
- Produces:
  `evaluate_health(plan: Option<&KernelPlan>, snapshot: &KernelSnapshot)
  -> HealthReport`.
- Produces: `Diagnostic` ve sabit `DiagnosticCode`.

- [ ] **Step 1: Healthy ve missing interface contract testlerini yaz**

```rust
#[test]
fn reports_healthy_when_table_rules_routes_and_interfaces_match() {
    let plan = fixture_plan("tests/fixtures/health/healthy/plan.json");
    let snapshot = fixture_snapshot("tests/fixtures/health/healthy/");

    let report = evaluate_health(Some(&plan), &snapshot);

    assert_eq!(report.state, HealthState::Healthy);
    assert!(report.diagnostics.is_empty());
}

#[test]
fn reports_missing_lane_without_collapsing_other_lane_health() {
    let plan = fixture_plan("tests/fixtures/health/missing-lane/plan.json");
    let snapshot = fixture_snapshot("tests/fixtures/health/missing-lane/");

    let report = evaluate_health(Some(&plan), &snapshot);

    assert_eq!(report.state, HealthState::Degraded);
    assert_eq!(
        report.diagnostics[0].code,
        DiagnosticCode::LaneInterfaceMissing
    );
    assert_eq!(report.lanes[&LaneId::from("work")], HealthState::Degraded);
    assert_eq!(report.lanes[&LaneId::from("direct")], HealthState::Healthy);
}
```

- [ ] **Step 2: Health API eksik olduğu için RED durumunu doğrula**

Run:

```bash
rtk cargo test --test health_contract
```

Expected: health modules missing compile failure.

- [ ] **Step 3: Diagnostic ve aggregate state modelini uygula**

Aggregate sırası:

```text
Failed > NeedsAuthorization > Degraded > Applying > Healthy > Inactive
```

İlk faz kodları:

```rust
pub enum DiagnosticCode {
    LaneInterfaceMissing,
    LaneGatewayUnreachable,
    NftablesFeatureMissing,
    PolicyApplyRolledBack,
    RoutingResourceConflict,
}
```

Her diagnostic `summary`, `technical_detail`, `severity`, `subject` ve
`suggested_action` taşır.

- [ ] **Step 4: Kernel snapshot karşılaştırmasını uygula**

Kontroller:

- Plan lane interface'i mevcut mu?
- IPv4/IPv6 gateway beklenen ailede mi?
- RouteLane nft table generation/hash eşleşiyor mu?
- Her lane table için route var mı?
- Her lane mark için doğru priority ve lookup table var mı?
- Yabancı resource conflict var mı?

- [ ] **Step 5: Health contract testlerini geçir**

Run:

```bash
rtk cargo test --test health_contract
```

Expected: 2 tests PASS.

- [ ] **Step 6: Daemon GetStatus ve apply event'ine HealthReport bağla**

`DaemonResponse::Status` tam `HealthReport` döndürmeli. Helper apply başarısızsa
`PolicyApplyRolledBack`; Polkit reddederse `NeedsAuthorization` üretilmeli.

Run:

```bash
rtk cargo test --test ipc_contract --test health_contract
```

Expected: IPC ve health testleri PASS.

- [ ] **Step 7: Commit**

```bash
rtk git add src/lib.rs src/health src/daemon tests/health_contract.rs tests/fixtures/health
rtk git commit -m "feat: report routing connection health"
```

---

### Task 9: GTK'yi daemon ve Multi-Lane modele geçir

**Files:**

- Modify: `src/main.rs`
- Modify: `src/ui/window.rs`
- Modify: `src/ui/settings.rs`
- Modify: `src/ui/rule_row.rs`
- Modify: `src/ui/i18n.rs`
- Modify: `src/config.rs`
- Modify: `src/models.rs`
- Delete after migration: `src/routing/manager.rs`
- Delete after migration: `src/routing/executor.rs`
- Test: `src/ui/window.rs`
- Test: `src/config.rs`

**Interfaces:**

- Consumes: `ipc::Client`, `Profile`, `HealthReport`.
- Produces: Multi-Lane editor, rule action/enable controls ve Bağlantı Sağlığı
  görünümü.

- [ ] **Step 1: Eski config'ten Default profile dönüşüm testini yaz**

```rust
#[test]
fn migrates_legacy_rules_into_default_lane_profile() {
    let legacy = r#"{
      "language":"tr",
      "alt_interface":"tun0",
      "rules":[
        {"target_str":"203.0.113.0/24","is_domain":false,"interface":"tun0"}
      ]
    }"#;

    let migrated = ConfigDocument::from_json(legacy).unwrap().into_current().unwrap();

    assert_eq!(migrated.schema_version, 2);
    assert_eq!(migrated.profiles[0].name, "Default");
    assert_eq!(migrated.profiles[0].lanes[0].interface.name(), "tun0");
    assert_eq!(
        migrated.profiles[0].rules[0].action,
        RuleAction::Route(migrated.profiles[0].lanes[0].id.clone())
    );
    assert!(!migrated.profiles[0].enabled);
}
```

- [ ] **Step 2: Migration testi yeni şema olmadığı için RED durumunu doğrula**

Run:

```bash
rtk cargo test config::tests::migrates_legacy_rules_into_default_lane_profile
```

Expected: `ConfigDocument` missing compile failure.

- [ ] **Step 3: Sürümlü config repository'yi uygula**

Config v2:

```rust
pub struct ConfigV2 {
    pub schema_version: u32,
    pub language: Language,
    pub profiles: Vec<Profile>,
}
```

Save: temp file, `sync_all`, backup, atomic rename. Load: v2 parse; değilse legacy
parse ve migration. Routing state her zaman inactive başlar.

- [ ] **Step 4: GTK main'i IPC client'a geçir**

`main.rs` içindeki embedded Tokio routing engine channel'larını kaldır. Tokio
thread'i yalnızca daemon IPC client'ını çalıştırır. Daemon yoksa systemd user
service activation denenir; başarısızsa UI `DaemonUnavailable` sağlık durumu
gösterir.

- [ ] **Step 5: Lane ve kural editor'ünü ekle**

Settings:

- Lane listesi: ad, interface, failure policy, sil.
- Lane ekleme: mevcut interface listesinden.

Rule row:

- Target.
- Action: Route/Direct/Block.
- Route seçilirse lane combo.
- Enabled switch.
- Priority görünür ve düzenlenebilir.

Block switch kapatıldığında `SetRuleEnabled` IPC mutation'ı gönderilir; kural
satırı silinmez.

- [ ] **Step 6: Bağlantı Sağlığı görünümünü ekle**

Ana sayfada:

- Genel state ve son apply zamanı.
- Lane bazında Healthy/Degraded/Failed.
- Diagnostic satırlarında kısa mesaj ve önerilen eylem.
- Teknik ayrıntı açılır bölümde.

Eski “Rota Testi” UI veya mesaj tiplerini geri getirme. Onların yerine
`HealthReport` event'lerini kullan.

- [ ] **Step 7: UI/config testlerini geçir**

Run:

```bash
rtk cargo test config::tests
rtk cargo test ui::window::tests
rtk cargo build --bin routelane
```

Expected: Migration ve UI saf yardımcı testleri PASS; GUI build exit 0.

- [ ] **Step 8: Eski engine ve legacy helper yüzeyini kaldır**

`main.rs` artık `mod routing;` kullanmadığında eski manager/executor/mod
engine kodunu kaldır. `src/routing/resolver.rs`,
`src/dns/periodic.rs` hedefine taşınır ve `src/dns/mod.rs` üzerinden export
edilir; davranışı bu fazda değiştirilmez. Deneysel ipset/iptables
`dns_router.rs`, eski `HelperOp` tipleri ve helper'ın `--legacy-stdin` modu
silinir; Faz 2 bunların yerine nftables dynamic-set connector'ını ekleyecektir.

Run:

```bash
rtk cargo test
rtk cargo build --bins
```

Expected: Eski engine olmadan tüm testler ve binary'ler PASS.

- [ ] **Step 9: Commit**

```bash
rtk git add src/main.rs src/ui src/config.rs src/models.rs src/routing src/dns src/bin/routelane_helper.rs
rtk git commit -m "feat: manage multi-lane routing through daemon"
```

---

### Task 10: Root network namespace entegrasyon testleri

**Files:**

- Create: `tests/root/mod.rs`
- Create: `tests/root/namespace_routing.rs`
- Modify: `Cargo.toml`
- Create: `scripts/run-root-integration.sh`

**Interfaces:**

- Consumes: Gerçek `routelane-helper`, nftables ve iproute2.
- Produces: İzole namespace içinde Route/Direct/Block ve rollback kanıtı.

- [ ] **Step 1: Root test feature ve skip sözleşmesini ekle**

`Cargo.toml`:

```toml
[features]
root-integration = []

[[test]]
name = "root"
path = "tests/root/mod.rs"
required-features = ["root-integration"]
```

Test başında root ve binary capability preflight yap. Eksik capability,
başarılı test gibi sessizce geçilmemeli; script `77` skip exit code'u
üretmeli.

- [ ] **Step 2: Üç lane namespace testini önce başarısız çalıştır**

Test topolojisi:

```text
client namespace
├── lane-a → gateway-a → sink-a
├── lane-b → gateway-b → sink-b
└── lane-c → gateway-c → sink-c
```

Kurallar:

- `203.0.113.10/32` → lane-a
- `198.51.100.10/32` → lane-b
- `192.0.2.10/32` → lane-c
- `192.0.2.99/32` → Block enabled

Run:

```bash
rtk ./scripts/run-root-integration.sh
```

Expected before harness implementation: capability'ler varsa namespaces/topology
oluşturulmadığı için FAIL; capability yoksa exit 77 ve açık skip sebebi.

- [ ] **Step 3: Namespace harness'ı uygula**

Harness yalnızca `routelane-test-*` isimli namespace ve veth nesneleri
oluşturabilir. `Drop` cleanup ve shell trap ikisi de aynı açık isim listesini
temizler; wildcard veya geniş flush kullanmaz.

- [ ] **Step 4: Gerçek route ve block assertions ekle**

Her sink yalnızca kendisine yönlendirilen probe'u görmeli. Block hedefinde
hiçbir sink paket görmemeli. nft counters ilgili kural için artmalı.

- [ ] **Step 5: Gerçek rollback fault testi ekle**

Helper yalnızca `root-integration` Cargo feature'ı ile derlenen test binary'sinde
ve yalnızca `ROUTELANE_TEST_FAIL_POINT=activate_nft` olduğunda nft
activation'dan önce kontrollü hata üretir. Production build bu branch'i
derlemez. Önce generation 1'i uygula; generation 2 hata verdiğinde generation 1
route ve nft counters çalışmaya devam etmeli.

- [ ] **Step 6: Root entegrasyonunu çalıştır**

Run:

```bash
rtk cargo build --bins
rtk ./scripts/run-root-integration.sh
```

Expected: Uygun root ortamında tüm root testleri PASS; capability eksik
ortamda script exit 77 ve açık sebep.

- [ ] **Step 7: Commit**

```bash
rtk git add Cargo.toml Cargo.lock tests/root scripts/run-root-integration.sh
rtk git commit -m "test: verify multi-lane routing in namespaces"
```

---

### Task 11: systemd/DBus paketleme ve Faz 1 doğrulama

**Files:**

- Create: `data/routelane-daemon.service`
- Create: `data/routelane-helper.service`
- Modify: `packaging/deb/build-deb.sh`
- Modify: `README.md`
- Modify: `Cargo.toml`
- Modify: `LICENSE` or `README.md` license declaration so both state GPLv3

**Interfaces:**

- Consumes: Dört binary ve service/policy dosyaları.
- Produces: Kurulabilir `.deb`, daemon/helper activation ve Faz 1 kullanım
  belgeleri.

- [ ] **Step 1: Paket içerik testini build script'e ekle**

Paket build tamamlandıktan sonra `dpkg-deb --contents` sonucu şu dosyaları
içermiyorsa script başarısız olmalı:

```text
/usr/bin/routelane
/usr/bin/routelane-daemon
/usr/bin/routelanectl
/usr/lib/routelane/routelane-helper
/usr/lib/systemd/user/routelane-daemon.service
/usr/lib/systemd/system/routelane-helper.service
/usr/share/dbus-1/system-services/io.github.routelane.Helper.service
/usr/share/polkit-1/actions/io.github.routelane.policy
```

Faz 1'de `routelanectl` yalnızca `status`, `apply-profile` ve `deactivate`
komutlarını içerir; geniş CLI Faz 3'te tamamlanır.

- [ ] **Step 2: Eksik binary/service nedeniyle paket testinin başarısız olduğunu doğrula**

Run:

```bash
rtk ./packaging/deb/build-deb.sh
```

Expected: `routelanectl` veya service unit eksikliği nedeniyle FAIL.

- [ ] **Step 3: Minimal routelanectl ve systemd unit'lerini ekle**

`routelanectl` daemon IPC client'ını kullanır:

```text
routelanectl status [--json]
routelanectl apply-profile <config.json>
routelanectl deactivate
```

User daemon unit:

```ini
[Unit]
Description=RouteLane user routing daemon
After=default.target

[Service]
ExecStart=/usr/bin/routelane-daemon
Restart=on-failure
RuntimeDirectory=routelane
RuntimeDirectoryMode=0700

[Install]
WantedBy=default.target
```

System helper unit D-Bus activation ile başlar ve doğrudan enable edilmez.

- [ ] **Step 4: Paket script'ini dört binary ve service dosyaları için güncelle**

Debian dependencies'e şu çalışma zamanı paketlerini ekle:

```text
nftables, iproute2, policykit-1, dbus, systemd
```

dnsmasq Faz 1'de zorunlu dependency değildir.

- [ ] **Step 5: Lisans bildirimini GPLv3 olarak tutarlı hale getir**

`LICENSE` GPLv3 kalır. `README.md` ve `Cargo.toml`:

```toml
license = "GPL-3.0-only"
```

README lisans bölümü `GNU GPLv3` demelidir.

- [ ] **Step 6: Tüm doğrulama komutlarını taze çalıştır**

Run:

```bash
rtk cargo fmt --check
rtk cargo test
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk cargo build --release --bins
rtk ./packaging/deb/build-deb.sh
rtk git diff --check
```

Expected:

- Format check exit 0
- Tüm normal testler PASS
- Clippy warning/error yok
- Dört release binary build edilir
- `.deb` gerekli dosyaları içerir
- Whitespace error yok

- [ ] **Step 7: Faz 1 kabul kriterlerini elle eşleştir**

Kanıt listesi:

```text
[ ] En az üç lane planner ve namespace testinde çalışıyor
[ ] Route ve Direct eylemleri contract testinde çalışıyor
[ ] Block kuralı enabled=false iken plandan çıkıyor
[ ] IPv4 ve IPv6 route testleri var
[ ] Fault injection önceki generation'ı koruyor
[ ] Health kernel state'i Healthy/Degraded ayırıyor
[ ] Helper ayrılmış alan dışını reddediyor
[ ] GUI eski rota testi yerine Bağlantı Sağlığı gösteriyor
```

Eksik madde varsa Faz 1 tamamlandı olarak raporlama.

- [ ] **Step 8: Commit**

```bash
rtk git add Cargo.toml Cargo.lock README.md LICENSE data packaging src/bin/routelanectl.rs
rtk git commit -m "build: package routelane daemon and helper"
```

---

## Faz 1 sonu

Faz 1 tamamlandığında aşağıdaki komutların taze çıktıları rapora eklenir:

```bash
rtk cargo fmt --check
rtk cargo test
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk cargo build --release --bins
rtk ./packaging/deb/build-deb.sh
rtk git diff --check
```

Root entegrasyon ortamı varsa ayrıca:

```bash
rtk ./scripts/run-root-integration.sh
```

Root ortamı yoksa bu açıkça belirtilir; normal test başarısı root entegrasyon
başarısı gibi raporlanmaz.
