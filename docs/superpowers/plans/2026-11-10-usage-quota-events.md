# Veri kullanımı, kota ve olay geçmişi (Plan 1/3) — uygulama planı

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Kotalı hatta (ör. VINN) ne kadar veri harcandığını, kota durumunu ve RouteLane'de olanları (yedeğe geçiş, uygulama, hata, kota eşikleri) terminal açmadan gösteren, yalnız daemon tarafında çalışan ilk dilim.

**Architecture:** Yeni `routelane::usage` modülü (takvim, kota ayarı, kart sayaçları, kullanım deposu, olay deposu, örnekleyici) kullanıcı daemon'unda çalışır; root/helper gerekmez. Daemon IPC'si protocol 4'e çıkar ve kullanım/kota/olay isteklerini sunar; kota eşiklerinde oturum D-Bus'ı üzerinden masaüstü bildirimi gönderir veya açık arayüze olay yayınlar. GTK tarafı Bağlantılar listesi, hat ayrıntısı, kota penceresi, Genel Bakış kartları ve olay geçmişi sayfasını ekler; grafikler `gtk::DrawingArea` + cairo.

**Tech Stack:** Rust 2021, tokio, serde/serde_json, zbus 5 (oturum bus'ı), libc (`localtime_r`/`mktime`), gtk4 0.5 / libadwaita 0.2 / glib 0.16 (sabit).

**Spec:** `docs/superpowers/specs/2026-11-09-usage-and-events-design.md`

**Kapsam bölmesi (spec §2'deki dört parça bağımsız alt sistemdir):**
- **Plan 1 (bu belge):** A — kart toplamları, kota, bildirim, C1 — olay geçmişi ve bunların ekranları. Helper, nft, runtime protokolü değişmez.
- **Plan 2:** B — RouteLane'in yönlendirdiği adlı sayaçlar, grant sahibine açık helper sayaç metodu, yükseltme şartı (§5.2).
- **Plan 3:** Canlı bağlantılar, NFLOG, systemd-resolved ad eşlemesi, kullanıcı filtreleme (§5.3).
Bu planın ekranlarında B ve canlı liste alanları **gösterilmez** (yer tutucu yok); Plan 2/3 ekler.

## Global Constraints

- Yeni bağımlılık yok; `gtk4 0.5` / `libadwaita 0.2` / `glib 0.16` yükseltilmez.
- “Genel bir sysctl'i, yabancı nft tablolarını veya conntrack durumunu değiştirmek” yok; bu plan kernel'e yazmaz, yalnız `/sys/class/net/<arayüz>/statistics/{rx_bytes,tx_bytes}` ve `/sys/class/net/<arayüz>/ifindex` okur.
- Config şeması (v4) değişmez. Kota ayarları ayrı dosyada: `~/.config/routelane/usage-settings.json` (0600, sürümlü). Sayaçlar `~/.local/state/routelane/usage-v1.json`, olaylar `~/.local/state/routelane/events-v1.jsonl` (0600). `XDG_CONFIG_HOME`/`XDG_STATE_HOME` öncelikli.
- GB = 10⁹ bayt, bir ondalık; değerler “yaklaşık” etiketli; RouteLane trafiği kesmez.
- Dönem: `reset_day` günü 00:00 yerel saat; ayda o gün yoksa ayın son günü. Varsayılan uyarı eşiği %80.
- Kotayı kartın toplam trafiği belirler (↓+↑) + elle düzeltme farkı. Kota girilmezse uyarı yok.
- Eşikler (uyarı, doldu) dönem başına bir kez tetiklenir ve olay geçmişine yazılır.
- Örnekleme: hat ayrıntısı açıkken 1 sn, aksi halde 30 sn. Sayaç azalırsa veya ifindex değişirse yeni değer sıfırdan başlamış sayılır. Sayım lane id ile tutulur.
- Olaylar: 30 gün ve en fazla 5.000 kayıt; “Temizle” hepsini siler; olay kayıtlarında bağlantı adresi tutulmaz.
- Bildirim: UI kapalıyken masaüstü bildirimi; UI açıkken yalnız toast (çift bildirim yok).
- Daemon IPC protokolü 4 (seçici yönlendirmenin 3'ünün üzerine); runtime/helper protokolü 3 kalır. Eski peer mutasyondan önce anlaşılır hata alır.
- Tüm metinler TR/EN `src/ui/i18n.rs` içinde; spec §4 metinleri birebir.
- Kernel/root testi yok; GUI değişikliği izole GTK smoke (`scripts/run-gtk-smoke.sh`) ile doğrulanır. Host'a kurulum yapılmaz.

## Review Focus

1. **Saat/yaz saati ve ay sonu:** 31 sıfırlama günü şubatta/30 çeken ayda ayın son gününe kayar, ertesi ay yine 31'e döner; yerel gece yarısı DST gününde de tek dönem sınırı üretir — Task 1.
2. **Yeniden başlatma ve kart yeniden oluşma:** daemon yeniden başlayınca aynı boot'ta kalıcı son örnekten devam eder (çift sayım yok); reboot/ifindex değişimi/sayaç azalması negatif delta üretmez — Task 2.
3. **Elle düzeltme sonrası eşik:** kullanıcı kullanılanı operatör değerine yükseltince eşik hemen ve dönemde bir kez tetiklenir; düşürünce tekrar tetiklenmez — Task 2.
4. **Bozuk/eksik dosyalar:** bozuk `usage-v1.json` veya `events-v1.jsonl` daemon'u çökertmez; bozuk dosya korunur (`.corrupt-<zaman>`), yeni depo boş başlar, bir uyarı olayı yazılır — Task 2/3.
5. **Hat silinip arayüz adı değişince:** sayım lane id'ye bağlı kalır; profilde olmayan lane'in verisi raporda görünmez ama silinmez (geri gelirse sürer) — Task 2/4.

---

## Dosya ve sorumluluk haritası

| Dosya | Sorumluluk |
| --- | --- |
| `src/usage/mod.rs` | Modül kökü, ortak tipler (`UsageTotals`, `LocalDate`), yol çözümü |
| `src/usage/calendar.rs` | Yerel tarih, dönem başlangıcı/sonraki sıfırlama (libc) |
| `src/usage/quota.rs` | `QuotaSettings` dosyası, doğrulama |
| `src/usage/counters.rs` | sysfs örnekleme, delta kuralı |
| `src/usage/store.rs` | `UsageStore`: günlük toplamlar, düzeltme, eşik durumu, kalıcılık |
| `src/usage/events.rs` | `EventStore`: olay türleri, saklama, filtre |
| `src/usage/service.rs` | `UsageService`: örnekleyici döngü, rapor, bildirim kararı |
| `src/usage/notify.rs` | Oturum bus'ı `org.freedesktop.Notifications` istemcisi |
| `src/ipc/protocol.rs`, `src/daemon/service.rs`, `src/bin/routelane_daemon.rs` | Protocol 4, yeni istek/sonuç/olay, olay kaydı, kablolama |
| `src/ui/usage_widgets.rs` | Kota çubuğu ve günlük/hız grafiği (DrawingArea) |
| `src/ui/lane_detail.rs`, `src/ui/quota_dialog.rs`, `src/ui/events_page.rs` | Yeni ekranlar |
| `src/ui/{connections,overview,window,session,controller,i18n}.rs` | Mevcut ekranlara bağlama |
| `tests/usage_*_contract.rs` | Rootless sözleşmeler |

---

### Task 1: Yerel takvim ve kota ayarı

**Files:**
- Create: `src/usage/mod.rs`, `src/usage/calendar.rs`, `src/usage/quota.rs`, `tests/usage_calendar_contract.rs`
- Modify: `src/lib.rs` (`pub mod usage;`)

**Interfaces:**
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)] pub struct LocalDate { pub year: i32, pub month: u8, pub day: u8 }` (serde: `"YYYY-MM-DD"` dizesi), `LocalDate::parse(&str) -> Option<Self>`, `Display`.
  - `pub fn days_in_month(year: i32, month: u8) -> u8`
  - `pub fn period_start(today: LocalDate, reset_day: u8) -> LocalDate` ve `pub fn next_reset(today: LocalDate, reset_day: u8) -> LocalDate`
  - `pub fn local_date(unix_seconds: i64) -> LocalDate` (libc `localtime_r`), `pub fn local_midnight(date: LocalDate) -> i64` (libc `mktime`, `tm_isdst = -1`).
  - `#[serde(deny_unknown_fields)] pub struct LaneQuota { pub enabled: bool, pub limit_bytes: u64, pub reset_day: u8, pub warn_percent: u8 }`, `LaneQuota::validate(&self) -> Result<(), QuotaError>` (`limit_bytes ≥ 10⁸` yani 0,1 GB, `reset_day ∈ 1..=31`, `warn_percent ∈ 1..=99`).
  - `#[serde(deny_unknown_fields)] pub struct QuotaSettings { pub version: u32, pub lanes: BTreeMap<LaneId, LaneQuota> }`, `QUOTA_SETTINGS_VERSION = 1`, `QuotaSettings::load(path) -> anyhow::Result<Self>` (yoksa boş), `save(&self, path)` (atomik, 0600, `config.rs`'deki `write_private_file`/`ensure_private_directory` desenini paylaşarak — bu iki fonksiyon `pub(crate)` yapılır).
  - `pub fn usage_settings_path() -> anyhow::Result<PathBuf>`, `pub fn usage_state_dir() -> anyhow::Result<PathBuf>` (mod.rs).

- [ ] **Step 1: Write the failing tests** (`src/usage/calendar.rs` birim testleri)

```rust
#[test]
fn period_boundaries_follow_the_reset_day_and_clamp_to_month_end() {
    let d = |y, m, d| LocalDate { year: y, month: m, day: d };
    assert_eq!(period_start(d(2026, 11, 9), 1), d(2026, 11, 1));
    assert_eq!(next_reset(d(2026, 11, 9), 1), d(2026, 12, 1));
    assert_eq!(period_start(d(2026, 11, 9), 15), d(2026, 10, 15));
    assert_eq!(period_start(d(2027, 2, 28), 31), d(2027, 2, 28)); // şubatta son gün
    assert_eq!(next_reset(d(2027, 2, 28), 31), d(2027, 3, 31));
    assert_eq!(period_start(d(2027, 3, 30), 31), d(2027, 2, 28));
    assert_eq!(period_start(d(2028, 2, 29), 30), d(2028, 2, 29)); // artık yıl
    assert_eq!(period_start(d(2027, 1, 5), 10), d(2026, 12, 10)); // yıl sınırı
}

#[test]
fn quota_validation_rejects_out_of_range_values() {
    let ok = LaneQuota { enabled: true, limit_bytes: 20_000_000_000, reset_day: 1, warn_percent: 80 };
    assert!(ok.validate().is_ok());
    for bad in [
        LaneQuota { reset_day: 0, ..ok }, LaneQuota { reset_day: 32, ..ok },
        LaneQuota { warn_percent: 0, ..ok }, LaneQuota { warn_percent: 100, ..ok },
        LaneQuota { limit_bytes: 0, ..ok },
    ] { assert!(bad.validate().is_err(), "{bad:?}"); }
}
```

`tests/usage_calendar_contract.rs` (tek test, süreç başında `TZ=Europe/Istanbul` ve `libc::tzset()`; ayrı bir test ikilisi olduğu için ortam yarışmaz):

```rust
#[test]
fn local_midnight_round_trips_and_settings_file_is_private() {
    // 2026-11-10 00:00 İstanbul (UTC+3) = 1794258000
    let date = LocalDate { year: 2026, month: 11, day: 10 };
    assert_eq!(local_midnight(date), 1_794_258_000);
    assert_eq!(local_date(1_794_258_000), date);
    assert_eq!(local_date(1_794_257_999), LocalDate { year: 2026, month: 11, day: 9 });
    // kayıt/okuma, 0600, sürüm ve bilinmeyen alan reddi
    // (tempdir; QuotaSettings::save sonra load eşit; mode & 0o777 == 0o600;
    //  {"version":2,...} ve fazladan alan → Err)
}
```

- [ ] **Step 2: Run to verify RED** — `rtk cargo test --lib usage::calendar` ve `rtk cargo test --test usage_calendar_contract` → derleme hatası (modül yok).
- [ ] **Step 3: Implement** yukarıdaki imzaları. `period_start`: `clamp(y,m) = min(reset_day, days_in_month(y,m))`; bugünün günü ≥ clamp(bu ay) ise bu ayın o günü, değilse önceki ayın clamp günü. `next_reset`: dönem başlangıcından sonraki ayın clamp günü.
- [ ] **Step 4: Run to verify GREEN** — aynı komutlar → PASS.
- [ ] **Step 5: Commit** `feat(usage): add local calendar and quota settings`

### Task 2: Kart sayaçları ve kullanım deposu

**Files:**
- Create: `src/usage/counters.rs`, `src/usage/store.rs`, `tests/usage_store_contract.rs`

**Interfaces:**
- Consumes: Task 1 `LocalDate`, `LaneQuota`, `period_start`.
- Produces:
  - `#[derive(Serialize, Deserialize)] pub struct InterfaceSample { pub ifindex: u32, pub rx_bytes: u64, pub tx_bytes: u64 }`
  - `pub trait InterfaceCounters: Send + Sync { fn sample(&self, interface: &str) -> Option<InterfaceSample>; }`, `pub struct SysfsCounters { pub root: PathBuf }` (varsayılan `/sys/class/net`; arayüz adı `[A-Za-z0-9_.:-]{1,15}` dışındaysa `None` — yol enjeksiyonu yok).
  - `pub fn counter_delta(previous: Option<&InterfaceSample>, current: &InterfaceSample) -> UsageTotals` (`UsageTotals { pub rx: u64, pub tx: u64 }`, mod.rs). Kural: önceki yok → 0 (ilk örnek taban); ifindex farklı veya herhangi bir sayaç azaldı → `current`'ın kendisi; aksi halde fark.
  - `pub struct UsageStore` (dosya `usage-v1.json`, `version: 1`), alanlar: lane başına `daily: BTreeMap<LocalDate, UsageTotals>` (son 400 gün), `last_sample: Option<InterfaceSample>`, `boot_id: String`, `correction: Option<(LocalDate /*period_start*/, i64 /*offset_bytes*/)>`, `fired: Option<(LocalDate, bool /*warn*/, bool /*full*/)>`.
  - `UsageStore::open(path, boot_id: &str) -> (Self, Option<String> /*corruption note*/)` — bozuk dosya `path.corrupt-<unix>` olarak yeniden adlandırılır, boş depo döner; farklı `boot_id` ise tüm `last_sample`'lar düşer (reboot: sayaçlar sıfırdan).
  - `fn record_sample(&mut self, lane: &LaneId, today: LocalDate, sample: InterfaceSample) -> UsageTotals` (delta'yı bugüne ekler, `last_sample`'ı günceller).
  - `fn day(&self, lane, date) -> UsageTotals`, `fn period_measured(&self, lane, start: LocalDate, today: LocalDate) -> UsageTotals`, `fn used_bytes(&self, lane, quota: &LaneQuota, today) -> u64` (ölçülen ↓+↑ + aynı dönemin düzeltmesi, 0'ın altına inmez).
  - `fn set_used(&mut self, lane, quota, today, used_bytes: u64)` (düzeltme = hedef − ölçülen).
  - `fn crossings(&mut self, lane, quota, today) -> Vec<QuotaCrossing>`; `pub enum QuotaCrossing { Warning { used: u64, limit: u64 }, Full { used: u64, limit: u64 } }` — dönem değişince `fired` sıfırlanır; her biri dönemde bir kez.
  - `fn lifetime_bytes(&self, lane: &LaneId) -> u64` (tüm günlerin ↓+↑ toplamı; Task 5 yedek hat harcaması için).
  - `fn save(&self) -> anyhow::Result<()>` (atomik, 0600).

- [ ] **Step 1: Write the failing tests** (`tests/usage_store_contract.rs`, tempdir)

```rust
#[test]
fn deltas_survive_restart_and_never_go_negative() {
    // aynı boot: open → record(1000/100) taban → record(1500/150) → save → yeniden open
    // → record(1700/170): bugün = 500+200 rx, 50+20 tx (çift sayım yok)
    // farklı boot_id ile open → record(300/30) → bugüne +300/+30 (sıfırdan başladı)
    // aynı boot, ifindex 3→9 → yeni değer tam eklenir; rx azalması → yeni değer tam eklenir
}

#[test]
fn quota_usage_correction_and_thresholds_fire_once_per_period() {
    let quota = LaneQuota { enabled: true, limit_bytes: 20_000_000_000, reset_day: 1, warn_percent: 80 };
    // 1 Kasım'dan beri 12,4 GB ölçülmüş → used_bytes == 12_400_000_000, crossings boş
    // set_used(16_000_000_000) → crossings == [Warning{16e9, 20e9}]; tekrar çağrı → boş
    // set_used(15e9) sonra 17e9 → boş (dönemde bir kez)
    // +4 GB ölçüm → [Full{..}] bir kez
    // 1 Aralık: used_bytes == aralık ölçümü (düzeltme düştü), crossings yeniden mümkün
}

#[test]
fn corrupt_state_is_preserved_and_replaced_with_an_empty_store() {
    // dosyaya "{bozuk" yaz → open → note Some, yeni depo boş, *.corrupt-* dosyası var
}

#[test]
fn sysfs_counters_read_only_safe_interface_names() {
    // tempdir'de wwan0/{ifindex,statistics/rx_bytes,tx_bytes} → Some(sample)
    // "../etc", "a/b", 16 karakterlik ad → None
}
```

- [ ] **Step 2: Run to verify RED** — `rtk cargo test --test usage_store_contract` → derleme hatası.
- [ ] **Step 3: Implement** imzaları. `boot_id` daemon'da `/proc/sys/kernel/random/boot_id`'den okunur (test parametre verir).
- [ ] **Step 4: Run to verify GREEN** — aynı komut → PASS; `rtk cargo test --lib usage` → PASS.
- [ ] **Step 5: Commit** `feat(usage): count interface traffic per lane and evaluate quota thresholds`

### Task 3: Olay deposu

**Files:**
- Create: `src/usage/events.rs`, `tests/usage_events_contract.rs`

**Interfaces:**
- Produces:
  - `#[serde(tag = "kind")] pub enum EventKind { ProfileApplied { profile: String }, ProfileDeactivated, ApplyFailed { code: String, message: String, rolled_back: bool }, RuleApplied { rule_id: RuleId }, Failover { from: Option<LaneId>, to: LaneId, families: Vec<String> }, Failback { to: LaneId, backup_seconds: u64, backup_bytes: Option<u64> }, RuntimeAttention { phase: String, message: Option<String> }, QuotaWarning { lane_id: LaneId, used: u64, limit: u64 }, QuotaFull { lane_id: LaneId, used: u64, limit: u64 }, StoreRecovered { file: String } }`
  - `pub struct UsageEvent { pub at_unix_ms: i64, pub kind: EventKind }`
  - `pub enum EventCategory { All, Problems, Transitions, Rules, Quota }`, `EventKind::category(&self) -> EventCategory` (Problems: ApplyFailed, RuntimeAttention, StoreRecovered; Transitions: Failover, Failback, ProfileApplied, ProfileDeactivated; Rules: RuleApplied; Quota: QuotaWarning, QuotaFull).
  - `pub struct EventStore` (`events-v1.jsonl`, satır başına bir JSON): `open(path) -> (Self, Option<String>)` (bozuk satırlar atlanır, sayıları not edilir), `append(&mut self, event) -> anyhow::Result<()>`, `list(&self, category, now_unix_ms) -> Vec<UsageEvent>` (en yeni önce, 30 günden eski hariç), `clear(&mut self)`, `MAX_EVENTS = 5_000`, `RETENTION_DAYS = 30`. Sınır aşılınca veya açılışta eski kayıtlar düşülür ve dosya atomik yeniden yazılır; normal ekleme `O_APPEND` (0600).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn events_are_kept_for_30_days_and_at_most_5000() {
    // 5.001 olay ekle → list(All).len() == 5_000, en eski düştü
    // 31 gün önceki olay list'te yok; yeniden open sonrası da yok
}
#[test]
fn categories_filter_and_clear_removes_everything() {
    // her türden bir olay → list(Quota) yalnız QuotaWarning/QuotaFull; clear → dosya boş, list boş
}
#[test]
fn a_corrupt_line_is_skipped_not_fatal() {
    // geçerli + "bozuk" + geçerli satır → 2 olay, note Some("1 ...")
}
#[test]
fn event_file_holds_no_connection_addresses() {
    // her türün serileştirmesinde IP adresi deseni yok (EventKind alanları adres taşımaz)
}
```

- [ ] **Step 2: RED** — `rtk cargo test --test usage_events_contract` → derleme hatası.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: GREEN** — aynı komut → PASS.
- [ ] **Step 5: Commit** `feat(usage): keep a 30-day local event history`

### Task 4: Daemon protokol 4 ve kullanım istekleri

**Files:**
- Create: `src/usage/service.rs`, `tests/usage_ipc_contract.rs`
- Modify: `src/ipc/protocol.rs`, `src/daemon/service.rs`, `src/bin/routelane_daemon.rs`, `tests/primary_packaging_contract.rs` (IPC=4, runtime=3 ayrı assert; README cümlesi), `README.md` (protokol paragrafı), tüm `IPC_PROTOCOL_VERSION == 3` varsayan testler.

**Interfaces:**
- Consumes: Task 1–3.
- Produces:
  - `IPC_PROTOCOL_VERSION = 4`.
  - `DaemonRequest::{GetUsage { fast_lane: Option<LaneId> }, SetQuota { lane_id: LaneId, quota: Option<LaneQuota> }, CorrectUsage { lane_id: LaneId, used_bytes: u64 }, ListEvents { category: EventCategory }, ClearEvents}`.
  - `DaemonResult::{Usage(UsageReport), QuotaSaved, Events(Vec<UsageEvent>), EventsCleared}`.
  - `pub struct UsageReport { pub lanes: Vec<LaneUsage> }`; `pub struct LaneUsage { pub lane_id: LaneId, pub interface: String, pub available: bool, pub today: UsageTotals, pub quota: Option<QuotaStatus>, pub daily: Vec<(LocalDate, UsageTotals)> /* dönem başından bugüne, kotasızda son 7 gün */, pub rate: Option<UsageRate> /* son iki örnekten bayt/sn */, pub recent_rates: Vec<UsageRate> /* son 60 sn */ }`; `pub struct QuotaStatus { pub settings: LaneQuota, pub period_start: LocalDate, pub next_reset: LocalDate, pub used_bytes: u64, pub measured: UsageTotals }`; `pub struct UsageRate { pub rx_per_second: u64, pub tx_per_second: u64 }`.
  - `DaemonEvent::QuotaCrossed { sequence: u64, lane_id: LaneId, crossing: QuotaCrossing }` ve `DaemonEvent::EventRecorded { sequence: u64 }`.
  - `pub struct UsageService` (Arc, iç `Mutex`): `new(store, settings_path, events, counters: Arc<dyn InterfaceCounters>, clock: Arc<dyn Fn() -> i64 + Send + Sync>) `, `tick(&self, lanes: &[Lane]) -> Vec<(LaneId, QuotaCrossing)>`, `report(&self, lanes, fast_lane) -> UsageReport`, `set_quota(...) -> Result<(), DaemonError>` (doğrulama; `None` kaldırır), `correct(...)`, `record(&self, EventKind)`, `events(...)`, `clear_events()`, `interval(&self) -> Duration` (son 5 sn içinde `fast_lane` istendiyse 1 sn, değilse 30 sn).
  - `DaemonService::with_usage(self, usage: Arc<UsageService>) -> Self`; usage yoksa yeni istekler `DAEMON_USAGE_UNAVAILABLE` döner. Kullanım istekleri `mutation` kilidini almaz, `ensure_reconciled` çağırmaz, helper'a gitmez, profil/kernel değiştirmez.

- [ ] **Step 1: Write the failing tests** (`tests/usage_ipc_contract.rs`, sahte helper/topology, sahte `InterfaceCounters`, sabit saat)

```rust
#[tokio::test]
async fn usage_requests_never_touch_routing_or_config() {
    // GetUsage/SetQuota/CorrectUsage/ListEvents/ClearEvents → helper çağrı sayısı 0,
    // config.save 0, state.generation/event_sequence (QuotaCrossed hariç) değişmez
}
#[tokio::test]
async fn quota_report_matches_the_vinn_example() {
    // VINN 20 GB, gün 1, %80; 1–9 Kasım ölçümü 12,4 GB → used_bytes 12_400_000_000,
    // period_start 2026-11-01, next_reset 2026-12-01; kotasız Ethernet → quota None, daily 7 gün
}
#[tokio::test]
async fn invalid_quota_is_rejected_without_saving() {
    // reset_day 32 → DAEMON_VALIDATION_FAILED, usage-settings.json yok
}
#[tokio::test]
async fn protocol3_client_gets_an_actionable_mismatch() {
    // protocol_version 3 zarfı → IPC_UNSUPPORTED_PROTOCOL_VERSION, hiçbir yan etki yok
}
#[tokio::test]
async fn fast_sampling_lasts_five_seconds_after_the_last_request() {
    // GetUsage{fast_lane: Some} → interval 1 sn; saat +6 sn → 30 sn
}
```

- [ ] **Step 2: RED** — `rtk cargo test --test usage_ipc_contract` → derleme hatası.
- [ ] **Step 3: Implement.** Daemon main: `UsageService`'i XDG yollarıyla açar, açılış notlarını `StoreRecovered` olayı yapar; `tokio::spawn` döngüsü `service.interval()` aralığıyla `tick(desired_profile.lanes)` çağırır, `save` eder, kesişimleri Task 5'in bildirim yoluna verir. Mevcut testlerde protokol 3 varsayımları güncellenir (ör. `primary_daemon_contract.rs`'deki `(IPC, RUNTIME) == (3, 3)` → `(4, 3)`).
- [ ] **Step 4: GREEN** — `rtk cargo test --test usage_ipc_contract --test ipc_contract --test primary_daemon_contract --test cli_contract --test primary_packaging_contract` → PASS.
- [ ] **Step 5: Commit** `feat(daemon): serve usage, quota and event history over IPC protocol 4`

### Task 5: Olay kaydı ve kota bildirimleri

**Files:**
- Create: `src/usage/notify.rs`
- Modify: `src/daemon/service.rs`, `src/usage/service.rs`, `src/bin/routelane_daemon.rs`
- Test: `tests/usage_events_contract.rs` (daemon senaryoları eklenir), `src/usage/service.rs` birim testleri

**Interfaces:**
- Consumes: Task 3 `EventKind`, Task 4 `UsageService::record`, `DaemonEvent::QuotaCrossed`.
- Produces:
  - `pub struct FailoverMark { pub started_unix: i64, pub backup_lane: LaneId, pub backup_total_at_start: u64 }` (`UsageService` tutar; `Failover` olayında kurulur, `Failback`'te düşer).
  - `pub fn runtime_events(previous: &RuntimeStatus, next: &RuntimeStatus, mark: Option<&FailoverMark>, now_unix: i64, backup_total_now: Option<u64>) -> Vec<EventKind>` — etkin IPv4/IPv6 çıkışı tercihten yedeğe değişince `Failover` (aileler `"IPv4"`/`"IPv6"`), tercihe dönünce `Failback` (`backup_seconds = now − started_unix`, `backup_bytes = backup_total_now − backup_total_at_start`; mark yoksa `None`), faz `NeedsReauthorization`/`Degraded`'e geçince `RuntimeAttention`. Aynı durum tekrarında olay yok. “A toplamı” = `UsageStore`'un lane için tüm günlerdeki ↓+↑ toplamı (`fn lifetime_bytes(&self, lane) -> u64`, Task 2'ye eklenir).
  - Daemon kayıt noktaları: `apply_profile_locked` başarı → `ProfileApplied` + önceki uygulanan profile göre yeni/etkinleşen her kural için `RuleApplied`; başarısızlık → `ApplyFailed { code, message, rolled_back }` (`DAEMON_KERNEL_ROLLBACK_FAILED` ise `false`); `deactivate` başarı → `ProfileDeactivated`; `follow_runtime` → `runtime_events`.
  - `pub trait Notifier: Send + Sync { fn notify(&self, summary: &str, body: &str); }`, `SessionNotifier` (zbus oturum bus'ı, `org.freedesktop.Notifications.Notify`, uygulama adı `RouteLane`, simge `network-transmit-receive-symbolic`; bus yoksa sessizce günlükler).
  - Karar: `DaemonService::subscriber_count() > 0` ise `DaemonEvent::QuotaCrossed` yayınlanır, değilse `Notifier::notify`. Her iki durumda `QuotaWarning`/`QuotaFull` olayı kaydedilir. Bildirim metni daemon'un dil ayarına göre (config `language`) spec §4.7'den birebir:
    - Dil: `ConfigRepository` trait'ine varsayılan gövdeli `fn language(&self) -> Language { Language::default() }` eklenir; `AtomicJsonConfigRepository` config belgesinin `language` alanını okur (salt okunur).
    - TR uyarı: `"{lane} kotasının %{percent}'i doldu — {used} / {limit} GB · {left} GB kaldı · {date} yenilenir"`; TR doldu: `"{lane} kotası doldu — trafik kesilmedi; operatörünüz ek ücret alabilir."`; EN karşılıkları i18n'de.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn runtime_transitions_produce_one_event_each() {
    // tercih ethernet; etkin ethernet → vinn (v4+v6): [Failover{from: ethernet, to: vinn, families: [IPv4, IPv6]}]
    // aynı durum tekrar: []; vinn → ethernet, 420 sn, 410 MB: [Failback{to: ethernet, backup_seconds: 420, backup_bytes: Some(410_000_000)}]
    // Armed → Degraded: [RuntimeAttention{phase: "Degraded", ..}]
}
#[tokio::test]
async fn apply_and_deactivate_are_recorded() {
    // apply başarılı → ProfileApplied + yeni kural için RuleApplied; ikinci aynı apply → RuleApplied yok
    // helper hata → ApplyFailed{code, rolled_back: true}; deactivate → ProfileDeactivated
}
#[tokio::test]
async fn quota_crossing_notifies_desktop_only_without_an_open_ui() {
    // abone yok → sahte Notifier 1 çağrı (TR metin birebir), DaemonEvent yok
    // abone var → Notifier 0 çağrı, 1 QuotaCrossed olayı; ikisinde de 1 QuotaWarning kaydı
}
```

- [ ] **Step 2: RED** — `rtk cargo test --test usage_events_contract --lib usage::service` → FAIL/derleme hatası.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: GREEN** — aynı komut + `rtk cargo test --test primary_daemon_contract` → PASS.
- [ ] **Step 5: Commit** `feat(daemon): record routing events and notify quota thresholds`

### Task 6: Arayüz oturumu, kota çubuğu ve grafik bileşenleri

**Files:**
- Create: `src/ui/usage_widgets.rs`
- Modify: `src/ui/session.rs`, `src/ui/controller.rs`, `src/ui/i18n.rs`, `src/ui/mod.rs`

**Interfaces:**
- Consumes: Task 4 istek/sonuçları, `DaemonEvent::{QuotaCrossed, EventRecorded}`.
- Produces:
  - `UiSession::usage(&self) -> Option<&UsageReport>`, `UiSession::events(&self) -> &[UsageEvent]`; `GetUsage`/`ListEvents` salt okunur: `begin_request` meşgul durumunu değiştirmez, `save_state`'e dokunmaz (`GetStatus` gibi).
  - `UiController::refresh_usage(fast_lane: Option<LaneId>)`, `UiController::save_quota(lane_id, Option<LaneQuota>) -> bool`, `UiController::correct_usage(lane_id, u64) -> bool`, `UiController::clear_events() -> bool`, `UiController::connect_quota_crossed(Rc<dyn Fn(&LaneId, &QuotaCrossing)>)`. Yoklama: 30 sn; `fast_lane` verilmişse 1 sn (glib timeout, pencere görünürken).
  - `pub fn format_gb(bytes: u64, language) -> String` (bir ondalık, TR virgül: `12,4`; EN nokta), `pub fn format_rate(bytes_per_second, language) -> String` (kB/s, MB/s).
  - `pub struct QuotaBar` (DrawingArea; doluluk, eşik çizgisi, %100 üstü kırmızı), `pub struct UsageChart` (DrawingArea; `ChartMode::{Period, LastSevenDays, LiveRate}`; kotalı hatta günlük ortalama sınır çizgisi `= kalan / kalan gün`; ortalamanın üstündeki gün vurgulu; erişilebilir etiket: “Günlük kullanım grafiği, en yüksek {gün} {değer} GB”).
  - i18n (TR/EN, spec §4 birebir): `usage.today_down_up` (“Bugün ↓ {down} ↑ {up}”), `usage.period_used` (“Bu dönem {used} / {limit} GB”), `usage.quota_badge` (“Kota %{percent}”), `usage.approximate` (“yaklaşık”), `usage.remaining` (“Kalan {left} GB · {date} sıfırlanır · %{warn} uyarı”), `usage.not_connected` (“Bağlı değil”), `usage.download`/`upload`, grafik sekmeleri (“Bu dönem”, “Son 7 gün”, “Canlı hız”), kota penceresi ve olay sayfası metinleri (Task 7/8'de kullanılır).

- [ ] **Step 1: Write the failing tests** (`ui::session::tests`, `ui::usage_widgets::tests`)

```rust
#[test]
fn usage_reads_do_not_block_or_change_save_state() {
    // GetUsage gönderilirken is_busy false, save_state değişmez; Usage cevabı usage()'a yazılır
}
#[test]
fn gigabytes_use_one_decimal_and_locale_separator() {
    assert_eq!(format_gb(12_400_000_000, Language::Tr), "12,4");
    assert_eq!(format_gb(12_400_000_000, Language::En), "12.4");
    assert_eq!(format_rate(1_800_000, Language::Tr), "1,8 MB/s");
}
#[test]
fn daily_limit_line_is_remaining_over_remaining_days() {
    // 20 GB, 9 Kasım'da 12,4 GB kullanılmış, 22 gün kaldı → 0,345 GB/gün (±0,001)
}
```

- [ ] **Step 2: RED** — `rtk cargo test --bin routelane ui::session::tests ui::usage_widgets::tests` → derleme hatası.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: GREEN** — aynı komut → PASS.
- [ ] **Step 5: Commit** `feat(ui): fetch usage and add quota bar and usage chart`

### Task 7: Bağlantılar listesi, hat ayrıntısı ve kota penceresi

**Files:**
- Create: `src/ui/lane_detail.rs`, `src/ui/quota_dialog.rs`
- Modify: `src/ui/connections.rs`, `src/ui/window.rs` (smoke)

**Interfaces:**
- Consumes: Task 6 `UiController` ve bileşenleri.
- Produces:
  - Satır (spec §4.1): alt başlık `"{arayüz} · Bugün ↓ … ↑ …"` (kotasız) veya `"{arayüz} · Bu dönem {used} / {limit} GB"` + `QuotaBar` + “Kota %{n}” rozeti; arayüz yoksa “Bağlı değil”. Satır etkinleştirilince (`activatable`) hat ayrıntısı açılır; Düzenle/Sil düğmeleri aynen kalır. Satır adları: `lane-row-{lane_id}`, ayrıntı `lane-detail`, kota düğmesi `quota-open`.
  - Hat ayrıntısı (spec §4.2, B ve canlı liste hariç): başlıkta geri düğmesi “‹ Bağlantılar”; kota kartı (kotalıysa çubuk ve “Kalan … sıfırlanır · %80 uyarı”, kotasızsa yalnız “Kota ayarla”); ↓/↑ kartları (dönem veya son 7 gün toplamı, “yaklaşık”); `UsageChart` üç sekmeli. Açıkken `refresh_usage(Some(lane_id))`.
  - Kota penceresi (spec §4.6): “Kota sınırı kullan” anahtarı, aylık kota (GB, 0,1 adım), yenilenme günü (1–31), uyarı eşiği (% , varsayılan 80), “Bu dönem kullanılan” (elle düzeltme), Wi-Fi arayüzünde (`/sys/class/net/<if>/wireless` varsa) “aynı kartla bağlandığınız diğer ağlar da sayılır” notu, alt not “RouteLane trafiği kesmez… operatörün sayacıyla birebir tutmayabilir.” Kaydet doğrular, tek istek gönderir (çift tık tek `SetQuota`; düzeltme değiştiyse ardından tek `CorrectUsage`); İptal/Escape/kapatma yan etkisiz. Adlar: `quota-enabled`, `quota-limit`, `quota-reset-day`, `quota-warn`, `quota-used`, `quota-save`.

- [ ] **Step 1: Write the failing smoke** (`window.rs`, `gtk_smoke_` testine `assert_usage_screens(&app, language)`; her iki dilde)

```rust
// Snapshot + Usage cevabı: ethernet kotasız (bugün 3,1 GB / 420 MB), vinn kotalı 12,4/20 GB
// lane-row-vinn alt başlığı "usb0 · Bu dönem 12,4 / 20 GB" ve "Kota %62" rozeti
// lane-row-vinn etkinleştir → lane-detail görünür; GetUsage{fast_lane: Some("vinn")} isteği
// quota-open → pencere; değerler 20 / 1 / 80; Escape → istek yok
// tekrar aç, quota-limit 25, quota-save iki kez tıkla → tek SetQuota{vinn, limit 25e9}
// ekran görüntüleri: connections-usage-{tr,en}.png, lane-detail-{tr,en}.png, quota-dialog-{tr,en}.png
```

- [ ] **Step 2: RED** — `rtk proxy bash scripts/run-gtk-smoke.sh` → FAIL (`lane-row-vinn` yok).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: GREEN** — `ROUTELANE_SMOKE_SHOTS=<dir> rtk proxy bash scripts/run-gtk-smoke.sh` → PASS; görüntüler okunup TR/EN metinler doğrulanır.
- [ ] **Step 5: Commit** `feat(ui): show per-lane usage, lane detail and quota settings`

### Task 8: Genel Bakış kartları, olay geçmişi ve toast

**Files:**
- Create: `src/ui/events_page.rs`
- Modify: `src/ui/overview.rs`, `src/ui/window.rs`, `src/ui/i18n.rs`

**Interfaces:**
- Consumes: Task 6 `UiController::{events, clear_events, connect_quota_crossed}`.
- Produces:
  - Genel Bakış (spec §4.4): “Veri kullanımı (bu dönem)” kartı (kotalı hatlar `QuotaBar`, diğerleri ↓/↑), “Son olaylar” kartı (son 3 olay + “Tümünü göster ›”, ad `events-open`).
  - Olay geçmişi (spec §4.5): geri “‹ Genel Bakış”, filtreler `Tümü/Sorunlar/Geçişler/Kurallar/Kota` (`ListEvents{category}`), satır başlıkları spec metinleriyle (“Yönlendirme uygulanamadı, önceki ayar geri yüklendi”, “VINN kotasının %80'i kullanıldı — 16,0 / 20 GB”, “Ethernet kesildi, VINN'e geçildi — IPv4 ve IPv6”, “Ethernet'e geri dönüldü — 7 dk yedekte, 410 MB”, ““chatgpt.com” kuralı uygulandı”), hata satırında açılır seçilebilir ayrıntı (kod · mesaj · geri alma), alt not “Son 30 gün saklanır, yalnız bu bilgisayarda.”, “Temizle” onaylı (İptal yan etkisiz). Ad: `events-page`, `events-clear`.
  - `QuotaCrossed` geldiğinde toast + “Ayrıntı” (hat ayrıntısını açar); `EventRecorded` olay listesini tazeler.

- [ ] **Step 1: Write the failing smoke** (`assert_overview_and_events(&app, language)`)

```rust
// Events cevabı 5 tür → Genel Bakış'ta 3 satır; events-open → events-page, 5 satır
// Kota filtresi → ListEvents{category: Quota} isteği
// events-clear → onay; Escape → istek yok; onay → tek ClearEvents
// QuotaCrossed olayı → toast görünür (adw::Toast başlığı TR/EN metin)
// ekran görüntüleri: overview-usage-{tr,en}.png, events-{tr,en}.png
```

- [ ] **Step 2: RED** — smoke → FAIL (`events-open` yok).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: GREEN** — smoke → PASS; görüntüler kontrol edilir.
- [ ] **Step 5: Commit** `feat(ui): add usage overview, event history and quota toasts`

### Task 9: Tam regresyon, belgeler ve devir

**Files:**
- Modify: `README.md` (yeni “Data usage, quota and event history” bölümü: kaynak, yaklaşıklık, dosya yolları, gizlilik, Wi-Fi notu, B/canlı listenin Plan 2/3'te olduğu), `docs/handoff.md`, `tests/primary_packaging_contract.rs` (README `usage-settings.json` ve `events-v1.jsonl` adlarını içerir)

- [ ] **Step 1:** `rtk cargo test --all-targets --no-fail-fast` (bilinen Snap hatası adıyla raporlanır), `rtk cargo clippy --all-targets -- -D warnings`, `rtk cargo clippy --all-targets --features root-integration -- -D warnings`, `rtk proxy cargo fmt --all -- --check`, `rtk git diff --check`, `rtk cargo build --release --bins`, `rtk proxy bash scripts/run-gtk-smoke.sh` — hepsinin çıktısı okunur.
- [ ] **Step 2:** README ve sözleşme güncellenir; `rtk cargo test --test primary_packaging_contract` → PASS.
- [ ] **Step 3:** Spec §1–9'a karşı öz-inceleme ve Review Focus beş maddesinin testleri adlarıyla doğrulanır; eksik test eklenir.
- [ ] **Step 4:** `docs/handoff.md` güncellenir (durum, NOT VERIFIED: gerçek masaüstü bildirimi oturumda, uzun süreli örnekleme doğruluğu, Plan 2/3).
- [ ] **Step 5: Commit** `docs(usage): document data usage, quota and event history`

## Tasarım kapsamı öz-incelemesi

- Spec §1 (üç an): kota anı Task 2/4/7; kural “çalışıyor mu” anı Plan 3'e (canlı liste) bırakıldı, olay tarafı (kural uygulandı) Task 5/8; sorun anı Task 3/5/8.
- §2 A Task 2/4/7; kota Task 1/2/7; C1 Task 3/5/8; B Plan 2; canlı liste Plan 3. Non-goal/anti-goal: Global Constraints.
- §3 kararları: 20 GB/gün 1 örneği Task 4 testi; kart toplamı Task 2; %80/doldu Task 2/5; kotasız davranış Task 4/7; DNS ve canlı liste kararları Plan 3.
- §4.1, 4.2 (B ve canlı hariç), 4.4, 4.5, 4.6, 4.7 Task 6–8; §4.3 Plan 3.
- §5.1 Task 2; §5.4 Task 1/2; §5.5 Task 3/5; §5.6 Task 5; §5.7 (daemon kısmı) Task 4; §5.2/5.3 Plan 2/3.
- §6: dosya izinleri Task 1–3; adres tutmama Task 3; dürüst metinler Task 6–8. §7 rootless/GTK Task 1–8; sözleşme/kernel kısımları Plan 2/3.
