# RouteLane Açık Kaynak Platform Tasarımı

**Durum:** Kullanıcı tarafından onaylandı  
**Tarih:** 28 Temmuz 2026  
**Kaynak:** `docs/commercial-roadmap.md` açık kaynak özellikleri  
**Hedef:** RouteLane'i yerel, güvenilir, çoklu-lane destekli bir Linux trafik
politika platformuna dönüştürmek

## 1. Kapsam

Bu tasarım, ticari yol haritasında açık kaynak olarak sınıflandırılan yerel
özellikleri kapsar:

- nftables tabanlı routing engine ve dar yetkili helper
- Domain, wildcard domain, IP ve CIDR hedefleri
- Multi-Lane ve çoklu ağ arayüzü
- Route, Direct ve aktif/pasif Block kuralları
- Hibrit DNS/CDN backend'i
- Yerel otomatik profil aktivasyonu
- Bağlantı Sağlığı, leak ve failover kontrolü
- GTK GUI, CLI, profil ve import/export
- Yerel tanılama ve audit kaydı
- Uygulama bazlı yönlendirme
- Açık servis paketi formatı ve community paketleri
- Ubuntu, Debian ve Fedora için resmî paketleme ve doğrulama

## 2. Kapsam dışı

İlk açık kaynak uygulama çalışması aşağıdakileri kapsamaz:

- Hesap, ödeme veya lisans servisi
- Bulut senkronizasyonu ve yedekleme
- Premium servis paketi akışı
- Merkezi takım ve cihaz yönetimi
- SSO, SCIM, MDM veya SIEM entegrasyonu
- Çoklu tenant kontrol düzlemi
- Enterprise HA veya SLA
- OEM/white-label lisans altyapısı
- Kullanıcı onayı olmadan üçüncü taraf dış-IP test servisine bağlanma

## 3. Teslimat stratejisi

Uygulama tek büyük sürüm yerine beş bağımsız ve doğrulanabilir faz halinde
teslim edilir:

1. nftables Multi-Lane çekirdeği, atomik rollback ve Bağlantı Sağlığı
2. Hibrit DNS/CDN connector'ları ve wildcard kuralları
3. Profil, otomasyon, CLI ve import/export
4. Uygulama bazlı yönlendirme
5. Community servis paketleri ve üç dağıtımlı paketleme

Her faz sonunda çalışan ve test edilmiş bir ürün artımı bulunur. Sonraki faz,
önceki fazın testleri temiz olmadan başlamaz.

## 4. Resmî platform matrisi

| Dağıtım | Sürümler | Mimariler |
| --- | --- | --- |
| Ubuntu | 22.04, 24.04, 26.04 LTS | x86_64, aarch64 |
| Debian | 12, 13 | x86_64, aarch64 |
| Fedora | 43, 44; sonrasında güncel iki sürüm | x86_64, aarch64 |

Fedora desteği sabit bir alt sınır yerine güncel sürüm ve bir önceki sürüm
politikasıyla sürdürülür. Ubuntu yalnızca standart desteği devam eden LTS
sürümlerini, Debian ise stable ve oldstable/LTS sürümlerini kapsar.

## 5. Mimari

### 5.1 Süreç sınırları

```text
GTK GUI ─────┐
             ├── Unix socket ── routelane-daemon (normal kullanıcı)
CLI ─────────┘                       │
                                    │ sürümlü ve doğrulanmış plan
                                    ▼
                         system D-Bus + Polkit
                                    │
                                    ▼
                         routelane-helper (root)
                                    │
                    nftables + ip rule + route + cgroup
```

| Bileşen | Yetki | Sorumluluk |
| --- | --- | --- |
| `routelane` | Normal kullanıcı | GTK arayüzü; daemon API istemcisi |
| `routelanectl` | Normal kullanıcı | Profil, kural, sağlık ve tanılama CLI'si |
| `routelane-daemon` | Normal kullanıcı | İstenen durum, DNS, otomasyon ve sağlık |
| `routelane-helper` | Root | Beyaz listeli kernel ve cgroup işlemleri |
| Ortak Rust library | Yetkisiz | Model, IPC, planlama ve doğrulama |

