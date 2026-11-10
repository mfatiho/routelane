# RouteLane — geliştirici devir belgesi

Tarih: 22 Ekim 2026. Bu belge, devralan geliştiricinin geçmiş kararları bulması ve yarım kalan işi güvenli şekilde sürdürmesi içindir. Kalıcı devir kaydı `docs/handoff.md` dosyasıdır; `/tmp` kopyasına veya oturumlar arası asistan hafızasına bağımlı değildir. Ayrıntılı değişiklik geçmişinin kaynağı Git ve aşağıdaki depo belgeleridir.

## En son kayıt — inceleme bulguları düzeltmeleri (10 Kasım 2026)

- 10 Kasım proje incelemesinin 5 hatası (H1–H5) ve 11 eksiği (E1–E11) `docs/superpowers/plans/2026-11-10-audit-fixes.md` (`4e41925`) planına alındı; kullanıcı `/sous-chef:fire` ile yürüttü (Codex biletleri, baş aşçı satır satır inceleme ve yerel doğrulama). Defter: `.superpowers/sdd/2026-11-10-audit-fixes/progress.md`.
- Kapananlar:
  - H1 Snap/Flatpak sembolik bağlantı tespiti — `656ce38`, `tests/application_sandbox_contract.rs`, `ui::desktop_catalog::tests::sandbox_launcher_is_not_selectable_as_a_host_application` (artık geçiyor).
  - H2 gizli pencerede masaüstü bildirimi, kapanan abone hemen düşer — `656ce38`, `ipc_contract::closed_subscriber_is_dropped_without_waiting_for_an_event`, GTK smoke.
  - H3 kayıtlı ama uygulanmamış hatlar, kota penceresinde daemon hatası — `656ce38`, `usage_ipc_contract::{saved_but_unapplied_lanes_are_sampled_and_accept_a_quota, applied_lane_wins_over_a_saved_lane_with_the_same_id}`, `ui::controller::tests::quota_save_reports_the_daemon_error_and_sends_correction_only_after_success`.
  - H4/H5/E4 kullanım sorguları ayrı hata alanı, köprü eşzamanlı teslim, 3 sn zaman aşımı — `b321835`, `ui::session::tests::usage_failures_never_replace_routing_errors_or_block_polling`, `tests::{events_and_polls_flow_while_a_mutation_is_waiting, a_usage_timeout_does_not_reconnect}`.
  - E2 engellenme olayı, E3 adres gizleme — `b321835`, `losing_every_approved_exit_is_recorded_once`, `recorded_messages_never_contain_addresses`.
  - E9 TR yüzde eki, ondalık virgül, hız taşıma, bağlı olmayan kotalı hat — `703ae53`, `turkish_percent_suffix_follows_the_spoken_number`, `same_second_samples_carry_into_the_next_rate`, GTK smoke.
  - E8 değişmeyen depoyu yazmama ve kapanışta flush — `703ae53`, `idle_samples_do_not_dirty_the_store`, `fast_ticks_write_at_most_every_thirty_seconds_and_flush_on_demand`; kalıcı kayıt hatasında 30 sn'lik yeniden deneme (bağımsız incelemenin #5 bulgusu) — `a_failing_save_is_retried_on_the_thirty_second_cadence`.
  - E10 yalnız kullanım yolu için kapandı: durum dizini/boot id çözülemezse daemon kullanım servisi olmadan açılır (`open_usage_without_*`). HOME ve XDG_CONFIG_HOME ikisi de yoksa daemon yapılandırma dosyasını bulamadığı için hâlâ açılmaz; bu bilinçli ve değişmedi.
  - E1 CLAUDE.md, E7 CLI taşıma notu, E11 CI tanımı — `16e3191` (CI push edilmediği için çalışmadı: NOT VERIFIED).
- Yeni bulgu ve düzeltme: 0.1.2 kilit güncellemesi MSRV'yi bozmuştu (`uuid` 1.29.2 → rustc 1.89); `uuid` 1.23.3'e sabitlendi — `8df2141`. MSRV'yi yükseltmek kullanıcı kararı.
- Bağımsız inceleme (taze Opus incelemecisi, `4e41925..7fdf637`): Kritik/Önemli yok; 10 Küçük bulgu. Kalıcı kayıt hatasında saniyede bir yeniden deneme etkisi nedeniyle Önemli'ye yükseltilip düzeltildi; kalanlar defterde ertelendi.
- Doğrulama (10 Kasım, `703ae53`): rootless suite stable'da ve rustc 1.87'de 595 passed, **0 failed**, 2 ignored; fmt, clippy (iki özellik seti), release build, root no-run, GTK smoke temiz.
- Açık kalanlar: E5 (geri almada korumalı geçiş, Görev 9) ve E6 (aralıklı katı RPF testi, Görev 10) yeni onaylı tek kullanımlık VM bekliyor; etkileşimli Polkit, Debian/Fedora/aarch64, CI'ın gerçek çalışması NOT VERIFIED. Ertelenen küçükler defterde (örnekleyici her turda `config.json` okur, kota penceresi daemon'un İngilizce hata metnini gösterir, ayrıntı penceresi açıkken kota eşiği masaüstü bildirimine düşer).
- Push yapılmadı.

### Sonraki adım

VM onayı gelirse Görev 9–10; gelmezse kullanım Plan 2 için `writing-plans`.

## En son kayıt — veri kullanımı, kota ve olay geçmişi Plan 1/3 (10 Kasım 2026)

- Onaylı tasarım (`docs/superpowers/specs/2026-11-09-usage-and-events-design.md`) üç plana bölündü. Plan 1 (`docs/superpowers/plans/2026-11-10-usage-quota-events.md`, `c8afed1`) tamamlandı; kullanıcı `/sous-chef:fire` ile yürüttü: görevler Codex'e aşamalı biletlerle verildi, her aşama baş aşçı tarafından satır satır incelendi, testler/GTK smoke yerelde koşuldu, commit'ler baş aşçının. Karar defteri: `.superpowers/sdd/2026-11-10-usage-quota-events/progress.md` (yerel).
  - `7d753c5` Task 1–3: `routelane::usage` takvim, kota ayarı, sysfs sayaç, kullanım deposu (yeniden başlatma/reboot güvenli delta, düzeltme, dönemde bir eşik), 30 gün/5.000 olay deposu.
  - `fa8e059` Task 4–5: daemon IPC protocol 4 (runtime 3 kaldı), GetUsage/SetQuota/CorrectUsage/ListEvents/ClearEvents, örnekleyici (30 sn / ayrıntı açıkken 1 sn), olay kaydı, kota masaüstü bildirimi (UI açıkken toast olayı).
  - `48b9250` Task 6–7: UI oturumu, kota çubuğu ve grafik, Bağlantılar satırları, hat ayrıntısı, kota penceresi.
  - `e510b31` Task 8: Genel Bakış kullanım/son olaylar kartları, olay geçmişi sayfası, kota toast'u.
  - Task 9: README “Data usage, quota and event history”, paket contract'ı, bu kayıt.
