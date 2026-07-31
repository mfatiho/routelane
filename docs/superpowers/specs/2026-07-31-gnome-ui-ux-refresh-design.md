# RouteLane GNOME UI/UX Refresh Tasarımı

**Durum:** Kullanıcı tarafından onaylandı  
**Tarih:** 31 Temmuz 2026  
**Hedef:** RouteLane'in mevcut routing işlevlerini koruyarak sade, modern,
erişilebilir ve GNOME uyumlu bir masaüstü deneyimi sunması.

## Kapsam

- Ana pencerede durum kartı, tek ana eylem ve ihtiyaç-temelli başlangıç akışı.
- Aktif rota, kurallar ve bağlantı sağlığı için az yoğunluklu bölümler.
- Tutarlı başarı, yüklenme, yetkilendirme, hata ve boş durum geri bildirimi.
- Kural/rota düzenleme için erişilebilir modal ve Preferences akışları.
- Klavye odağı, Enter/Escape davranışı, metinsel eylemler ve iki dilde copy.

Routing planı, daemon IPC, helper yetki sınırı, `ConfigV2` veri biçimi ve
mevcut GTK4/Libadwaita bağımlılıkları değişmez.

## Bilgi Mimarisi

Ana pencere sırası:

1. **Durum kartı:** Etkin, pasif, uygulanıyor, yetki bekleniyor, sağlıklı,
   uyarı veya hata durumunun metni, sembolü ve tek ana eylemi.
2. **Başlangıç kartı:** Yalnız eksik rota veya kural varsa görünür; sonraki
   adım için Tercihler veya Kural Ekle eylemini açar.
3. **Aktif rota:** Rota adı, arayüz ve hata politikası kısa özet olarak.
4. **Kurallar:** Sayaç/açıklama, görünür “Kural ekle” eylemi ve sıralı kural
   satırları.
5. **Bağlantı Sağlığı:** Eyleme geçirilebilir teşhisler; teknik ayrıntılar
   isteğe bağlı expander içinde.

Tercihler; rota, hata politikası, dil ve gelişmiş ayarları taşır. Ana ekran
doğrudan düzenleme alanlarıyla doldurulmaz.

## Durum ve geri bildirim

| Durum | Sunum | Kullanıcı eylemi |
| --- | --- | --- |
| Pasif | Nötr kart, açıklayıcı ana eylem | Etkinleştir |
| Uygulanıyor | Ana eylem devre dışı, yüklenme metni | Bekle |
| Yetki bekleniyor | Polkit açıklaması, tekrarlı istek yok | Sistem penceresini tamamla |
| Yetki reddedildi | Açık hata metni ve yeniden dene | Yetkilendirmeyi yeniden dene |
| Sağlıklı | Başarı metni ve seçili rota özeti | Devre dışı bırak |
| Uyarı/hata | Özet, önerilen eylem, kopyalanabilir ayrıntı | Sorunu çöz / tekrar dene |

GUI `sudo` çalıştırmaz veya parola saklamaz. Ayrıcalıklı işlem mevcut system
D-Bus + Polkit helper üzerinden sürer.

## Görsel ve erişilebilirlik ilkeleri

- Libadwaita `PreferencesGroup`, `ActionRow`, `ToastOverlay` ve önerilen/
  destructive CSS sınıfları kullanılacak; özel tema veya yeni bağımlılık yok.
- Boşluklar 6/12/24 px GTK aralığı ile sınırlı; ikonlar yalnız destekleyicidir.
- Renk hiçbir zaman tek anlam taşımaz; metin ve sembol eşlik eder.
- Her ana eylemin görünür metni, tooltip'i ve erişilebilir adı olur.
- Enter, varsayılan dialog eylemini; Escape, iptali çalıştırır. Hata sonrası
  odak ilgili girdiye geri döner.
- Dar pencerede satırlar taşmaz; kontrol yoğunluğu dialog/Preferences içinde
  dikey akışa alınır.

## Kod sınırları ve doğrulama

- Saf durum/karar fonksiyonları `src/ui/experience.rs` içinde kalır.
- Widget kurulum ve olay bağlama `src/ui/window.rs` içinde, tekrar eden
  sunum yardımcıları küçük fonksiyonlarda tutulur.
- İki dilde tüm yeni metinler `src/ui/i18n.rs` üzerinden gelir.
- Testler önce RED: başlangıç adımı, durum/eylem eşleşmesi, yetki ret/başarı,
  kural dialog doğrulaması, erişilebilir metin yüzeyi ve tek pencere aktivasyonu.
- Final: fmt, rootless test suite, clippy, release build ve Debian paket
  doğrulaması.