GUI hiçbir routing iş mantığı taşımaz. GUI ve CLI aynı IPC istemcisini ve aynı
daemon state'ini kullanır.

### 5.2 Kaynak modülleri

```text
src/
  lib.rs
  domain/
    rule.rs
    profile.rs
    health.rs
  ipc/
    protocol.rs
    client.rs
    server.rs
  daemon/
    state.rs
    planner.rs
    service.rs
  backend/
    nftables.rs
    policy_route.rs
    privileged.rs
  dns/
    connector.rs
    periodic.rs
    dnsmasq.rs
  automation/
    network_events.rs
    activation.rs
  app_routing/
    discovery.rs
    cgroup.rs
  service_packs/
    schema.rs
    loader.rs
  health/
    probes.rs
    report.rs
  bin/
    routelane_daemon.rs
    routelanectl.rs
    routelane_helper.rs
```

Sorumluluklar:

- `domain`: Platformdan bağımsız kullanıcı niyeti ve durum modelleri
- `ipc`: Sürümlü istemci/daemon sözleşmesi
- `daemon`: İstenen durumun sahibi ve event döngüsü
- `backend`: Kernel'a uygulanacak planın deterministik üretimi
- `dns`: Değiştirilebilir DNS gözlem ve çözümleme connector'ları
- `automation`: Ağ ve zaman koşullarının değerlendirilmesi
- `app_routing`: Uygulama keşfi ve cgroup v2 yaşam döngüsü
- `service_packs`: Açık paket şeması ve doğrulama
- `health`: Kernel, DNS ve uygulama state'i doğrulama

### 5.3 IPC

GUI ve CLI, daemon'a şu socket üzerinden bağlanır:

```text
$XDG_RUNTIME_DIR/routelane/routelane.sock
```

Güvenlik ve protokol kuralları:

- Socket dizini ve socket yalnızca kullanıcı tarafından erişilebilir; izin
  `0600` olur.
- Mesajlar sürümlü ve uzunluk önekli JSON olarak kodlanır.
- Her request benzersiz `request_id` taşır.
- Mutation cevapları yeni `generation` değerini döndürür.
- İstemciler durum değişikliklerine event aboneliği açabilir.
- Desteklenmeyen protokol sürümü açık hata koduyla reddedilir.
- Daemon kapanınca istemci otomatik olarak routing state'ini sıfırlamaz.

Daemon, root helper'a system D-Bus üzerinden erişir. Helper ihtiyaç halinde
systemd tarafından aktive edilir. Her mutation çağrısı çağıran kullanıcı ve
Polkit yetkisiyle doğrulanır.

Helper serbest shell komutu veya ham nftables metni kabul etmez. Yalnızca ortak
library'de tanımlı, ayrılmış RouteLane isim alanına ilişkin yapılandırılmış
işlemleri kabul eder.

## 6. Domain modeli

### 6.1 Kural

```rust
struct Rule {
    id: RuleId,
    enabled: bool,
    priority: u32,
    source: RuleSource,
    target: RuleTarget,
    action: RuleAction,
}
```

```rust
enum RuleSource {
    Any,
    Application(ApplicationSelector),
}

enum RuleTarget {
    Any,
    Domain(String),
    WildcardDomain(String),
    Ip(IpAddr),
    Cidr(IpNetwork),
    ServicePack(ServicePackId),
}

enum RuleAction {
    Route(LaneId),
    Direct,
    Block,
}
```

Block kuralları diğer kurallar gibi `enabled` alanıyla silinmeden aktif veya
pasif yapılır.

