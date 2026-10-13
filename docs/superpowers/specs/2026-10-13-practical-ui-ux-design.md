# RouteLane: pratik ve güvenilir UI/UX tasarımı

Tarih: 13 Ekim 2026.
Durum: Yaklaşım ve yazılı tasarım kullanıcı tarafından onaylandı.
Uygulama planı incelemeye hazır; ürün kodunda bu tasarım için henüz değişiklik yapılmadı.

## 1. Amaç ve kapsam

Kullanıcı RouteLane'i ağ yönlendirme ayrıntılarını bilmeden kullanabilmeli:
bağlantısını tanımlar, program veya hedef seçer, yönlendirmeyi etkinleştirir.
Her an neyin kaydedildiğini, neyin uygulandığını ve sorun varsa ne yapacağını görür.

Başarı ölçütleri:

- İlk kurulum üç anlaşılır adımdan oluşur: bağlantı, uygulama/kural, etkinleştirme.
- Günlük kullanımda tek bir global etkinleştirme/kapatma kontrolü vardır.
- Kurallar ve uygulamalar listeden taranabilir; satır başına sürekli açık editör yoktur.
- Çoklu bağlantılar doğru temsil edilir; ilk bağlantı bütün sistemin çıkışı gibi gösterilmez.
- Kaydetme, uygulama, yetkilendirme, hata ve bilinmeyen durumlar birbirine karışmaz.
- Türkçe/İngilizce, klavye, açık/koyu tema ve dar pencere kullanılabilirliği korunur.

Bu çalışma ekran hiyerarşisini ve UI durum yönetimini birlikte değiştirir.
Routing planlayıcısı, helper'ın yetki sınırı, daemon IPC wire sözleşmesi ve
ConfigV2 biçimi değişmez. Yeni bağımlılık, GTK/Libadwaita sürüm yükseltmesi,
kernel değişikliği veya otomatik ağ onarımı eklenmez.

Bu belge, 31 Temmuz GNOME UI/UX tasarımının tek uzun ana sayfa ve Tercihler
içindeki bağlantı yönetimi kararlarının yerine geçer. Önceki güvenlik,
yerelleştirme ve erişilebilirlik gereksinimleri korunur.

## 2. Bilgi mimarisi

Pencerenin üstünde uygulama başlığı ve Tercihler menüsü bulunur. Altındaki
kalıcı durum alanı kısa açıklama, destekleyici ikon ve tek global ana eylem içerir.
Header'daki tekrarlayan yönlendirme anahtarı kaldırılır.

| Bölüm | İçerik ve ana görev |
| --- | --- |
| Genel Bakış | Başlangıç rehberi, bağlantı/kural/uygulama sayıları, tüm bağlantıların kısa özeti, Bağlantı Sağlığı |
| Kurallar | Domain/IP/CIDR kuralları, arama, üstte Kural Ekle, kısa satırlar ve düzenleme akışı |
| Uygulamalar | Program adı/ikonu, arama, üstte Uygulama Ekle, bağlantı özeti ve başlatma |
| Bağlantılar | Bağlantı adı/arayüzü, ekleme/düzenleme/silme ve kesinti politikası |

Dört bölüm tek GTK pencere içindeki stack ile değişir. Mevcut GTK4 v4_4 ve
Libadwaita v1_1 API sınırı korunur. Yeni sürümlere özel gezinme widget'ları kullanılmaz.
Gezinme metin ve sembolik ikonlarla yapılır; dar pencerede yatay kaydırılabilir
seçici kullanılır, bölüm etiketleri üst üste binmez. Her sayfa bağımsız dikey kayar.

Başlangıç pencere boyutu 900×720 mantıksal pikseldir. 560×600 mantıksal pikselde
ve yüzde 200 metin ölçeğinde temel işlemler erişilebilir kalmalıdır. Durum alanı
dar pencerede dikey düzenlenir; buton ve uzun metinler kesilerek erişilemez olmaz.

Tercihler yalnızca dil ve mevcut genel uygulama ayarlarını içerir; bağlantı
editörünün ikinci bir kopyasını içermez. Yeni profil yöneticisi bu kapsamda değildir.
Mevcut seçili profil kimliği, profil birleştirme ve başka profilleri koruma davranışı sürer.

