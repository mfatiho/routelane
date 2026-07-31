# RouteLane Profesyonel UI/UX Tasarımı

**Durum:** Kullanıcı tarafından onaylandı  
**Tarih:** 31 Temmuz 2026  
**Hedef:** RouteLane'i ilk kullanımda anlaşılır, günlük kullanımda hızlı ve
ayrıcalıklı ağ değişikliklerinde güven veren tek pencereli bir masaüstü
uygulamasına dönüştürmek.

## 1. Kapsam

Bu çalışma aşağıdaki kullanıcı deneyimi sorunlarını çözer:

- İlk kez kullanan kişi, rota seçme, hedef ekleme ve etkinleştirme adımlarını
  hangi sırayla yapacağını doğrudan anlayabilmelidir.
- Günlük kullanımda aktif durum, seçili rota ve kural sayısı tek bakışta
  anlaşılmalıdır.
- Yeni kural eklemek görünür bir ana eylem olmalı; alan adı, IP ve CIDR
  doğrulamaları eyleme yakın gösterilmelidir.
- Ayrıcalıklı bir değişiklik gerekirse uygulama Polkit üzerinden sistemin
  yönetici kimlik doğrulama penceresini istemeli, bekleme/reddetme durumunu
  açıkça anlatmalıdır.
- Masaüstü dosyasına ikinci kez tıklamak ikinci pencere açmamalı; var olan
  pencere görünür ve odaklı hale gelmelidir.

Bu çalışma routing modeli, helper yetki sınırı, profil veri modeli ve CLI
davranışını değiştirmez.

## 2. Kullanıcı akışı

### İlk kullanım

Ana ekran eksik kurulum durumunu algılar ve bir başlangıç kartı gösterir:

1. Kullanıcı bir rota için ağ arayüzü seçer.
2. Kullanıcı alan adı, IP veya CIDR hedefi ekler ve bu hedef için Route,
   Direct ya da Block eylemini seçer.
3. Kullanıcı **Yönlendirmeyi etkinleştir** düğmesini seçer.
4. Ayrıcalıklı değişiklik gerektiğinde Polkit doğrulama penceresi açılır.
5. Başarıda kart, sağlıklı durum ve seçili rota özetine dönüşür.

Başlangıç kartı yalnızca gerçekten eksik olan adımı vurgular. Var olan profil
ve kuralları değiştirmez; kullanıcı doğrudan gelişmiş ayarlara da geçebilir.

### Günlük kullanım

Ana ekran şu sırayı kullanır:

1. Durum kartı: etkin/pasif, kısa sağlık özeti ve ana eylem.
2. Aktif rota özeti: seçili rota adı, arayüz ve hata politikası.
3. Kurallar: sayısı, kısa açıklaması ve belirgin **Kural ekle** eylemi.
4. Bağlantı Sağlığı: yalnız eyleme geçirilebilir hata/uyarı özeti; teknik
   ayrıntılar isteğe bağlı açılır bölümde.

Rota düzenleme, hata politikası, dil, profil ve teknik tanı ayrıntıları
**Tercihler** altında kalır. Böylece ana ekran temel akışı korur.

## 3. Bileşenler ve durumlar

| Bileşen | Sorumluluk | Durumlar |
| --- | --- | --- |
| Tek pencere denetleyicisi | Var olan pencereyi bulur ve öne getirir; yoksa oluşturur | mevcut, yeni, kapalı |
| Durum kartı | Routing ve yetkilendirme durumunu anlaşılır biçimde sunar | kurulum gerekli, pasif, uygulanıyor, yetki bekleniyor, sağlıklı, uyarı, hata |
| Başlangıç kartı | Eksik ilk adımı ve sonraki tek eylemi gösterir | rota gerekli, kural gerekli, etkinleştirmeye hazır |
| Kural ekleme akışı | Hedef/eylem alır, yerel doğrular, sonra atomik kaydeder | boş, geçersiz, kaydediliyor, hazır |
| Yetkilendirme sunumu | Polkit etkileşimini ve sonucu kullanıcıya bağlar | istek gönderildi, pencere bekleniyor, reddedildi, yetkilendirildi |
| Gelişmiş tercihler | Rota, profil, hata politikası, dil ve teknik ayrıntıları düzenler | mevcut davranış korunur |

