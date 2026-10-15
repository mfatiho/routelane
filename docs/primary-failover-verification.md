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

## Task 12 — dağıtım, paket ve yayın kapısı (15 Ekim 2026)

### Paket sözleşmesi (rootless)

`rtk cargo test --test primary_packaging_contract` (3 test):

- `packages_ship_helper2_policy_and_consistent_versions`: deb ve rpm aynı 11
  dosyayı aynı modlarla tam bir kez kurar; sürüm iki pakette de `Cargo.toml`’dan
  aynı yolla okunur; IPC ve runtime protokol sürümleri eşit; helper root ve
  sabit yolda; bus politikası Helper1/Helper2’yi birer kez açar, yetki
  method içinde (Polkit) uygulanır; deb bağımlılığı `polkitd | policykit-1`.
- `service_keeps_kernel_tunable_protection`: `ProtectKernelTunables=yes`,
  yazılabilir yollar yalnız `/proc/sys/net/ipv4/conf` (Task 6 istisnası) ve
  `-/sys/fs/cgroup/routelane.apps`; paketler drop-in eklemez.
- `package_upgrade_preserves_private_config_and_journal`:
  `RuntimeDirectoryPreserve=yes`; yaşam döngüsü betikleri config/journal/kernel
  durumu silen komut çalıştırmaz, yalnız `try-restart` kullanır; durdurma
  yalnız kaldırmada.

Mutasyonlar (rpm’den tmpfiles çıkarma, `ReadWritePaths=/proc/sys`, preserve
silme, `restart`, koşulsuz `%preun`, Helper2 politikasını silme) her biri bir
testi düşürdü.

### Spec kabul maddeleri → kanıt

| # | Kanıt | Durum |
|---|---|---|
| 1 | `default_lane_id_is_not_a_role`, `two_lane_aliases_for_system_default_require_explicit_selection`, `primary_proposal_uses_the_other_lane_not_names_or_ids` | PASS (rootless) |
| 2 | kernel `primary_and_rule_paths_have_correct_source_and_return` | PASS (Ubuntu satırları) |
| 3 | `primary_cancel_escape_close_emit_no_effect`, `primary_double_confirmation_saves_once`, GTK smoke `assert_primary_selection_is_confirmed_cancel_safe_and_reopens` | PASS (rootless/GTK) |
| 4 | kernel `gui_closed_link_loss_fails_over_without_authorization_prompt`; system bus carrier kaybı | PASS (Ubuntu satırları); gerçek fiziksel NIC NOT VERIFIED |
| 5 | system bus: yetki geri çekildikten sonra failover, genişlemeler `AccessDenied` | PASS (test kuralı ile); etkileşimli ajan NOT VERIFIED |
| 6 | `failback_requires_5000_ms_continuous_readiness`, kernel `runtime_refresh_fails_over_and_back_without_breaking_backup_sessions`, `backup_tcp_session_survives_new_flow_failback` | PASS |
| 7 | `all_links_down_then_recovery_is_family_safe`, kernel `loss_signals_fail_closed_without_escape` | PASS |
| 8 | kernel `local_dhcp_dns_and_foreign_resources_survive_cleanup`, yabancı sentinel eşitliği | PASS; Fedora firewalld/SELinux NOT VERIFIED |
| 9 | kernel IPv6/kaynak/strict-loose RPF senaryoları, iki modda | PASS (Ubuntu satırları) |
| 10 | `primary_config_contract` (7 test) | PASS (rootless) |
| 11 | `partial_transaction_never_reports_armed`, `primary_daemon_contract`, system bus `kill -9` ve yükseltme | PASS |
| 12 | Aşağıdaki dağıtım matrisi | **AÇIK** — Debian, Fedora ve aarch64 doğrulanmadı |

### Dağıtım matrisi

Her Ubuntu VM’i tek kullanımlık bir Multipass VM’idir (x86_64, 4 vCPU). 22.04
ve 26.04’te kaynak ağacı VM içinde yerel olarak derlendi (`cargo test
--all-targets`, `build-deb.sh`, root süiti); 24.04’te derleme ortamı aynı
sürümdeki host’tur. Her VM’de: root süiti (standart ve strict RPF),
`tests/root/system_bus_acceptance.sh` (0.1.0 → 0.1.1 silahlı yükseltme dahil),
yeniden başlatma ve kaldırma.