## 3. Genel Bakış ve başlangıç

Başlangıç rehberi yalnız gerekli adımı vurgular:

1. Bağlantı yok: Bağlantı Ekle doğrudan bağlantı formunu açar.
2. Bağlantı var, kullanılabilir açık kural/uygulama yok: Uygulama Ekle veya
   Kural Ekle seçenekleri verilir. Devre dışı tanımlar varsa mevcut listeye yönlendirilir.
3. Açık kural/uygulama var: Yönlendirmeyi Etkinleştir ana eylemidir.

Direct/Block kuralları bağlantı gerektirmez; bunlar için sahte bir bağlantı
ekletilmez. Kullanılabilir açık tanım, mevcut planlayıcının kabul ettiği etkin
program veya domain/IP/CIDR/Any kuralıdır; yalnız kayıtlı tanım sayısı yeterli değildir.

Etkin bir plan varken eksik yerel kurulum hiçbir zaman Devre Dışı Bırak eylemini
gizlemez. Genel Bakış ayrıca bütün tanımlı bağlantıları isim ve arayüzle gösterir.
Yerel sağlık kanıtı bulunmayan bağlantı için tahmini canlı durum üretilmez.
Sayaçlar ayrı ayrı bağlantı, hedef kuralı ve uygulama tanımı sayılarını gösterir;
tanım sayısı yönlendirilmiş süreç veya başarılı bağlantı sayısı gibi sunulmaz.

## 4. Kurallar

Kural Ekle butonu liste ve boş durumun üzerinde kalır. Arama hedef ve bağlantı
adını filtreler; hiçbir kaydı silmez ve yürütme önceliğini değiştirmez. Eşleşme
bulunamaması, hiç kural olmamasından farklı bir mesajdır.

Her satır hedefi, işlem/bağlantı özetini, etkinlik anahtarını ve seçenekler menüsünü
gösterir. Örnek: `claude.ai` / `VINN bağlantısını kullan`. Anahtar kuralın
aktif/pasif tanımını değiştirir; silmez. Block için ayrı bir daraltma veya yeni
mod sistemi eklenmez.

Seçenekler menüsü Düzenle, Yukarı Taşı, Aşağı Taşı ve Sil içerir. İlk/son satırda
geçersiz taşıma eylemi kapalıdır. Arama sırasında da sıra işlemi filtrelenmiş
görsel indeks yerine gerçek yürütme sırasına uygulanır. Silme, hedefi adıyla
belirten onay gerektirir.

Düzenleme formu hedef, işlem ve gerekliyse bağlantı alanlarını dikey gösterir.
Görünür etiketler kullanılır; placeholder alanın tek etiketi değildir. Domain,
IP, CIDR ve mevcut Any desteği korunur. Boş, geçersiz ve desteklenmeyen hedefler
yerel doğrulamada reddedilir; wildcard/service-pack desteği varmış gibi sunulmaz.
Bir URL'yi domain'e sessizce dönüştürmek veya mevcut hedef kapsamını genişletmek
bu tasarımda yoktur. Örnekler geçerli hedef biçimini açıklar.

Dialog açılması veya arama yapılması ağ değişikliği başlatmaz. İptal değişikliği
kaydetmez. Başarılı doğrulama ve açık Kaydet eylemi mevcut persist/apply akışına gider.

## 5. Uygulamalar

Uygulama Ekle listenin üstünde kalır. Seçici program adına göre aranır; mevcut
desktop metadata'sından görünen ad/ikon alınır. Metadata yoksa executable adı
ve genel uygulama ikonu kullanılır. Kimlik ve executable yolu teknik ayrıntılardır.
Görünen ad/ikon yalnız sunum metadata'sıdır; kalıcı yapılandırma biçimi değişmez.

Seçim formu program, bağlantı ve kapsam açıklamasını içerir. Snap/Flatpak ortak
başlatıcıları mevcut destek sınırına göre seçilemez ve neden gösterilir. Diğer
tanınmamış launcher'lar destekleniyor diye işaretlenmez; backend doğrulaması korunur.

Satırda program adı, bağlantı özeti, etkinlik anahtarı, Bu Bağlantıyla Başlat
butonu ve seçenekler menüsü vardır. Bağlantı değişikliği Düzenle formundadır;
her satırda kalıcı dropdown gösterilmez.

