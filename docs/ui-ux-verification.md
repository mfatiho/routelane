# Pratik UI/UX uygulaması ve doğrulama

Tarih: 14 Ekim 2026. Onaylanan tasarım ve plan `docs/superpowers/` altındadır.
Çalışma main üzerinde, alt ajan veya bağımsız reviewer kullanılmadan yürütüldü.
Bu kayıt ürünün bütün dağıtımlarda yayına hazır olduğu iddiası değildir.

## Uygulanan akış

- Genel Bakış, Kurallar, Uygulamalar ve Bağlantılar ayrı bölümlerdir.
- Kalıcı durum alanında tek global yönlendirme eylemi vardır. Etkinlik,
  sağlık enum'undan değil doğrulanmış `applied_profile_id` bilgisinden türetilir.
- Bağlantı yoksa ekleme; kullanılabilir tanım yoksa ekleme/gözden geçirme;
  hazırsa etkinleştirme yönlendirmesi yapılır. Direct/Block sahte lane gerektirmez.
- Ekle/ara kontrolleri listelerin üstündedir. Kompakt satır menüleri kalıcı
  dropdown/buton kalabalığının yerini alır. Arama yürütme sırasını değiştirmez.
- Dikey editörler taslak üzerinde çalışır. Kaydet açık eylemdir; İptal, Escape
  ve pencereyi kapatma mutasyon göndermez. Silme ve süreç taşıma onay gerektirir.
- `UiSession` / `UiController`, persist ve mutation için tek kapı sağlar.
  Yanıtlar request ID ve bağlantı epoch'u ile eşleştirilir. Status/event, bekleyen
  isteği tamamlamaz. Apply yanıtından sonra ilgili doğrulanmış snapshot beklenir.
- Kayıt hatası etkin planın güvenli kapatılmasını engellemez. Bağlantı kesilirse
  durum bilinmiyor olarak, önceki gözlem ise son bilinen durum olarak sunulur.
- Başlatma ve süreç taşıma, aynı profil ID'sine değil kaydedilen/uygulanan
  içeriğin eşleşmesine bağlıdır. Reconnect sonrası yeniden uygulama gerekir.
- Desktop kataloğu ve arayüz keşfi GTK dışında çalışır. Seçim stable ID ile
  korunur; Save sırasında varlık yeniden kontrol edilir. Launcher/argümanlar korunur.
- Sağlık metni yalnız yerel kuralların doğrulandığını söyler; web sitesi,
  browser DNS/CDN eşleşmesi veya mevcut bağlantıların taşınması garanti edilmez.
- Ctrl+F, Ctrl+N, Enter ve Escape akışları; görünür alan etiketleri, erişilebilir
  adlar, odak dönüşü ve ID tabanlı liste/arama/kaydırma korunması eklendi.
- GTK4 v4_4 / Libadwaita v1_1 sınırı ve mevcut bağımlılıklar korundu.

## Kanıt ve kapsam

| Kontrol | Sonuç / kanıt |
| --- | --- |
| `rtk cargo fmt --check` | Başarılı |
| `rtk cargo test -- --test-threads=1` | 310 geçti, 1 GTK testi normal koşuda bilinçli ignored |
| `rtk proxy bash scripts/run-gtk-smoke.sh` | Ignored GTK testi ayrıca gerçekten çalıştırıldı ve geçti |
| `rtk cargo clippy --all-targets --all-features -- -D warnings` | Hata/uyarı yok |
| `rtk cargo build --release --bins` | Başarılı |
| `rtk proxy bash packaging/deb/build-deb.sh` | Yerel `.deb` üretildi; kurulmadı |
| `rtk git diff --check` | Başarılı |

GTK testi private D-Bus, Broadway ve geçici config/runtime dizini kullanır.
`G_DEBUG=fatal-warnings` sayesinde GTK uyarısı bile testin başarısız olmasına yol açar.
Gerçek ana pencere ve editörler çalışır; daemon mesajları bellek kanallarından
gelir. Test hiçbir canlı yönlendirme, helper veya Polkit mutasyonu yapmaz.