Kural önceliğinde küçük sayısal değer önce değerlendirilir. Aynı priority
değerindeki kurallar `RuleId` artan sırasıyla değerlendirilir. Böylece config
dosyasındaki fiziksel sıra kernel davranışını değiştirmez.

Include ve exclude ayrı kernel modları değildir:

- Include, seçili hedef için `Route(lane)` eylemine dönüşür.
- Exclude, seçili hedef için `Direct` eylemine dönüşür.
- Engelleme, seçili hedef için `Block` eylemine dönüşür.

Bu normalizasyon GUI, CLI ve kernel planner'ın aynı anlamı kullanmasını sağlar.

### 6.2 Profil ve lane

```rust
struct Profile {
    id: ProfileId,
    name: String,
    enabled: bool,
    lanes: Vec<Lane>,
    rules: Vec<Rule>,
    activation: ActivationPolicy,
}

struct Lane {
    id: LaneId,
    interface: InterfaceSelector,
    fallback_lane: Option<LaneId>,
    failure_policy: LaneFailurePolicy,
}

enum LaneFailurePolicy {
    Block,
    Direct,
    Inactive,
}
```

`failure_policy` ile kullanıcı tanımlı `RuleAction::Block` birbirinden
bağımsızdır. İlki lane kaybındaki davranışı, ikincisi normal trafik politikasını
belirler.

### 6.3 Uygulama seçicisi

```rust
struct ApplicationSelector {
    desktop_id: Option<String>,
    executable_path: PathBuf,
    include_children: bool,
}
```

Desktop ID ve executable path birlikte saklanır. Path kaybolursa desktop ID ile
yeniden keşif yapılır. Birden fazla olası eşleşme varsa kullanıcı seçimi
olmadan politika uygulanmaz.

### 6.4 Aktivasyon

```rust
struct ActivationPolicy {
    mode: ConditionMode,
    conditions: Vec<ActivationCondition>,
    manual_override: ManualOverride,
}

enum ConditionMode {
    All,
    Any,
}

enum ActivationCondition {
    InterfacePresent(InterfaceSelector),
    ConnectionName(String),
    NetworkManagerUuid(String),
    WifiSsid(String),
    TimeWindow(TimeWindow),
}

enum ManualOverride {
    Automatic,
    ForceOn,
    ForceOff,
}
```

`ForceOn` ve `ForceOff`, otomatik koşullardan önceliklidir.

## 7. Kalıcı durum ve migration

Profiller sürümlü şemayla aşağıdaki dosyada saklanır:

```text
~/.config/routelane/config.json
```

Kurallar:

- Dosyanın tek yazarı daemon'dır.
- Yazma işlemi aynı dosya sisteminde geçici dosya, `fsync` ve atomik rename
  kullanır.
- Her başarılı yazma öncesinde son geçerli yapılandırma yedeklenir.
- Parse veya migration hatasında orijinal dosya değiştirilmez.
- Uygulanan generation çalışma durumu XDG state dizininde tutulur; kullanıcı
  konfigürasyonuyla karıştırılmaz.

Mevcut şema migration'ı:

```text
alt_interface
    → "Default" profilindeki ilk Lane

mevcut domain/IP kuralları
    → source=Any, action=Route(default_lane)

eski aktif durumu
    → güvenlik için Inactive

eski rota testi
    → kalıcı veri taşımadığı için migration gerektirmez
```

## 8. İstenen ve uygulanan durum

Daemon iki ayrı durum tutar:

- `desired_state`: Kullanıcının kaydettiği profiller ve kurallar
- `applied_state`: Kernel'a başarıyla uygulanmış son plan ve generation

Akış:

```text
GUI/CLI komutu
    ↓
Daemon doğrulama
    ↓
İstenen durumun atomik kaydı
    ↓
Planner → KernelPlan
    ↓
Helper yetki ve semantik doğrulama
    ↓
Kernel transaction
    ↓
Bağlantı Sağlığı doğrulaması
    ↓
Applied, Degraded veya Failed eventi
```

