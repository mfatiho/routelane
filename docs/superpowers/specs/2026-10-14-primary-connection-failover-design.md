# Ana bağlantı ve otomatik yardımcı hat geçişi

Tarih: 14 Ekim 2026.
Durum: Kullanıcının “devam et” yanıtıyla tasarım onaylandı; plan Task 1–12 main üzerinde uygulandı (15 Ekim 2026).
İlk sürüm kapısı açık değil: kanıt ve NOT VERIFIED satırları `docs/primary-failover-verification.md`.
Bu belge ürün kodunu veya canlı ağ ayarlarını değiştirmez.
Plan: `docs/superpowers/plans/2026-10-14-primary-connection-failover.md`.

## 1. Amaç ve onaylanan davranış

Kullanıcı Ethernet'i ana bağlantı, VINN'i yardımcı bağlantı olarak kullanmak istiyor.
Özel kurallarla VINN'e atanmış trafik VINN'den, diğer internet trafiği ana hattan çıkar.
Ana hat kesildiğinde bütün yönlendirilebilir yeni internet bağlantıları otomatik
olarak kullanılabilir yardımcı hatta geçer. Başlangıçtaki ana hat seçimi işletim
sisteminin varsayılan bağlantısıdır. Kullanıcının ana hattı değiştirmesi önce
uyarı ve açık onay gerektirir; iptal hiçbir ayar veya yönlendirme değiştirmez.

Ana bağlantı seçimi, link rolleri, olay izleme ve yetkili otomatik geçiş mevcut
UI/daemon/helper sınırlarını birlikte etkilediğinden çalışma mimari değişikliktir.
Önce yazılı tasarım, ardından kullanıcı incelemesi ve uygulama planı gerekir.

## 2. Yaklaşım seçimi

İki yaklaşım değerlendirildi:

- İşletim sisteminin kalıcı bağlantı önceliklerini değiştirmek: RouteLane kapalıyken
  de etkili olur; NetworkManager/diğer ağ yöneticilerinin ayarlarına müdahale eder.
- Ana hat tercihini RouteLane'in kendi politikasında tutmak: işletim sisteminin
  mevcut varsayılanı başlangıç tercihini sağlar, manuel seçim yalnız RouteLane
  etkinliğinde uygulanır; kapatma kendi kaynaklarını kaldırır.

Önerilen tasarım ikinci yaklaşımdır. Sistem varsayılanını başlangıçta kullanmak,
NetworkManager'ın kalıcı route metric veya DNS ayarlarını değiştirmek anlamına gelmez.
14 Ekim salt okunur kontrolünde sistem varsayılanı VINN arayüzü
`enxce496090f74a` idi; bu durumda başlangıçta VINN ana görünür. Ethernet için
kullanıcının `enp128s31f6` bağlantısını ana seçmesi ve onaylaması gerekir.

## 3. Terimler ve kalıcı seçim

Birbirine karıştırılmayacak üç durum vardır:

- Sistem varsayılanı: RouteLane kaynakları dışında işletim sisteminin seçtiği çıkış.
- Tercih edilen ana bağlantı: kullanıcının onayladığı bağlantı veya sistem varsayılanını izle tercihi.
- Şu an kullanılan ana çıkış: ana bağlantı ya da kesinti nedeniyle seçilen yardımcı bağlantı.

Failover, tercih edilen ana bağlantıyı değiştirmez; yalnız etkin çıkış değişir.
Lane'in ID'sinin `default` olması, adının Ethernet/VINN olması veya listenin ilk
elemanı olması rol tayin etmez. Bütün seçimler stable LaneId/arayüz kimliğiyle yapılır.

Profile politikası ana seçimini, sıralı yardımcı LaneId listesini ve otomatik
geçiş tercihini taşır. Yardımcı sıra ana hattı veya aynı lane'i iki kere içeremez.
İki bağlantıda tek yardımcı otomatik önerilir; daha çok bağlantıda sıra UI'da görünür.
Tek kullanılabilir bağlantı başlangıçta otomatik seçilebilir. Manuel ana seçim
kaydedilmeden, ad veya arayüz değişiminden yanlış bir başka lane'e çevrilmez.

Bu semantik eski programın sessizce yok sayabileceği bir optional alan olarak
ConfigV2'ye eklenmez. Yeni schema sürümü ve V2/legacy'den açık migrasyon gerekir;
mevcut kurallar, isimler, lane/rule ID'leri, dil ve launcher argümanları korunur.
Eski kayıtlar sistem varsayılanı tercihiyle açılır; yükleme tek başına aktivasyon yapmaz.

## 4. Trafik kararı

