# Uygulama-Oturum Yönlendirmesi Tasarımı

**Tarih:** 2026-08-04  
**Durum:** Onaylandı

## Amaç

RouteLane, seçilen Linux masaüstü uygulamalarının tüm yeni ağ bağlantılarını
seçilen lane üzerinden yönlendirecektir. Bu, HTTPS içeriğini incelemeden
uygulamanın CDN, kimlik doğrulama ve Cloudflare doğrulama bağlantılarının da
aynı dış IP'den çıkmasını sağlar.

Kullanıcı hem RouteLane üzerinden yeni bir uygulama başlatabilecek, hem de
halihazırda çalışan uygun süreçleri seçili lane'e alabilecektir.

## Kapsam ve sınırlar

- Uygulama politikası domain, IP/CIDR ve blok kurallarına ek bir yönlendirme
  kaynağıdır; mevcut kuralların davranışını değiştirmez.
- Uygulama politikası, seçilen uygulamanın tüm bağlantılarını seçili lane'e
  yönlendirir. Aynı tarayıcı profilinde açılan diğer sekmeler de bu kapsama
  girer.
- RouteLane HTTPS trafiğini çözmez, araya girmez ve üçüncü taraf alan adlarını
  tahmin etmeye çalışmaz.
- İlk sürüm yalnızca Linux cgroup v2 ve nftables `socket cgroupv2` desteği
  bulunan Ubuntu, Debian ve Fedora kurulumlarını resmi kapsamda destekler.
- Flatpak, Snap, portal ve sandbox sınırları ilk sürümde açıkça tespit edilip
  desteklenmiyorsa anlaşılır bir durum mesajı gösterilir; host süreçlerini
  zorla taşıma denenmez.

## Kullanıcı deneyimi

Yapılandırma ekranına bir **Uygulamalar** bölümü eklenir.

1. Kullanıcı sistemdeki `.desktop` girdilerinden uygulama seçer. Girdi
   bulunmadığında veya özel araçlar için çalıştırılabilir dosya yolu ekleyebilir.
2. Kullanıcı her uygulama politikası için bir lane seçer ve politikayı
   etkinleştirir veya pasifleştirir.
3. **RouteLane ile başlat** eylemi, uygulamayı RouteLane'in ayrılmış cgroup'u
   altında başlatır.
4. Açık bir eşleşen süreç algılandığında, satırda **Şimdi lane'e al** eylemi
   görünür. Başarılı aktarım, erişilebilir canlı durum metniyle bildirilir.
5. Satır, etkisini açıkça söyler: “Bu uygulamanın tüm yeni bağlantıları
   [lane adı] üzerinden yönlendirilir.”

Klavye ile satırlar odaklanabilir; başlatma ve süreç taşıma düğmelerinin
erişilebilir isimleri uygulama ve lane adını içerir. Başlatma, taşıma,
yetkilendirme ve uyumsuz ortam hataları toast yerine kalıcı, okunabilir durum
satırında da gösterilir.

## Veri modeli

`Profile` içine sıralı `application_policies` eklenir. Her kayıt şunları
taşır:

- Değişmez politika kimliği
- Görünen uygulama adı ve isteğe bağlı `.desktop` kimliği
- Başlatılacak komutun mutlak yürütülebilir yolu ile argümanları
- Süreç eşleştirmesi için yürütülebilir dosya kanonik yolu
- Hedef `LaneId`
- Etkinlik durumu

Doğrulama kuralları: kimlikler benzersizdir; hedef lane mevcut olmalıdır;
yürütülebilir yol boş olmamalıdır; `.desktop` kimliği varsa benzersiz olmalıdır.
Konfigürasyon sürümü, eski profil dosyalarını boş uygulama politikası listesiyle
geriye uyumlu okur.

## Cgroup ve ayrıcalık sınırı

Helper, yalnızca `/sys/fs/cgroup/routelane.apps` altındaki RouteLane'e ait
cgroup hiyerarşisini yönetir. Politika kimliği için sabit, güvenli biçimde
kodlanmış bir alt cgroup oluşturulur.

Helper API'si ham cgroup yolu veya komut kabul etmez. Şu dar operasyonları
sağlar:

- Etkin profil planında yer alan politika için cgroup hazırlama
- Doğrulanmış bir PID'yi, kanonik yürütülebilir yolu politika ile eşleşiyorsa
  ilgili cgroup'a taşıma
- Devre dışı bırakılan politikaların boş cgroup'larını temizleme

Başlatma akışı, uygulama sürecini doğrudan doğru cgroup'a yerleştirir; daemon
başlatılan PID'yi helper'a bildirir. Mevcut süreç aktarımında helper `/proc/PID/exe`
ile izinli yürütülebilir dosyayı yeniden doğrular. PID tekrar kullanımı, süreç
başlangıç zamanı denetimiyle engellenir.

