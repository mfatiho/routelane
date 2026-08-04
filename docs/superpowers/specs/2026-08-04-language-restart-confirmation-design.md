# Dil Değişikliği Yeniden Başlatma Onayı

**Durum:** Kullanıcı tarafından onaylandı

## Amaç

Arayüz dili değişmeden önce kullanıcıya uygulamanın yeniden başlatılması
gerektiğini açıkça bildirmek ve açık onay almadan yapılandırmayı değiştirmemek.

## Akış

1. Kullanıcı Tercihler'deki dil seçicisini değiştirir.
2. Seçim önceki değerine geri alınır ve modal onay iletişim kutusu gösterilir.
3. İletişim kutusu başlığı `Yeniden başlatma gerekli` olur.
4. Metin şu anlamı taşır: Dil paketini güncellemeden önce uygulamayı yeniden
   başlatmak gerekir; kullanıcıdan değişikliği onaylaması istenir.
5. `Vazgeç`, dili ve yapılandırmayı değiştirmez.
6. `Onayla`, yeni dili kalıcı yapılandırmaya kaydeder ve uygulamayı kapatır.
   Kullanıcı uygulamayı yeniden açtığında yeni dil yüklenir.

## Sınırlar

- Mevcut rota, kural, daemon ve Polkit akışları değişmez.
- Yeni bağımlılık eklenmez.
- Yeni görünür metinler Türkçe ve İngilizce olarak `Texts` üzerinden sağlanır.
- Dil seçimi geri alma sırasında tekrar onay penceresi üretmez.

## Doğrulama

- Saf karar fonksiyonu, aynı dil için onay istemediğini; farklı dil için
  onay istediğini doğrular.
- Onaylı değişiklik dilin kalıcı olarak kaydedilmesini; iptal ise mevcut dilin
  korunmasını doğrular.
- UI testleri, seçici geri alma ve onay/iptal callback akışlarını kapsar.
