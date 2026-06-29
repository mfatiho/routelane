# RouteLane

Policy-based routing (PBR) desktop uygulaması. Belirli alan adlarını veya IP adreslerini seçtiğiniz ağ arayüzünden yönlendirirken diğer tüm trafiğin varsayılan bağlantıdan akmasını sağlar.

**Örnek kullanım:** ChatGPT, OpenAI gibi siteleri VPN üzerinden; geri kalan trafiği normal internet bağlantısından yönlendirme.

## Özellikler

- GTK4 + Libadwaita arayüzü (GNOME HIG uyumlu)
- Alan adı veya IP/CIDR bazlı yönlendirme kuralları
- Tek bir `pkexec` çağrısıyla ayrıcalık yükseltme (Polkit)
- Uygulama kapatıldığında veya switch kapatıldığında tüm kernel kuralları güvenli biçimde temizlenir
- Ayarlar yeniden açıldığında otomatik geri yüklenir (`~/.config/routelane/config.json`)

## Mimari

```
┌───────────────────┐  UiToEngine  ┌────────────────────────┐
│  GTK main thread  │ ────────────>│  Tokio (arka plan)     │
│  (window.rs)      │ <────────────│  routing/mod.rs         │
└───────────────────┘  EngineToUi └────────────────────────┘
                                           │  pkexec (tek seferlik)
                                           ▼
                                   routelane-helper  (root)
                                   ip rule / ip route
```

**İki durumlu model:**

| Switch | Kernel Durumu | Açıklama |
|--------|--------------|----------|
| KAPALI | Boş | Kurallar yalnızca bellekte saklanır, kernel'a dokunulmaz |
| AÇIK   | Aktif | Tüm kurallar tek `pkexec` çağrısıyla kernel'a uygulanır |

## Gereksinimler

### Çalışma zamanı

- Ubuntu 22.04 (veya uyumlu)
- `libgtk-4-1` ≥ 4.6
- `libadwaita-1` ≥ 1.1
- `pkexec` / `polkit`
- `iproute2` (`ip` komutu)

### Derleme

```
libgtk-4-dev
libadwaita-1-dev
cargo (Rust stable)
```

```bash
sudo apt install libgtk-4-dev libadwaita-1-dev
```

## Derleme

```bash
git clone <repo-url>
cd routelane
cargo build --release
```

Çıktılar:

| Dosya | Açıklama |
|-------|----------|
| `target/release/routelane` | GUI uygulaması |
| `target/release/routelane-helper` | Ayrıcalıklı yardımcı ikili |

## Deb Paketi Olusturma

Ubuntu icin kurulabilir `.deb` paketi uretmek:

```bash
./packaging/deb/build-deb.sh
```

Paket `dist/` altina yazilir:

```text
dist/routelane_0.1.0_amd64.deb
```

Yerel kurulum:

```bash
sudo apt install ./dist/routelane_0.1.0_amd64.deb
```

Paket su dosyalari kurar:

| Dosya | Hedef |
|-------|-------|
| `routelane` | `/usr/bin/routelane` |
| `routelane-helper` | `/usr/lib/routelane/routelane-helper` |
| `io.github.routelane.desktop` | `/usr/share/applications/io.github.routelane.desktop` |
| `io.github.routelane.policy` | `/usr/share/polkit-1/actions/io.github.routelane.policy` |

## Kurulum

```bash
# Yardımcı ikiliyi kur
sudo mkdir -p /usr/lib/routelane
sudo cp target/release/routelane-helper /usr/lib/routelane/routelane-helper
sudo chmod 755 /usr/lib/routelane/routelane-helper

# Polkit politikasını kur (tek seferlik oturum yetkilendirmesi)
sudo cp data/io.github.routelane.policy \
     /usr/share/polkit-1/actions/io.github.routelane.policy

# GUI ve Ubuntu dock launcher kaydını kur
sudo cp target/release/routelane /usr/bin/routelane
sudo cp data/io.github.routelane.desktop \
     /usr/share/applications/io.github.routelane.desktop

# GUI'yi başlat
routelane
```

### Geliştirme ortamında çalıştırma

Kurulum yapmadan test etmek için helper yolunu çevre değişkeniyle belirtin:

```bash
ROUTELANE_HELPER=./target/debug/routelane-helper cargo run --bin routelane
```

> **Not:** `routelane-helper`'ın root olarak çalışabilmesi için polkit politikası yüklenmiş olmalıdır.

## Kullanım

1. Uygulamayı başlatın.
2. **İstisna Ağı** açılır menüsünden yönlendirme yapılacak ağ arayüzünü seçin (örn: `wlan0`, `tun0`).
3. **Adres Ekle** alanına bir alan adı (`chatgpt.com`) veya IP (`8.8.8.8`) yazıp Enter'a basın.
4. Switch'i **açın** — kurallar kernel'a uygulanır, polkit şifre sorar.
5. Switch'i **kapatın** veya uygulamayı kapatın — tüm kernel kuralları otomatik temizlenir.

Ayarlar (`~/.config/routelane/config.json`) uygulama kapanınca kaydedilir; sonraki açılışta geri yüklenir. Switch varsayılan olarak kapalı gelir; kullanıcı manuel açar.

## Kayıt yapısı

```
~/.config/routelane/config.json
```

```json
{
  "alt_interface": "wlan0",
  "rules": [
    { "target_str": "chatgpt.com", "is_domain": true, "interface": "wlan0" },
    { "target_str": "8.8.8.8",    "is_domain": false, "interface": "wlan0" }
  ]
}
```

## Güvenlik

- GUI uygulaması root yetkisi gerektirmez.
- `routelane-helper` yalnızca `pkexec` aracılığıyla çalışır; doğrudan çağrılamaz.
- Helper, her girdi için whitelist doğrulaması yapar: tablo numarası (100–199), öncelik aralığı (10000–10999), IP/CIDR formatı.
- Polkit politikası `allow_active = auth_admin_keep`: oturumda bir kez şifre sorulur, sonraki işlemler şifresiz geçer.
- Kernel yönlendirme tablosu: `100 (routelane)`, öncelik aralığı: `10000–10999`.

## CDN Alan Adları Hakkında

`chatgpt.com` gibi Cloudflare/Fastly CDN'li alan adları her DNS sorgusunda farklı IP adresleri döndürebilir. Bu durumda `ip rule + çözümlenen IP` yaklaşımı tam güvenilir değildir. Sürekli çalışan yönlendirme için `dnsmasq + ipset + fwmark` mimarisine geçiş planlanmaktadır (`src/routing/dns_router.rs`).

## Proje Yapısı

```
src/
├── main.rs                  — Giriş noktası; GTK + Tokio başlatma
├── config.rs                — Ayar kalıcılığı (JSON)
├── models.rs                — Paylaşılan veri yapıları ve kanal mesajları
├── routing/
│   ├── mod.rs               — Engine ana döngüsü
│   ├── manager.rs           — RoutingStateManager (iki durumlu model)
│   ├── executor.rs          — pkexec / routelane-helper iletişimi
│   ├── resolver.rs          — Async DNS çözümleme
│   └── dns_router.rs        — (WIP) dnsmasq + ipset backend
├── ui/
│   ├── window.rs            — Ana pencere
│   └── rule_row.rs          — Kural satırı widget'ı
└── bin/
    └── routelane_helper.rs  — Ayrıcalıklı yardımcı ikili
data/
├── io.github.routelane.desktop — Ubuntu/GNOME launcher kaydı
└── io.github.routelane.policy  — Polkit politikası
```

## Lisans

MIT
