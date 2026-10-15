# Birincil bağlantı ve otomatik yedeğe geçiş — doğrulama kaydı

Bu belge planın (`docs/superpowers/plans/2026-10-14-primary-connection-failover.md`)
Task 11 ve Task 12 kabul kanıtını tutar. Her satır ya **PASS** (komut, ortam ve
gözlenen çıktı ile), ya **NOT VERIFIED** (altyapı yok ya da çalıştırılmadı) olarak
yazılır. Exit 77 ve “derlendi” PASS değildir.

## Task 11 — runtime uçtan uca güvenlik kabulü (15 Ekim 2026)

### Ortam

| Alan | Değer |
|---|---|
| VM | Multipass `routelane-test-u2404-20261014`, tek kullanımlık, x86_64 |
| Dağıtım / kernel | Ubuntu 24.04.5 LTS, `6.8.0-142-generic` |
| iproute2 / nftables | `iproute2-6.1.0`, `nftables v1.0.9` |
| systemd / D-Bus / Polkit / GLib | `255.4-1ubuntu8.18`, `dbus 1.14.10-4ubuntu4.1`, `polkitd 124-2ubuntu1.24.04.4`, `glib 2.80.0` |
| Paket | `routelane_0.1.0_amd64.deb`, sha256 `6ae37f678b076e22cc4a59c35557b8870be6eb1277ca6cb548a6493c28cc7e6e` (`packaging/deb/build-deb.sh`, 33aa274 + çalışma ağacı) |

Host üzerinde hiçbir ağ, firewall, Polkit ya da paket değişikliği yapılmadı.

### 1. Rootless kabul (privileged birim düzeneği)

`rtk cargo test --all-targets` içinde, `src/backend/privileged.rs`:

| Test | Kanıtladığı |
|---|---|
| `runtime_rejects_replayed_generation_and_foreign_caller` | Eski generation tekrarı ve başka UID reddedilir; `applied_generation_after_replay == applied_generation_before_replay` |
| `partial_transaction_never_reports_armed` | Aday apply hatası, grant journal commit (fsync) hatası, değiştirilmiş nft parmak izi, rollback hatası + yeniden başlatma: hiçbiri `Armed` raporlamaz; son doğrulanmış durum ve hata korunur |
| `all_links_down_then_recovery_is_family_safe` | Tüm hatlar düşünce aile bazında `Blocked`, geri dönüşte yalnız gerçekten var olan aile açılır |
| `interface_flap_resets_the_failback_deadline` | Arayüz çırpınması 5 s failback süresini sıfırlar (7,1 s’de hâlâ yedek, 10,1 s’de birincil) |
| `carrier_loss_keeps_linkdown_routes_and_still_fails_over` | Carrier kaybında kernel’in bıraktığı `linkdown` route’lar sahiplikten düşmeden yedeğe geçilir |
| `linkdown_route_of_a_lane_that_is_still_used_is_refused`, `only_an_exact_linkdown_flag_is_owned` | `linkdown` toleransı yalnız tam `["linkdown"]` bayrağı ve taze planın artık kullanmadığı hat×aile için geçerli |

Arayüzün “kaydedildi ama etkin değil” durumunu doğru göstermesi Task 10’daki
`primary_saved_primary_is_not_effective_after_apply_failure` ile sabitlendi.

### 2. Kernel kabulü (root süiti, gerçek helper, ağ ad alanları)

Komut (VM içinde, önceden derlenmiş test ikilisi ile):
`rtk cargo test --test root --features root-integration --no-run` → ikili VM’e
kopyalandı → `sudo sh vm-run-t11.sh` (registry/token temizliği ile
`routelane-root --test-threads=1 --nocapture`), standart ve strict-RPF modlarında.

- **RED:** Task 8 helper’ı ile `gui_closed_link_loss_fails_over_without_authorization_prompt`,
  ilk carrier kaybı yenilemesinde `Degraded` oldu, etkin çıkış `lane-a`’da kaldı
  (kernel carrier kaybında unicast default’ları `linkdown` bayrağıyla tutar;
  eski kod bunu yabancı semantik saydı). Düzeltme: 33aa274.
