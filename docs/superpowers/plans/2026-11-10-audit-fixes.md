# İnceleme bulguları düzeltmeleri — uygulama planı

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 10 Kasım 2026 proje incelemesinde bulunan 5 hatayı ve giderilebilir eksikleri, her biri önce kırmızı görülen bir testle düzeltmek.

**Architecture:** Değişikliklerin çoğu kullanıcı tarafında (GUI, daemon, `routelane::usage`) ve rootless test edilir. Yalnız Task 9 (geri alma sırasında korumalı geçiş) ve Task 10 (aralıklı kernel testi) root helper/kernel davranışına dokunur ve onaylı tek kullanımlık VM ister. IPC ve runtime protokol sürümleri değişmez (4 / 3).

**Tech Stack:** Rust 2021, tokio, serde, zbus 5, gtk4 0.5 / libadwaita 0.2 / glib 0.16 (sabit), GitHub Actions.

**Spec:** Ayrı tasarım belgesi yok; bağlayıcı kaynak aşağıdaki “Kaynak bulgular” listesidir. İlgili tasarımlar: `docs/superpowers/specs/2026-11-09-usage-and-events-design.md` (§4.7, §5.5, §6), `docs/superpowers/specs/2026-10-22-selective-routing-design.md` (§5).

## Kaynak bulgular (10 Kasım 2026 incelemesi, `b04e421`)

Hatalar:
- **H1** Snap/Flatpak koruması Ubuntu'da çalışmıyor: `/usr/bin/snap → ../lib/snapd/snapd`, çözülen ad `snapd` (`src/ui/desktop_catalog.rs:39`, `src/daemon/service.rs:598`). Bilinen başarısız test bunu yakalıyor.
- **H2** Pencere tepsiye gizliyken kota bildirimi yalnız gizli penceredeki toast'a gidiyor (`src/ui/window.rs:1326`).
- **H3** Uygulanmamış hatta kullanım “kontrol ediliyor”da kalıyor, kota kaydı reddediliyor ve kota penceresi hatayı göstermeden kapanıyor.
- **H4** Uygulama sürerken zaman aşımına uğrayan kullanım sorgusunun kaydı silinmiyor; aynı sorgu yeniden gönderilmiyor (`src/ui/session.rs:603`).
- **H5** Kullanım hatası genel hata alanının üzerine yazıp “sahipsiz kaynakları kurtar” seçeneğini kaybettirebiliyor (`src/ui/session.rs:652`).

Eksikler: **E1** `CLAUDE.md` eski · **E2** “tüm çıkışlar engellendi” olayı yok · **E3** olay metinlerinde adres olabilir · **E4** GUI↔daemon köprüsü sıralı, kullanım sorgusu 180 sn zaman aşımı kullanıyor · **E5** geri almada tablo silinip yeniden kurulurken korumasız an · **E6** katı RPF'de aralıklı fazladan paket · **E7** CLI taşıma uyarısı (davranış doğru, belgelenmemiş) · **E8** depo değişmese de sürekli diske yazılıyor · **E9** TR yüzde eki sabit, kota penceresinde nokta ondalık, canlı hız zıplıyor, bağlı olmayan kotalı hatta kullanılan gizli · **E10** HOME yoksa daemon açılmıyor · **E11** CI yok.

**Bu planın dışında (ayrı karar/ortam ister):** etkileşimli Polkit ajanı, Debian 12/13, Fedora 43/44, aarch64 doğrulaması; kullanım tasarımının Plan 2 (yönlendirilen trafik sayaçları) ve Plan 3'ü (canlı bağlantılar).

## Global Constraints