| Dağıtım | Kernel / iproute2 / nft | systemd / Polkit / D-Bus | GTK4 / Libadwaita | Rootless | Root süiti | System bus + yükseltme | Reboot + kaldırma |
|---|---|---|---|---|---|---|---|
| Ubuntu 22.04.5 | 5.15.0-198 / 5.15.0 / 1.0.2 | 249.11 / 0.105 (pkla) / 1.12.20 | 4.6.9 / 1.1.7 | 433 PASS | 18/18 + 18/18 PASS | PASS | PASS |
| Ubuntu 24.04.5 | 6.8.0-147 / 6.1.0 / 1.0.9 | 255.4 / 124 / 1.14.10 | 4.14.5 / 1.5.0 | 433 PASS (host) | 18/18 + 18/18 PASS | PASS | PASS |
| Ubuntu 26.04.1 | 7.0.0-34 / 6.19.0 / 1.1.6 | 259.5 / 127 / 1.16.2 | 4.22.4 / 1.9.1 | 433 PASS | `0203fab` sonrası standart 2/3, strict 1/2 koşu temiz; diğerlerinde sayaç/eşik kaynaklı tek hata — aşağıya bkz. | PASS | PASS |
| Debian 12, 13 | — | — | — | — | NOT VERIFIED | NOT VERIFIED | NOT VERIFIED |
| Fedora 43, 44 | — | — | — | — | NOT VERIFIED | NOT VERIFIED | NOT VERIFIED |
| aarch64 (tümü) | — | — | — | — | NOT VERIFIED | NOT VERIFIED | NOT VERIFIED |

Reboot satırı: tmpfiles `routelane.apps` cgroup’unu açılışta oluşturdu, eski
journal yoktu, D-Bus ile başlatılan helper `Inactive` döndü (yeniden
etkinleştirme taze onay ister). Kaldırma: paket dosyaları silindi, helper
etkin değil, kaldırma notu gösterildi.

İlk dağıtım koşusu iki gerçek uyumsuzluk buldu (ikisi de nft JSON
listelemesinde; kernel durumu doğruydu, helper commit’i doğrulayamadı ve geri
aldı, yani fail-closed davrandı):

- 26.04 / nft 1.1.6: IPv6 kaynak-kanıt kurallarında `meta nfproto ipv6`
  listelemeden düşüyordu → kurallar gereksiz bağımlılık olmadan render edilir
  (`source_proof_rules_carry_no_redundant_family_dependency`).
- 22.04 / nft 1.0.2: zincirler kurallarıyla ardışık listeleniyordu →
  normalleştirme kararlı bölme yapar
  (`fingerprint_and_seal_accept_nft_1_0_2_chain_interleaved_listing_order`).

Yukarıdaki satırlar bu düzeltmelerden ve son incelemedeki `0203fab`
(başarısız geçişin geri çekilmesi) düzeltmesinden sonraki koşulardır. Koşular
commit edilmiş HEAD’den değil, kullanıcının henüz commit edilmemiş güvenilirlik
hunk’larını da içeren çalışma ağacından yapıldı.

26.04’te `0203fab` sonrası standart modda 3 koşunun 2’si, strict modda 2
koşunun 1’i temiz geçti (`d8949e7` sonrası, `0203fab` öncesi koşu iki modda da
temizdi). Düşen koşularda birer test kaldı ve tekrarlanmadı: (a) Faz 1 testi
`real_helper_routes_three_lanes_and_preserves_generation_one_on_activate_failure`
sink arayüzünün link RX sayaçlarını okur (iki farklı satırda, ölçüm
penceresinde fazladan paket; bir kez standart, bir kez strict modda);
(b) `runtime_refresh…` içinde `exchanges > 20` verim eşiği 19 ile kaçtı
(0 UDP yanıt kaybı). Hata moda bağlı değil; hipotez: kernel 7.0’da veth
üzerindeki IPv6 ND/MLD trafiği link sayaçlarını kirletiyor ve yük altında verim
eşiği sınırda kalıyor; doğrulanmadı. Bu satır bu yüzden temiz PASS değildir;
Task 7’deki gibi nft ingress gözlemcilerine geçmek ertelendi. Matris
`0203fab` + çalışma ağacında ölçüldü; ardından gelen kurtarma-durumu
düzeltmesi yalnız rootless testlerle doğrulandı.

Fedora rpm: `packaging/rpm/build-rpm.sh`, Docker `fedora:latest` (Fedora 44,
cargo 1.99.0, RPM 6.0.2) içinde son ağaçtan üretildi
(`routelane-0.1.0-1.x86_64`, sha256 `b2ae2b8a26c139281ef2698f5f3b167f95b589cfcb10905dd1ff5d08761b759f`); 11 dosya + `/usr/lib/routelane`
dizini beklenen mod ve root/root sahipliğiyle, scriptlet’ler mevcut. Bu yalnız
paket üretimidir: Fedora’da kurulum, firewalld/SELinux, Polkit ve kernel
kabulü **NOT VERIFIED**.

### Task 12 — NOT VERIFIED

- Debian 12/13, Fedora 43/44 (bu makinede imaj/hipervizör yok) ve tüm
  dağıtımlarda aarch64.
- Ubuntu VM’lerinde GTK başlatma (ekransız VM); GTK smoke yalnız host’ta.
- Ufw/firewalld ile birlikte çalışma ve Fedora SELinux; yabancı nft/rule
  sentinel’leri yalnız test ad alanında doğrulandı.
- Erişilebilirlik/ekran okuyucu, manuel tema incelemesi, yayın sitesi DNS/CDN.
- CI: matris henüz CI’da çalışmıyor; satırlar elle VM koşularıdır.

**Yayın kapısı açık değildir.**