Menüde Düzenle, Çalışan Süreçlere Uygula ve Sil bulunur. Çalışan süreçlere
uygulamadan önce browser'ın diğer sekmelerinin de etkilenebileceği, mevcut
bağlantıların taşınmasının garanti olmadığı açıklanır ve onay alınır.
Program otomatik kapatılmaz veya yeniden başlatılmaz.

Başlatma/taşıma yalnız ilgili uygulanmış profil ve açık kural doğrulanmışsa
kullanılabilir. Kaydedilmiş ama henüz uygulanmamış düzenleme için eski planla
başlatma yapılmaz. Kapalı eylemin nedeni görünür metinle açıklanır.
Seçili bağlantı kullanılamıyorsa kesinti/fallback davranışı satırda açıklanır;
normal sistem çıkışına düşen uygulama seçili bağlantıdan geçiyormuş gibi gösterilmez.
Bu durumda başlatma etiketi nötr Uygulamayı Başlat olur; mevcut politika değiştirilmez.
Sonuçta PID veya taşınan/atlanmış süreç sayısı gösterilebilir; bu bilgi trafik
çıkışının veya web sayfası yüklenmesinin kanıtı olarak sunulmaz.

## 6. Bağlantılar

Ekleme ve düzenleme ayrı dikey formlardır: anlaşılır bağlantı adı, sistem arayüzü,
kesinti davranışı. Arayüz bulunamadığında başka arayüz sessizce seçilmez;
mevcut seçim korunur ve bulunamadığı açıklanır. Kaydet geçerli seçim gerektirir.

Kesinti davranışlarının metinleri:

- Block: Bu bağlantıya atanmış trafiği engelle.
- Direct: Normal sistem bağlantısını kullan.
- Inactive: Bu bağlantıya atanmış yönlendirmeyi pasifleştir.

Direct ve Inactive seçeneklerinde trafiğin seçili bağlantı dışına çıkabileceği
açıklanır. Bunlar kill-switch koruması gibi sunulmaz. Mevcut fallback alanları
varsa korunur; yeni fallback yapılandırıcısı eklenmez.

Silme onayı etkilenen hedef kurallarını ve uygulamaları ayrı sayar; mevcut silme
semantiği değiştirilmez. Formdan vazgeçmek veri veya kernel değişikliği üretmez.

## 7. Doğrulanmış durum ve işlem yaşam döngüsü

UI tek bir sunum modelinden türetilir. Kaynaklar ayrıdır:

- Seçili profilin yerel düzenlemeleri ve başarıyla kaydedilmiş revizyonu.
- Daemon'un doğrulanmış uygulanmış profil kimliği ve generation'ı.
- Yerel kaynak sağlığı; tek başına etkinlik bayrağı değildir.
- Bekleyen işlemin yerel kimliği, türü ve aşaması.
- Bağlantı epoch'u ve en son kabul edilen generation/event sequence.

| Kanıt/durum | Ana eylem ve geri bildirim |
| --- | --- |
| İlk snapshot henüz yok | Durum kontrol ediliyor; ağ işlemleri kapalı |
| Uygulanmış plan yok | Kurulum adımı veya Yönlendirmeyi Etkinleştir |
| Uygulanmış plan var | Devre Dışı Bırak; varsa sağlık uyarısı ayrıca |
| Kaydetme sürüyor | Kaydediliyor; çakışan değişiklikler kapalı |
| İstek gönderildi | İşlemle uyumlu Uygulanıyor / Kapatılıyor / Başlatılıyor / Kurtarılıyor |
| Apply onayı bekleniyor | Sistem yetkilendirme penceresi açılabilir açıklaması; açıldığına dair kanıt olmadan kesin Yetki Bekleniyor iddiası yok |
| Yetki reddedildi | İşlem uygulanmadı; tekrar dene, doğrulanmış etkin durum korunur |
| IPC kesildi veya mutasyon timeout oldu | Durum doğrulanamıyor; son bilinen durum açıkça etiketlenir |