- Yeni bağımlılık yok; `gtk4 0.5` / `libadwaita 0.2` / `glib 0.16` yükseltilmez.
- `IPC_PROTOCOL_VERSION = 4`, `RUNTIME_PROTOCOL_VERSION = 3`, journal 3, config şeması 4 değişmez.
- Genel sysctl, yabancı nft tablosu, conntrack, ana yönlendirme tablosu değiştirilmez; helper yalnız doğrulanmış RouteLane kaynaklarına dokunur.
- Root testleri yalnız kullanıcının açıkça onayladığı tek kullanımlık `routelane-test-*` VM içinde; host'a kurulum yok; kullanıcının diğer VM'lerine dokunulmaz.
- Olay kayıtlarında bağlantı adresi tutulmaz. RouteLane trafiği kesmez.
- Tüm metinler TR/EN `src/ui/i18n.rs` (daemon bildirimleri `src/usage/notify.rs`).
- Her görev: önce başarısız test, sonra en küçük düzeltme; görev sonunda `rtk cargo test --all-targets --no-fail-fast`, `rtk proxy cargo fmt --all -- --check`, `rtk cargo clippy --all-targets -- -D warnings`. GUI değişen görevlerde ayrıca `rtk proxy bash scripts/run-gtk-smoke.sh`.
- Task 1'den sonra suite tam yeşil olmalı; “bilinen Snap hatası” artık kabul edilmez.

## Review Focus

1. **Snap dışı aynı adlı ikililer:** `/opt/x/snap` gibi sandbox olmayan ama adı `snap` olan dosya da reddedilir (bilinçli: güvenli taraf); `/snap/bin/firefox` ve `flatpak` sarmalayıcıları reddedilir; sıradan ana makine uygulaması reddedilmez — Task 1.
2. **Aynı lane id'li iki profil:** config'teki ikinci profilin aynı id'li hattı, uygulanan profilin arayüzünü ezmez — Task 3.
3. **GUI çökmüş ama abonelik açık:** ölü abone “arayüz açık” sayılmaz; bildirim masaüstüne gider — Task 2.
4. **Geri alma ortasında helper çökmesi:** yeniden başlayan helper ara (yalnız guard) durumu tanır ve geri almayı tamamlar; yabancı kaynak silmez — Task 9.
5. **Kayıt atlanırken kapanma:** “değişmediyse yazma” optimizasyonu, daemon düzgün kapanırken son örnekleri kaybetmez (en fazla 30 sn'lik veri) — Task 7.

---

### Task 1: Sandbox başlatıcı tespiti (H1)

**Files:** Create `src/application/sandbox.rs`; modify `src/application/mod.rs`, `src/ui/desktop_catalog.rs:35-45`, `src/daemon/service.rs:589-605`; test `tests/application_sandbox_contract.rs`.

**Interfaces:**
- Produces: `pub fn is_sandbox_launcher(program: &Path) -> bool` — true when the path's own file name **or** its canonicalized target's file name is one of `snap`, `snapd`, `flatpak`, or the canonical path starts with `/usr/lib/snapd/`, `/snap/` or `/var/lib/flatpak/`. A path that cannot be canonicalized is judged by its own name only.

- [ ] **Step 1: failing tests** (`tests/application_sandbox_contract.rs`, tempdir symlinks)

```rust
#[test]
fn symlinked_snap_launcher_is_detected() {
    // dir/lib/snapd/snapd (file), dir/bin/snap -> ../lib/snapd/snapd, dir/bin/firefox -> snap
    assert!(is_sandbox_launcher(&dir.join("bin/snap")));
    assert!(is_sandbox_launcher(&dir.join("bin/firefox")));
    assert!(is_sandbox_launcher(Path::new("/usr/bin/flatpak-missing/../flatpak")));
}
#[test]
fn host_applications_are_not_sandbox_launchers() {
    // dir/bin/browser (file), dir/bin/snapshot-tool (file)
    assert!(!is_sandbox_launcher(&dir.join("bin/browser")));
    assert!(!is_sandbox_launcher(&dir.join("bin/snapshot-tool")));
}
```

- [ ] **Step 2:** `rtk cargo test --test application_sandbox_contract` → derleme hatası. Mevcut `ui::desktop_catalog::tests::sandbox_launcher_is_not_selectable_as_a_host_application` zaten RED (gerçek `/usr/bin/snap` ile).
- [ ] **Step 3:** Fonksiyonu yaz; katalog ve daemon'daki iki `matches!(… "snap" | "flatpak")` bloğunu ona çevir (daemon'da çözülemeyen yol için mevcut “executable is unavailable” hatası korunur).
- [ ] **Step 4:** `rtk cargo test --test application_sandbox_contract` ve `rtk cargo test --bin routelane ui::desktop_catalog` → PASS; tam suite 0 failed.
- [ ] **Step 5:** Commit `fix(application): detect symlinked snap and flatpak launchers`