Tek örnek davranışı `adw::Application` aktivasyonunda uygulanır: uygulamanın
aktif bir penceresi varsa pencere `present()` ile öne getirilir; yalnızca aktif
pencere yoksa yeni `ApplicationWindow` oluşturulur. Bu kural hem masaüstü
başlatıcısı hem uygulama yeniden etkinleştirilmesi için aynıdır.

## 4. Yetkilendirme akışı

GUI root veya `sudo` kullanmaz. Bir routing mutasyonu daemon üzerinden system
D-Bus helper'a gider; helper Polkit'in kullanıcı etkileşimine izin veren
yetkilendirme çağrısını yapar. Böylece işletim sistemindeki doğrulama aracısı
parola/kimlik doğrulama penceresini gösterir.

Arayüz, mutation isteği gönderildiğinde ana eylemi devre dışı bırakır ve
**Yönetici izni bekleniyor** durumunu gösterir. Bu sırada yeni bir apply
isteği başlatılmaz. Yetki verilirse daemon'ın başarılı durumu kartı günceller.
Kullanıcı iptal eder veya Polkit reddederse uygulama mevcut routing durumunu
korur, ana eylemi yeniden etkinleştirir ve yeniden denemeyi açıklayan bir
mesaj gösterir. Yetki süresi Polkit politikasının `auth_admin_keep` ayarına
tabidir; uygulama parola veya yetki belirteci saklamaz.

## 5. Erişilebilirlik ve yerelleştirme

- Tüm görünür yeni metinler Türkçe ve İngilizce `Texts` alanlarından gelir.
- Ana eylemler yalnız ikonla bırakılmaz; metin, erişilebilir ad ve tooltip
  içerir.
- Sağlık rengi tek başına anlam taşımaz; metin ve sembolle desteklenir.
- Klavye odağı, Enter ile kural ekleme ve hata sonrasında ilgili girdiye geri
  dönüş korunur.
- Dar pencerede durum kartı, rota özeti ve kural eylemleri dikey akışa sarar;
  yatay taşma yaratmaz.

## 6. Hata yönetimi ve veri koruma

- Geçersiz hedef, yerel doğrulamada reddedilir; profil diske yazılmaz.
- Kaydetme başarısızsa ana durum daemon'ın son doğrulanmış durumuna geri
  döner; yanlışlıkla etkin görünmez.
- Yetki reddi veya daemon hatası mevcut kuralları/profili silmez ve mutation
  tekrarını kullanıcı eylemine bırakır.
- Var olan kural, profil ve rota verileri yeni görünüm tarafından aynı
  `ConfigV2` kaynağından okunur; görünüm geçişi veri migrasyonu gerektirmez.

## 7. Test stratejisi

Testler üretim kodundan önce yazılır ve önce RED gözlemlenir:

- Pencere aktivasyon seçimi: mevcut pencereyi oluşturmak yerine öne getirir;
  pencere yoksa yalnız bir tane oluşturur.
- Kurulum ilerlemesi: eksik rota/kural için doğru sonraki eylemi üretir;
  hazır profilde başlangıç kartını gizler.
- Yetkilendirme görünüm modeli: bekleyen, reddedilen ve başarılı mutation
  sonuçları doğru kullanıcı durumuna dönüşür; reddetme mevcut aktif durumu
  değiştirmez.
- Kural ekleme: geçersiz hedefin kaydetme isteği üretmediği; geçerli hedefin
  mevcut async persistence akışını kullandığı doğrulanır.
- Türkçe/İngilizce metin kapsamı ve ana eylemlerin erişilebilir adları test
  edilir.

GTK widget entegrasyonu, saf durum/karar fonksiyonları üzerinden dar birim
testleriyle; mevcut IPC sözleşme testleri ise gerçek daemon yanıtlarıyla
doğrulanır. Tam rootless suite, biçimlendirme, clippy ve release build final
kontrolüdür.

## 8. Başarı ölçütleri

- İkinci başlatma yeni pencere oluşturmaz; mevcut pencere öne gelir.
- Boş profil kullanan kişi ana ekrandan üç anlaşılır adımda routing'i
  etkinleştirebilir.
- Yetkilendirme gereken değişiklikte sistem Polkit penceresi tetiklenir ve
  UI bekleme/ret durumunu açıkça gösterir.
- Var olan kullanıcı yapılandırması korunur; yeni arayüzde kurallar ve
  rotalar aynı şekilde uygulanır.
- Yeni kullanıcı metinleri iki desteklenen dilde eksiksizdir.
