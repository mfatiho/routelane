# Seçici yönlendirme ve sistem davranışının korunması

Tarih: 22 Ekim 2026.
Durum: Kullanıcı yazılı tasarımı 22 Ekim'deki “Devam et” yanıtıyla onayladı. [Uygulama planı](../plans/2026-10-22-selective-routing.md) hazırlanmıştır ve ayrı inceleme bekler; ürün kodu ve canlı ağ değiştirilmedi.

## 1. Amaç ve önceki kararla ilişkisi

Kullanıcının güncel isteği: RouteLane'e eklenen kurallar dışındaki trafik, RouteLane kapalıyken işletim sistemi nasıl yönlendiriyorsa öyle devam etsin. Multipass/Docker/VPN veya başka ağların çalışması, yalnız RouteLane'in etkinleştirilmesiyle bozulmasın.

Bu karar, [14 Ekim ana bağlantı tasarımının](2026-10-14-primary-connection-failover-design.md) eşleşmeyen tüm internet trafiğini ana hatta gönderen varsayılanını değiştirir. Önceki özellik silinmez; ayrı ve açıkça seçilen bir mod olur. Yeni varsayılan seçici yönlendirmedir. Kod tabanı hâlen eski davranışı uygular; bu belge uygulanmış bir düzeltme değildir.

Başarı ölçütü: aynı ortamda yeni ve eşleşmeyen bir bağlantının paket/conntrack işareti, kaynak adresi ve çıkışı RouteLane nedeniyle değişmez; eşleşen Route ve Block kuralları ise öncelik sırasıyla çalışır. İşletim sisteminin veya başka bir aracın kendi değişiklikleri dondurulmaz.

## 2. Yaklaşım seçimi

- Lab için `priority 9000 lookup main` eklemek: tek alt ağ için geçici bir RPDB istisnasıdır; diğer ağları, yabancı işaretlerin reddini ve nftables geçiş korumasını çözmez. Kalıcı ürün çözümü olarak seçilmedi.
- Yalnız nftables'ın son ana-hat kuralını kaldırmak: normal yeni trafiğe yardımcı olur ancak global foreign-mark reddini ve global transaction guard'ını bırakır. Eksik çözüm olduğu için seçilmedi.
- Profilde açık kapsam seçimi ve bu kapsamın planner/helper/guard/UI boyunca taşınması: önerilen yaklaşım. Yeni ağ yöneticisi, daemon, proxy, kernel modülü veya bağımlılık eklenmez.

Bu çalışma mevcut bileşenlerin ortak politika sözleşmesini değiştirdiği için mimaridir. Önce bu tasarım, ardından ayrı yazılı uygulama planı onaylanır. Yürütme tercihi önceki kullanıcı kararıyla main üzerinde inline; yeni worktree veya alt ajan yoktur.

## 3. Veri modeli ve iki mod

`RoutingPolicy` zorunlu bir `unmatched_traffic` alanı taşır:

- `PreserveSystem`: UI adı **Yalnız kuralları yönlendir**. Yeni profillerin varsayılanı. Hiçbir etkin kuralla eşleşmeyen yeni trafik için mark, conntrack mark, NAT veya çıkış seçimi yapılmaz. Ana hat/yedek seçimi yalnız açık Direct ve ilgili lane fallback kararlarında kullanılır; bütün sisteme failover uygulanmaz.
- `UsePrimary`: UI adı **Diğer trafiği de ana hatta yönlendir**. Kullanıcının ayrı onayıyla etkinleşir. Açık kurallar dışındaki yeni internet trafiğini ana hatta, gerekirse onaylı yedeklere yönlendiren önceki özelliği korur.

Mod `KernelPlan` içinde zorunlu olarak taşınır, plan ve runtime policy hash'lerine katılır. Mod değişimi yeni kullanıcı yetkilendirmesi gerektirir; mevcut runtime grant'i otomatik olarak kapsam genişletemez. IPv4 ve IPv6 ayrı değerlendirilir.

Mevcut Route/Direct/Block kimlikleri ve öncelik semantiği korunur. `Direct` halen **Ana hattı kullan** anlamındadır; UI artık “sistem rotasını koru” diye sunmaz. Bu çalışmada Direct sessizce yeniden yorumlanmaz ve dördüncü bir kural eylemi eklenmez. `Any` hedefli bir kuralın tüm trafiği açıkça eşleştirebileceği önizlemede belirtilir.