### Task 2: Gizli pencerede ve ölü abonede kota bildirimi (H2)

**Files:** modify `src/ui/window.rs` (kota geçişi geri çağrısı), `src/ipc/server.rs` (`serve_connection`), test `tests/ipc_contract.rs`, `src/ui/window.rs` smoke.

**Interfaces:**
- GUI: `QuotaCrossed` geldiğinde ana pencere `is_visible() && is_active()` ise yalnız toast; değilse `gio::Notification` (başlık `texts.usage.quota_notification_title`, gövde toast metni, kimlik `quota-{lane_id}`) `application.send_notification` ile gönderilir (mevcut `announce_primary_transition` deseni).
- Sunucu: abonelik döngüsü `tokio::select!` ile `events.recv()` yanında `stream.readable()`/EOF'u izler; karşı uç kapanınca alıcıyı düşürüp döner.

- [ ] **Step 1: failing tests**

```rust
// tests/ipc_contract.rs
#[tokio::test]
async fn closed_subscriber_is_dropped_without_waiting_for_an_event() {
    // Subscribe → subscriber_count == 1; istemci soketini kapat;
    // 500 ms içinde hiçbir olay yayınlanmadan subscriber_count == 0
}
// src/ui/window.rs smoke (assert_overview_and_events içine)
// window.hide(); QuotaCrossed gönder → pencere görünmezken
// `ui.last_desktop_notice()` == Some(lokalize uyarı metni); window.present(); ikinci
// QuotaCrossed → toast var, last_desktop_notice değişmedi
```

`last_desktop_notice` yalnız test için `WindowUi` üzerinde `RefCell<Option<String>>` (gönderilen son gio bildirimi gövdesi).

- [ ] **Step 2:** `rtk cargo test --test ipc_contract closed_subscriber` → FAIL (sayaç 1 kalır); smoke → FAIL.
- [ ] **Step 3:** Uygula.
- [ ] **Step 4:** İkisi PASS; `tests/usage_daemon_contract.rs::quota_crossing_notifies_desktop_only_without_an_open_ui` hâlâ PASS.
- [ ] **Step 5:** Commit `fix(usage): notify the desktop when the window is hidden or the subscriber is gone`

### Task 3: Uygulanmamış hatlarda kullanım ve kota; kota penceresinde hata (H3)

**Files:** modify `src/daemon/service.rs` (`ConfigRepository`, `usage_request`, `sample_usage`, `require_usage_lane`), `src/ui/controller.rs`, `src/ui/quota_dialog.rs`, `src/ui/session.rs`; tests `tests/usage_ipc_contract.rs`, `src/ui/controller.rs`, smoke.

**Interfaces:**
- `ConfigRepository::saved_lanes(&self) -> Vec<Lane>` (varsayılan gövde `vec![]`); `AtomicJsonConfigRepository` komşu `config.json`'u salt okunur yükleyip tüm profillerin hatlarını döndürür (okunamazsa boş; asla yazmaz/taşımaz).
- `DaemonService::usage_lanes(&self) -> Vec<Lane>`: önce desired profilin hatları, sonra `saved_lanes()` içinden **id'si henüz listede olmayanlar**. `GetUsage`, `SetQuota`, `CorrectUsage` ve `sample_usage` bunu kullanır.
- `UiController::save_quota(lane_id, quota: Option<LaneQuota>, used: Option<u64>, on_done: Box<dyn FnOnce(Result<(), IpcError>)>) -> bool`: `SetQuota`'yı gönderir; başarılı cevaptan sonra `used` varsa `CorrectUsage` gönderir; son cevap/hata `on_done`'a gider. `UiSession` usage cevabındaki hatayı bu istek için döndürür.
- Kota penceresi cevap gelene kadar açık kalır (Kaydet duyarsız); `Ok` → kapanır, `Err` → `form.error(mesaj)` ve yeniden denenebilir.