1. Loopback, yerel servisler ve bağlı yerel ağ erişimi güvenli kapsamda korunur.
   Kullanıcının açık yerel hedef Block/Route kuralları ayrıca değerlendirilir.
2. Etkin açık kurallar mevcut yürütme önceliklerine göre değerlendirilir.
3. Route kuralı atanmış hatta gider; Block her durumda Block olarak kalır.
4. Direct, bu yeni politikada ana internet çıkışını kullanır; OS-default'a kaçış
   anlamına gelmez. UI metni bu anlamı açıkça anlatır; eski semantik migrasyonda belirtilir.
5. Hiçbir açık kuralla eşleşmeyen internet trafiği etkin ana çıkışa gider.

Ana bağlantı sağlıklıyken VINN kuralı VINN'de kalır. Ana bağlantı kullanılamadığında
eşleşmeyen/Direct trafik sıradaki kullanılabilir yardımcı hatta geçer. VINN kesilirse
VINN'e atanmış kuralların mevcut açık fallback ve Block/Direct/Inactive politikaları
korunur; failover kullanıcının Block kararını veya yardımcı hattın korumasını kaldırmaz.
Hiçbir kullanılabilir çıkış yoksa bu politikaya tabi internet trafiği engellenir;
rastgele başka bir sistem bağlantısına sessiz kaçış yapılmaz.

IPv4 ve IPv6 ayrı değerlendirilir. Seçili hatta ilgili aile yoksa onaylanmış
yardımcı sıra kullanılır; yoksa o aile engellenir. “Ana hat IPv4 çalışıyor”
gerekçesiyle IPv6'nın başka bir arayüzden kontrolsüz çıkmasına izin verilmez.

## 5. Otomatik geçiş ve geri dönüş önerisi

Otomatik geçiş kullanıcı tarafından istenmiştir; kesinti anında onay penceresi
açılmaz. UI/tray “Ana bağlantı kullanılamıyor; VINN kullanılıyor” bildirimini verir.
Bu kullanımın mobil veri tüketebileceği ilk etkinleştirme açıklamasında gösterilir.

Bu sürümün kesinti sinyali yerel kernel kanıtıdır: arayüz/link/carrier, adres
ve gerekli default route'un kaybolması. Yeni harici IP servisi, sürekli internet
probu veya HTTPS trafiğini inceleme eklenmez. Kablo bağlıyken upstream internet
kesintisinin eksiksiz tespiti bu yerel sinyalle garanti edilmez.

Önerilen geri dönüş: tercih edilen ana hat gerekli yerel kanıtla 5 saniye kesintisiz
hazır kaldığında yeni bağlantılar yeniden ana hatta alınır. Mevcut yardımcı hat
bağlantıları gereksiz yere kesilmez; tekrar tekrar hat değiştirme önlenir.
Kullanıcının manuel ana hat değişikliği ise her zaman ayrı uyarı/onay gerektirir.

Mevcut TCP/QUIC oturumlarının farklı uplink/public IP'ye kopmadan taşındığı iddia
edilmez. Kesilen ana hat üzerindeki oturumlar uygulamanın yeniden bağlanmasına
ihtiyaç duyabilir. RouteLane uygulamaları kapatmaz veya tarayıcıyı yeniden başlatmaz.

## 6. UI ve onay akışı

Bağlantılar bölümünde “Ana bağlantı” seçimi ve yardımcı sıra açıkça görünür.
Genel Bakış tercih edilen ana hat ile şu an kullanılan çıkışı ayrı gösterir.
Sistem varsayılanı seçeneği hangi arayüze karşılık geldiğini adıyla belirtir.

Manuel seçimde mevcut tercih değiştirilmeden modal açılır:

> Ana bağlantı Ethernet olarak değiştirilecek. Özel kurallar dışındaki yeni
> internet bağlantıları Ethernet'i kullanacak. Mevcut oturumlar etkilenebilir.
> Bu değişikliği onaylıyor musunuz?

Varsayılan düğme İptal'dir. İptal/Escape/kapatma aynı güvenli davranıştır.
Onay → doğrulanmış taslağı kaydet → etkinse yetkili uygula → ilgili snapshot ile doğrula.
Kayıt/uygulama başarısızlığı, “kaydedildi” ile “şu an etkin” durumunu karıştırmaz.
Ana seçim değiştirilirken açık editörler stable ID ile tekrar doğrulanır.

## 7. Mimari ve yetki sınırı

- Domain/config: ana seçim, yardımcı sıra, doğrulama ve açık migrasyon.
- Topology: sistem varsayılanını RouteLane'in kendi tablolarından bağımsız bulur;
  aile, arayüz, adres, carrier ve route kanıtını taşır.