HealthState::Failed tek başına Devre Dışı Bırak seçtirmez. Etkin plan varsa
kurulum veya yapılandırma hatası kapatma yolunu gizlemez. Config yazılamıyorsa
yeni düzenleme/etkinleştirme durur; doğrulanmış planı kapatmak için mevcut daemon
isteği yine kullanılabilir. Backend'in kendi hatası varsa sonuç dürüstçe gösterilir.

Tek bir çakışan mutasyon çalışır. Bekleyen işlem varken kural anahtarları,
bağlantı değişiklikleri ve başlatma/taşıma tekrar gönderilemez. Gezinme, arama,
ayrıntı okuma ve kopyalama açık kalır. Kaydetmeden doğan apply aynı yaşam
döngüsünün devamıdır; ikinci bağımsız işlem gibi başlatılmaz.

GetStatus, ListProfiles veya ilgisiz event bekleyen işlemi bitmiş saymaz.
Mevcut wire request_id UI köprüsünde korunarak yalnız ilgili sonuç işlemi
tamamlar; gerekirse iç UiToDaemon/DaemonToUi mesajları genişletilir. Wire
protokolü değiştirilmez. Yeniden bağlanmada eski epoch'un yanıtları yok sayılır;
yeni snapshot başarılı olsa bile eski mutasyon otomatik tekrar gönderilmez.

Yerel düzenleme pasifken kaydedilirse Kaydedildi — Yönlendirme Kapalı gösterilir.
Etkin düzenleme Kaydediliyor → Uygulanıyor → Uygulandı aşamalarını izler.
Apply hatasında kaydedilmiş düzenleme korunur ve uygulanmadığı işaretlenir;
önceki kernel planının korunduğu yalnız güncel kanıt bunu doğruluyorsa söylenir.

Yeni durum/profil snapshot'ı işlenirken bölüm, arama metni, scroll ve mümkünse
odaklanan satır kimliği korunur. Eski generation/sequence mevcut ekranı geri almaz.

## 8. Sağlık, hatalar ve güvenli kurtarma

Bağlantı Sağlığı Genel Bakış'ta kısa insan diliyle gösterilir. Healthy için
Yerel Yönlendirme Kuralları Doğrulandı denir; İnternet Sağlıklı veya Site Açılıyor
denmez. Domain çözümlemesinin mevcut snapshot/DoH sınırları yardım metninde korunur.

Hatalar üç katmanlıdır: kısa kullanıcı açıklaması, uygulanabilir sonraki adım,
açılabilir/kopyalanabilir teknik ayrıntı. Kalıcı sorun yalnız geçici toast içinde
kaybolmaz. Ham D-Bus mesajı ana başarı/hata başlığı yerine teknik ayrıntıda kalır.
Tanımlanamayan hata için genel açıklama ve kayıpsız teknik ayrıntı vardır.

Yetim kaynak kurtarma mevcut doğrulama ve açık kullanıcı onayıyla çalışır;
yalnız ilgili başarısız etkinleştirme için en fazla bir otomatik yeniden deneme
korunur. Kullanıcı vazgeçerse kernel değişikliği veya yeniden deneme yapılmaz.
Onay verilse bile sahipliği belirsiz kaynaklar silinmez. Sayfa açılması, profil
görüntülenmesi veya sağlık kontrolü onarım çağrısı üretmez.

## 9. Görsel dil, erişilebilirlik ve performans

- Sistem teması ve Libadwaita sınıfları; özel renk paleti veya tema override yok.
- Yerleşimde 6/12/24 piksel boşluk ritmi; tipografi title/body/caption rolleriyle tutarlı.
- Renk tek anlam taşımaz; durum metin ve ikonla belirtilir. İkon-only menülerin erişilebilir adı vardır.
- Bütün kullanıcı metinleri i18n üzerinden Türkçe/İngilizce gelir; enum debug adı görünmez.
- Tab sırası görsel sırayı izler. Ctrl+F Kurallar/Uygulamalar bölümünün aramasını açar;
  Genel Bakış/Bağlantılar bölümünde Kurallar aramasına geçer. Ctrl+N aktif listenin
  ekleme formunu açar; Genel Bakış'ta Kural Ekle formunu açar.
