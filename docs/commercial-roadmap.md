# RouteLane Ticari Ürün Yol Haritası

**Durum:** Taslak  
**Tarih:** 28 Temmuz 2026  
**Kapsam:** Ürün konumlandırması, teknik öncelikler, gelir modeli ve ilk 90 günlük uygulama planı

## 1. Yönetici özeti

RouteLane, yalnızca bir Linux split-tunneling aracı olarak değil, **VPN
sağlayıcısından bağımsız bir trafik politika yöneticisi** olarak
konumlandırılmalıdır.

Önerilen temel ürün vaadi:

> Hangi uygulamanın veya servisin hangi VPN ya da ağ bağlantısından çıkacağını
> otomatik ve doğrulanabilir biçimde yönet.

Domain, IP ve CIDR yönlendirme tek başına sürdürülebilir bir ticari farklılık
yaratmaz. RouteLane'in savunulabilir değeri aşağıdaki bileşimden gelmelidir:

- Birden fazla VPN ve ağ arayüzünü aynı anda yönetme
- CDN ve değişken DNS yanıtlarında güvenilir yönlendirme
- Teknik kural listeleri yerine hazır servis profilleri
- Ağ değişikliklerine göre otomatik aktivasyon ve hata kurtarma
- Trafiğin gerçekten beklenen hattan çıktığını doğrulama
- Takımlar için merkezi politika dağıtımı ve denetim

## 2. Hedef kullanıcı ve çözülecek iş

### 2.1 Birincil hedef kitle

İlk hedef kitle, Linux kullanan geliştiriciler, DevOps ekipleri ve uzaktan
çalışan teknik kullanıcılardır. Bu kullanıcılar genellikle aynı cihazda şirket
VPN'i, kişisel VPN, yerel ağ, Docker ağları ve doğrudan internet bağlantısı
kullanır.

Temel ihtiyaçları şudur:

> VPN'i sürekli açıp kapatmadan, belirli iş yüklerini doğru bağlantıya güvenli
> ve tekrarlanabilir biçimde yönlendirmek.

### 2.2 İlk kullanım senaryoları

- Şirket içi GitLab, registry ve API'leri şirket VPN'ine yönlendirme
- AI servislerini belirli bir VPN veya çıkış ülkesinden kullanma
- Video konferans trafiğini düşük gecikmeli doğrudan bağlantıya verme
- Bankacılık servislerini VPN dışında tutma
- Docker ve yerel ağ trafiğini kurumsal VPN etkisinden koruma
- Belirli uygulamaları gizlilik VPN'ine gönderme veya tamamen engelleme

### 2.3 İlk aşamada hedeflenmemesi gereken alanlar

- Yeni bir VPN altyapısı veya VPN abonelik hizmeti kurmak
- Tailscale veya Cloudflare One kapsamındaki tam Zero Trust platformuyla
  rekabet etmek
- Aynı anda Linux, Windows, macOS ve mobil istemciler geliştirmek
- Ürün-pazar uyumu doğrulanmadan kapsamlı bir bulut kontrol paneli kurmak

RouteLane ilk aşamada kullanıcının mevcut WireGuard, OpenVPN, kurumsal VPN veya
fiziksel bağlantılarının üzerinde çalışan bağımsız politika katmanı olmalıdır.

## 3. Pazar konumu ve farklılaşma

Linux'ta uygulama, hostname ve IP tabanlı split tunneling artık tek başına
benzersiz değildir. Örneğin Windscribe, Linux dahil desteklediği platformlarda
uygulama ve hostname/IP tabanlı include/exclude yönlendirme sunmaktadır.

RouteLane şu alanlarda ayrışmalıdır:

1. **VPN sağlayıcısından bağımsızlık:** Kullanıcı tek bir VPN markasına
   bağlanmamalıdır.
2. **Multi-Lane yönlendirme:** Trafik yalnızca “VPN veya doğrudan” arasında
   değil, birden fazla arayüz arasında dağıtılabilmelidir.
3. **Doğrulanabilirlik:** Ürün yalnızca kural yazmamalı, gerçek rotayı ve olası
   sızıntıları da kontrol etmelidir.
4. **Hazır ve güncel servis paketleri:** Kullanıcı bir servisin tüm teknik
   domain bağımlılıklarını bilmek zorunda kalmamalıdır.
5. **Yerel öncelikli mimari:** Trafik içeriği RouteLane sunucularına
   gönderilmemeli; bulut özellikleri politika ve cihaz yönetimiyle sınırlı
   kalmalıdır.