Bir uygulamanın fork ettiği alt süreçler cgroup üyeliğini devralır. Başlatılan
ana süreç kapansa bile kalan alt süreçler kendi bağlantılarını aynı politikayla
sürdürür. Cgroup boşaldığında ek işlem gerekmez; devre dışı politika temizliği
yalnızca boş cgroup üzerinde yapılır.

## nftables ve politika yönlendirme

Her etkin uygulama politikası için nftables `output` zincirine, mevcut domain
ve IP/CIDR işaretleme kurallarından önce bir `socket cgroupv2` eşleşmesi
eklenir. Eşleşme pakete hedef lane'in ayrılmış markını yazar; mevcut `ip rule`
ve lane tablo rotaları paketi seçili arayüze ulaştırır.

Öncelik uygulama politikasındadır: seçilen uygulamanın bağlantıları her zaman
uygulama lane'ini kullanır. Uygulama dışında domain/IP/CIDR kuralları mevcut
sıralarıyla çalışır. Bu öncelik kullanıcı arayüzünde ve dokümantasyonda
gösterilir.

IPv4 ve IPv6 eşleşmeleri aynı cgroup markından faydalanır. Hedef lane'de
uygun aile rotası yoksa mevcut lane hata politikası (`Direct`, `Fallback`,
`Block`) uygulanır.

## Daemon akışı

1. Profil kaydedilir ve planlayıcı etkin uygulama politikalarını lane markları
   ile kernel planına dönüştürür.
2. Helper atomik nftables/policy-route etkinleştirme işleminde cgroup
   hazırlığını da yapar.
3. Daemon, masaüstü girdilerini okur ve çalışan süreçleri salt-okunur olarak
   tarar; eşleşen fakat RouteLane cgroup'unda olmayanları UI'a bildirir.
4. Kullanıcı başlattığında daemon uygulamayı tanımlı komutla başlatır ve
   helper PID kimlik doğrulamasından sonra süreci cgroup'a alır.
5. Kullanıcı “Şimdi lane'e al” dediğinde daemon PID ile başlangıç zamanını
   helper'a gönderir; helper eşleşmeyi tekrar doğrular ve aktarımı yapar.
6. Helper sonucu daemon'a döner; UI kalıcı durum satırını günceller.

Plan uygulanamazsa mevcut atomik geri alma davranışı korunur. Uygulama cgroup
hazırlama hatası, yarım etkinleşmiş bir uygulama yönlendirme durumu bırakmaz.

## Hata ve sağlık davranışı

- cgroup v2 veya `socket cgroupv2` bulunamazsa uygulama yönlendirmesi
  etkinleşmez; domain kuralları çalışmaya devam eder ve UI uyumluluk nedenini
  gösterir.
- Uygulamanın yürütülebilir dosyası değişmişse taşıma reddedilir; kullanıcı
  politikayı güncellemeye yönlendirilir.
- Yetki reddi, uygulama adı ve tekrar deneme eylemi ile gösterilir.
- Çalışan süreç bulunamazsa başlatma eylemi sunulur; bu hata değildir.
- Sağlık raporu, her etkin uygulama politikasının cgroup ve nft eşleşmesini
  doğrular; başarısızlık ilgili lane'i değil uygulama politikasını `Degraded`
  olarak belirtir.

## Test stratejisi

- Domain modeli: benzersizlik, lane referansı, geriye uyumlu serileştirme ve
  hatalı yürütülebilir tanımları.
- Planlayıcı/nft renderer: uygulama işaretinin domain kurallarından önce
  üretildiği ve doğru lane markını kullandığı.
- Helper: yalnız RouteLane cgroup yoluna erişim, PID yürütülebilir doğrulaması,
  başlangıç zamanı denetimi ve boş cgroup temizliği.
- Daemon: masaüstü girdi çözümlemesi, başlatma, mevcut süreç keşfi ve IPC hata
  dönüşümü.
- UI: uygulama seçim, lane değişimi, başlatma/taşıma eylemleri ve erişilebilir
  etiketler.
- Entegrasyon: test süreci bir uygulama cgroup'una alındığında paket markının
  doğru policy-route tablosunu seçtiği; domain kurallarının uygulama dışı
  süreçlerde değişmediği.

## Başarı ölçütleri

- Kullanıcı, desteklenen bir masaüstü uygulamasını tek seçimle lane'e bağlayıp
  başlatabilir.
- Önceden açık eşleşen süreç, kullanıcı eylemiyle aynı lane'e güvenle taşınır.
- Uygulamanın CDN ve doğrulama bağlantıları ek domain tanımı olmadan aynı
  dış ağdan çıkar.
- Desteklenmeyen sandbox ortamı, trafik sızıntısı veya sessiz başarısızlık
  üretmez.