- Doğrulama (10 Kasım, `e510b31` + Task 9): rootless suite 570 passed, 1 bilinen Snap hatası, 2 ignored; fmt, clippy (iki özellik seti), release build, root no-run, GTK smoke (TR/EN ekran görüntüleri `.superpowers/sdd/2026-11-10-usage-quota-events/screens/`) temiz.
- NOT VERIFIED: gerçek oturumda masaüstü bildirimi, uzun süreli örnekleme doğruluğu ve operatör sayacıyla karşılaştırma, gerçek Wi-Fi arayüzü notu; kernel değişikliği yok (bu plan kernel'e yazmaz).
- Ertelenen küçükler defterde: hızlı modda her saniye kayıt, TR yüzde eki ('i sabit), HOME yokken daemon açılmaz, bozuk usage-settings.json kenara alınmaz, kota penceresi TR'de nokta ondalık, ifindex değişiminde yeni kartın boot'tan beri sayacı bir kez eklenir.
- Kullanıcının `Cargo.toml`/`Cargo.lock` (0.1.2) değişiklikleri hâlâ commit edilmedi; bu işe karışmadı. Push yapılmadı.

### Sonraki adım

Plan 2 (B: RouteLane'in yönlendirdiği adlı sayaçlar, grant sahibine açık helper sayaç metodu, yükseltme şartı) için `writing-plans`; ardından Plan 3 (canlı bağlantılar, NFLOG, resolved ad eşlemesi). İkisi de helper/nft değiştirir; kernel kanıtı için yeniden onaylı tek kullanımlık VM gerekir.

## En son kayıt — seçici yönlendirme Task 9–10 (10 Kasım 2026)

- Kullanıcı açık onayıyla tek kullanımlık VM `routelane-test-u2404-20261109` (Ubuntu 24.04.5, 6.8.0-147) oluşturuldu; root testleri ve paket kurulumu yalnız onun içinde. Diğer Multipass VM'lerine, host ağına ve kurulu pakete dokunulmadı.
- `e9bb59b` Task 9: `tests/root/selective_routing.rs` + harness'e hat olmayan lab köprüsü (`10.65.201.0/24`, `fd00:65::/64`); 5 gerçek trafik senaryosu PASS, `UsePrimary` taban RED 5/5 gerçek assertion'da; tam root suite 25/25 standart ve strict RPF (bir strict koşuda mevcut bir senaryoda tek prob fazlası → aralıklı, çözülmedi). Kanıt: `docs/selective-routing-verification.md`.
- Task 10: README seçici kapsam, önizleme/onay, CLI `--acknowledge-routing-impact` (çıkış 6), protocol 3, schema-2 cleanup→yeniden yetkilendirme ile güncellendi (taslağı sous-chef:fire/Codex yazdı, satır satır incelendi); paket contract'ı README/sürüm sabitlerini sabitler. `system_bus_acceptance.sh` protocol 3 + açık `UsePrimary` ile VM'de kurulu `.deb` (sha256 `5809d7ef…`) üzerinde PASS (test Polkit kuralı).
- Doğrulama (10 Kasım): rootless suite 529 passed, 1 bilinen Snap hatası, 2 ignored; fmt, clippy (iki özellik seti), `git diff --check`, release build, GTK smoke, root no-run temiz.
- NOT VERIFIED: etkileşimli Polkit ajanı, silahlıyken paket yükseltmesi, gerçek bus'ta sürüm karışımı, V1/V3 kesinti gerçek kernel'de, seçici modda uygulama cgroup'u, gerçek D-Bus topology yakalaması, Debian/Fedora/aarch64, CI.
- Host paket güncellemesi, canlı RouteLane'in yeniden etkinleştirilmesi, mevcut lab kuralının düzenlenmesi ayrı kullanıcı kararıdır; bu plan yürütmesinden çıkarılmaz. Push yapılmadı.

### Sonraki adım

Son bütün-dal incelemesi (öz-inceleme; istenirse sous-chef:taste ile çapraz model), ardından onaylı kullanım/olay tasarımının (`docs/superpowers/specs/2026-11-09-usage-and-events-design.md`) uygulama planı (`writing-plans`).

## En son kayıt — seçici yönlendirme Task 3–8 tamamlandı (9 Kasım 2026)

- Plan `docs/superpowers/plans/2026-10-22-selective-routing.md` main üzerinde inline (alt ajan/worktree yok) yürütülüyor; ayrıntılı karar defteri `.superpowers/sdd/2026-10-22-selective-routing/progress.md` (yerel). Task 1–8 tamam, ledger'da `Task N: complete` satırları var:
  - `7a7c636` Task 3: journal V3, cleanup-only V2; boş V2 journal canlı kaynakları sahiplenmez.
  - `0b5164a` Task 4: `KernelPlan.unmatched_traffic`; PreserveSystem'de eşleşmeyen trafik işaretsiz/NAT'sız OS'ye kalır, foreign mark yalnız açık Route/Direct kapsamında reddedilir.
  - `9284a96` Task 5: seçici guard yalnız active∪pending açık kural kapsamı + sahip akışlar; aktivasyon fazında guard değiştirilen neslin kapsamını da tutar (`render_activated_nft_document`, journal V3 activated fingerprint aynı renderer).
  - `dceda46` Task 6: IPC/runtime protocol 3, helper `GetRuntimeProtocol` ön kontrolü (kayıt/cleanup/orphan öncesi), `RuntimeStatus.unmatched_traffic` helper kaydından; daemon plan moduyla eşleştirir.
  - `560c843` Task 7: salt okunur `PreviewProfile`/`routing_preview` (bağlı ağ çakışması, Any/Any uyarısı); helper bus sınırında taze topology; CLI `--acknowledge-routing-impact`, onaysız çıkış kodu 6, önizleme yoksa her zaman ret.
  - `c8da514` Task 8: GTK `routing-scope` seçici, önizleme→etki onayı (draft+epoch bağlı, tek kullanımlık), Genel Bakış kayıtlı/uygulanan kapsam ayrı, Direct = “Ana hattı kullan”. TR/EN ekran görüntüleri `.superpowers/sdd/2026-10-22-selective-routing/screens/`.
- Doğrulama (9 Kasım, `c8da514`): `cargo test --all-targets --no-fail-fast` → 528 passed, 1 failed (önceden bilinen `ui::desktop_catalog::tests::sandbox_launcher_is_not_selectable_as_a_host_application`, Snap symlink), 2 ignored; fmt ve clippy (default + root-integration) temiz; izole GTK smoke (`scripts/run-gtk-smoke.sh`) 1 passed. Suite tam yeşil değil.
- NOT VERIFIED: gerçek kernel/nft trafiği, helper'ın gerçek topology yakalaması (`inspect_with_observed_networks`), gerçek system-bus/Polkit, Debian/Fedora/aarch64. Onaylı tek kullanımlık VM yok (`multipass list`: yalnız kullanıcının central/master/slave/pits VM'leri; dokunulmadı).
- Push yapılmadı; main origin'in önünde. Host kurulum/servis/profil/canlı ağ değişmedi.

### Sonraki adım

Task 9 (gerçek kernel/lab/yükseltme kabulü) yeni bir `routelane-test-*` tek kullanımlık VM gerektirir; **kullanıcının açık onayı olmadan VM oluşturma**. Onay gelirse `tests/root/selective_routing.rs` + `docs/selective-routing-verification.md`; gelmezse compile-only + NOT VERIFIED kaydı. Ardından Task 10 (README, paket contract, `.deb` içerik kontrolü, handoff) ve son bütün-dal incelemesi. Sonra onaylı kullanım/olay tasarımının (`docs/superpowers/specs/2026-11-09-usage-and-events-design.md`) uygulama planı.

## En son kayıt — seçici yönlendirme uygulama planı (22 Ekim 2026)

- Kullanıcı son “Devam et” ile yazılı seçici yönlendirme tasarımını onayladı. `writing-plans` kullanılarak `docs/superpowers/plans/2026-10-22-selective-routing.md` yazıldı; tasarımın durum satırı onaylandı olarak güncellendi. Yalnız bu iki belge `d09124d` (`docs(routing): plan selective routing implementation`) commit'ine alındı. Önceki tasarım commit'i `8e96baf` korunuyor.
- **Plan kullanıcı incelemesini bekliyor; Task 1 dahil hiçbir uygulama görevi başlamadı.** Kullanıcının bir sonraki onayı planı kapsıyorsa Task 1'den başlanabilir; yeniden tasarım/plan döngüsü başlatma. Korunan yürütme seçimi main/inline, alt ajan ve yeni worktree yoktur.
- 10 görev: frozen V2 journal/V3 profil kanıtı → zorunlu mod/ConfigV4 → cleanup-only yükseltme → seçici nft/planner → scoped transaction guard → protocol3/grant/applied mode → root-observed lab önizlemesi/CLI onayı → GTK → gerçek kernel/VM kabulü → tam regresyon/paket/devir. Task 9'un trafik kanıtı olmadan düzeldi veya kurulum hazır deme.
- Plan öz incelemesi: spec §1–10 görevlerle eşleştirildi, beş Review Focus durumu ilgili testlere bağlandı; kesin tip/API adları, literal davranış assertion'ları ve RED/GREEN komutları yazıldı. `git diff --check` ve staged dokümantasyon kontrolü geçti. Bu tur yalnız dokümantasyon: uygulama testleri/derleme/VM testi çalıştırılmadı.
- Önceden kirli kaynak/Cargo/README/CIDR/nesil değişiklikleri korundu ve commit edilmedi. `docs/handoff.md` önceki kullanıcı tercihiyle kalıcı kayıt olarak güncellendi, bu commit'e alınmadı; AGENTS.md değişmedi. Host servis/paket/profil/kernel/lab VM'lerine dokunulmadı; canlı lab erişimi hâlâ doğrulanmış değil.

### Sonraki adım ve Suggested Skills

`rtk git status --short --branch` ve `rtk proxy sed -n '1,280p' docs/superpowers/plans/2026-10-22-selective-routing.md` ile başla. Plan onayından sonra `executing-plans`, `test-driven-development` ve gerektiğinde `implement`; önce tam başlangıç test koşusu ve dirty baseline ayrımı, sonra frozen kanıt/Task 1. Sonuçlar için `verification-before-completion`, kalıcı kayıt için `handoff`. Plan yürütme onayı canlı RouteLane'i kapatma/yeniden açma, host kurulumu veya mevcut lab kuralını düzenleme izni değildir.

## En son kayıt — seçici yönlendirme tasarımı (22 Ekim 2026)

- Kullanıcı eşleşmeyen trafiğin OS davranışını korumasını istedi ve açıklanan hedefe “devam et” dedi. Bu, 14 Ekim'deki tüm eşleşmeyen internet trafiğini ana hatta taşıma varsayılanını değiştirir; önceki özellik ayrı opt-in olarak korunacak.
- Mimari tasarım becerisi kullanıldı. Yazılı tasarım: `docs/superpowers/specs/2026-10-22-selective-routing-design.md`; yalnız bu belge commit edildi: `8e96baf` (`docs(routing): define selective routing and system preservation`). **Yazılı tasarım kullanıcı incelemesini bekliyor; uygulama planı ve ürün uygulaması henüz yok.** Genel “devam et” onayını henüz var olmayan planın onayı sayma.
- Tasarım: `PreserveSystem` varsayılan, `UsePrimary` açık onaylı seçenek; mode planner/hash/runtime grant'e dahil. Global foreign-mark reject ve seçici transaction guard kapsamı daralır; açık Block/Route önceliği, owned-flow NAT/affinity ve dar yetki korunur. Config/desired schema 4, IPC/runtime 3, journal 3 öneriliyor; eski V2 plan/hash dondurulmuş decoder ile yalnız exact cleanup, modern activation öncesi temizlik ve yeniden yetkilendirme.
- `Direct` ana hattır; gerçek OS-bypass diye sunulmayacak. Etkin `10.65.201.0/24 → Ethernet` varsa bu açık bir eşleşmedir: sessiz silme/bypass değil, çakışma önizlemesi ve kullanıcının düzenleme/devre dışı bırakma/açık onayı gerekir. Eski mark/NAT uygulanmış socket'lerin kesintisiz taşınacağı iddia edilmez; yeni bağlantı ile doğrulama şart.
- Bu turda yalnız dokümantasyon yazıldı; uygulama testleri yeniden çalıştırılmadı. `git diff --check` ve yeni belgenin staged diff kontrolü geçti; kaynak incelemesi global output fallback, foreign reject, global guard ve exact rollback riskini doğruladı. Önceki 451 passed/1 failed/2 ignored ve diğer doğrulamalar tarihsel, bu tasarımın uygulandığına kanıt değil.
- Önceki CIDR/nesil düzeltmeleri ile başlangıçta kirli bulunan `Cargo.toml`/`Cargo.lock` korundu; bu commit'e alınmadı. AGENTS.md değiştirilmedi, alt ajan/worktree yok. Profil, kurulu paket, servis, VM ve canlı kernel ağına dokunulmadı; kullanıcının lab erişimi düzeltilmiş sayılmıyor.

### Sonraki adım ve Suggested Skills

Önce `rtk proxy sed -n '1,180p' docs/superpowers/specs/2026-10-22-selective-routing-design.md` ve `rtk git status --short --branch`. Yazılı tasarım için kullanıcı yanıtını al; onaydan sonra `writing-plans` ile ayrı uygulama planını yaz ve yürütme öncesi plan onayını tamamla. Ardından `test-driven-development` / `implement`, en sonda `verification-before-completion`; devir kaydı `handoff` ile yine bu dosyaya. Canlı lab için RouteLane'i devre dışı bırakma/manuel RPDB istisnası ayrıca konuşuldu fakat kullanıcı bu host mutasyonu seçeneklerinden birini onaylamadı.

## En son kayıt — kapatma sonrası nesil uyuşmazlığı (22 Ekim 2026)

- Kullanıcı `ActivateRuntime: runtime activation expected generation 2 but 0 is applied` hatasını bildirdi ve dar düzeltmeyi onayladı. Salt okunur gözlemde daemon generation=2/applied_profile=null, helper plan=null/runtime generation=0/Inactive ve RouteLane nft tablosu yoktu.
- Neden: `DaemonService::deactivate` başarılı cleanup sonrasında planı siliyor ama `state.generation` değerini koruyordu. Artık yalnız cleanup başarılı olduktan sonra nesil 0 olur; başarısız cleanup uygulanmış nesli ve planı korur. Helper'ın generation/CAS, Polkit ve sahiplik kontrolleri değiştirilmedi; otomatik hata tekrar denemesi eklenmedi.
- `src/ui/session.rs`: aynı bağlantı epoch'unda önce daemon'un monoton `event_sequence` değeriyle sırala; daha yeni sıra numarası cleanup sonrası daha düşük helper neslini taşıyabilir. Düşük sıra numaralı eski güncellemeler ve aynı sıra numarasındaki düşük nesiller reddedilir. Böylece başarılı yeniden aktivasyon arayüzü beklemede bırakmaz.
- Regresyonlar: `tests/primary_daemon_contract.rs` nesil 2 → cleanup → 0 → activate → 1 döngüsünü iki kez doğrular; gecikmiş eski helper bildirimi kapatılan runtime'ı canlandıramaz. UI testi nesil sıfırlanmasını/yeni aktivasyonu kabul eder ve eski yüksek nesilli olayı reddeder. `tests/ipc_contract.rs` başarısız cleanup'ta neslin korunmasını, başarılı cleanup'ta sıfırlanmasını doğrular.
- RED gözlendi: yeni daemon testleri `stale expected generation` ve 2≠0; yeni UI testi 8≠0 hatası verdi. Düzeltmeden sonra IPC + primary daemon **47/47 PASS**, UI session **18/18 PASS**.
- Güncel tam tur: `rtk cargo test --all-targets --no-fail-fast` **451 passed, 1 failed, 2 ignored**, exit 101. Tek hata bu turdan önce yeniden üretilen `ui::desktop_catalog::tests::sandbox_launcher_is_not_selectable_as_a_host_application`; bağımsız Snap/symlink sorunu çözülmedi. Tam suite GREEN değil.
- `rtk cargo clippy --all-targets -- -D warnings`, `rtk proxy cargo fmt --all -- --check`, `rtk git diff --check`, `rtk cargo build --release --bins`: PASS. `rtk proxy bash scripts/run-gtk-smoke.sh`: izole GTK oturumunda **1/1 PASS**. Gerçek kernel/Polkit/VM testi bu turda çalıştırılmadı.
- Kurulu binary/paket/servisler, kullanıcı yapılandırması ve canlı host ağı değiştirilmedi. Önceki CIDR/mükerrer değişiklikler korundu; yeni düzeltme de commit edilmedi. Yeni worktree/alt ajan kullanılmadı; inceleme aynı ajan tarafından yapıldı.

### Kalan iş ve sonraki adım

Multipass yerel köprü trafiğini lane yönlendirmesinden koruma işi daha önce onaylandı fakat **henüz uygulanmadı**. Önceki inceleme: `10.65.201.0/24` için açık lane kuralı yerel `mpqemubr0` rotasının önüne geçiyor; grant-filtered topology köprünün connected prefix'ini plandan çıkarıyor. Direct, OS-yerel bypass değildir. Çözüm açık Block kurallarını, dar exit grant'ini ve eski journal'ın exact rollback/cleanup uyumunu korumalı; yalnız RFC1918 geneli için koşulsuz bypass ekleme. Mevcut kullanıcı lab VM'lerini test ortamı sanma ve canlı ağı yeniden etkinleştirme.

Önce `rtk git diff -- src/daemon/service.rs src/ui/session.rs tests/primary_daemon_contract.rs tests/ipc_contract.rs` ile bu düzeltmeyi ayır; hızlı doğrulama `rtk cargo test --test primary_daemon_contract --test ipc_contract`. Paket kurulumu veya canlı servis/ağ işlemini ayrıca kapsamla eşleştir; kaynak düzeltmesi kurulu sürümün güncellendiği anlamına gelmez.

### Suggested Skills

Yerel köprü sorunu için `systematic-debugging`, `test-driven-development`; tamamlanma kanıtı için `verification-before-completion`; sonraki kalıcı devir kaydı için `handoff`. Kullanıcının repo içindeki kalıcı kayıt tercihi `/tmp` varsayımından üstündür.

## En son kayıt — CIDR ve mükerrer kurallar (22 Ekim 2026)

- Kullanıcı `10.65.201.0/24` girişi ve uygulama/kernel tarafında mükerrer kayıtların engellenmesini istedi; kaynak + normalize hedef + eylem/lane tanımını onayladı. Ticari analiz talebinin yerine bu uygulama işi geçti.
- Başlangıç: temiz `main`, HEAD `ae518c8` (`add handoff.md`), origin ile aynı. Bu turdaki değişiklikler commit edilmedi; alt ajan/worktree kullanılmadı. AGENTS.md değiştirilmedi. Bu dosya artık Git takibinde; aşağıdaki eski ignore uyarısı tarihsel.
- CIDR zaten destekleniyordu; artık editor ve planner host bitlerini sıfırlıyor (`10.65.201.25/24` → `10.65.201.0/24`). IP ile `/32` veya `/128` karşılığı, domain case/son nokta farkı mükerrerlik kontrolünde eşdeğer. ID/öncelik/aktiflik farkı yeni kural sayılmaz. Farklı kaynak, eylem/lane veya yalnız örtüşen farklı ağlar korunuyor; düzenleme kendisini dışlıyor.
- `src/domain/{rule,profile}.rs`: tipli eşdeğerlik anahtarı ve ortak profil doğrulaması. Uygulama seçiminde executable/include_children esas; launcher/desktop alias metadata'sı aynı süreç kuralını ikinci kez tanımlayamaz. `src/ui/rule_editor.rs` ve `i18n.rs`: kaydetmeden önce TR/EN uyarı. Config ve CLI aynı domain doğrulamasını kullanıyor.
- `src/backend/{plan,planner}.rs`: DNS alias'ları aynı aile/hedef/eyleme derlenirse ilk eşleşmeyi koruyarak tek verdict üret; kayıtlı mantıksal kuralları silme. `privileged.rs`: yalnız yeni ApplyPlan/backend.apply girişlerinde mükerrer derlenmiş planı kernel komutundan önce reddet. Eski journal decoder'ına daha katı kontrol eklenmedi; eski mükerrer içeren committed journal'ın exact cleanup regresyonu geçti.
- Eski JSON'daki mantıksal mükerrerler yükleme/import sırasında reddedilir, dosya sessizce düzeltilmez/silinmez; hata verilen kayıtlar kullanıcı tarafından düzenlenmeli. Yabancı nftables/iptables/routing kaynaklarına dokunulmadı; yeni iptables backend'i eklenmedi.

### Bu turdaki doğrulama

- RED: yeni domain testlerinde iki, editor testlerinde üç, planner testlerinde iki, privileged duplicate-plan testlerinde iki davranış hatası görüldü; ardından uygulama yapıldı. Yeni tek başına eski-journal cleanup fixture'ında fazladan metainfo placeholder hatası düzeltildi; üretim sahiplik kontrolü gevşetilmedi.
- `rtk cargo test --all-targets --no-fail-fast`: **448 passed, 1 failed, 2 ignored**, exit 101. Tek hata başlangıçta da bağımsız yeniden üretilen `ui::desktop_catalog::tests::sandbox_launcher_is_not_selectable_as_a_host_application`; `/usr/bin/snap` bu makinede `../lib/snapd/snapd` symlink'i olduğundan mevcut basename kontrolü `sandbox` döndürmüyor. Bu ayrı hata kapsam dışı bırakıldı; tam suite GREEN değil.
- `rtk cargo test --test duplicate_rule_contract`: **4/4 PASS**. Config no-write/import, CLI daemon'a hiç bağlanmama, planner/nft verdict tekilleştirme ve tekrarlanan apply'da NFT/route/rule büyümemesi regresyonları tam turda geçti.
- `rtk proxy bash scripts/run-gtk-smoke.sh`: **1/1 PASS**; gerçek diyalog mükerrerde açık kalıyor/uyarı veriyor/save çağırmıyor; hedef düzeltilince canonical CIDR kaydediliyor. Özel Broadway/session bus; kullanıcının masaüstü/config'i kullanılmadı.
- `rtk cargo clippy --all-targets -- -D warnings`, `rtk proxy cargo fmt --all -- --check`, `rtk git diff --check`: PASS.
- `rtk cargo test --test root --features root-integration --no-run`: derlendi. Gerçek kernel/VM testleri bu turda **çalıştırılmadı**; host ağ/servis/paket kurulumu değiştirilmedi. Önceki dağıtım yayın kapıları hâlâ ayrı kanıt gerektirir.

### Devam noktası

Önce `rtk git status --short --branch` ve bu turdaki diff'i oku. Ayrı Snap tespiti sorununa geçilecekse başlangıç komutu:

```bash
rtk cargo test --bin routelane ui::desktop_catalog::tests::sandbox_launcher_is_not_selectable_as_a_host_application -- --exact
```

Bu yeni davranışın kernel kabulü gerekirse yalnız yetkilendirilmiş disposable VM'de çalıştır; canlı host üzerinde genel kural temizliği yapma. Geçmiş failover doğrulaması için `docs/primary-failover-verification.md` geçerli kaynak olmaya devam ediyor.

## En son kayıt — hafıza güncellemesi (15 Ekim 2026)

- Kullanıcının son isteği son yapılanların hatırlanmasıdır; bu turda yalnız bu belge güncellendi. Yeni kod, paket kurulumu, VM işlemi veya canlı ağ değişikliği yapılmadı.
- Başlangıç HEAD'i `6d0bf080a50c9823a335557127835d0c8b489d35` (`add AGENTS, CLAUDE`). `rtk git status --short --branch` çıktısında `main...origin/main` ve temiz çalışma ağacı görüldü. Aşağıdaki oturum 3'teki kirli baseline bilgisi artık güncel değildir: kalan değişiklikler bu commit'e alınmış.
- 12 görevin uygulanmış olması bütün yayın kapılarının geçtiği anlamına gelmez. Güncel kabul kaynağı `docs/primary-failover-verification.md` dosyasıdır. Ubuntu 22.04/24.04 kayıtlarında iki root modunda temiz 18/18 var; Ubuntu 26.04'te sayaç/verim eşiği kaynaklı aralıklı başarısızlıklar var, bu satır temiz PASS değil.
- Son runtime düzeltmesi `6a0994f` yalnız rootless testlerle doğrulanmış. Eski VM sonuçları veya 433 testlik tarihsel kayıt, güncel HEAD'in yeniden doğrulandığı şeklinde sunulmamalı. Bu hafıza güncellemesinde testler yeniden çalıştırılmadı.
- Debian 12/13, Fedora 43/44 gerçek kurulum/kernel/Polkit, tüm aarch64 satırları, etkileşimli Polkit ajanı, dağıtımlarda GTK açılışı, firewalld/SELinux ve CI matrisi hâlâ NOT VERIFIED. Fedora container'ında RPM üretmek bunların yerine geçmez.
- Devralan kişi önce bu bölümü ve doğrulama kaydını okumalı. Eski Task 6'dan tekrar başlamamalı; §1–9 tarihsel oturum 1 notlarıdır. Sıradaki teknik iş, yeni kullanıcı talebi doğrultusunda Ubuntu 26.04 aralıklı testlerini teşhis etmek veya eksik dağıtım/architecture kabul altyapısını hazırlamaktır; yeni VM/host yetkilerini önce kapsamla eşleştir.
- Devir kaydını güncellerken her komutu yığmak yerine önemli kararları, commit'i, gerçekten çalıştırılmış doğrulamaları, açık riskleri ve bir sonraki dar adımı kaydet. AGENTS.md bu turda değiştirilmedi.
- Saklama sınırı: `.gitignore` içindeki `docs/*` kuralı bu dosyayı yok sayıyor; `git ls-files -- docs/handoff.md` boş. Kayıt bu çalışma dizininde kalıcıdır fakat yeni klona otomatik gelmez. Bu turda ignore kuralı değiştirilmedi, dosya stage/commit edilmedi.

Güncel duruma dönmek için depo kökünde ilk komutlar:

```bash
rtk git status --short --branch
rtk git log -8 --oneline
rtk proxy sed -n '1,240p' docs/primary-failover-verification.md
```

Bu bölümün altındaki eski oturum durumları, bu güncel kayıtla çeliştiklerinde tarihsel bağlam olarak değerlendirilmelidir.

## 0. Güncel durum (oturum 3, 15 Ekim 2026 — önce bunu oku)

- **Planın 12 görevi uygulandı** (main, inline). Task 6–10 commit'leri: `0ea197b`, `d602642`, `8b8c6bd`, `490ad13`, `68e17f5`, `725e48e`, `500df8b`, `3007a1c`.
- **Task 11:** `547ced5` (carrier kaybında `linkdown` route'larla failover), `be6faa4` (kernel kabulü + gerçek system-bus/Polkit betiği `tests/root/system_bus_acceptance.sh`, `docs/primary-failover-verification.md`).
- **Task 12:** `65dd39f` (kullanıcının paketleme güvenilirlik hunk'ları, kullanıcı onayıyla ayrı commit), `3ed607e` (nft 1.0.2/1.1.6 dump uyumu), `9546a18` (rpm, paket sözleşmesi, `polkitd | policykit-1`, Ubuntu matrisi).
- **Son inceleme (self-review, alt ajan yok):** `d1583de` başarısız geçişin kendi rollback bildirimleriyle sonsuz tekrarını ve tutulan journal’ın sürekli replay’ini üstel beklemeyle durdurdu; `6a0994f` tekrar başarısız kurtarmanın Degraded durumunu ve sonraki deneme zamanlayıcısını kaybetmemesini sağladı. 26.04’te sayaç tabanlı iki testte aralıklı hata var (doğrulama belgesi).
- **Kanıt:** Ubuntu 22.04/24.04/26.04 (x86_64) root süiti iki modda 18/18, system bus + silahlı yükseltme, reboot ve kaldırma PASS. Fedora rpm yalnız container'da üretildi.
- **Yayın kapısı AÇIK DEĞİL:** Debian 12/13, Fedora 43/44, aarch64, etkileşimli Polkit ajanı, GTK'nın dağıtımlarda başlatılması, firewalld/SELinux, CI matrisi NOT VERIFIED.
- **VM'ler:** onaylı test VM'i `routelane-test-u2404-20261014` duruyor; `routelane-test-u2204-20261015` ve `routelane-test-u2604-20261015` bu oturumda kullanıcı onayıyla açıldı ve iş bitince silindi.
- **Kirli ağaç:** kullanıcının kalan hunk'ları unstaged (`.gitignore`, `README.md` (+ schema v3 düzenlemesi), `src/application/process.rs`, `src/backend/inspect.rs`, `src/backend/privileged.rs` baseline hunk'ları, `src/daemon/service.rs`, `tests/ipc_contract.rs`, `docs/reliability-audit.md`). VM koşuları bu hunk'lar dahil çalışma ağacından yapıldı.
- **Sıradaki dar adım:** Debian/Fedora/aarch64 için hipervizör/imaj; kalan kullanıcı hunk'larının kendi commit'i; ertelenen küçükler `progress.md` içinde `Final: minor (deferred)`.
- Aşağıdaki §1–9 oturum 1 sonundaki durumu anlatır; "yarım/bekliyor" ifadeleri artık geçerli değildir.

## 1. Önce bunu bil

**Ana bağlantı/failover özelliği henüz tamamlanmadı.** Onaylı 12 görevden 1–5 commit edildi; 6. görev çalışma ağacında devam ediyor; 7–12 bekliyor. Ara derlemeleri tamamlanmış ürün gibi kurma, yayınlama veya kullanıcının canlı ağına uygulama.

Son kullanıcı talebi geliştirmeyi sürdürmek değil, `AGENTS.md` ve bu devir belgesini oluşturmaktır. Bu turda kaynak koduna dokunulmadı. Devam etmeden önce güncel kullanıcı talebini doğrula.

Çalışma dizini: RouteLane deposunun kökü. Dal: `main`; bu belgenin hazırlanmasında `origin/main` karşısında 11 commit ilerideydi. HEAD:

```text
9fe6e0162eab89feb865938b9e2babdde48dc517
```

Kullanıcı önceki uygulamada **main üzerinde, inline; yeni worktree ve alt ajan olmadan** ilerlemeyi seçti. Bağımsız inceleme yapılmış gibi raporlama. Multipass'ta ayrı test VM'i kullanımı onaylı; host kurulumu/canlı ağ mutasyonu için bu onayı genelleme.

## 2. Nereden okunmalı?

Depo köküne geçtikten sonra sırayla:

1. `AGENTS.md`: kısa katkı, komut ve güvenlik kuralları.
2. `docs/superpowers/specs/2026-10-14-primary-connection-failover-design.md`: onaylı gereksinimler ve güvenlik sınırı.
3. `docs/superpowers/plans/2026-10-14-primary-connection-failover.md`: görevler, kabul testleri ve bağımlılıklar. Sıradaki başlık **Task 6**.
4. `.superpowers/sdd/2026-10-14-primary-connection-failover/progress.md`: en ayrıntılı yerel karar/test günlüğü. **Git tarafından yok sayılıyor; yeni klonda gelmeyebilir.**
5. `docs/research/primary-failover-kernel.md`: kernel/nft ABI araştırması ve kanıtlar.
6. `README.md`, `docs/reliability-audit.md`, `docs/ui-ux-verification.md`: ürün/mimari ve önceki doğrulamalar. Tarihsel sonuçlar güncel kabul kanıtı değildir.
7. `docs/commercial-roadmap.md`: ticari/açık kaynak kapsamı; teknik görevlere alternatif spesifikasyon değildir.

Yerel çalışma alanındaki `baseline.patch`, `task-*-brief.md`, `task-*-tests.log`, `task*-feature.patch` dosyaları geçmişi yeniden kurmaya yardımcıdır. Bunlar da klona taşınmayabilir. Plan kutucukları tek başına güncel durum kaynağı değildir; commit, diff ve test günlüğüyle karşılaştır.

## 3. Ürün amacı ve sabit kararlar

- Başlangıçta işletim sisteminin varsayılan bağlantısını ana hat kabul et; kullanıcı başka ana hat seçerse önce uyarı/onay al.
- Örnek: Ethernet ana, VINN yardımcı; kurallı siteler/uygulamalar VINN'den, diğer yeni internet bağlantıları ana hattan çıksın.
- Ana hat kesilince yalnızca kullanıcı tarafından onaylanmış yedeklere geç. Ana hat geri geldiğinde 5 saniyelik kararlılık süresini uygula; mevcut sahiplenilmiş yedek bağlantıları zorla koparma, yeni akışları ana hatta yönelt.
- IPv4/IPv6 kararları ayrı. Uygun çıkış yoksa fail-closed; OS default'una sessiz kaçış olmasın.
- Route/Direct/Block ve uygulama bağlılığını koru. V3 migrasyonu salt okunur yüklemede ağı etkinleştirmesin; ilk aktivasyonda semantik değişikliği açıkla.
- OS ana routing tablosunu, DNS'i, yabancı firewall/policy kurallarını ve global sysctl değerlerini yeniden yazma. Global NAT, conntrack flush, IPv6 kapatma veya proxy ile testleri geçiştirme yok.
- Onay, bilinmeyen kaynağın silinmesine izin vermez. Root helper sadece doğrulanmış sahiplik ve dar, tipli sözleşmelerle mutasyon yapar. `Inspect` salt okunur kalır.
- İlk resmi sürümden Ubuntu, Debian, Fedora destek kapsamında; tek Ubuntu testi veya `.deb` üretimi bu taahhüdü doğrulamaz.

Domain kuralları şu anda uygulama anındaki sistem resolver IP snapshot'una dayanır; sürekli DNS/TTL/CDN keşfi yok. DoH/farklı DNS cevapları kaçırılabilir. Desteklenen host uygulamasının süreç ağacını aynı lane'e bağlamak yeni üçüncü taraf bağlantılarını kapsar; aynı browser sürecinin başka sekmelerini de etkileyebilir. Snap/Flatpak bağlılığı ve mevcut socket'leri sonradan taşıma garantisi yok. Bağlantı Sağlığı yerel plana uygunluğu gösterir, sitenin açıldığını kanıtlamaz.

## 4. Tamamlanan commit'ler

| Commit | Teslim edilen parça |
| --- | --- |
| `00e071f` | Ana bağlantı/failover tasarımı |
| `54102dd` | 12 adımlı uygulama planı |
| `57471c3` | Task 1: ConfigV3, routing policy, eski kayıt migrasyonu |
| `254d69a` | Task 2: gözlenen topology, sistem default'u/owned route ayrımı |
| `a2415b6` | Task 3: deterministik aile bazlı failover/failback seçici |
| `af1bf23` | Task 4: explicit primary/source/identity planner modeli |
| `59f3886` | Kernel-canonical nft reject ifadeleri; exact ownership düzeltmesi |
| `9fe6e01` | Task 5: ortak nft JSON/text semantiği, akış bağlılığı, dar kaynak düzeltmesi |

Task 5 öncesi kernel kanıtı: A lane'i doğru mark alırken ARP isteği B'nin `10.10.2.2` kaynağıyla çıkıyordu; cevap yoktu. Düzeltilmiş disposable VM testinde A kaynağı `10.10.1.2` oldu ve cevap alındı. Mark-only route relookup kaynak adresi tek başına düzeltmiyor.

Ortak nft belgesi yerel akış/yerel orijinal kaynak kanıtı, conntrack bağlılığı, kontrol/LAN istisnaları ve scoped egress koruması içerir. `rules_v2_<hash>` zinciri tutarlılık mührüdür; kriptografik yetki imzası değildir. Mühür geçici guard'ı dışlar; pending/guarded journal fingerprint'leri guard'ın tamamını doğrular. Journal kaybolmuşken guard bulunan modern tabloyu otomatik temizlemek şu an güvenli kabul edilmiyor.

## 5. Çalışma ağacı: kullanıcı değişikliklerini koru

Bu belge öncesi index boştu; **hiçbir dosya staged değildi**. Aşağıdaki eski değişiklikler bu failover çalışmasından önce vardı; kullanıcıya ait kabul et:

```text
.gitignore
README.md
data/routelane-helper.service
packaging/deb/build-deb.sh
src/application/process.rs
src/backend/inspect.rs
src/backend/privileged.rs
src/daemon/service.rs
tests/ipc_contract.rs
```

Önceden untracked: `data/routelane-tmpfiles.conf`, `docs/reliability-audit.md`, `packaging/deb/postinst`.

**`src/backend/privileged.rs` hem eski değişiklik hem yeni Task 6 hunks içerir.** Dosyayı topluca commit etme, baseline'a restore etme veya başka dosyadan kopyalama. `baseline.patch` ve Git diff ile hunk bazında ayır. `git add .`, hard reset/clean, tüm dosyayı checkout etme yok.

Yeni Task 6 değişiklikleri:

- `src/backend/interface_settings.rs` (untracked): `InterfaceSettingReceipt`, `MarkAwareRpf`, fixed-purpose backend/system adapter. Yalnız strict effective RPF gerekiyorsa arayüz `src_valid_mark=1`; intent journal callback'i başarılı olmadan yazma yok. Aynı tam cihaz kimliği ve beklenen değerle compare-and-restore; all/default/diğer tunable yazımı yok. **Üretim transaction/journal akışına henüz bağlı değil.**
- `tests/primary_transaction_contract.rs` (untracked): 6 fake-backend scope/lifecycle testi. Bunlar gerçek sysctl yazımı kanıtı değil.
- `src/backend/{nft_document,topology,privileged}.rs`, `tests/nftables_contract.rs`, `tests/fixtures/nftables/three-lane.nft`: değişmez sayısal iif/oif indeksleri, canlı nft dump çözümlemesi. Sembolik çıktıyı eski ad→index ile değil, bounded gerçek Links snapshot'ının kararlı önce/sonra tam kimlik haritasıyla çöz. Sadece indeks tipli operandlar; sayısal-only interface adları belirsiz olduğu için reddediliyor.
- `src/backend/mod.rs`: yeni modül export'u.
- `tests/root/namespace_routing.rs`: `ROUTELANE_TEST_STRICT_RPF=1` sadece testin sahip olduğu client namespace'i strict yapar.
- `src/backend/privileged.rs`: `FakeAppliedStateStore.fail_commit_generation` ve `pending_commit_failure_restores_exact_previous_state`; committed journal save hatasında önceki journal/NFT/plan geri geliyor mu kontrolü.
- `docs/research/primary-failover-kernel.md` ve ignored ilerleme günlüğü: yeni ABI kanıtları/kararlar.

Bu dokümantasyon turunda ayrıca yeni `AGENTS.md` oluşturuldu. Handoff `/tmp` altındadır, Git diff'e girmez.

## 6. Doğrulama: geçmiş ile bugün ayrımı

| Kanıt | Sonuç ve sınırı |
| --- | --- |
| Task 5 son rootless turu | 360 passed, 2 açıkça ignored; fmt/clippy temizdi |
| Task 6 son önceki full rootless turu | Oturum kaydına göre 370 passed, 2 ignored; son rollback testi eklenmeden önce. Bu belge turunda full suite tekrar çalıştırılmadı |
| **Bu tur:** `rtk cargo test --lib pending_commit_failure_restores_exact_previous_state` | **1 passed**, 143 filtered out, exit 0 |
| **Bu tur:** `rtk proxy cargo fmt --all -- --check` | **FAIL**, exit 1; `privileged.rs` yeni fake-store/test bloğu biçimlendirilmemiş. Kod değiştirilmedi |
| Task 5 gerçek helper namespace suite | Ayrı Ubuntu VM'de 6/6 passed; TCP/UDP/IPv6/failback kapsamının tamamı değil |
| Task 6 strict-RPF ek fixture | **5 passed / 1 failed**; A-lane ilk yanıtta %100 kayıp. Journal/settings bağlantısı yapılmadan GREEN değil |
| Gerçek nft 1.0.9 JSON/text gate | Her iki biçimde EXACT 86 obje; eski ifindex2 silinip aynı ad ifindex9 olunca eski literal indeks değişmiyor |
| `root-integration --no-run` | Derleme geçmişte başarılı; kernel testi veya Polkit geçişi değil |
| Gerçek system-bus/Polkit ve Debian/Fedora | **Henüz doğrulanmadı** |

Sayısal-index hardening sonrası yeni helper/root test ikilisiyle standart gerçek root suite yeniden koşulmalı. Önceki 6/6'yı güncel dirty kodun kapsamlı geçişi diye sunma. Exit77, derleme başarısı veya izin hatası beklenen davranış RED'i yerine geçmez. Son test sayısını yeniden koşmadan artırıp “371 geçti” deme.

## 7. İzole VM ve tekrar üretme notları

Onaylı tek yeni VM: `routelane-test-u2404-20261014`; Ubuntu 24.04.5, kernel 6.8.0-142-generic, nft 1.0.9; 2 vCPU/2 GiB/12 GiB. Repo/home mount veya ek fiziksel bridged interface kullanılmadı. `slave-01`, `master-01`, `master-02`, `central-lab-01` kapsam dışı; dokunma.

Önceki oturum sonunda test namespace listesi boştu; host routing/DNS/sysctl/kurulu servisler değiştirilmedi. Bu dokümantasyon turunda VM durumu yeniden sorgulanmadı.

Ignored çalışma alanında VM scriptleri: `vm-baseline-run.sh`, `vm-capture-baseline.sh`, `vm-nft-roundtrip.py`; fixture'lar: `primary-policy.{json,nft}`, `expected-primary-policy.json`.

Guest çalışma dosyaları `/home/ubuntu/` altında: `routelane-root-baseline`, `routelane-helper-candidate`, dondurulmuş `routelane-helper-baseline`, `vm-baseline-run.sh`, `vm-nft-roundtrip.py`. Scriptin beklediği helper yolu ve CARGO_MANIFEST_DIR kaynak script yolu guest içinde oluşturulmuştu; gerçek yolları runner/kaynak koddan doğrula. Guest'e kopyalanan artifact'ler yeni çalışma ağacından eski olabilir; tarih/hash kontrolü yap.

**Önemli build tuzağı:** `cargo test --all-targets`, `target/debug/routelane-helper` dosyasını root-integration özelliği olmayan binary ile değiştirebilir. Feature'lı helper'ı root tests ile seri olarak yeniden derle ve hemen guest'e aktar; taşınan artifact kimliğini doğrula. Test stdio yolu gerçek system-bus/Polkit yetkilendirme testi değildir; bypass'u üretime taşıma.

Frozen eski nft fixture: `tests/fixtures/nftables/legacy-primary-baseline.nft`, kaynak `254d69a`, SHA256 `f581c1775f73d2742a70bea46a68343a26e5bac0260eaf16db9e753203fbe2a3`.

## 8. Sıradaki uygulama adımları ve kabul sınırı

1. Yeni talep uygulamaya devam etmekse önce dirty diff'i/Task 6'yı oku, son test bloğunu rustfmt ile biçimlendir; baseline'ı koruduğunu kontrol et. Aşağıdaki odaklı testleri tekrar çalıştır.
2. Journal'ı explicit version'la; gerçek eski V1 `StoredAppliedState`/`KernelPlan` DTO ve legacy doğrulayıcıyı **cleanup-only** dondur. Eski state'e uydurma primary default ekleyip runtime yetkisi verme. Boyut limitlerini kontrollü güncelle.
3. Setting receipts'i durable pending/commit/rollback/cleanup'a bağla. Journal/fsync hatası sıfır ayar yazımı yapmalı; committed save hatası receipt kaybettirmemeli; kimlik/değer değişmişse yabancı ayarı geri yazma. Aynı-değer admin yazımı compare-and-restore ile ayırt edilemez; bu sınırı belgele.
4. Terminal unreachable route/rule komutlarını exact ownership/crash-prefix validator'larıyla **birlikte** bağla; planner builder var diye runtime'ı tamamlanmış sayma. Tam kimliği mutasyondan hemen önce yeniden doğrula.
5. Servis `ProtectKernelTunables=yes` kalsın; gerekiyorsa yalnız belgelenmiş arayüz src_valid_mark yazımını sağlayan dar exception. Gerçek systemd sandbox doğrulaması ayrıca gerekir.
6. Task 6 unit/contracts + standart/strict izole helper testleri GREEN olmadan Task 7'yi geçmiş veya Task 8 otomasyonunu etkin sayma.
7. Task 7: gerçek TCP, UDP request/reply, IPv6/no-family escape, strict/loose RPF, uzun yedek oturumunun failback sonrası devamı, source/address kaybı, yerel kontrol ve foreign sentinel cleanup kanıtları.
8. Task 8–10: bounded root runtime/olaylar ve grant; IPC/restart correlation; GTK primary/backup seçimi, onay ve gerçek effective durum. Helper planını root topology'den üret; daemon preview'ını etkin gerçek sanma.
9. Task 11–12: fault/restart/system-bus Polkit ve Ubuntu/Debian/Fedora install/upgrade/remove/sandbox/kernel/GTK kabul matrisi. Ürün ancak bütün release kapılarıyla tamamlanır.

### Açık güvenlik riskleri

- Link kaybında kernel owned route'u silebilir. Auto refresh yalnız **bilinen eksik route + taze loss/identity kanıtı + değişmemiş tam NFT + ek/değişmiş reserved kaynak yok** ise bunu açıklayabilir. Keyfi subset kabulü veya hazır lane'deki admin route silinmesini link kaybı sayma yok.
- Manual lane-ID değişiklikleri slot→mark eşleşmesini değiştirebilir; eski conntrack/NAT kaynağı yeni lane'den kaçabilir. Gerçek kernel reproducer ve gerekirse dar **post-SNAT source+identity guard** gerekir. Mevcut pre-SNAT guard bunu kanıtlamaz. Global CT flush veya varsayımsal session survival vaat etme.
- Root runtime derleyicisi topology'yi dondurulmuş grant kimlikleriyle filtrelemeli; yalnız `PrimarySelector` kontrolü, planner'ın onaysız yeniden kullanılan cihazı görmesini engellemez.
- Modern guard var/journal yok durumunun otomatik recovery yolu henüz güvenli kanıtlanmadı; adında routelane geçiyor diye temizleme yok.
- Sayısal interface adının nft dump ABI belirsizliğini normalize ederek gizleme; fail-closed kal.

## Suggested Skills

- `superpowers:using-superpowers`: başlangıçta ilgili yönergeleri yükle; kullanıcı kapsamı ve no-agent tercihi öncelikli.
- `matt-skills-curated:implement` veya `superpowers:executing-plans`: onaylı planın Task 6 sınırını uygula.
- `superpowers:test-driven-development`: yeni journal/legacy/rollback davranışında önce gerçek RED assertion.
- `superpowers:systematic-debugging`: strict-RPF ve kernel ABI sorunlarında kanıt→hipotez→dar test.
- `superpowers:verification-before-completion`: geçmiş sonucu yeni başarı diye sunma; gerçek komut çıktısını doğrula.
- `matt-skills-curated:handoff`: oturum sonunda yeni durum ve bir sonraki adımı aktar.

## 9. Tam devam komutları

Depo kökünde **ilk komut**:

```bash
rtk git status --short --branch
```

Sonra yalnız okuma/odaklı rootless kontrol:

```bash
rtk git diff -- src/backend/privileged.rs src/backend/interface_settings.rs
rtk proxy sed -n '236,269p' docs/superpowers/plans/2026-10-14-primary-connection-failover.md
rtk cargo test --test primary_transaction_contract
rtk cargo test --lib pending_commit_failure_restores_exact_previous_state
rtk proxy cargo fmt --all -- --check
```

`git diff` untracked dosyanın içeriğini göstermez; `interface_settings.rs` için ayrıca `rtk proxy sed -n '1,300p' src/backend/interface_settings.rs` kullan. Şu an fmt kontrolünün yukarıdaki test bloğunda başarısız olması beklenir; belge isteği bunu düzeltme yetkisi değildir.

Uygulamaya devam talebi geldikten sonra değişikliği tamamla; `rtk cargo test --test transaction_contract`, `rtk cargo test --lib backend::privileged`, `rtk cargo test --all-targets`, `rtk cargo clippy --all-targets -- -D warnings` ve fmt gate'ini çalıştır. Kernel testlerini yalnız onaylı guest içinde root olarak `bash scripts/run-root-integration.sh` ile veya doğrulanmış prebuilt guest runner üzerinden çalıştır; workstation'da sudo ile başlatma.

Yeni geliştiriciye verilebilecek başlangıç istemi:

> Bu handoff'u, AGENTS.md'yi ve ana bağlantı/failover spec/planını oku. Kullanıcıya ait dirty baseline'ı koru. Özellik tamamlanmış değil: Task 6 journal/legacy cleanup/receipt/terminal ownership entegrasyonundan devam et. Rootless ve gerçek izole kernel kanıtını ayır; host ağına/kurulumuna dokunma. Ana dalda inline ve alt ajansız çalış. Önce durumu ve sıradaki dar RED testini bildir.