- Enter geçerli formu gönderir; geçersiz form göndermez. Escape iptal eder; silme onayında varsayılan odak İptal'dir.
- Dialog kapanınca odak açan kontrole döner. Hata alan yanında açıklanır ve ilgili alana odaklanılır.
- Uzun ad/teknik ayrıntı satırları yatay pencere taşmasına neden olmaz; ayrıntılar seçilip kopyalanabilir.
- Arama sadece yerel listeyi filtreler; DNS, helper, daemon mutasyonu başlatmaz.
- Desktop keşfi ve bloklayıcı dosya okumaları GTK ana döngüsünü durdurmaz; sonuçlar kimliğiyle UI'a teslim edilir.
- Yeni sürekli polling, DNS yenilemesi, dış servis veya arka plan ağ trafiği eklenmez.

## 10. Kod sınırları

- `src/ui/window.rs`: pencere kabuğu, sayfa bağlama ve controller; bütün editörlerin tek dosyada birikmesi azaltılır.
- `src/ui/experience.rs`: saf sunum/ana eylem/işlem kararları; widget veya ayrıcalıklı işlem içermez.
- Sayfa/editör yardımcıları `src/ui/` altında amaca göre ayrılır; proje dışı UI framework eklenmez.
- `src/ui/rule_row.rs` ve `application_row.rs`: kompakt satır ve eylemler; domain kuralları burada yeniden tanımlanmaz.
- `src/ui/settings.rs`: genel tercihler; mevcut lane düzenleme mantığı Bağlantılar sayfasının editörüne yeniden kullanılarak taşınır.
- `src/ui/i18n.rs`: tüm yeni durum, form, yardım ve erişilebilirlik metinleri.
- `src/models.rs` ve `src/main.rs`: gerekirse UI içi işlem korelasyonu; mevcut IPC istemcisi ve epoch/sequence koruması sürer.
- Desktop metadata yardımcısı yalnız görünen ad/ikon/destek açıklaması sağlar; süreç kimliği ve launcher argümanları korunur.

Önceki güvenilirlik denetiminin çalışma ağacındaki değişiklikleri bu işten
bağımsız korunur. Main üzerinde ilerlenir; tasarım commit'ine ürün kodu dahil edilmez.

## 11. Kabul ve doğrulama

Kodlama sırasında davranışlar önce başarısız testle tanımlanır. Kabul koşulları:

1. Gerçek GTK penceresinde dört bölüm açılır; tek global routing kontrolü bulunur.
2. Kural Ekle/Uygulama Ekle boş, dolu ve filtreli listelerin üstünde kalır.
3. Dialog açma, arama, bölüm değiştirme, İptal ve sağlık görüntüleme mutasyon göndermez.
4. Kaydetmeden sonraki apply, toggle, başlatma ve taşıma çift tıklamayla çoğaltılmaz.
5. Araya giren status/event bekleyen işlemi tamamlamaz; eski yanıt/epoch ekranı geri almaz.
6. Failed + uygulanmış plan yok durumunda yanlış kapatma eylemi gösterilmez.
7. Config hatası + doğrulanmış uygulanmış plan durumunda kapatma erişilebilir kalır.
8. IPC kesintisinde son bilinen durum etiketlenir; gerçek uygulanmış durum tahmin edilmez.
9. Birden fazla bağlantı doğru adlarla görünür; ilk lane global çıkış gibi sunulmaz.
10. Block aktif/pasif, Direct, Route, öncelik sırası, mevcut IPv4/IPv6 kuralları ve silme onayları korunur.
11. Desteklenmeyen sandbox uygulamalarının başlatma/taşıma sınırı korunur; program adları okunabilirdir.
12. Dil değişikliği öncesi mevcut yeniden başlatma onayı korunur; iptal dil/config değişikliği yapmaz.
13. Klavye, odak dönüşü, dar pencere, yüzde 200 metin, uzun adlar ve iki dil GTK testleri/manüel kontrolde incelenir.
14. Fmt, rootless testler, Clippy, izole gerçek GTK smoke ve release build başarılıdır.

Rootless testler, gerçek browser trafiği veya bütün dağıtımlar için başarı
iddiası oluşturmaz. Ubuntu/Debian/Fedora destek kapsamı azaltılmaz; gerçek
Polkit, paket kurulumu ve kernel entegrasyonu ayrı release doğrulaması olarak kalır.