- Planner: açık kurallar + ana çıkış politikası + aile bazlı güvenli sonuç üretir.
- Yetkili runtime: yalnız ilk yetkili aktivasyonda onaylanan lane kümesi ve
  politika sınırları içindeki otomatik geçişleri yapar; yabancı kaynaklara dokunmaz.
- Daemon: durum/nesil/event ve UI korelasyonunu taşır; UI açık olmadan da gözlem sürer.
- GTK: seçimi, manuel onayı, gerçek etkin çıkışı ve geçiş/hata bildirimini gösterir.

Bugünkü `auth_admin_keep` önbelleğinin süresiz geçerli olduğu varsayılmaz.
Otomatik geçiş, her olayda yeni interaktif Polkit diyaloğu isteyen bir timer'a
bağlanamaz. İlk yetkili aktivasyon, helper'a root-owned ve kullanıcı/politika/lane
kümesiyle sınırlı runtime yetkisi verir. Yeni profil, farklı lane, farklı kapsam
ve manuel politika değişimi bu yetkiyi genişletemez; yeniden yetkilendirilir.
Salt okunur Inspect hiçbir zaman onarım veya aktivasyon başlatmaz.
Restart sonrası yetki/journal güvenilir biçimde doğrulanamazsa çalışma tahminle
devam etmez; hata görünür ve güvenli kapatma/yeniden yetkilendirme sunulur.

Olay tabanlı link/address/route izleme kullanılır; sadece değişim ve kısa geri
dönüş stabilizasyonu işlem üretir. Durum aynıysa yeni apply/nesil üretilmez.
Plan değişimi mevcut transaction, ownership kontrolü ve rollback akışını kullanır.

Yalnız en sona Any/Route eklemek yeterli kabul edilmez: kaynak adresi, conntrack,
geri dönüş yolu, link kaybı ve IPv6 davranışı gerçek kernel testleriyle doğrulanır.
Kapsamlı global NAT, firewall flush, kalıcı DNS/metric ve global sysctl değişikliği yoktur.
Gerekirse yalnız kullanılan arayüze ait dar kernel ayarı ayrı ilk onayda açıklanır,
önceki değeri journal'a alınır ve yalnız hâlâ RouteLane'e aitse geri yüklenir.

Domain yönlendirmesi DNS/IP eşleşmesi sınırına tabidir. Ana hat seçimi browser
DoH/cache/CDN farkını veya Snap/Flatpak uygulama yönlendirmesini kendiliğinden
çözmüş gibi sunulmaz; DNS ve site erişimi ayrı teşhis bilgisi olarak görünür.

## 8. Kabul ve doğrulama

1. Sistem varsayılanı başlangıçta doğru seçilir; `default` ID'si rol diye yorumlanmaz.
2. Ethernet ana, VINN yardımcıyken açık VINN kuralları VINN; diğer internet trafiği Ethernet.
3. Manuel seçim iptalinde config/IPC/kernel mutasyonu yoktur; çift onay tek işlem gönderir.
4. Fiziksel Ethernet/route kaybında UI kapalıyken de yeni trafik otomatik VINN'e geçer.
5. Polkit keep süresi bittikten sonra onaylı failover çalışır; yetki başka lane'e genişletilemez.
6. Geri dönüş stabilizasyonu yeni bağlantıları ana hatta alır; mevcut oturumları zorla kapatmaz.
7. Yardımcı da kesilirse açık Block/fallback politikaları ve güvenli tüm-hatlar-kapalı sonucu korunur.
8. Loopback, yerel ağ, DHCP/DNS erişimi ve üçüncü taraf routing/firewall kaynakları korunur.
9. IPv4/IPv6, non-default uplink kaynak adresi ve strict/loose reverse-path koşulları kernel namespace testlerinde doğrulanır.
10. Migrasyon ve kayıt hataları mevcut ayarları kaybettirmez; eski sürüm yeni semantiği sessiz yok sayamaz.
11. Helper/daemon restart, yarım transaction ve sahiplik belirsizliğinde yanlış “sağlıklı” gösterilmez.
12. Rootless/TDD/GTK testleri yanında Ubuntu, Debian ve Fedora üzerinde gerçek paket/Polkit/kernel kabulü yapılır.

Tasarım incelemesi ve uygulama planı onayından önce ürün kodu, aktif profil,
servisler veya canlı kernel kaynakları değiştirilmez. Önceki dirty reliability
değişiklikleri korunur; yürütme tercihi main üzerinde native/inline'dır.