- [ ] **Step 1: failing tests**

```rust
// tests/usage_ipc_contract.rs
#[tokio::test]
async fn saved_but_unapplied_lanes_are_sampled_and_accept_a_quota() {
    // desired yok; config.saved_lanes = [vinn(usb0)] → GetUsage 1 hat, available true;
    // SetQuota{vinn} → QuotaSaved; sample_usage sonrası today artar
}
#[tokio::test]
async fn applied_lane_wins_over_a_saved_lane_with_the_same_id() {
    // desired vinn(usb0), saved vinn(wlan9) → rapor interface == "usb0", tek satır
}
// src/ui/controller.rs
#[test]
fn quota_save_reports_the_daemon_error_and_sends_correction_only_after_success() {
    // save_quota(.., used: Some(16e9), on_done) → yalnız SetQuota gönderildi;
    // hata cevabı → on_done(Err(code DAEMON_PROFILE_INVALID)), CorrectUsage gönderilmedi;
    // yeniden: QuotaSaved → CorrectUsage gönderildi → QuotaSaved → on_done(Ok)
}
```

Smoke: `quota-save` sonrası pencere açık; hata cevabında `form-feedback` görünür; başarıda kapanır; çift tık yine tek `SetQuota`.

- [ ] **Step 2:** `rtk cargo test --test usage_ipc_contract saved_but applied_lane` → FAIL; controller testi derleme hatası.
- [ ] **Step 3:** Uygula.
- [ ] **Step 4:** PASS + smoke PASS.
- [ ] **Step 5:** Commit `fix(usage): count saved lanes before activation and surface quota errors`

### Task 4: Kullanım sorgularının arayüz durumundan ayrılması (H4, H5, E4)

**Files:** modify `src/ui/session.rs`, `src/main.rs` (`send_with_timeout`, `daemon_bridge`), `src/ui/overview.rs` (hata gösterimi gerekiyorsa); tests `src/ui/session.rs`, `src/main.rs`.

**Interfaces:**
- `UiSession::usage_error(&self) -> Option<&IpcError>`; kullanım/olay cevabı hataları yalnız buraya yazılır, `last_error()`'a yazılmaz; başarılı kullanım cevabı temizler.
- `DaemonUnavailable { request_id: Some(id) }` bir kullanım isteğine aitse: kayıt `usage_requests`'ten silinir, `usage_error` yazılır, `known`/`pending`/`preview`/`save_state` değişmez, `vec![]` döner.
- Köprü: `GetUsage` ve `ListEvents` `request_timeout` (3 sn) kullanır; bu ikisinin taşıma hatası aboneliği kapatmaz/yeniden bağlanma başlatmaz.
- Köprü: `UiToDaemon::Request` teslimi `tokio::spawn` ile yapılır; sonuç `mpsc` ile döngüye döner (yalnız kullanım dışı isteklerin hatası yeniden bağlanmayı tetikler). Böylece Polkit beklenirken olaylar ve sorgular akar.

- [ ] **Step 1: failing tests**

```rust
// ui::session::tests
#[test]
fn usage_failures_never_replace_routing_errors_or_block_polling() {
    // error = DAEMON_ORPHANED_RESOURCES iken GetUsage hata cevabı → last_error hâlâ ORPHANED,
    // usage_error Some; recovery isteği hâlâ kabul edilir
    // Apply beklerken GetUsage için DaemonUnavailable{request_id} → operation() değişmedi,
    // known true; aynı GetUsage yeniden begin_request → Send (koalesans engellemiyor)
}
// src/main.rs tests
#[tokio::test]
async fn events_and_polls_flow_while_a_mutation_is_waiting() {
    // sahte daemon ApplyProfile'a 300 ms cevap vermez; bu sırada yayınlanan olay ve
    // GetUsage cevabı UI kanalına Apply cevabından ÖNCE ulaşır
}
#[tokio::test]
async fn a_usage_timeout_does_not_reconnect() {
    // GetUsage cevapsız → 3 sn politikası testte 20 ms; DaemonUnavailable{request_id}
    // gelir, connection epoch artmaz
}
```