GTK kabul senaryoları tek test girişinde aynı OS thread'inde çalışır:

- Dört bölüm, tek global kontrol, boş/dolu/filtreli listede üstte ekleme.
- Navigasyon, arama ve düzenlenen bağlantı taslağını iptal etmek yazma yapmaz.
- Enter ile geçersiz hedefte odak/hata ve geçerli Block tanımında açık submit.
- Escape ve kapatma İptal'dir; yıkıcı onayın varsayılan eylemi İptal'dir.
- Dil seçimi onaydan önce geri alınır; iptal config dosyası oluşturmaz.
- Sağlık snapshot'ı satır kimliğini, aramayı ve kaydırmayı korur. Silinen
  odaklı satırın yerine hayatta kalan komşuya odaklanılır.
- Ctrl+F/Ctrl+N action bağlantıları; uygulama seçiminin filtrede korunması.
- Katalog sonucu iptalden sonra yok sayılır; Save öncesi kaldırılan uygulama
  reddedilir; yalnız lane düzenlemek özgün launcher/argümanları değiştirmez.
- Aynı kayıtta Apply sonrası başlatma/taşıma menüsü güncellenir. Çift etkinleştirme
  ve çift başlatma yalnız birer daemon mutation gönderir. Taşıma iptali göndermez.
- Yerel lane sağlığı yoksa başlatma metni bağlantının doğrulandığını iddia etmez.
- Türkçe/İngilizce, 900×720 varsayılan boyut, 560×600 tahsis ve Sans 22
  büyük metin koşulu; uzun boşluksuz bağlantı ve uygulama adları.
- `&` / `<…>` içeren adlar markup olarak değil metin olarak gösterilir.

Saf testler ayrıca eski epoch/foreign yanıtı, eşleşmeyen kayıt neslini,
timeout'ta replay yapılmamasını, readonly config ile deactivate'i, başka
bir işlemin apply neslini geçersizleştirmesini, sınırlı recovery/retry'ı,
dil kaydının yalnız kendi başarılı sonucu sonrası çıkmasını, fallback hedefi
silme engelini ve gizli uygulama kuralları dahil eşit öncelikli sıralamayı kapsar.

## İnceleme

Standart ekseni: widget'lar ayrı sayfa/editör modüllerinde; kararlar saf session
modelinde; etkiler controller'da. Kullanıcı eylemleri mevcut tipli IPC'yi kullanır.
Yeni ayrıcalıklı yüzey, bağımlılık, polling veya harici sağlık servisi eklenmedi.

Tasarım ekseni: tasarımın 1–14 kabul alanları yukarıdaki saf/GTK kontrolleriyle
eşleştirildi. İncelemede bulunan kapatma/iptal, kalıcı config hatası, yeni
kullanıcı recovery döngüsü, daha yeni snapshot'ta kilitlenme, statik taşıma menüsü,
dar/büyük metin taşması ve markup sorunları düzeltildi ve test edildi.
Bağımsız bir gözün incelemesi yapılmadı; yazar öz-incelemesi daha zayıf kanıttır.

## Uygulama kararları ve bedelleri

1. Kullanıcının onayladığı şekilde main üzerinde inline çalışıldı; ayrı worktree
   ve reviewer yok. Bedel: izolasyon ve bağımsız inceleme güvencesi daha düşük.
2. Önceki dirty reliability değişiklikleri korunarak yalnız UI ve gerekli
   launcher metadata önkoşulları checkpoint edilir. Bedel: ara task'lar ayrı
   checkout yapılabilir commit'ler yerine ledger kayıtlarıdır.
3. Gönderim transport ACK sağlamadığından Sending/AwaitingReply ayrımı yapılmadı.
   Bedel: ayrı “kuyrukta/gönderildi” görsel aşaması yoktur.
4. `deliver_request` tek transport sınırı olarak eklendi. Bedel: küçük bir iç helper.
5. GTK kabul vakaları aynı thread'de tek ignored girişte toplandı. Bedel: test
   sayacı tek aggregate testtir; vakalar ayrı test adları olarak görünmez.
