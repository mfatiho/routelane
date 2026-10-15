# Ana hat geçişi: kernel araştırma notu

Tarih: 14–15 Ekim 2026. Kapsam: uygulama planı ve izole test VM’si;
ana makinenin yönlendirme durumunda değişiklik yapılmadı.

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

## Tasarım öncesi başlangıç durumu

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

## İzole Multipass ortamı ve ilk bulgular

Kullanıcı onayıyla `routelane-test-u2404-20261014` oluşturuldu: 2 vCPU,
2 GiB bellek, 12 GiB disk; Ubuntu 24.04.5, kernel `6.8.0-142-generic`,
nftables `1.0.9`. Mevcut dört VM değiştirilmedi; kişisel dizin veya repo
mount edilmedi. VM içinde `sudo unshare --net ip -j link show` başarılı.
Bu, ortam hazırlık kanıtıdır; ürün/failover/Polkit kabul sonucu değildir.

Multipass özel cloud image için cloud-init ve SSH gerektirir;
Ubuntu VM’de başarı Debian/Fedora kabul testlerinin yerine geçmez.
[Canonical Multipass Image belgesi](https://canonical.com/multipass/docs/latest/explanation/image/).

Mevcut feature-gated gerçek-helper harness’i VM’de çalıştırıldı. İlk
uygulama kernel/journal eşleştirmesinde başarısız oldu. Aynı VM’nin geçici
network namespace’inde eski text fixture yüklenip `nft -j list table`
ile geri okunduğunda boş `reject` ifadesinin kernel tarafından IPv4 için
`{"type":"icmp","expr":"port-unreachable"}`, IPv6 için
`{"type":"icmpv6","expr":"port-unreachable"}` olarak genişletildiği
görüldü. Helper’ın beklenen JSON’unda tür/kod açık yazıldı; farklı reject
kodlarının aynı ownership fingerprint’ine dönüşmediği ayrıca test edildi.
Counter değerleri dışında semantik silen bir normalizasyon eklenmedi.
[JSON REJECT biçimi](https://manpages.debian.org/bookworm/libnftables1/libnftables-json.5.en.html#REJECT),
[yerel üretim ve regression testi](../../src/backend/privileged.rs).

Düzeltmeden sonraki root koşusu commit eşleştirmesini geçti, ancak
ilk `203.0.113.10` paket probunda yanıt alamadı: **5 geçti, 1 başarısız**.
Bu, mark-only yönlendirmenin gerçek kaynak/dönüş yolunu henüz
sağlamadığını gösteren RED kanıtıdır; scoped NAT/flow-affinity başarısı
olarak raporlanamaz. Stdio harness’i gerçek backend’i çalıştırır ama
Polkit’i test etmez; system-bus/Polkit kabul kapısı ayrı kalır.
[Gerçek helper namespace testi](../../tests/root/namespace_routing.rs).

## 15 Ekim: ortak renderer ve gerçek-helper ilerlemesi

Ortak politika üreticisi artık JSON ve text çıktısını aynı belgeye dayandırıyor.
İzole VM’de explicit IPv4 Route ve IPv6 Block içeren 86 nesnelik politika
iki biçimde de yüklenip kernel’den geri okundu: **JSON EXACT, TEXT EXACT**.
Karşılaştırma yalnız nft metadata/handle ve çalışan counter değerlerini yok saydı;
mark, aile, sıra, hedef, hook, kaynak kanıtı ve verdict ifadeleri korunarak karşılaştırıldı.
[Ortak renderer ve davranış testleri](../../tests/nftables_contract.rs).

Kernel, IP payload’dan çıkarabildiği redundant protokol koşullarını dump’tan
kaldırıyor; ardışık mark sınırlarını range’e birleştiriyor. Bu biçimleri doğrudan
üretmek exact ownership hatalarını giderdi; semantik silen genel bir eşitlik
normalizasyonu eklenmedi. `meta iif/oif` adlarının yükleme sırasında ifindex’e
bağlanması ve `ct original ip/ip6 saddr` dump biçimi de gerçek VM’de doğrulandı.
[nftables ifadeleri](https://netfilter.org/projects/nftables/manpage.html),
[ortak politika kodu](../../src/backend/nft_document.rs).

Güncel gerçek-helper root harness’i **6/6 geçti**: üç lane IPv4 paket erişimi,
Block/Direct sayaçları, activation-failure rollback, önceki generation’a restore
ve journal kaybı sonrası unchanged owned-table recovery. Bu stdio test transport’u
Polkit kanıtı değildir. Strict RPF, gerçek TCP/UDP/IPv6 bağlantıları, otomatik
failover/failback runtime’ı ve Debian/Fedora kabul kapıları hâlâ tamamlanmalıdır.
Ana makinenin routing/kernel ayarları ve mevcut dört VM değiştirilmedi.

Son koşuda pasif paket gözlemi de kaynak düzeltmesini doğruladı: A hattındaki
ARP artık `who-has 10.10.1.1 tell 10.10.1.2`; ardından
`10.10.1.2 > 203.0.113.10` istek ve karşılık gelen ICMP yanıtı görüldü.
B/C hatlarında da kendi kaynak adresleriyle istek/yanıt alındı. Önceki
mark-only koşudaki çapraz kaynaklı, yanıtsız ARP yeniden oluşmadı.

Yeni `rules_v2_…` consistency seal, committed NFT politikasının değişmediğini
kontrol eder; yetkilendirme imzası değildir. Geçici guard bunun dışında tutulur,
ama journal’ın guarded/pending exact fingerprint’i bütün guard ifadelerini
doğrulamaya devam eder. Journal yoksa modern guarded tablo otomatik orphan
cleanup ile silinmez; doğrulanamayan bir geçiş durumunda temizlik reddedilir.
[Ownership ve regression testleri](../../src/backend/privileged.rs).