- [ ] **Step 2:** `rtk cargo test --bin routelane usage_failures events_and_polls a_usage_timeout` → FAIL.
- [ ] **Step 3:** Uygula.
- [ ] **Step 4:** PASS; mevcut köprü testleri (`src/main.rs`) ve `ui::session::tests` PASS.
- [ ] **Step 5:** Commit `fix(ui): keep usage polling separate from routing state and unblock the bridge`

### Task 5: Engellenme olayı ve olay metinlerinde adres gizleme (E2, E3)

**Files:** modify `src/usage/events.rs`, `src/usage/service.rs`, `src/ui/events_page.rs`, `src/ui/i18n.rs`; tests `tests/usage_events_contract.rs`, birim testleri.

**Interfaces:**
- `EventKind::Blocked { families: Vec<String> }` (kategori `Problems`); `runtime_events` bir ailenin etkin çıkışı `Lane` → `Blocked` olduğunda üretir (aileler tek olayda birleşir). TR başlık: “Kullanılabilir onaylı bağlantı yok; yönetilen trafik engellendi — {aileler}”, EN: “No approved connection is available; managed traffic is blocked — {families}”.
- `pub fn redact_addresses(text: &str) -> String` (`src/usage/events.rs`): IPv4 dörtlülerini (isteğe bağlı `/önek` ve `:port`) ve en az iki `:` içeren IPv6 değişmezlerini `[adres]` ile değiştirir. `UsageService::record`, `ApplyFailed.message` ve `RuntimeAttention.message` alanlarını yazmadan önce buradan geçirir.

- [ ] **Step 1: failing tests**

```rust
#[test]
fn losing_every_approved_exit_is_recorded_once() {
    // ethernet/ethernet → Blocked/Blocked: [Blocked{families: ["IPv4","IPv6"]}]; tekrar: []
}
#[test]
fn recorded_messages_never_contain_addresses() {
    assert_eq!(redact_addresses("resolve 203.0.113.7:443 via 10.0.0.1/24 failed"),
               "resolve [adres] via [adres] failed");
    assert_eq!(redact_addresses("gateway 2001:db8::1 and fe80::1%eth0"),
               "gateway [adres] and [adres]%eth0");
    assert_eq!(redact_addresses("generation 12:30 code 4.2"), "generation 12:30 code 4.2");
    // record(ApplyFailed{message: "… 198.51.100.9 …"}) → dosyada "198.51.100.9" yok
}
```

- [ ] **Step 2:** RED. **Step 3:** uygula. **Step 4:** `rtk cargo test --lib usage --test usage_events_contract --bin routelane ui::events_page` PASS.
- [ ] **Step 5:** Commit `feat(usage): record blocked exits and redact addresses in events`

### Task 6: Metin ve gösterim düzeltmeleri (E9)

**Files:** modify `src/usage/notify.rs`, `src/usage/service.rs`, `src/ui/usage_widgets.rs`, `src/ui/quota_dialog.rs`, `src/ui/connections.rs`, `src/ui/events_page.rs`, `src/ui/i18n.rs`.