İlgili pazar referansları:

- [Windscribe Split Tunneling](https://windscribe.com/features/split-tunneling)
- [Cloudflare One Split Tunnels](https://developers.cloudflare.com/cloudflare-one/team-and-resources/devices/cloudflare-one-client/configure/route-traffic/split-tunnels/)
- [Tailscale fiyatlandırması](https://tailscale.com/pricing)
- [NetBird fiyatlandırması](https://netbird.io/pricing)

## 4. Ürün öncelikleri

| Öncelik | Yatırım | Ticari etkisi | Yaklaşık zorluk |
| --- | --- | --- | --- |
| P0 | Güvenilir domain/CDN yönlendirme | Temel ürün vaadini güvenilir kılar | Yüksek |
| P0 | Bağlantı doğrulama ve hata kurtarma | Güven ve destek maliyetini iyileştirir | Orta |
| P1 | Çoklu arayüz ve profil sistemi | RouteLane'i standart VPN özelliğinden ayırır | Yüksek |
| P1 | Hazır servis paketleri | Kurulumu tek tıklamaya indirir | Orta |
| P1 | Uygulama bazlı yönlendirme | Pazar beklentisini karşılar | Yüksek |
| P1 | Otomatik aktivasyon | Günlük kullanım ve bağlılığı artırır | Orta |
| P2 | Takım politikaları | Abonelik gelirini mümkün kılar | Yüksek |
| P2 | OEM/white-label ağ motoru | B2B lisanslama kanalı açar | Yüksek |

### 4.1 P0 — Güvenilir domain ve CDN yönlendirme

Mevcut yaklaşım domainleri IP adreslerine çözerek kernel kurallarına dönüştürür.
Bu yaklaşım kararlı DNS yanıtlarında çalışsa da CDN kullanılan servislerde IP
adresleri zamanla ve konuma göre değişebilir.

Üretim seviyesine geçmek için:

- Aktif yönlendirme sırasında DNS değişiklikleri güvenli biçimde uygulanmalı
- Eski ve yeni IP'ler için kontrollü yaşam süresi tanımlanmalı
- Wildcard domain kuralları desteklenmeli
- Sistem DNS'i, tarayıcı secure DNS/DoH ve DNS önbelleği davranışları tespit
  edilerek kullanıcı uyarılmalı
- Ortak CDN IP'lerinin istenmeyen trafiği eşleştirme riski görünür olmalı
- Seçilen DNS entegrasyonu deneysel olmaktan çıkarılıp test edilen tek bir
  üretim backend'i haline getirilmeli

Başarı ölçütü:

- Desteklenen servis paketlerinde 24 saatlik kullanım boyunca yanlış arayüze
  çıkan yeni bağlantı oranı ölçülebilir testlerde sıfıra yakın olmalı

### 4.2 P0 — Doğrulama, sağlık kontrolü ve kurtarma

Kullanıcıya yalnızca “aktif” durumu göstermek yeterli değildir. RouteLane
uyguladığı politikanın etkisini doğrulamalıdır.

Gerekli yetenekler:

- Hedefin kernel tarafından hangi arayüze yönlendirildiğini gösterme
- IPv4, IPv6 ve DNS yollarını ayrı ayrı kontrol etme
- VPN/gateway erişilemezse anlaşılır hata ve düzeltme önerisi sunma
- Uygulama çökmesi veya sistem kapanması sonrası kalan kuralları temizleme
- VPN yeniden bağlandığında kuralları otomatik ve atomik biçimde yeniden kurma
- Tanılama paketi üretme; hassas bilgileri varsayılan olarak maskeleme

Bu özellik arayüzde teknik bir “route test” aracı olarak değil, kullanıcı
diline yakın bir **Bağlantı Sağlığı** görünümü olarak sunulmalıdır.

### 4.3 P1 — Multi-Lane kural motoru

Her kural bağımsız bir hedef arayüze, moda ve önceliğe sahip olmalıdır.

Önerilen kural modeli:

- Kaynak: tüm sistem, uygulama, kullanıcı, süreç grubu veya profil
- Hedef: domain, wildcard domain, IP, CIDR veya servis paketi
- Eylem: belirli arayüze yönlendir, doğrudan bağlan veya engelle
- Öncelik: çakışan kurallarda açık ve görünür sıralama
- Failover: birincil arayüz yoksa ikincil arayüz ya da bağlantıyı engelleme
- Aktivasyon koşulu: VPN bağlantısı, Wi-Fi ağı, konum veya zaman aralığı

Örnek profil:

| Trafik | Eylem |
| --- | --- |
| Şirket GitLab ve registry | `wg-work` |
| AI servis paketi | `tun-privacy` |
| Zoom ve Teams | Doğrudan |
| Yerel ağ CIDR'leri | Doğrudan |
| Telemetri listesi | Engelle |

### 4.4 P1 — Hazır servis paketleri

Kullanıcıların domain listeleri yerine amaç seçmesi sağlanmalıdır:

- Microsoft 365 doğrudan
- Şirket kaynakları WireGuard üzerinden
- AI servisleri VPN üzerinden
- Video konferans düşük gecikmeli hat üzerinden
- Yerel ağ her zaman doğrudan

Servis paketleri:

- İnsan tarafından okunabilir içerik göstermeli
- Sürüm numarasına sahip olmalı
- İmzalı ve doğrulanabilir olmalı
- Kullanıcı değişikliklerini ezmeden güncellenebilmeli
- Son güncelleme tarihi ve değişiklik özetini göstermeli

Paketlerin sürekli bakımı, Pro aboneliği için gerçek ve devamlı bir değer
oluşturabilir.

### 4.5 P1 — Uygulama bazlı yönlendirme

Uygulama bazlı split tunneling pazarın temel beklentilerinden biridir. Ancak
RouteLane bunu yalnızca “VPN'i atla” şeklinde değil, uygulamayı herhangi bir
mevcut arayüze yönlendirecek biçimde sunmalıdır.

İlk açık kaynak uygulaması tamamlandı:

- `.desktop` girdilerinden program seçimi ve lane eşleştirmesi
- RouteLane cgroup'u içinde yeni program başlatma
- Halihazırda açık, yürütülebilir yolu doğrulanan süreçleri aynı cgroup'a alma
- Uygulamanın CDN ve doğrulama trafiğini domain tahmini yapmadan aynı lane'de
  tutma
- Polkit, çağıran UID, PID başlangıç zamanı ve `/proc/PID/exe` doğrulaması
- GTK içinde etkin/pasif durum, lane seçimi ve açık başarı/hata geri bildirimi

Flatpak ve Snap desteği host/sandbox aracılığı gerektirdiği için ayrı uyumluluk
çalışmasıdır; desteklenmeyen durumda trafik sessizce farklı bir lane'e
çıkarılmamalıdır.

İlk sürüm için:

- Çalışan ağ uygulamalarını keşfetme
- Masaüstü girdilerinden uygulama seçme
- Uygulama yeniden başladığında kuralı koruma
- Alt süreçlerin davranışını açıkça tanımlama
- Flatpak/Snap ve container uygulamalarında sınırlamaları belirtme

## 5. Gelir modeli

Önerilen açık kaynak sınırı şu ilkedir:

> Tek kullanıcı, tek cihaz ve tamamen yerel çalışan RouteLane eksiksiz açık
> kaynak; cihazlar ve kişiler arası yönetim ile sürekli işletilen hizmetler
> ücretli.

Routing ve güvenlik yazılımında yerel çekirdeğin açık olması, kod katkısından
önce güven ve benimsenme sağlar. Ticari değer, temel doğruluğu ücretli yapmak
yerine sürekli güncellenen içerik, senkronizasyon, takım yönetimi ve kurumsal
işletim katmanlarında oluşturulmalıdır.

### 5.1 Açık kaynak sınırı

| Özellik | Açık kaynak | Ücretli |
| --- | :---: | :---: |
| Routing engine ve privileged helper | ✓ | |
| DNS/CDN backend'i | ✓ | |
| Domain, wildcard, IP ve CIDR kuralları | ✓ | |
| Multi-Lane ve çoklu arayüz | ✓ | |
| Include, exclude ve block | ✓ | |
| Uygulama bazlı yönlendirme | ✓ | |
| Yerel otomatik aktivasyon | ✓ | |
| Bağlantı Sağlığı, leak ve failover kontrolü | ✓ | |
| GUI, CLI, profil ve import/export | ✓ | |
| Yerel tanılama ve audit kaydı | ✓ | |
| Servis paketi formatı | ✓ | |
| Başlangıç ve community servis paketleri | ✓ | |
| Premium ve sürekli doğrulanan paket akışı | | ✓ |
| Şifreli profil senkronizasyonu ve yedekleme | | ✓ |
| Merkezi cihaz ve politika yönetimi | | ✓ |
| Kurumsal audit ve trafik olayları | | ✓ |
| SSO, SCIM, MDM ve SIEM entegrasyonları | | ✓ |
| HA, çoklu tenant ve kurumsal self-hosting | | ✓ |
| SLA ve öncelikli destek | | ✓ |
| OEM/white-label kullanım hakkı | | ✓ |

P0 güvenilirlik özellikleri ile P1 yerel ürün özellikleri ücret duvarının
arkasına konulmamalıdır. Bunlar premium kolaylıklar değil, RouteLane'in
güvenilirliğini ve farklılığını kanıtlayan çekirdektir.

Bu ayrım için uygulanabilecek karar ilkesi:

- Özelliğin birincil alıcısı bireysel teknik kullanıcıysa açık kaynak
- Özelliğin birincil alıcısı yönetici, güvenlik ekibi veya organizasyonsa
  ücretli
- Güvenlik güncellemeleri ve temel doğruluk hiçbir zaman ödeme durumuna
  bağlanmamalı

Bu yaklaşım, GitLab'ın alıcı temelli open-core modelindeki “özelliği en çok
önemseyen alıcı kim?” prensibiyle uyumludur:
[GitLab open-core yaklaşımı](https://handbook.gitlab.com/handbook/company/stewardship/).

### 5.2 Community

- Eksiksiz yerel routing ürünü
- Sınırsız yerel kural ve profil
- Domain, wildcard, IP ve CIDR hedefleri
- Multi-Lane ve uygulama bazlı yönlendirme
- Include, exclude ve block eylemleri
- Yerel otomatik aktivasyon
- Bağlantı Sağlığı ve tanılama
- CLI, import/export ve community servis paketleri
- Topluluk desteği

Amaç, güven oluşturmak, katkı almak ve ücretli sürüm için doğal bir edinme
kanalı yaratmaktır.

### 5.3 Pro/Supporter

Önerilen özellikler:

- Premium ve sürekli doğrulanan servis paketleri
- Şifreli cihazlar arası profil eşitleme
- Bulut yedekleme ve profil geçmişi
- Gelişmiş güncelleme kanalı
- Öncelikli kullanıcı desteği

Test edilecek fiyatlar:

- Yıllık **€29–49**
- veya premium paket akışı için daha düşük fiyatlı bağımsız abonelik
- Pro hizmetlerine ihtiyaç duymayan kullanıcılar için isteğe bağlı tek seferlik
  destekçi lisansı

Pro katmanı, yerel istemcinin çalışmasını lisans anahtarına bağlamamalıdır.
Ücretli haklar ayrı hizmetler olarak tanımlanmalıdır. Nihai fiyat, ücretli beta
dönüşümü ve kullanıcı görüşmeleriyle belirlenmelidir.

### 5.4 Team

Önerilen özellikler:

- Merkezi ve imzalı politika dağıtımı
- Cihaz envanteri ve durum görünürlüğü
- Yapılandırma audit logları
- CLI ve headless agent
- Rol tabanlı yönetim
- Politika sürümleme ve geri alma
- Öncelikli destek

Başlangıç fiyat deneyi:

- Kullanıcı başına aylık **€5–8**

RouteLane bu fiyat düzeyini ancak merkezi yönetim, denetim ve destek sağladığında
haklı çıkarabilir.

### 5.5 Enterprise ve OEM

- Self-hosted kontrol düzlemi
- SSO/SCIM
- SIEM ve webhook entegrasyonları
- SLA ve kurumsal destek
- Özel servis paketleri
- VPN üreticilerine gömülebilir motor veya white-label istemci

OEM kanalı, güvenilir ağ motoru kanıtlandıktan sonra az sayıda yüksek değerli
müşteriyle gelir üretme fırsatı sunabilir.

VPN üreticileri açık kaynak istemciyi kendi kapalı ürünlerine gömmek isterse
ayrı bir ticari OEM lisansı sunulabilir. Çift lisanslamanın sürdürülebilmesi
için dış katkılar başlamadan önce telif sahipliği, katkı sözleşmesi ve bağımlılık
uyumluluğu hukuk danışmanıyla netleştirilmelidir.

### 5.6 Mimari sınır

Önerilen kod ve hizmet ayrımı:

- **Açık istemci:** Engine, helper, DNS backend'i, GUI, CLI ve yerel özellikler
- **Açık ekosistem:** Servis paketi şeması, community paketleri ve entegrasyon
  test araçları
- **Kapalı hizmet:** Hesap, ödeme, şifreli senkronizasyon, premium paket
  kataloğu ve güncelleme servisi
- **Kapalı kontrol düzlemi:** Organizasyon, cihaz, politika, audit, kurumsal
  entegrasyonlar ve çoklu tenant işletimi
- **Ticari dağıtım:** Enterprise self-hosted kontrol düzlemi ve OEM lisansı

İstemci ile kontrol düzlemi arasındaki protokol belgelenebilir; kontrol
düzleminin uygulama kodunun ilk aşamada açık olması gerekmez. Benzer şekilde
Tailscale, istemci ve daemon kodunun büyük kısmını açarken yönetilen
koordinasyon, yönetişim ve destek katmanını ticari hizmet olarak işletmektedir:
[Tailscale açık kaynak yaklaşımı](https://tailscale.com/opensource/).

### 5.7 Lisans kararı

Depoda şu anda giderilmesi gereken bir lisans tutarsızlığı vardır:

- `LICENSE` dosyası GNU GPLv3 metni içeriyor
- `README.md` lisansı MIT olarak tanımlıyor

Yeni sürüm veya ücretli beta yayımlanmadan önce tek bir lisans kararı verilmeli,
README, paket metadata'sı ve kaynak dosya bildirimleri bununla uyumlu hale
getirilmelidir.

Önerilen yapı:

- İstemci, engine, helper ve yerel özellikler: **GPLv3**
- Bulut kontrol düzlemi ve kurumsal backend: **kapalı/ticari**
- Servis paketi şeması ve community paketleri: **açık**
- Premium doğrulanmış katalog ve güncelleme hizmeti: **ücretli**
- OEM kullanımı: **ayrı ticari lisans**

GPLv3 altında yazılım ücret karşılığı dağıtılabilir; ancak yazılımı alanların
kaynak koda erişim, değiştirme ve yeniden dağıtma hakları devam eder. Bu nedenle
GPL çekirdeğinden türetilen kapalı bir “Pro Desktop binary” yerine ayrı ağ
hizmetleri ve yönetim katmanlarının ücretlendirilmesi daha temiz bir sınırdır.
Lisansın kesin uygulaması için hukuk danışmanlığı alınmalıdır:
[GNU GPL SSS](https://www.gnu.org/licenses/gpl-faq.html.en).

## 6. Dağıtım ve güven

Ticari ağ yazılımında paketleme ve güncelleme, ürün özelliğinin bir parçasıdır.

Gerekli çalışmalar:

- Markaya ait ikon ve tutarlı uygulama kimliği
- Gerçek maintainer ve destek bilgileri
- İmzalı APT deposu
- Fedora/RPM paketi
- Desteklenen dağıtım ve sürüm matrisi
- Otomatik güncelleme bildirimi
- CI üzerinde test, paket ve sürüm üretimi
- SBOM ve bağımlılık güvenlik taraması
- Privileged helper için tehdit modeli ve bağımsız güvenlik incelemesi
- Sürüm notları, geri alma ve hata bildirim süreci
- README, LICENSE ve paket metadata'sında tek ve tutarlı lisans bildirimi

Flatpak ve Snap desteği, privileged helper ve host network erişimi nedeniyle
ayrı bir teknik değerlendirme sonrasında ele alınmalıdır.

## 7. İlk 90 günlük uygulama planı

### Gün 1–30: Güven temeli

- Aktif kullanımda domain/CDN yenilemesini tamamla
- Ağ ve VPN bağlantı değişikliklerini olay bazlı algıla
- Bağlantı Sağlığı ekranını tasarla
- IPv4, IPv6, DNS ve failover testlerini ekle
- Network namespace tabanlı entegrasyon testleri oluştur
- CI ve tekrarlanabilir paket üretimini kur
- Gerçek ikon, maintainer ve destek bilgilerini tamamla
- İmzalı beta `.deb` paketi yayımla

Çıkış kriterleri:

- Temel ağ senaryoları otomatik testlerle doğrulanıyor
- Hatalı aktivasyon sistemi yarı yapılandırılmış halde bırakmıyor
- Pilot kullanıcı tanılama çıktısıyla sorunu raporlayabiliyor

### Gün 31–60: Açık ürün temeli ve Pro hizmet prototipi

- Çoklu arayüz kural modelini tamamla
- Include, exclude ve block eylemlerini ekle
- Profil oluşturma, çoğaltma, import/export ekle
- Ağ/VPN bağlantısına göre otomatik aktivasyon ekle
- İlk üç hazır servis paketini yayımla
- CLI veya headless agent prototipini oluştur
- Uygulama bazlı yönlendirmeyi desteklenen host uygulamaları için tamamla
- Premium paket akışı ve şifreli senkronizasyon için hizmet sınırını tanımla

Çıkış kriterleri:

- Bir kullanıcı aynı anda en az iki farklı VPN/arayüz için politika kurabiliyor
- Yeni kullanıcı örnek profili belgeleri okumadan etkinleştirebiliyor
- Pilot kullanıcılar ürünü günlük iş akışında en az iki hafta kullanabiliyor
- Açık istemci, hesap veya lisans anahtarı olmadan eksiksiz yerel çalışabiliyor

### Gün 61–90: Pazar ve fiyat doğrulaması

- En az 20 hedef kullanıcı görüşmesi yap
- En az 3 küçük teknik takımda pilot başlat
- Pro hizmetleri için iki fiyat seçeneğiyle ücretli beta testi yap
- Yerel açık kaynak çekirdeği kısıtlamayan abonelik ve ödeme akışını kur
- Gizlilik korumalı ve açık rızaya dayalı ürün metriklerini ekle
- Pilot geri bildirimlerine göre B2C Pro veya B2B Team odağını seç

Çıkış kriterleri:

- Kullanıcıların en sık çözdüğü ilk iki senaryo belirlenmiş
- Pilotların en az biri ödeme veya satın alma niyeti göstermiş
- Teknik destek yükünün ana kaynakları ölçülmüş
- Sonraki çeyreğin ürün odağı veriye dayanarak seçilmiş

## 8. Başarı metrikleri

### Ürün

- İlk profil oluşturma süresi
- Başarılı ilk aktivasyon oranı
- Yanlış arayüze çıkan bağlantı oranı
- Haftalık aktif profil sayısı
- Otomatik düzelen ağ değişikliği oranı
- Kullanıcı başına destek talebi

### Büyüme

- İndirmeden başarılı aktivasyona dönüşüm
- 4 haftalık kullanıcı devamlılığı
- Community sürümünden Pro denemesine geçiş
- Denemeden ücretli lisansa dönüşüm
- Takım başına aktif cihaz ve kullanıcı sayısı

### Ticari doğrulama

- Ödeme niyeti belirten görüşme oranı
- Ücretli beta müşterisi sayısı
- Aylık kullanıcı başına gelir
- Müşteri edinme maliyeti
- Destek ve altyapı sonrası brüt marj

İlk aşamada toplam indirme veya GitHub yıldızı, başarılı aktivasyon ve devamlılık
metriklerinin önüne geçirilmemelidir.

## 9. Temel riskler ve azaltma planı

| Risk | Etki | Azaltma |
| --- | --- | --- |
| CDN ve ortak IP'ler yanlış trafik eşleştirir | Güven kaybı | DNS entegrasyonu, servis paketi testi ve sağlık uyarıları |
| VPN kopunca trafik yanlış hattan çıkar | Güvenlik riski | Açık failover/kill politikası ve atomik rollback |
| Dağıtım farklılıkları destek yükünü artırır | Maliyet artışı | Dar destek matrisi ve otomatik tanılama |
| Ürün VPN istemcilerindeki ücretsiz özelliklerle karıştırılır | Düşük ödeme isteği | Multi-Lane, bağımsızlık ve otomasyon mesajı |
| Bulut paneli erken geliştirilir | Zaman ve sermaye kaybı | Önce yerel Pro beta ve tasarım ortakları |
| Privileged helper güvenlik açığı taşır | Kritik itibar riski | Tehdit modeli, sıkı yetki sınırı ve bağımsız inceleme |

## 10. Karar ilkeleri

Yeni bir özellik aşağıdaki sorulardan en az birine güçlü biçimde “evet” cevabı
vermiyorsa önceliklendirilmemelidir:

1. Yönlendirmenin doğruluğunu veya güvenliğini artırıyor mu?
2. Kullanıcının teknik ağ bilgisi ihtiyacını azaltıyor mu?
3. Birden fazla ağ/VPN kullanma deneyimini belirgin biçimde iyileştiriyor mu?
4. Sürekli güncellenen veya takımca yönetilen bir değer üretiyor mu?
5. Ölçülebilir biçimde ödeme isteğini veya kullanıcı devamlılığını artırıyor mu?

Bu yaklaşım RouteLane'in kapsamını kontrollü tutarken, teknik olarak güçlü bir
araçtan sürdürülebilir ticari ürüne dönüşmesini hedefler.
