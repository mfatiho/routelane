# Genel Bakış ve tepsi menüsü

11 Kasım 2026 — kullanıcının onayladığı ekran ve menü önerilerinin uygulanması.

## Bilgi hiyerarşisi

- Üstte tek durum kartı ve kısa ana eylem bulunur. Tam eylem adı ekran okuyucu etiketi ve araç ipucunda korunur.
- Genel Bakış önce dikkat gerektiren durumları, ardından **uygulanan** kapsamı gösterir. Etkin planda IPv4/IPv6 çıkış özeti de görünür; kayıtlı tercih uygulanmış gibi sunulmaz.
- Sağlık, kayıtlı kapsam ve ayrıntılı çıkış bilgileri klavyeyle açılabilen **Yönlendirme ayrıntıları** bölümündedir. Yerel durum kanıtı internet erişim garantisi değildir.
- Kullanıcının sonraki seçimiyle Genel Bakış'tan **Kısayollar**, **Bağlantılar** ve **Son 7 gün veri kullanımı** kartı kaldırıldı. İlgili listeler kendi sekmelerindedir; uygulama/kural ekleme düğmeleri orada korunur.
- Grafik ve kota ayrıntıları yalnız bağlantının detay penceresindedir. Günlük sütunlar tüketimi, canlı çizgiler indirme/yükleme hızını gösterir. Tarih aralığı ve sayısal tepe miktarı grafiğin altında görünür. Kota dolması trafiğin durdurulduğu anlamına gelmez.
- Ayrıntılı olay geçmişi **Olaylar** sekmesinde; canlı hız grafikleri bağlantı ayrıntısındadır.

## Verinin anlamı ve sınırları

Kullanım verisi arayüz sayaçlarından gelir: yalnız RouteLane'in yönlendirdiği
trafik değildir, yaklaşık ölçümdür ve operatör faturasının kanıtı sayılmaz.
Bağlantılar toplanmaz; aynı arayüzün iki kez sayılması önlenir.
Kayıtlı bağlantının kimliği **ve arayüz adı** raporla eşleşmelidir.

Grafik daemon'un döndürdüğü geçmişi kullanır. Kota dönemi yeni başlamışsa
yedi günden az veri olabilir; gösterilen tarih aralığı ve günlük tepe miktarı
açıkça yazılır. Eksik günler uydurulmaz. Kalan dönem bütçesinden hesaplanan
günlük tahmin, geçmiş tüketim grafiğine limit çizgisi olarak çizilmez.
Var olan arka plan sorgulama sıklığı değişmez; Genel Bakış hızlı örnekleme başlatmaz.

Yüklenme, geçmiş bulunmaması, arayüzün bağlı olmaması ve verinin alınamaması
ayrı metinlerle sunulur. Daemon durumu doğrulanamıyorsa eski grafik ve kota
güncelmiş gibi gösterilmez. Bağlantı seçimi ve grafikler salt okunurdur.

## Tepsi menüsü ve güvenlik

Tepsi, özel bir GTK popover çizimi değil, mevcut `ksni`/StatusNotifier yerel
menüsüdür. Görünümü ve kullanılabilirliği masaüstünün tepsi desteğine bağlıdır.

- Doğrulanmış etkinlik durumu, uygulanan kapsam, IPv4/IPv6 çıkışları ve varsa yedek hat uyarısı gösterilir. Yetkilendirme ihtiyacı ayrıca belirtilir.
- **Etkinleştir / devre dışı bırak** GTK ana eylemine gider; önizleme, gerekli onay ve mevcut yetkilendirme akışını atlamaz. Açık onay penceresi varsa öne getirilir.
- Eski bir menüden gelen komut GTK tarafında tekrar doğrulanır. Bir `Enable` komutu sonradan `Disable` komutuna dönüştürülmez. Bilinmeyen/meşgul durumlarda yönlendirme işlemi yapılmaz.
- Sorun varsa **Sorunu görüntüle** Genel Bakış'ı açar. **RouteLane'i aç** mevcut pencereyi öne getirir.
- **Arayüzden çık** yalnız GUI'yi kapatır; yönlendirmeyi kapatmaz. Bu ayrım menüde açıklanır. Devam eden işlem sırasında menüden çıkış devre dışıdır.

GTK nesneleri tepsi iş parçacığına gönderilmez. GTK tarafı sahipli metin/bayrak
anlık görüntüsü üretir; tepsi işçisi güncellemeleri birleştirip yerel menüyü
yeniler. Menü güncellemesi GTK iş parçacığını D-Bus üzerinde bekletmez.
Yeni bağımlılık, IPC şeması, ayrıcalıklı komut veya otomatik yönlendirme değişikliği eklenmedi.

## Kod ve doğrulama

- `src/ui/overview.rs`: özet, dikkat gerektirenler ve katlanabilir yönlendirme ayrıntıları.
- `src/ui/{lane_detail,usage_widgets}.rs`: grafik, ölçülen tarih/tepe miktarı ve kota.
- `src/ui/tray.rs`: GTK içermeyen sunum modeli ve yerel menü.
- `src/ui/window.rs`: güvenli eylem aktarımı, durum yayını ve GTK smoke kontrolleri.
- `src/ui/{shell,i18n}.rs`: kompakt durum kartı, erişilebilir eylem adları, TR/EN metinler.

Kontroller: köksüz testler, her iki özellik setinde Clippy, rustfmt, release
derlemesi ve özel D-Bus/Broadway oturumunda GTK smoke. Smoke; TR/EN,
420 piksel görünüm, mevcut büyük yazı kontrolü, bağlantı/kota seçimi,
yüklenme/boş/bilinmeyen durumlar ve tepsiden önizleme/onay korumasını kapsar.
Güncel sonuçlar `docs/handoff.md` dosyasındadır.

İzole test verisiyle ekran görüntüleri:
[TR](screenshots/overview-tray/overview-tr.png),
[EN](screenshots/overview-tray/overview-en.png),
[dar TR](screenshots/overview-tray/overview-narrow-tr.png),
[dar EN](screenshots/overview-tray/overview-narrow-en.png).

Bu UI değişikliği için host yönlendirmesi, kurulu paket/servisler veya kernel
değiştirilmedi. Gerçek GNOME tepsi uzantısı, yardımcı programın etkileşimli
Polkit penceresi ve canlı trafik bu testlerle yeniden doğrulanmış sayılmaz.