Lane'in Block/Direct/Inactive hata politikaları mevcut anlamlarını korur. Inactive nedeniyle üretilmeyen bir verdict sonraki açık kurallara, onlar da eşleşmezse seçilen modun varsayılanına gider; bunun Block garantisi olmadığı açıklanır.

## 4. Paket akışı ve müdahale sınırı

Yeni renderer aynı semantik kaynaktan hem JSON hem okunabilir nft çıktısı üretir:

1. Mevcut dar host-local ve link kontrol istisnaları korunur; genel bir RFC1918 muafiyeti eklenmez.
2. Açık kurallar mevcut öncelik/ilk-eşleşme sırasıyla değerlendirilir. Route yalnız seçilen veya açık fallback lane'ini, Block yalnız eşleşen kapsamı etkiler. Gerekli aile/çıkış yoksa ilgili korumalı trafik fail-closed kalır.
3. Başka araçlara ait meta/conntrack mark'ları silinmez veya RouteLane mark'ına dönüştürülmez. `PreserveSystem` modunda global foreign-mark reject yoktur. Yabancı işaretli trafik açık Route/Direct kuralıyla eşleşirse işareti değiştirmeden yalnız o eşleşme reddedilir; bu desteklenmeyen çakışma UI açıklamasında belirtilir. Açık Block yine uygulanır. Eşleşmeyen yabancı işaretli trafik aynen bırakılır.
4. Doğrulanmış mevcut RouteLane oturumlarının lane bağlılığı korunur. Eski bildirimler veya yalnız mark aralığı, yeni bir çıkış yetkisi vermez; mevcut dar arayüz/yerel kaynak kanıtları korunur.
5. `PreserveSystem` modunun kalan yeni trafiği `return` ile sonlanır; primary seçme veya genel reject üretilmez. `UsePrimary` yalnız kendi onaylı kapsamı için önceki primary davranışını kullanır.

Prerouting reply işareti geri yükleme, postrouting egress guard ve masquerade yalnız kanıtlanmış RouteLane akışları için çalışır. Forwarded VM/container trafiğine yeni mark veya NAT uygulanmaz. Yabancı tablolar, main/default/local routing tabloları, DNS ve genel sysctl değerleri değiştirilmez.

`10000/10001` fwmark satırlarının mevcut olması tek başına başarısızlık değildir: açık kurallarla işaretlenen trafik için kalabilirler. Kabul ölçütü, eşleşmeyen yeni lab trafiğinin bu işaretleri almaması ve gerçek paketlerin eski sistem çıkışını kullanmasıdır; yalnız `ip route get` yeterli kanıt değildir.

## 5. Transaction guard, hata ve rollback

Mevcut renderer'ın geçici `guard` zinciri yerel istisnalar dışındaki her şeyi reddediyor. `PreserveSystem` → `PreserveSystem` geçişlerinde bu davranış kaldırılır: guard sadece active/pending açık kural kapsamlarının ve RouteLane'e ait akışların birleşimini korur. Yeni politika ile eşleşmeyen ve sahiplenilmemiş trafik geçiş/rollback sırasında da engellenmez. Uygulama cgroup eşleşmeleri dahil aynı predicate üreticisi kullanılır; guard için ikinci ve farklı bir kural yorumlayıcısı kurulmaz.

Block koruması açık Route/Direct/Block önceliğiyle uyumlu kalır; union guard geçici olarak yalnız bu yönetilen kapsamı daha sıkı kapatabilir. `UsePrimary` kapsamı bütün yeni internet trafiğini içerdiğinden bu moda girme/çıkma geçişi daha geniş koruma gerektirebilir; kullanıcı onayında etkisi belirtilir.

Apply/rollback/cleanup yalnız exact ownership proof ve journal ile çalışır. Yetki, journal tutarsızlığı veya kernel doğrulama hatası broad flush, yabancı kaynak silme veya sessiz sistem çıkışına kaçışla aşılmaz. Inspect salt okunurdur. Başarılı kapatma runtime grant'ini kaldırır; başarısız kapatma temizlenmiş gibi gösterilmez. Önceki nesil sıfırlama düzeltmesi korunur.

