# Ana hat geçişi: kernel araştırma notu

Tarih: 14 Ekim 2026. Kapsam: uygulama planı; çalışan sistemde deney yapılmadı.

## Doğrulanan kaynak bilgileri

- Linux'ta `rp_filter=1` strict, `2` loose doğrulamadır. Etkin değer,
  `all` ile arayüz değerinin maksimumudur. `src_valid_mark=1`, reverse-path
  sorgusuna paket işaretini dahil eder; işaretle iki yönlü routing için destek
  sağlar. Burada da maksimum değer kullanılır.
  [Linux IP sysctl belgesi, rp_filter ve src_valid_mark](https://docs.kernel.org/networking/ip-sysctl.html#conf-interface).
- nftables `route/output` zinciri yeniden route lookup için kullanılır.
  `ct mark set meta mark` bağlantı işareti kaydeder. NAT bağlantı takibine
  bağlıdır; masquerade çıkış arayüzünün adresini kullanır. `inet` NAT için
  kernel 5.2 desteği belgelenmiştir. Başka base chain'in drop kararı,
  RouteLane'in accept kararıyla geçersiz kılınmaz.
  [nftables resmî kılavuzu: CHAINS, CONNTRACK STATEMENT, NAT STATEMENTS](https://netfilter.org/projects/nftables/manpage.html).
- `ProtectKernelTunables=yes`, `/proc/sys` ve ilgili kernel yollarını servis
  içinde salt okunur yapar. Dar bir yazma ihtiyacı mevcut sandbox ile birlikte
  değerlendirilmelidir; tüm korumayı kaldırmak gerekmemelidir.
  [systemd.exec birincil kaynak, ProtectKernelTunables](https://github.com/systemd/systemd/blob/main/man/systemd.exec.xml#L2110).

Bu kaynaklar bugünkü upstream belgeleridir; dağıtımların kurulu kernel/nft/systemd
sürümleri için çalıştırılmış uyumluluk kanıtı değildir.

## Depoda doğrulanan durum

- [Topology keşfi](../../src/daemon/service.rs:1019), bütün tablolardaki default
  rotaları topluyor; metric, carrier ve kaynak adresi taşımıyor. Kendi routing
  tablolarını sistem varsayılanından ayırmak gerekiyor.
- [KernelPlan](../../src/backend/plan.rs:17) ana çıkış veya akış bağlılığı taşımıyor.
- [Text renderer](../../src/backend/nftables.rs) ile
  [ayrı JSON üreticisi](../../src/backend/privileged.rs:1753) aynı yeni davranışı
  üretmeli; yalnız text renderer değişikliği çalışan helper'ı düzeltmez.
- [Helper servis API'si](../../src/backend/privileged.rs:3763), mutasyonlarda
  yetkilendirme istiyor. Onaylı runtime kapsamı bugün yok.
- [Servis sandbox'ı](../../data/routelane-helper.service:20) kernel tunable
  korumasını etkin tutuyor. Gerçek servis altında dar ayar testi zorunlu.

## Mühendislik kararı; henüz çalışma kanıtı değil

Önce bağlantı takibine kayıtlı, yalnız RouteLane mark/interface eşleşmesine
uygulanan kaynak adresi düzeltmesini namespace testinde kanıtla. Yerel,
forward edilen ve yabancı işaretli trafiğe kapsamı genişletme. IPv4 strict RPF
için önce sadece onaylı arayüzün `src_valid_mark` ayarını kullan; `rp_filter`,
`all`, `default`, forwarding veya kalıcı sysctl dosyalarını değiştirme.
Önceki değeri journal'a yaz; cleanup'ta kimlik ve mevcut değer hâlâ eşleşiyorsa
geri al. systemd sandbox istisnasını gerçek servis altında test et.

Bu yaklaşımın yerel TCP/UDP, IPv6, dönüş trafiği ve mevcut yardımcı hat
oturumlarını koruduğu ancak gerçek kernel testleriyle söylenebilir. Başarısız
testte özellik yayınlanmaz; test atlanması başarı sayılmaz. TUN/proxy,
tarayıcıya müdahale veya global NAT alternatifine kendiliğinden geçilmez.
