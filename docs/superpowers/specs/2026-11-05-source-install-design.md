# Kaynaktan kurulum: tek komut, bağımlılıklar ve `routelanectl doctor`

Tarih: 5 Kasım 2026. Durum: kullanıcı 5 Kasım'da önerilen 1–3. maddeleri onayladı.

## Amaç

Açık kaynak koddan derleyip kurmak isteyen bir kullanıcı tek komutla kurulum
yapabilmeli, gerekli paketleri bilmeli ve kurulumun doğru olup olmadığını tek
komutla görebilmelidir. Kurulum her zaman yerelde paket üretip paket
yöneticisiyle yapılır: helper sabit yolda root olarak çalışır ve D-Bus, Polkit,
systemd, tmpfiles dosyaları bu yola bağlıdır; dosya kopyalayan bir `make
install` temiz kaldırma ve yükseltme sağlamaz.

## 1. `scripts/install-from-source.sh`

- `bash`, `set -euo pipefail`. Kullanıcı olarak çalışır; EUID 0 ise reddeder
  (“run as your user; sudo is used only for the package manager”). `sudo`
  yalnız paket yöneticisi komutları için çağrılır; parola saklanmaz.
- Seçenekler: `--yes` (onay sorma), `--dry-run` (değiştiren hiçbir komutu
  çalıştırma, yalnız yazdır), `--skip-deps`, `--build-only` (paketi üret,
  kurma), `--help`.
- Dağıtım tespiti: `${ROUTELANE_OS_RELEASE:-/etc/os-release}` içindeki
  `ID`/`ID_LIKE`. `ubuntu`/`debian` → deb ailesi (apt), `fedora` → rpm ailesi
  (dnf). Diğerleri: exit 2, README'deki elle kurulum adımlarına yönlendir.
  Desteklenen sürümler dışındaysa (Ubuntu 22.04/24.04/26.04, Debian 12/13,
  Fedora 43/44) uyarı verir ve devam eder (“not a validated release”).
- Derleme bağımlılıkları:
  - deb: `build-essential pkg-config libgtk-4-dev libadwaita-1-dev`
  - rpm: `gcc pkgconf-pkg-config gtk4-devel libadwaita-devel rpm-build`
  - Eksikler `dpkg-query -W -f='${Status}'` / `rpm -q` ile bulunur. Eksik
    varsa liste gösterilir, onay alınır (`--yes` hariç), sonra
    `sudo apt-get update` + `sudo apt-get install -y …` veya
    `sudo dnf install -y …`.
- Rust: `cargo` ve `rustc` PATH'te olmalı ve `rustc` sürümü `Cargo.toml`
  içindeki `rust-version` değerinden düşük olmamalı. Değilse rustup komutu
  yazdırılır ve exit 3. Betik rustup'ı kendisi indirmez/çalıştırmaz.
- Derleme: `packaging/deb/build-deb.sh` veya `packaging/rpm/build-rpm.sh`
  kullanıcı olarak (sudo'suz) çalıştırılır; son `Created <path>` satırından
  paket yolu alınır.
- Kurulum (`--build-only` değilse, onayla): `sudo apt install -y ./<yol>` veya
  `sudo dnf install -y <yol>`. Ardından `routelanectl doctor` çalıştırılır;
  FAIL varsa betik bunu söyler ve doctor'ın çıkış kodunu döndürür.
- Asla: helper'ı enable/start etmek, ağ yapılandırmasını değiştirmek,
  RouteLane profilini uygulamak/kapatmak, depo ve paket yöneticisi dışına
  yazmak.

## 2. Bağımlılıklar ve sürüm sabitleme

- `Cargo.toml`: `rust-version = "1.87"` (bağımlılıkların en yüksek
  `rust-version` değeri 1.87; kod `cargo +1.87 check --all-targets --locked`
  ile geçti).
- `build-deb.sh` ve `build-rpm.sh`: `cargo build --release --bins --locked`.
- README “Build” bölümü: önerilen yol olarak tek komut; deb ve rpm için tek
  satırlık bağımlılık komutları; rustup ve MSRV; elle derleme; kaldırma paket
  yöneticisiyle. Flatpak/Snap ve `cargo install` neden desteklenmiyor: root
  helper, sistem D-Bus'ı/Polkit ve host ağ erişimi.

## 3. `routelanectl doctor [--json]`

Salt okunur. Root gerektirmez. Daemon'a yalnız `GetStatus` gönderir; helper'ı
başlatmaz (D-Bus aktivasyonu yok), helper metodu çağırmaz, kernel/ağ durumunu
değiştirmez.

Kontroller (`id`, `PASS|WARN|FAIL`, ayrıntı, düzeltme önerisi):

| id | Koşul | Başarısızlıkta |
| --- | --- | --- |
| `helper_binary` | `/usr/lib/routelane/routelane-helper` düzenli dosya, root:root, çalıştırılabilir, grup/diğer yazamaz; `/usr/lib/routelane` root sahipli ve grup/diğer yazamaz | FAIL |
| `client_binaries` | `/usr/bin/routelane`, `/usr/bin/routelane-daemon`, `/usr/bin/routelanectl` | FAIL |
| `integration_files` | sistem/kullanıcı systemd unit'i, D-Bus system-services ve system.d dosyaları, Polkit action, tmpfiles: var, root sahipli, grup/diğer yazamaz | FAIL |
| `polkit_action` | action dosyası `io.github.routelane.apply` içerir; `pkaction` varsa `pkaction --action-id io.github.routelane.apply` başarılı | FAIL (pkaction yoksa WARN) |
| `dbus_activation` | sistem bus'ına bağlanılır; `ListActivatableNames` içinde `io.github.routelane.Helper` var; çalışıyor mu (`NameHasOwner`) bilgi olarak | FAIL |
| `kernel_tools` | `/usr/sbin/nft` ve `/usr/sbin/ip` var; sürümleri raporlanır | FAIL |
| `cgroup_v2` | `/sys/fs/cgroup/cgroup.controllers` var | FAIL |
| `app_cgroup` | `/sys/fs/cgroup/routelane.apps` var | WARN (yalnız uygulama kuralları etkilenir) |
| `user_daemon` | daemon soketine kısa zaman aşımıyla `GetStatus`; özet raporlanır | WARN (GUI başlatır / `systemctl --user start routelane-daemon.service`) |

Çıktı: kontrol başına bir satır, WARN/FAIL için `fix:` satırı, sonda özet.
`--json`: `{id,status,detail,hint}` dizisi. Çıkış kodu: FAIL yoksa 0, varsa 5.
Kontroller test edilebilir bir probe soyutlamasıyla kütüphanede yaşar; sistem
probe'u ayrı bir uygulamadır.

## Kabul

- Betik `--dry-run` ile sahte `os-release` ve PATH'e konmuş sahte araçlarla
  test edilir: deb/rpm ailesinin doğru paket listesi ve komutları, desteklenmeyen
  dağıtımda exit 2, eski Rust'ta exit 3 ve rustup ipucu, dry-run'da hiç `sudo`
  çağrısı olmaması.
- doctor: her kontrolün PASS/WARN/FAIL dalları sahte probe ile; CLI testi
  geçerli JSON ve tüm kontrol kimliklerini doğrular.
- Paket sözleşmesi: iki derleme betiği de `--locked` kullanır.
- Yeni bağımlılık eklenmez; gerçek `sudo`/`apt`/`dnf` testte çalışmaz; host'a
  kurulum yapılmaz.