## 6. Şema, sürüm ve yükseltme

- Kullanıcı config ve desired-profile şeması **4**, daemon IPC ve runtime activation protokolü **3**, yeni journal sürümü **3** olur. Yeni mod zorunludur; bilinmeyen mod/sürüm reddedilir. Yeni davranış eski peer'ın yok sayabileceği isteğe bağlı bir alan olarak eklenmez. Eski/yeni daemon-helper eşleşmesi yönlendirme mutasyonundan önce anlaşılır bir uyumsuzluk hatası verir; otomatik düşürme yoktur.
- V3/V2/legacy kullanıcı kayıtları salt okunur migrasyonda mevcut ID, hedef, lane, öncelik, dil ve launch bilgileriyle korunur; yeni seçim `PreserveSystem` olarak önerilir. Dosya yüklemek aktif ağı değiştirmez. İlk onayda eşleşmeyen trafiğin artık OS kararına bırakılacağı anlatılır; kullanıcı isterse `UsePrimary` seçer. İptal kayıt ve aktivasyon yapmaz.
- Journal V2'nin eski plan/profile/hash serileştirmesi dondurulmuş DTO'larla okunur; yeni mod alanıyla tekrar hash'lenmez. Mevcut V1 cleanup desteği korunur. Eski kayıt modern renderer ile yeniden üretilmez, yeni runtime grant'ine dönüştürülmez veya otomatik refresh ile yeniden silahlandırılmaz; eski sürüm durumu açıkça yeniden yetkilendirme/cleanup gerektiren durum olarak sunulur.
- Eski uygulanmış plan varsa önce kullanıcı onaylı, sahipliği doğrulanmış legacy cleanup gerekir; temizlenmeden modern activation reddedilir. Böylece V2 fingerprint'ine karşı V3 renderer ile rollback denenmez. Yarım kalmış V2 işlemlerinin cleanup'ı yalnız kendi exact pending/cleanup kanıtıyla sürdürülür; belirsizlikte durulur. Modern işlemlerin rollback'i journal V3 içinde kalır.
- Paket kurulumu, servis restart'ı veya bu tasarım dosyasının yazılması lab erişiminin düzeldiği anlamına gelmez. Canlı eski kural dönüşümü ve yeniden aktivasyon bu geliştirme işinin kendiliğinden yetkisi değildir.

## 7. Lab ve mevcut oturumlar

- Lab alt ağı için etkin açık Route/Direct/Block yoksa yeni `10.65.201.60` bağlantıları işaretlenmeden OS'nin `mpqemubr0` rotasını kullanmalı; aynı şey bağlı olmayan özel ağ rotaları dahil diğer eşleşmeyen hedefler için de geçerlidir.
- `10.65.201.0/24 → Ethernet` gibi etkin bir açık kural varsa lab hâlâ yönetilen kapsamdır. Kural sessizce silinmez veya bypass edilmez. Etkinleştirme önizlemesi gözlenen bağlı ağla çakışmayı gösterir; kullanıcı kuralı düzenleyebilir/devre dışı bırakabilir veya açıkça onaylayabilir. Önizleme lab erişiminin bu kural nedeniyle bozulabileceğini söyler; yokmuş gibi başarı raporlanmaz.
- `TopologySnapshot::restricted_to` yalnız exit grant'ine ait arayüzleri tutar. Bağlı yerel ağ bilgisi önizleme ve dar kontrol istisnaları için gerekirse taze, root-observed topology'den ayrı okunur; bridge'i listelemek onu yetkili internet çıkışına eklemez. Yeni bypass için tüm özel adresler veya tahmin edilen prefix'ler kullanılmaz.
- Daha önce RouteLane tarafından mark/NAT uygulanmış TCP/QUIC/SSH oturumları işletim sisteminin önceki haline kesintisiz taşınmış sayılmaz. Global conntrack flush veya süreç kapatma yoktur. Geçiş açıklaması yeni bağlantı açmayı belirtir; testler yeni tuple/oturum kullanır. Yönetilmeyen mevcut oturumlar yeniden bağlanmaz ve NAT'lanmaz.