6. Shell ve controller pencereye birlikte bağlandı. Bedel: Task 3/4 entegrasyon
   kabulü beraber kapandı; geçici ikinci UI sürümü oluşturulmadı.
7. Transport hatasında ek anlık refresh yerine bridge'in bounded reconnect'i
   kullanılır. Bedel: yeniden bağlanana/açık yenilemeye kadar durum bilinmiyor.
8. Editör callback'i yerel kabul için bool döndürür. Bedel: küçük iç API değişimi;
   busy/stale durumda taslak kapanıp kaybolmaz.
9. Kural hareketinde tam sıra 10'ar artan önceliklerle yeniden numaralanır.
   Bedel: sayısal öncelikler değişebilir; kimlikler/içerik/istenen sıra korunur.
10. Migration convenience/legacy UI adaptörleri son bağlamadan sonra kaldırıldı.
    Bedel: yalnız iç Rust API değişir; daemon metotları ve IPC sürümü değişmez.
11. Lane editör UI'ı Bağlantılar'a taşındı, saf eski yardımcılar tekrar kullanıldı.
    Bedel: bazı saf yardımcıların kaynak konumu `settings.rs` olarak kaldı.
12. Katalog için dar bir loader sınırı eklendi; her dialog tek initial load ve
    gated submit revalidation kullanır, kapalı dialog sonucu yok sayar.
    Bedel: küçük test edilebilirlik API'si; gereksiz çoklu-load token modeli yoktur.
13. FlowBox'ın büyük metindeki yüksekliği yerine sabit dikey durum alanı
    kullanıldı. Bedel: geniş pencerede biraz daha fazla dikey alan kullanılır.
14. Adlar GTK 1.1 uyumluluğu için escape edilir; arama ham görünen adla yapılır.
    Bedel: widget title property markup-escaped içeriktir; domain verisi değişmez.
15. Root/dağıtım/AT/tema doğrulaması bu rootless görevde yapılmadı. Bedel:
    release öncesinde gerçek sistem doğrulaması hâlâ zorunludur.
16. Önceki dirty UI'nin yedeği ve ledger git-ignored task workspace'inde tutulur.
    Bedel: az miktarda yerel scratch dosyası; önceki değişiklikler kaybolmaz.
17. Checkpoint, önceki optional launcher metadata değişikliğini ve daemon'un
    bunu kullanmasını içerir; bağımsız kernel/cleanup/süreç düzeltmeleri dışarıda
    korunur. Bedel: derlenebilir UI checkpoint'i birkaç baseline önkoşul hunk'ını içerir.

Kapsamlı rootless suite önceki reliability düzeltmelerini de içerir. UI checkpoint'i
ayrıca geçici bir index snapshot'ında test edildi: 301 geçti / 1 ignored;
fark, kapsam dışında bırakılan önceki reliability testleridir. Bu kontrol sonrası
ortak Cargo target cache'i temizlenerek asıl working tree'nin 310 testi yeniden
çalıştırıldı; iki snapshot'ın artifact'ları nihai doğrulamada karıştırılmadı.
Yerel `.deb` tüm mevcut working tree'den üretilir, önceki reliability düzeltmelerini
de içerir; bu belge veya UI checkpoint'i tek başına paket yayın onayı değildir.

## Henüz doğrulanmayan release kapıları

- Gerçek kernel/network namespace, browser trafiği ve Polkit akışları.
- Ubuntu, Debian ve Fedora üzerinde ayrı paket kurulum/yükseltme/kaldırma testleri.
  Üç dağıtım resmi destek hedefi olarak korunur; bu yerel build üçünü kanıtlamaz.
- Screen reader / gerçek assistive technology, gerçek masaüstü açık-koyu tema ve
  insan gözüyle görsel inceleme. Widget özellikleri bunların yerine sayılmadı.
- Sürekli DNS/TTL yenilemesi hâlâ kapsam dışındadır; domain snapshot sınırlaması sürer.

Sistem servisi kurulmadı/yeniden başlatılmadı, canlı ağ kuralları değiştirilmedi.
