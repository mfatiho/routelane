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

## Arayüz kaybı / aynı adla yeni cihaz: ek doğrulama

VM’de nftables 1.0.9, `meta oif 2` ifadesini cihaz varken `eth0`, silinince
`2` string’iyle dump ediyor. Ayrıca adı `0`, index’i `9` olan bir dummy cihaz
oluşturulduğunda native `iif 0` ile `iif 9` dump’ta aynı `"0"` string’iyle
göründü. Sayısal-only adlar bu ABI’de exact kimlik kanıtı için belirsizdir;
güvenlik kontrolü bunları tahminle eşleştirmek yerine reddeder.
[nft meta ifindex printer/parser kaynak kodu](https://sources.debian.org/src/nftables/1.1.3-1/src/meta.c/#L1185),
[literal-index regression testi](../../tests/nftables_contract.rs).

Üretici artık ifindex’i doğrudan sayı olarak yüklüyor; eski plan yeniden
yüklendiğinde adın yeni cihaz üzerinde çözülmesi mümkün olmuyor. Canlı dump’ın
yalnız ifindex-typed match operand’ları, iki read-only link kimlik snapshot’ı
aynı kalmışsa güncel gözlenen map ile sayıya çevriliyor. Eski name→index
varsayımları, name predicate’leri veya başka semboller bu dönüşümde kullanılmaz.
[Canlı exact proof sınırı](../../src/backend/privileged.rs),
[ortak üretici](../../src/backend/nft_document.rs).

VM kapısı 86 nesnede hem JSON hem TEXT için tekrar exact geçti; cihaz `2`
silinip `eth0` adı index `9` ile oluşturulduğunda da eski programın numeric
kimlik kanıtı değişmedi. Bu codec/scope kanıtıdır; otomatik runtime ve Polkit
kabul kapılarını tamamlamış sayılmaz.

## Terminal route ve dar arayüz ayarı (15 Ekim 2026, Task 6)

Onaylı Ubuntu 24.04 VM’de (kernel 6.8.0-142, iproute2 6.1.0, nft 1.0.9),
yalnız bu iş için açılıp kapatılan geçici netns içinde ölçüldü:

- IPv4’te `ip route replace` unreachable ile unicast default’u birbirine
  çevirir. Arayüz kapanınca veya adres silinince kernel lane tablosundaki
  unicast default’u siler; tablo boş kalırsa mark’lı trafik sonraki kurallara,
  dolayısıyla main tabloya düşer.
- Unicast (metric 0/1024) ile düşük öncelikli unreachable default aynı tabloda
  birlikte durur; link kaybında yalnız unreachable kalır ve lookup reddedilir.
- IPv6’da tabloda yalnız unreachable varken global adresli `via` gateway ile
  unicast eklemek `No route to host` ile başarısız olur (gateway önce bu
  tabloda doğrulanır). Link-local gateway, device route ve mevcut bir
  unicast’in değiştirilmesi çalışır.
- iproute2 6.1 `ip -6 route flush`, metric 4294967295 olan IPv6 unreachable
  default’u tablonun null entry’si sayıp atlar; 4294967294 ve daha düşükleri
  iki ailede de flush eder.

Karar: her lane×aile, unicast çıkışından bağımsız olarak metric 4294967294
unreachable default ve kendi policy rule’una sahip olur. IPv6 global gateway’li
unicast, yalnız terminal bulunan bir tabloya girerken transaction guard’ı
altında terminal geçici kaldırılıp unicast’ten sonra yeniden eklenir.
Komutlar ve crash-prefix durumları aynı tipli adım modelinden türetilir.
[Adım modeli](../../src/backend/policy_route.rs),
[ownership ve crash modeli](../../src/backend/privileged.rs).

`ProtectKernelTunables=yes` + `ReadWritePaths=/proc/sys/net/ipv4/conf`
(systemd 255) yalnız `conf` alt ağacını yazılabilir yapar; `ip_forward` salt
okunur kalır. Ölçüm değerleri değiştirmeden aynı değeri geri yazarak yapıldı.
Helper kodu yalnız seçili arayüzün `src_valid_mark` değerini yazar; all/default
yazılmaz. Gerçek kurulu servisle doğrulama Task 12 kapsamındadır.

Gerçek helper root süiti (standart ve run-owned client netns’te strict RPF)
6/6 geçti: strict modda üç lane arayüzünde `src_valid_mark=1` yazılıp temizlikte
0’a döndü, global değer hiç yazılmadı; lane C kapatıldığında tablo 20002’de
iki ailede yalnız terminal route kaldı ve mark’lı lookup reddedildi. Aynı test
Task 6 öncesi helper ile terminal route kontrolünde başarısız oldu. Journal
kaybolursa makbuz da kaybolur; orphan recovery önceki değeri kanıtlayamadığı
için ayara dokunmaz. Bu kanıtlar TCP/UDP/IPv6 trafik, failback, Polkit veya
Debian/Fedora kapılarını kapatmaz.

## Task 7 kernel kapısı (15 Ekim 2026)

Aynı onaylı VM’de, run-owned namespace’lerde gerçek helper ile
`tests/root/primary_failover.rs` senaryoları standart ve strict-RPF
modlarında geçti (tam root süiti 16/16). Uç noktalar gördükleri kaynak adresi
geri yazar; her TCP/UDP iddiası seçilen uplink’i ve yanıtın orijinal sokete
dönüşünü birlikte kanıtlar:

- OS default B iken birincil A (OS default olmayan uplink) ve kural lane’i C:
  TCP ve UDP kaynakları sırasıyla 10.10.1.2 ve 10.10.3.2.
- Yedek C üzerinde açılan uzun TCP oturumu, A geri gelip yeni nesil
  uygulandıktan sonra C üzerinden devam etti; yeni TCP/UDP akışları A’dan çıktı.
- Onaylı IPv6 lane’i yokken IPv6, OS default B’den kaçmadı (gateway B ingress
  sayacı değişmedi); cleanup OS IPv6 yönlendirmesini geri getirdi. IPv6-only
  lane B ile IPv6 B’den 2001:db8:2::2 kaynağıyla, IPv4 A’dan çıktı.
- Loose ve strict RPF’te doğru lane yanıtları kabul edildi; strict modda
  yanlış yoldan gelen kaynak düşürüldü, loose modda kabul edildi (RouteLane
  rp_filter değerini hiç yazmaz; loose modda yanlış-yol reddi RouteLane’in
  vaadi değildir).
- DHCP yayın/yanıt, bağlı LAN üzerindeki DNS/TCP ve 127.0.0.53 stub’ı aktif
  yönlendirme sırasında çalıştı; yabancı nft tablosu, ip rule, tablo 100,
  main route’lar ve global tunable’lar cleanup sonrası birebir aynı kaldı.
- Adres kaybı, owned tablodan default silinmesi ve tüm uplink’lerin kapanması
  sırasında hedef paketleri diğer gateway’lere ulaşmadı. Kernel, nft’nin
  OUTPUT’ta mark atadığı yerel paketi terminal route’a düşünce yeniden
  yönlendirme hatasıyla sessizce düşürür; uygulama ENETUNREACH değil zaman
  aşımı görür.

Aynı senaryolar schema-1 mark-only semantiğiyle (birincil kavramı yok, kaynak
düzeltmesi yok) 5/6 başarısız oldu: birincil/yedek hedefler OS default’tan
gidip “Network is unreachable” aldı, IPv6 OS default’tan kaçtı, adres kaybında
paket gateway B’ye ulaştı. Yerel kontroller senaryosu bir regresyon korumasıdır
ve eski semantikte de geçer. Bu kanıtlar otomatik runtime (Task 8), Polkit ve
Debian/Fedora kapılarını kapatmaz.