Her plan monoton artan `generation` taşır. Helper:

- Aynı generation tekrarını idempotent kabul eder.
- Uygulanmış generation'dan eski bir mutation'ı reddeder.
- Kendi yönettiği kernel nesnelerinde aktif generation bilgisini tutar.

## 9. nftables ve policy routing

RouteLane yalnızca kendine ait isim alanını yönetir:

```text
table inet routelane
```

Plan şu nesneleri üretir:

- Uygulama/cgroup sınıflandırma zincirleri
- IPv4 ve IPv6 domain/IP setleri
- Hedeften mark'a map'ler
- Direct, Route ve Block eylem zincirleri
- Paket ve byte sayaçları
- Her lane için ayrılmış mark
- Her lane için ayrılmış routing table
- `fwmark → lookup table` policy rule'ları

İlk sabit kaynak sınırları:

| Kaynak | Ayrılmış alan |
| --- | --- |
| Routing table | `20000..=20099` |
| fwmark | `0x524c0001..=0x524c0064` |
| Policy rule priority | `10000..=10099` |
| Profil başına lane | En fazla 100 |
| Profil başına kural | En fazla 10.000 |
| Dinamik set elemanı | Set başına en fazla 65.536 |

Helper bu alanların dışındaki planı kernel'a dokunmadan reddeder. Kullanımdaki
bir routing table veya priority RouteLane dışı state içeriyorsa otomatik
devralınmaz; `ROUTING_RESOURCE_CONFLICT` hatası üretilir.

Trafik akışı:

```text
Paket
  ↓
Uygulama/cgroup eşleşmesi
  ↓
Hedef domain/IP/CIDR set eşleşmesi
  ↓
Önceliği en yüksek etkin RuleAction
  ├── Route → fwmark → lane routing table
  ├── Direct → main routing table
  └── Block → reject/drop
```

Planner deterministiktir. Aynı `desired_state` aynı sıralı `KernelPlan` ve aynı
plan hash'ini üretir.

## 10. Transaction ve rollback

Helper planın tamamını kernel'a dokunmadan önce doğrular:

- RouteLane isim alanı
- Ayrılmış mark ve routing table aralıkları
- Arayüz adları
- IP aileleri ve CIDR'ler
- Kural ve set boyut sınırları
- Generation sırası
- Çağıran kullanıcı ve Polkit yetkisi

Uygulama sırası:

1. Gerekliyse geçici sızıntı koruması kurulur.
2. Yeni lane routing tabloları hazırlanır.
3. Yeni policy rule'lar eklenir.
4. nftables değişiklikleri tek atomik batch ile aktive edilir.
5. Kernel state doğrulanır.
6. Eski generation temizlenir.
7. Geçici koruma kaldırılır.

Güncellemede eski generation, yenisi tamamen hazır olana kadar çalışır.

| Hata | Davranış |
| --- | --- |
| Plan validation | Kernel'a dokunulmaz |
| Route hazırlama | Eski generation korunur |
| nftables batch | Batch uygulanmaz, hazırlık geri alınır |
| Sağlık doğrulama | Önceki generation'a rollback |
| İlk aktivasyon | Yeni RouteLane state'i tamamen temizlenir |
| Lane kaybı | Lane `failure_policy` uygulanır |
| Yetki reddi | `NeedsAuthorization`, kısmi mutation yok |
| Daemon çökmesi | Kernel korunur, yeniden başlatmada reconcile |
| Sistem açılışı | İstenen state yetki koşullarıyla yeniden uygulanır |

Helper yalnızca RouteLane nftables tablosuna, ayrılmış mark aralığına, RouteLane
routing tablolarına ve RouteLane cgroup alanına dokunabilir.

## 11. Hibrit DNS/CDN

DNS sistemi connector arayüzü kullanır:

```rust
trait DnsConnector {
    fn capabilities(&self) -> DnsCapabilities;
    async fn activate(&mut self, subscriptions: &[DomainSubscription])
        -> Result<()>;
    async fn next_event(&mut self) -> Result<DnsEvent>;
    async fn health(&self) -> DnsConnectorHealth;
}
```

İki connector bulunur:

1. `dnsmasq nftset`: Gözlenen gerçek DNS cevaplarını TTL'li nftables setlerine
   doğrudan ekler.
2. `periodic fallback`: Yalnızca aktif domainleri TTL-aware resolver ile çözer,
   güncellemeleri batch ederek helper üzerinden setlere ekler.

Akış:

```text
Domain hedefi
   ├── dnsmasq nftset → nftables sete doğrudan TTL'li ekleme
   └── periodic fallback → TTL-aware çözümleme
                              ↓
                        batch set update
                              ↓
                     nftables timeout temizliği
```

Kurallar:

- dnsmasq connector kullanılabiliyorsa tercih edilir.
- Kullanılamıyorsa fallback otomatik devreye girer.
- Wildcard subscription alt domainleri kapsar.
- Set boyutları sınırlıdır; taşma sağlık hatası üretir.
- Sağlık probunun çözdüğü adresler aktif domain setinde yoksa DNS görünürlük
  sorunu `Degraded` üretir.
- Bilinen secure DNS/DoH yapılandırması gözlemlenebiliyorsa
  `DNS_DOH_BYPASS_SUSPECTED`, gözlemlenemiyorsa `DNS_VISIBILITY_UNVERIFIED`
  raporlanır.
- Ortak CDN IP riski raporda görünürdür.
- DNS hatası IP ve CIDR kurallarını durdurmaz.

## 12. Uygulama bazlı yönlendirme

Uygulama politikası cgroup v2 ve nftables `socket cgroupv2` eşleşmesi kullanır.

İki kullanım biçimi:

1. **RouteLane ile başlat:** Daemon transient systemd user scope oluşturur,
   uygulamayı scope içinde başlatır.
2. **Çalışan uygulamayı bağla:** Aynı kullanıcıya ait seçilen PID ve istenirse
   alt süreçleri scope'a taşır.

Davranış:

- Scope'a taşınmadan önce kurulmuş bağlantılar değiştirilmez.
- Taşıma sonrasında açılan yeni socket'ler yeni politikayı kullanır.
- `include_children` aktifse yeni alt süreçler scope'u miras alır.
- Başka kullanıcıya ait süreçler attach edilemez.
- Flatpak, Snap ve container süreçleri preflight ile doğrulanır.
- Desteklenmeyen izolasyon modeli `APP_CGROUP_NOT_SUPPORTED` ile `Degraded`
  üretir.
- Uygulama kuralı pasif yapılınca scope classification kaldırılır; süreç
  sonlandırılmaz.

## 13. Otomasyon

Ağ olayları ortak connector arayüzü üzerinden gelir:

- NetworkManager D-Bus connector
- systemd-networkd connector

Olaylar:

- Arayüz eklendi/kaldırıldı
- Bağlantı aktif/pasif oldu
- VPN bağlantısı değişti
- SSID değişti
- Gateway veya adres ailesi değişti
- Zaman koşulu sınırı geçildi

Davranış:

- Ağ olayları debounce edilir.
- Her olay sonrası tüm profillerin koşulları deterministik yeniden
  değerlendirilir.
- Plan hash'i değişmediyse kernel mutation yapılmaz.
- Connector'ın sağlayamadığı koşul sessizce false sayılmaz;
  `UnsupportedCondition` üretilir.
- Manuel override otomasyonu geçersiz kılar.

## 14. Servis paketleri

Paket formatı açık, sürümlü JSON'dur:

```rust
struct ServicePack {
    schema_version: u32,
    id: ServicePackId,
    name: String,
    version: String,
    description: String,
    maintainers: Vec<String>,
    targets: Vec<ServicePackTarget>,
    recommended_action: Option<RuleActionTemplate>,
    generated_at: DateTime,
    signature: Option<PackSignature>,
}
```

Hedef türleri:

- Domain
- Wildcard domain
- IP
- CIDR

Kurallar:

- Loader tamamen açık kaynaktır.
- Yerel unsigned paket yüklenebilir; GUI ve CLI açık uyarı verir.
- Dağıtımla gelen paketler build sırasında doğrulanır.
- İmzalı paket hash ve imza doğrulanmadan aktive edilmez.
- Güncellemede hedef diff'i kullanıcıya gösterilir.
- Kullanıcıya ait profil özelleştirmeleri paket güncellemesiyle ezilmez.
- Dinamik listeler test edilmeden `Healthy` kabul edilmez.

İlk community paketleri:

- Özel/yerel ağ CIDR'leri
- AI servisleri
- Video konferans servisleri

## 15. Bağlantı Sağlığı

Sağlık durumları:

```rust
enum HealthState {
    Inactive,
    Applying,
    Healthy,
    Degraded,
    Failed,
    NeedsAuthorization,
}
```

Kontroller:

- Kernel ve kullanıcı alanı özellikleri
- nftables table, chain, set ve map bütünlüğü
- `ip rule` ve routing table eşleşmesi
- Arayüz ve gateway erişilebilirliği
- IPv4 ve IPv6 route seçimi
- DNS connector ve son gözlem zamanı
- Fallback resolver yenilemesi
- Uygulama cgroup eşleşmesi
- Süresi dolmuş veya sahipsiz RouteLane state'i

Rapor genel profil, lane ve kural seviyesinde üretilir.

Hata biçimi:

```rust
struct Diagnostic {
    code: DiagnosticCode,
    severity: Severity,
    summary: String,
    technical_detail: String,
    suggested_action: Option<SuggestedAction>,
    subject: DiagnosticSubject,
}
```

İlk sabit hata kodları:

```text
DNS_DOH_BYPASS_SUSPECTED
DNS_VISIBILITY_UNVERIFIED
LANE_INTERFACE_MISSING
LANE_GATEWAY_UNREACHABLE
NFTABLES_FEATURE_MISSING
POLICY_APPLY_ROLLED_BACK
ROUTING_RESOURCE_CONFLICT
APP_CGROUP_NOT_ATTACHED
APP_CGROUP_NOT_SUPPORTED
UNSUPPORTED_ACTIVATION_CONDITION
```

GUI kısa kullanıcı mesajını, CLI insan okunabilir veya JSON çıktıyı, tanılama
paketi ise hassas alanları maskelenmiş teknik ayrıntıyı gösterir.

Varsayılan sağlık kontrolü üçüncü taraf dış-IP servisine bağlanmaz.

## 16. Test stratejisi

| Katman | Kapsam |
| --- | --- |
| Unit | Öncelik, koşul, migration, paket doğrulama |
| Planner golden | Deterministik `KernelPlan` |
| Backend render | nftables, route ve policy rule çıktısı |
| Helper güvenlik | İsim alanı, mark, table, UID ve operation |
| Namespace integration | IPv4/IPv6, block, failover ve rollback |
| DNS integration | TTL, rotasyon, wildcard ve iki connector |
| Application integration | cgroup'a bağlı yeni socket routing |
| IPC contract | Protokol sürümü, request ve event |
| Packaging | Kurulum, yükseltme ve kaldırma |
| Fault injection | Transaction'ın her adımında rollback |

Normal `cargo test` root gerektirmez. Root entegrasyon testleri ayrı özellik ve
CI işi olarak çalışır.

CI seviyeleri:

- Her değişiklikte unit, planner, lint, build ve paketleme
- Nightly QEMU/KVM matrisinde systemd, D-Bus, Polkit, nftables ve namespace
  testleri