**Interfaces:**
- `pub fn turkish_percent_suffix(percent: u8) -> &'static str` (`src/usage/notify.rs`): son okunan sözcüğe göre — birler 1→`'i`, 2→`'si`, 3→`'ü`, 4→`'ü`, 5→`'i`, 6→`'sı`, 7→`'si`, 8→`'i`, 9→`'u`; birler 0 ise onlar 10→`'u`, 20→`'si`, 30→`'u`, 40→`'ı`, 50→`'si`, 60→`'ı`, 70→`'i`, 80→`'i`, 90→`'ı`. Daemon bildirimi, toast ve olay başlığı bunu kullanır.
- Hız: `tick` saniye farkı 0 ise delta bir sonraki örneğe taşınır (`Rates.carry: UsageTotals`), hız girdisi yazılmaz; sonraki girdi `carry + delta` üzerinden hesaplanır.
- Kota penceresi sayı alanları dile göre ondalık ayırıcı gösterir (`SpinButton::connect_output` ile `format!` + TR'de virgül; girişte ikisi de kabul).
- Bağlı olmayan kotalı hat satırı: `"{arayüz} · Bu dönem {used} / {limit} GB · Bağlı değil"`; çubuk ve rozet gösterilmeye devam eder.

- [ ] **Step 1: failing tests**

```rust
#[test]
fn turkish_percent_suffix_follows_the_spoken_number() {
    for (n, s) in [(80,"'i"),(90,"'ı"),(50,"'si"),(75,"'i"),(66,"'sı"),(40,"'ı"),(10,"'u"),(99,"'u"),(23,"'ü")] {
        assert_eq!(turkish_percent_suffix(n), s, "{n}");
    }
    // quota_notification(.., warn_percent 90, Tr) "VINN kotasının %90'ı doldu — …" ile başlar
}
#[test]
fn same_second_samples_carry_into_the_next_rate() {
    // t=100: taban; t=101: +1000 → 1000/s; t=101: +500 (hız yok); t=102: +500 → 1000/s
}
```

Smoke: TR'de `quota-limit` metni `20,0`, EN'de `20.0`; bağlı olmayan kotalı hat alt başlığı yukarıdaki metin.

- [ ] **Step 2:** RED. **Step 3:** uygula. **Step 4:** birim + smoke PASS.
- [ ] **Step 5:** Commit `fix(usage): correct Turkish suffixes, decimals, rate carry and offline quota rows`

### Task 7: Değişmeyen depoyu yazmama ve HOME'suz çalışma (E8, E10)

**Files:** modify `src/usage/store.rs`, `src/usage/service.rs`, `src/bin/routelane_daemon.rs`; tests `tests/usage_store_contract.rs`, `tests/usage_ipc_contract.rs`.

**Interfaces:**
- `UsageStore::is_dirty(&self) -> bool`; `record_sample` yalnız delta ≠ 0, ifindex/boot değişimi veya ilk örnekte kirletir; `set_used` ve durum değiştiren `crossings` kirletir; `save` temizler.
- `UsageService::tick`: depo kirliyse ve (son kayıttan ≥ 30 sn geçtiyse **veya** bu tick'te eşik geçildiyse) kaydeder. `UsageService::flush(&self)` kirliyse hemen kaydeder; `correct` hemen kaydeder (bugünkü gibi).
- Daemon: SIGTERM/SIGINT'te `usage.flush()` çağrılır. `usage_state_dir()`/boot id okunamazsa daemon uyarı günlükleyip **kullanım servisi olmadan** başlar (`with_usage` çağrılmaz; istekler `DAEMON_USAGE_UNAVAILABLE`).

- [ ] **Step 1: failing tests**

```rust
#[test]
fn idle_samples_do_not_dirty_the_store() {
    // taban + aynı sayaç değerleriyle 3 örnek → save sonrası is_dirty false kalır;
    // +1 bayt → dirty
}
#[tokio::test]
async fn fast_ticks_write_at_most_every_thirty_seconds_and_flush_on_demand() {
    // saat 0..=29 sn, her saniye değişen sayaçla tick → dosya mtime/len 1 kez değişti;
    // flush() → dosya güncel; saat 30 → yeniden yazıldı
}
```

Daemon başlatma kararı için `fn open_usage(env) -> Option<Arc<UsageService>>` ayrılır ve HOME'suz ortamda `None` döndüğü birim testiyle sabitlenir.

- [ ] **Step 2:** RED. **Step 3:** uygula. **Step 4:** PASS.
- [ ] **Step 5:** Commit `perf(usage): write the store only when it changed and start without a state directory`

### Task 8: Belgeler ve CI (E1, E7, E11)

**Files:** modify `CLAUDE.md`, `README.md`, `tests/primary_packaging_contract.rs`; create `.github/workflows/ci.yml`.

- [ ] **Step 1:** `CLAUDE.md` “Current state”, “Local-only documents”, “Reading order”, “Kernel/root tests” ve şema/protokol cümleleri güncel duruma getirilir: seçici yönlendirme 10 görev tamam (`…1bf11cc`), kernel planı modu kullanıyor, IPC 4 / runtime 3 / journal 3 / şema 4, kullanım Plan 1 tamam (`7d753c5…7a92986`), `docs/handoff.md` izleniyor, onaylı VM yok (20261109 silindi), bu plan ve sonraki planlar. Sayılar yazılmadan önce komutlar yeniden çalıştırılır.
- [ ] **Step 2:** README CLI bölümüne: eski şemalı dosya her çalıştırmada `--acknowledge-routing-impact` ister; kalıcı çözüm dosyayı şema 4 olarak (GUI ile) kaydetmektir. Sözleşmeye bu cümlenin anahtarı (`"saved as schema version 4"`) eklenir; `rtk cargo test --test primary_packaging_contract` önce FAIL sonra PASS.
- [ ] **Step 3:** `.github/workflows/ci.yml`: `ubuntu-24.04`, `push` + `pull_request`; adımlar: `libgtk-4-dev libadwaita-1-dev` kurulumu, Rust stable (MSRV 1.87 matrisi ile), `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo clippy --all-targets --features root-integration -- -D warnings`, `cargo test --all-targets --no-fail-fast`, `cargo test --test root --features root-integration --no-run`. Root/kernel testi CI'da **çalıştırılmaz**. Sözleşme testi dosyanın varlığını ve bu beş komutu içerdiğini doğrular.
- [ ] **Step 4:** CI'ın gerçekten yeşil olduğu ancak push sonrası görülebilir; push kullanıcı kararıdır. O zamana kadar README/handoff'ta “CI tanımlı, çalıştığı NOT VERIFIED” yazılır.
- [ ] **Step 5:** Commit `docs: refresh project state and add rootless CI`

### Task 9: Geri almada korumalı geçiş (E5) — VM onayı gerekir

**Files:** modify `src/backend/privileged.rs` (`restore`, `PendingExpectations`, kurtarma sınıflandırması), `src/backend/nft_document.rs` (gerekirse yardımcı); tests `src/backend/privileged.rs` birim düzeneği, `tests/transaction_contract.rs`, `tests/root/selective_routing.rs`; update `docs/selective-routing-verification.md`.

**Interfaces:**
- `restore(Some(state))`: tabloyu silmek yerine önce **tek atomik nft işlemiyle** `render_nft_document(None, Some(state.plan()), true)` (yalnız geri yüklenecek planın guard'ı) yüklenir; rotalar/kurallar yeniden kurulur; sonra tek atomik `TableMode::Replace` ile `render_owned_ruleset(state.plan(), …, false)` yüklenir. `restore(None)` bugünkü gibi tabloyu siler.
- `PendingExpectations.restore_guard_nft_fingerprint: Option<String>` (`#[serde(default)]`, journal sürümü 3 kalır); kurtarma bu parmak izini “geri alma yarıda” durumu olarak tanır ve geri almayı tamamlar.
- İlk adım olarak mevcut kod okunur: bugün geri alma ortasındaki çökme (tablo yok, rotalar yarım) nasıl sınıflandırılıyor; bulgu deftere yazılır ve yeni durum aynı güvenlik düzeyinde (yabancı kaynak silmeden) ele alınır.

- [ ] **Step 1: failing tests**

```rust
// tests/transaction_contract.rs — mevcut rollback_never_blocks_unmatched_packets genişletilir:
// InMemoryKernel "restore" probu iki ara adım kaydeder; her ikisinde yönetilen hedefler
// (eski kapsam) "reject", yönetilmeyenler serbest. Bugün: restore sırasında tablo yok → RED.
// src/backend/privileged.rs
#[test]
fn restore_never_deletes_the_table_while_a_previous_generation_exists() {
    // doğrulama hatasıyla geri alma: komut günlüğünde "delete table inet routelane" tek başına
    // batch olarak YOK; guard-only batch ve ardından owned Replace batch VAR
}
#[test]
fn restart_during_restore_guard_completes_the_rollback() {
    // journal pending + kernel'de guard-only doküman → ensure_reconciled → active plan geri,
    // guard yok, yabancı route/kural sentinel'leri değişmedi
}
```

- [ ] **Step 2:** RED (`rtk cargo test --test transaction_contract rollback_never --lib restore_never restart_during`).
- [ ] **Step 3:** Uygula.
- [ ] **Step 4:** Rootless PASS; her iki clippy.
- [ ] **Step 5 (VM):** Kullanıcı onayıyla yeni tek kullanımlık VM; `selective_switch_failure_preserves_lab` senaryosuna “geri alma duraklatıldığında yönetilen hedef reddedilir, lab ve eşleşmeyen akar” adımı eklenir (test-only ikinci duraklama noktası `restore_guard`); tam root suite standart + katı RPF. VM onayı yoksa: kod commit edilmez, görev “kernel kanıtı NOT VERIFIED — beklemede” olarak deftere yazılır.
- [ ] **Step 6:** Commit `fix(transaction): keep managed scope guarded while restoring the previous generation`

### Task 10: Aralıklı katı RPF testi (E6) — VM onayı gerekir

**Files:** inceleme; olası değişiklik `tests/root/namespace_routing.rs`; kayıt `docs/selective-routing-verification.md`.

- [ ] **Step 1:** `superpowers:systematic-debugging` ile: Task 9'un VM'inde `ROUTELANE_TEST_STRICT_RPF=1` altında `namespace_routing::real_helper_routes_three_lanes_and_preserves_generation_one_on_activate_failure` 30 kez tek başına, tam suite 10 kez çalıştırılır; başarısızlık oranı ve fazladan paketin hangi adımda (hangi prob, hangi sayaç) geldiği kaydedilir.
- [ ] **Step 2:** Hipotez (ör. önceki senaryodan kalan yeniden iletim, lab köprüsü eklendikten sonra ARP/yeniden deneme) dar bir gözlemle (sink'te nft sayaçlarını prob kimliğine göre ayırma) sınanır.
- [ ] **Step 3:** Kök neden test düzeneğindeyse düzeltilir ve 30/30 + 10/10 gösterilir; üründeyse ayrı bulgu olarak raporlanır ve bu planın kapsamı dışında bırakılır; bulunamazsa oran ve denenenler belgeye yazılır (“çözülmedi”).
- [ ] **Step 4:** Commit `test(routing): …` (yalnız gerçek bir düzeltme varsa) ve doğrulama belgesi güncellemesi.

### Task 11: Tam regresyon, devir ve son inceleme

- [ ] **Step 1:** `rtk cargo test --all-targets --no-fail-fast` (0 failed beklenir), her iki clippy, fmt, `rtk git diff --check`, `rtk cargo build --release --bins`, GTK smoke, root no-run.
- [ ] **Step 2:** Kaynak bulgular listesindeki her madde için kapatan test adı ve commit `docs/handoff.md`'ye yazılır; kapanmayanlar (Task 9/10 VM bekliyorsa, plan dışı doğrulamalar) NOT VERIFIED olarak kalır.
- [ ] **Step 3:** Bütün-dal incelemesi (yazar olmayan taze bir inceleyici); bulgular tek geçişte RED→GREEN düzeltilir.
- [ ] **Step 4:** Commit `docs: record audit fixes and remaining gates`

## Kapsam öz-incelemesi

- H1 → Task 1 · H2 → Task 2 · H3 → Task 3 · H4, H5, E4 → Task 4 · E2, E3 → Task 5 · E9 → Task 6 · E8, E10 → Task 7 · E1, E7, E11 → Task 8 · E5 → Task 9 · E6 → Task 10.
- Review Focus 1 → Task 1 testleri; 2 → Task 3 `applied_lane_wins…`; 3 → Task 2 `closed_subscriber…`; 4 → Task 9 `restart_during_restore_guard…`; 5 → Task 7 `…flush_on_demand` + SIGTERM flush.
- Plan dışı: etkileşimli Polkit, Debian/Fedora/aarch64, kullanım Plan 2/3.