## 8. GTK arayüzü ve gözlemlenebilirlik

Bağlantılar bölümünde iki mod açık ad ve kısa açıklamayla seçilir. Kaydedilen mod ile gerçekten uygulanan mod Genel Bakış'ta ayrı gösterilir. Seçici modda “Diğer trafik: işletim sistemi yönetiyor” denir; ana hat kesilince bütün sistemin yedeğe geçirildiği iddia edilmez. Direct açıklaması hem TR hem EN güncellenir.

Mod değişikliği aktifken önce sonuçları gösteren onay gelir; İptal/Escape/kapatma yan etkisizdir, çift onay tek işlem gönderir. Eski geniş kapsamlı yetki, mod değişimiyle sessizce tekrar kullanılmaz. Kayıt başarılı/uygulama başarısız durumları ayrılır; health yalnız yönetilen politika ve gerçekten incelenen kernel kaynakları hakkında konuşur. Lab erişimi veya internetin uçtan uca çalışması ayrı trafik kanıtı olmadan “sağlıklı” sayılmaz.

## 9. Kabul testleri

Üretim değişikliğinden önce RED görülür; sırasıyla odaklı testler, tam rootless suite, Clippy/fmt ve izole GTK doğrulaması çalıştırılır.

1. IPv4/IPv6 eşleşmeyen yeni public, private ve lab paketleri mark/ct-mark=0 kalır; NAT ve egress kararı RouteLane nedeniyle değişmez.
2. Eşleşmeyen foreign-mark ve forwarded VM/container trafiğinin işaretleri/verdict'i korunur. Eşleşen foreign-mark Route/Direct çatışması yalnız kendi kapsamını reddeder; explicit Block ve ilk eşleşme sırası korunur.
3. Açık Route doğru lane'e, Direct etkin ana hatta, Block reject'e gider; lane kaybında mevcut fallback/hata politikaları aile bazında çalışır.
4. Seçici geçişin guard, commit, rollback ve hata-enjeksiyonu aşamalarında eşleşmeyen trafik engellenmez; yönetilen trafik ara durumda sızmaz.
5. Başka bir araçça ana/default rota değiştirildiğinde eşleşmeyen yeni akış OS'nin yeni kararını izler; RouteLane bunu geri yazmaz. `UsePrimary` ayrı opt-in olarak eski failover kabulünü sağlar.
6. Eski plan/hash/fingerprint fixture'ları değişmez; V1/V2 exact cleanup, yarım işlem, sahiplik belirsizliği ve eski/yeni protokol uyuşmazlığı testleri geçer. Salt load/Inspect hiçbir mutasyon üretmez.
7. GUI mod/ilk migrasyon onayında iptal yan etkisizdir, aktif/saved ayrımı görünürdür; kapat → tekrar aç nesil regresyonu korunur.
8. Yetkili disposable VM'de gerçek dual-stack trafik, yeni lab SSH/ping bağlantısı, main route/yabancı nft/VPN sentinel ve iki lane çıkış sayaçları doğrulanır. Host ve mevcut kullanıcı lab VM'leri kernel testi olarak kullanılmaz.

Ubuntu, Debian ve Fedora'nın ayrı paket/kernel/Polkit kabul kapıları korunur. Derleme veya exit 77 başarı değildir; kanıt yoksa NOT VERIFIED raporlanır.

## 10. Uygulama sınırı ve sonraki adım

İlgili dikişler: `src/domain/routing.rs`, `src/config*`, `src/backend/{plan,planner,nft_document,privileged,runtime,topology}.rs`, daemon/IPC durum sözleşmesi ve mevcut GTK bağlantı/önizleme metinleri. Yeni bağımlılık veya genel mimari refactor kapsamda değildir. Üçüncü taraf CDN keşfi, yeni System-bypass eylemi ve anlık eski-socket taşıma bu işin parçası değildir.

Bu tasarım kullanıcı tarafından incelendikten sonra uygulama planı yazılır ve ayrıca onaylanır. O zamana kadar mevcut ürün kodu, kullanıcı profili, servisler ve kernel kuralları değiştirilmez. Önceden kirli CIDR/nesil düzeltmeleri ve kullanıcıya ait Cargo sürüm değişiklikleri korunur.