### 16.1 Faz kabul kriterleri

#### Faz 1 — Routing çekirdeği

- Aynı anda en az üç lane
- Route ve Direct eylemleri
- Aktif/pasif Block kuralı
- IPv4 ve IPv6
- Başarısız uygulamada önceki generation'a dönüş
- Bağlantı Sağlığı kernel state doğrulaması

#### Faz 2 — DNS/CDN

- DNS cevabı TTL'li sete girer ve süresi dolunca çıkar.
- Fallback ve dnsmasq connector aynı sözleşme testlerini geçer.
- Wildcard alt domainleri kapsar.
- DoH şüphesi `Degraded` üretir.

#### Faz 3 — Profil, CLI ve otomasyon

- GUI ve CLI aynı daemon state'ini görür.
- Manuel override deterministiktir.
- Ağ değişikliğinde profil reconcile edilir.
- Eski config kayıpsız migrate edilir.

#### Faz 4 — Uygulama yönlendirme

- RouteLane ile başlatılan uygulamanın ilk bağlantısı doğru lane'i kullanır.
- Çalışan uygulamanın yeni bağlantıları attach sonrasında politikayı kullanır.
- Alt süreç davranışı `include_children` ile doğrulanır.

#### Faz 5 — Paketler ve dağıtım

- Community paketleri şema ve imza testlerinden geçer.
- Resmî matrisin tamamında temiz kurulum, yükseltme ve kaldırma geçer.
- Kaldırma RouteLane state'ini temizler ve başka firewall state'ine dokunmaz.

## 17. Paketleme

Paket içerikleri:

- `routelane`
- `routelanectl`
- `routelane-daemon`
- `routelane-helper`
- systemd user daemon unit'i
- system D-Bus helper activation unit'i
- Polkit policy
- Desktop entry ve ikon
- Community servis paketleri
- Man sayfaları ve lisans

Dağıtımlar:

- Ubuntu/Debian için `.deb`
- Fedora için `.rpm`

Kurulum script'leri nftables, systemd, D-Bus ve seçili DNS connector
yeteneklerini preflight ile denetler. dnsmasq zorunlu bağımlılık değildir;
bulunmadığında periodic fallback kullanılır.

Kaldırma:

- RouteLane systemd ve D-Bus unit'lerini kaldırır.
- RouteLane nftables table, route ve policy rule'larını temizler.
- Başka uygulamaların firewall veya routing state'ine dokunmaz.
- Kullanıcı profilleri standart paket kaldırmada korunur; purge işleminde
  açıkça talep edilirse kaldırılır.

## 18. Lisans ve güvenlik

İstemci, daemon, helper ve yerel özellikler GNU GPLv3 altında yayımlanır.
README, `LICENSE`, Cargo metadata ve paket metadata'sı aynı lisansı göstermelidir.

Helper için:

- Tehdit modeli yazılır.
- D-Bus ve Polkit yüzeyi belgelenir.
- Serbest komut yürütme bulunmaz.
- Tüm yollar, arayüzler, mark'lar, tablolar ve boyutlar doğrulanır.
- Hassas log alanları varsayılan olarak maskelenir.
- Bağımsız güvenlik incelemesine uygun küçük bir API korunur.

## 19. Başarı tanımı

Tasarım tamamlanmış sayılır:

1. Beş fazın kabul kriterleri resmî matrisin tamamında geçiyorsa,
2. normal testler root olmadan çalışıyorsa,
3. helper RouteLane isim alanı dışına mutation yapamıyorsa,
4. başarısız plan önceki çalışan state'i koruyorsa,
5. GUI ve CLI aynı daemon state'ini gösteriyorsa,
6. mevcut kullanıcı konfigürasyonu kayıpsız migrate ediliyorsa,
7. paket kaldırma başka firewall/routing state'ine dokunmuyorsa.
