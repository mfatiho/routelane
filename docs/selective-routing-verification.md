# Seçici yönlendirme — doğrulama kaydı

Bu belge planın (`docs/superpowers/plans/2026-10-22-selective-routing.md`)
Task 9 gerçek kernel ve Task 10 system-bus kanıtını tutar. Her satır ya **PASS** (komut, ortam ve
gözlenen çıktı ile), ya **NOT VERIFIED** (altyapı yok ya da çalıştırılmadı)
olarak yazılır. Exit 77, “derlendi” ve rootless test PASS değildir.

## Task 9 — gerçek kernel, lab ve yükseltme kabulü (10 Kasım 2026)

### Ortam

| Alan | Değer |
|---|---|
| VM | Multipass `routelane-test-u2404-20261109`, tek kullanımlık, x86_64, 2 vCPU / 2 GiB / 12 GiB; host dizini bağlı değil |
| Dağıtım / kernel | Ubuntu 24.04.5 LTS, `6.8.0-147-generic` |
| nftables | `v1.0.9` |
| Kaynak | `c8da514` + Task 9 çalışma ağacı (lab namespace, test-only fail pause, `tests/root/selective_routing.rs`) |
| Root test ikilisi | `routelane-root-t9`, sha256 `cc78d1add9261b479fc5c22b13a7c4c86b7ef503adc6fd4154e5671dc4a48951` |
| Helper (root-integration) | sha256 `a5a93571ebad99de26cc5d6f031cf926523814cfd7f011b55e3949c612fa7d4e`; konuk içinde aynı mutlak yola kopyalandı, hash konukta doğrulandı |
| Eski helper (journal v2) | `996c66b` kaynağından (`4c3f174`'ün atası, `JOURNAL_VERSION = 2`) `git archive` + `--features root-integration` derlemesi, sha256 `d1ab49316e3dfc4f61266b0aef9822377927e6daabc2b86b69d8ee7db61d78e4` |

Host üzerinde hiçbir ağ, firewall, sysctl, Polkit ya da paket değişikliği
yapılmadı. Kullanıcının diğer Multipass VM'lerine dokunulmadı.

### Topoloji

Run'a ait namespace'ler: istemci, üç uplink (A = RouteLane ana hattı,
B = işletim sistemi varsayılanı, C = açık kural hattı) ve hiçbir zaman hat
olmayan doğrudan bağlı lab köprüsü `10.65.201.0/24` / `fd00:65::/64`
(istemci `.1`, lab ana makinesi `10.65.201.60` / `fd00:65::60`). Her kontrol
yeni bir TCP bağlantısı ve yeni UDP soketi açar; uç nokta gördüğü kaynak
adresi döndürür. İstemci namespace'indeki yabancı bir nftables gözlemcisi
(postrouting, öncelik 300) işaretli ayrılan paketleri sayar.

### Sonuçlar

| Senaryo | Sonuç | Kanıt |
|---|---|---|
| `selective_lab_and_unmatched_dual_stack_keep_system_exit` | **PASS** | Lab v4/v6 yeni oturumlar kaynak `10.65.201.1` / `fd00:65::1`; eşleşmeyen `198.51.100.10` B üzerinden `10.10.2.2`; açık kural C üzerinden `10.10.3.2`; lab/eşleşmeyen işaretli paket 0, kural paketleri 8 işaretli |
| `explicit_routes_use_both_lanes` | **PASS** | A kuralı, Direct (ana hat A) ve C kuralı kendi hatlarından (TCP+UDP); eşleşmeyen B'den |
| `selective_switch_failure_preserves_lab` | **PASS** | `activate_nft` hata noktasında guard kuruluyken duraklatıldı: lab v4/v6 ve eşleşmeyen trafik aktı, eski (`192.0.2.10`) ve yeni (`203.0.113.11`) yönetilen kapsam tutuldu; hata sonrası generation 1 geri yüklendi, guard zinciri yok, eski rota ve lab çalışıyor |
| `foreign_vpn_and_forwarded_container_keep_state` | **PASS** | Yabancı VPN işareti `0x1234` eşleşmeyen trafikte korundu (her paket işaretli, B üzerinden), açık Route kapsamında reddedildi; lab ana makinesinden yönlendirilen trafik kaynağı `10.65.201.60` (NAT yok); yabancı nft/kural/tablo/tunable öncesi = sonrası |
| `v2_upgrade_requires_exact_cleanup_then_reauthorization` | **PASS** | Eski helper schema-2 journal yazdı ve kuralı canlıya aldı; güncel helper Inspect'te `DAEMON_LEGACY_CLEANUP_REQUIRED` raporladı, modern apply'ı reddetti (canlı durum değişmedi), exact cleanup sahip kaynakları kaldırdı, ardından modern seçici plan uygulandı; yabancı sentinel değişmedi |

Komut (konukta, root): `sh /home/ubuntu/vm-run-t9.sh selective_routing::`
→ `t9-vm-selective.log` (ilk koşu 6/7; foreign senaryosu nft sözdizimi
hatasıyla — `mark` zincir adı nft anahtar kelimesi — test düzeneği hatası,
ürün değil; `vpnmark` olarak düzeltildi) ve `t9-vm-foreign.log` 1/1.

### RED kanıtı (seçici öncesi davranış)

`ROUTELANE_TEST_SELECTIVE_BASELINE=1` aynı senaryoları `UsePrimary`
(önceki varsayılan) planlarla çalıştırır → `t9-vm-red-baseline.log`:
2 passed / **5 failed**, hepsi gerçek assertion'da:

- `explicit_routes`: eşleşmeyen `198.51.100.10` → `Network is unreachable` (ana hat A'ya zorlandı).
- `foreign_and_forwarded`: yabancı işaretli eşleşmeyen trafik → `Connection refused` (global foreign-mark reject).
- `lab_and_unmatched`, `switch_failure`, `v2_upgrade`: IPv6 lab `fd00:65::60` → `Connection refused` (IPv6 ana hat yok → fail-closed).

Not: IPv4 lab, `UsePrimary`'de de bağlı ağ istisnasıyla erişilebilir; geniş
guard'ın IPv4 lab'ı geçiş sırasında tuttuğu bu koşuda ayrıca gösterilmedi
(senaryo IPv6 lab'da daha önce durdu).

### Tam root suite (mevcut senaryolar + yeni lab topolojisi)

| Koşu | Sonuç |
|---|---|
| Standart | **PASS** 25/25 (`t9-vm-full-standard.log`, 71 s) |
| `ROUTELANE_TEST_STRICT_RPF=1` | 1. koşu 24/25: `namespace_routing::real_helper_routes_three_lanes_and_preserves_generation_one_on_activate_failure` sink C'de `203.0.113.10` için 8 ≠ 7 prob saydı; izole 3/3 geçti; 2. tam koşu **PASS** 25/25 (`t9-vm-full-strict2.log`). Aralıklı tek paket fazlası olarak kaydedildi, çözülmüş sayılmadı. |

Koşular sonrası `ip netns list` boş, `/tmp` altında run durumu yok.

### Gerçek system bus ve Polkit (Task 10, 10 Kasım 2026)

`routelane_0.1.1_amd64.deb` (sha256
`5809d7efe6e5c2d4dfb2587cad7d9676011a18fe22d9999587b214526ffc8815`,
`packaging/deb/build-deb.sh`, `e9bb59b` + README/contract çalışma ağacı)
yalnız VM içine kuruldu; `ROUTELANE_DISPOSABLE_VM=$(hostname) sh
system_bus_acceptance.sh routelane_0.1.1_amd64.deb` → **PASS**
(`t10-vm-system-bus.log`). Betik protocol 3 ve açık `UsePrimary` ile
güncellendi (geniş yedeğe geçiş senaryosu bu opt-in'e aittir).

| Kanıt | Sonuç |
|---|---|
| Root olmayan kullanıcı `GetRuntimeProtocol` = 3 okur, yetki gerekmez | PASS |
| Yetkili `ActivateRuntime` Armed; durum `unmatched_traffic: "UsePrimary"` raporlar | PASS |
| Gerçek servis run'a ait netns içinde; `src_valid_mark` yalnız hat arayüzlerinde (all=0) | PASS |
| Yetki geri alınınca Cleanup, politika değişimi, hat ekleme, yabancı UID reddedilir (AccessDenied) | PASS |
| Taşıyıcı kaybında yetkisiz otomatik yedeğe geçiş, root olmayan kullanıcıya `RuntimeChanged` | PASS |
| `kill -9` sonrası systemd yeniden başlatır, Armed sürer; geri dönüş 5 s sonra | PASS |
| Yetkili cleanup durumu kaldırır, `src_valid_mark=0` geri yüklenir; teardown 0 namespace/paket/kullanıcı | PASS |

Polkit kararı test kuralıyla (`polkit.addRule`) verildi; etkileşimli
masaüstü Polkit ajanı **NOT VERIFIED**.

### NOT VERIFIED

- Etkileşimli Polkit ajanı ve GUI'nin kurulu paketle gerçek oturumu.
- Paket yükseltmesi (eski sürüm → bu sürüm) silahlıyken; betiğin ikinci `.deb` yolu bu turda çalıştırılmadı.
- Daemon/helper sürüm karışımı gerçek bus üzerinde (yalnız rootless contract).
- V1 legacy cleanup ve V3 kesinti/yeniden başlatma gerçek kernel'de (rootless birim testleri var).
- Uygulama cgroup seçimi seçici modda gerçek kernel'de.
- Helper'ın gerçek D-Bus `Inspect` topology yakalaması (`observed_connected_networks`).
- Debian 12/13, Fedora 43/44, aarch64, firewalld/SELinux, CI.