- **GREEN:** her iki modda 18/18 geçti. Yeni senaryo tek aktivasyondan sonra
  yalnız kernel kanıtıyla yenileme yapar (yeni yetkilendirme yok) ve şunları
  birbirinden bağımsız doğrular:
  - fiziksel carrier kaybı → `lane-c`, 5 s sonra `lane-a`;
  - adres kaldırma → `lane-c`, sonra geri dönüş;
  - main tablodaki default route kaybı (lease bitişi) → `lane-c`; sahipli tablo
    route’u yenilemede emekliye ayrılır;
  - helper yeniden başlatma → tam durum aynen sürdürülür, geçişler devam eder;
  - tüm hatlar kapalı → IPv4 `Blocked`, gateway ingress gözlemcilerinde kaçış yok;
  - temizlikten sonra yabancı nft/rule/sysctl/route sentinel’leri temizlik
    öncesi anlık görüntüleriyle birebir eşit.
- `runtime_refresh_fails_over_and_back_without_breaking_backup_sessions`: yedek
  üzerindeki TCP oturumu ve bağlı UDP soketi geçişler boyunca sürdü (standart
  26, strict 30 alışveriş; 0 UDP yanıt kaybı).

### 3. Gerçek system bus + Polkit kabulü

Betik: `tests/root/system_bus_acceptance.sh` (yalnız tek kullanımlık VM;
`ROUTELANE_DISPOSABLE_VM` hostname’e eşit değilse exit 2 ile reddeder).

```
multipass transfer tests/root/system_bus_acceptance.sh VM:/home/ubuntu/
multipass exec VM -- sudo env ROUTELANE_DISPOSABLE_VM=VM \
    sh /home/ubuntu/system_bus_acceptance.sh /home/ubuntu/routelane_0.1.0_amd64.deb
```

Kurulu paketin gerçek `routelane-helper.service`’i, `/run` altındaki geçici bir
drop-in ile çalıştırmaya ait ağ ad alanına kapatıldı; istemci root olmayan
`rltest` kullanıcısıdır. Polkit onayı, bir işaret dosyasına bağlı test kuralıyla
verildi; dosyanın silinmesi yetki önbelleğinin sona ermesini temsil eder.
GUI ve kullanıcı daemon’u çalışmıyordu.

Gözlenen çıktı (`RESULT: PASS`):

| Adım | Sonuç |
|---|---|
| `rltest` onaylı `ActivateRuntime` | `Armed`, etkin IPv4 `lane-a`; yeni akış kaynağı `10.10.1.2` |
| Sandbox’lı servis | `src_valid_mark` yalnız hat arayüzlerinde `1`, `all=0` |
| Yetki geri çekildi | `rltest`’in `Cleanup` çağrısı `AccessDenied` |
| Lane A carrier kaybı | Yetki istemi olmadan `lane-c`’ye geçiş; yeni akış kaynağı `10.10.3.2` |
| Sinyal | Root olmayan `rltest`, kurulu D-Bus politikasıyla `Helper2.RuntimeChanged` (etkin `lane-c`) aldı |
| Taze onaysız genişleme | Politika değişikliği, yeni hat (`lane-x`) eklenmesi ve başka UID (`rlother`) `ActivateRuntime`/`Cleanup`: hepsi `AccessDenied`; runtime değişmedi |
| `kill -9` helper | `Restart=on-failure` yeni PID ile `Armed` durumunu sürdürdü |
| Carrier döndü | 5 s sonra `lane-a`’ya dönüş; yeni akış kaynağı `10.10.1.2` |
| Onaylı `Cleanup` | RouteLane tablosu kaldırıldı, `src_valid_mark=0` geri yüklendi |
| Teardown | `namespaces=0 package=0 users=0` |

### Task 11 — NOT VERIFIED

- Etkileşimli Polkit ajan penceresi (gerçek parola/onay diyaloğu) ve
  `auth_admin_keep` önbelleğinin gerçek süre dolumu; test kuralı bunların
  yerini tuttu.
- Gerçek fiziksel NIC, Wi-Fi, USB modem ve DHCP/SLAAC istemcisi; testler veth
  ve elle yeniden eklenen adres/route kullanır.
- Yoğun trafikte transaction guard penceresinde sıfır kayıp.
- Debian, Fedora ve aarch64 (Task 12 kapısı).
