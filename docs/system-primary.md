# Uygulamadan sistem ana hattı yönetimi

11 Kasım 2026. Kullanıcı, terminalden yapılan Ethernet önceliği düzeltmesinin
ürün üzerinden uygulanmasını istedi ve önerilen birinci yaklaşım için “Uygula”
dedi. Bu özellik o isteği karşılar; seçici yönlendirmeyi genişletmez.

## Problem ve karar

`PreserveSystem` eşleşmeyen trafiği sistemin kendi yönlendirmesine bırakır.
RouteLane ana bağlantısının Ethernet seçilmesi, OS varsayılanının Ethernet
olduğu anlamına gelmez. VINN daha düşük metrikliyse başka alt ağdaki intranet
hedefleri de VINN'e gider. Sadece internet veya aynı segmentteki gateway'e ping
atmak bu hatayı dışlamaz.

**Seçilen çözüm:** Bağlantılar ekranında ayrı **Sistem ana hattı yap** işlemi.
Kullanıcıya gerçek sistem çıkışı ve seçilmiş bağlantı birlikte gösterilir.
Fark varsa Genel Bakış'ın dikkat alanı ve tepsi menüsü uyarı verir.
Durum, görünür pencere açıkken on saniyelik salt okunur kontrollerle yenilenir.
Yönlendirme aktivasyonu bu ayarı kendiliğinden yapmaz. `UsePrimary` moduna
sessiz geçilmez; özel ağlar için tahmini geniş prefix istisnaları eklenmez.

## Onay ve kapsam

1. Kullanıcı belirli bir ana bağlantı seçer; **Sistem varsayılanını izle**
   seçeneğinde sistem ayarı değiştirilmez.
2. Bağlantının etkinliği, adres/rota kanıtı, UUID, fiziksel kimlik, ayarlar ve
   sürümler gözlenir. Bağlantı kapalıysa açılmaz; kullanıcıya neden açıklanır.
3. **Sistem ana hattı yap** mevcut/yeni IPv4 metriğini, kalıcılığı, diğer trafik
   üzerindeki etkiyi ve oturumların yeniden bağlanabileceğini açıklar.
   İptal varsayılandır; Escape/kapatma yan etkisizdir. Çift onay tek işlem üretir.
4. NetworkManager kendi Polkit yetkilendirmesini yapar. Şifre alınmaz/saklanmaz;
   shell, `sudo`, ayrıcalıklı RouteLane helper komutu veya genel ağ yetkisi eklenmez.
5. Yalnız onaylanan aktif profilin `ipv4.route-metric` alanı değiştirilir.
   Diğer profiller, DNS/IPv6, bağlantı aç/kapat ve otomatik bağlanma ayarları korunur.

Öneri, diğer gözlenen main-table varsayılanlarından küçük ve en fazla 50 olan
metriktir. Başka çıkışın metriği sıfırsa otomatik çözüm reddedilir. VPN/yabancı
policy routing, eşit/karmaşık varsayılanlar, özel route-table/never-default veya
açık statik default durumunda zorlayıcı bir fallback yoktur.

Değişiklik RouteLane kapatıldıktan sonra da sürer. Sonradan yönetici/başka araç
önceliği değiştirirse RouteLane onunla yarışmaz; durumu gösterir. Kesilmiş bir
bağlantıyı otomatik açma, internet kesintisini eksiksiz algılama veya bütün
intranet servislerinin erişilebilirliğini garanti etme iddiası yoktur.

## Güvenlik ve geri alma

- Profil ve cihazın D-Bus yolları ile NetworkManager'ın benzersiz bus sahibi
  onaya bağlanır. Arada profil değişirse yeni nesneye yönlenilmez.
- Kaydedilmiş ayar ve etkin ayar ayrı tutulur. `Update2` profil sürümüyle,
  `Reapply` etkin sürümle eşzamanlı değişiklikleri reddeder.
- `Reapply` dışarıdan eklenmiş IP/rotaları koruma bayrağıyla çağrılır; bağlantı
  kesilmez. İlgisiz ayarlar taze, sürüm korumalı anlık görüntüde aynen taşınır.
- `~/.local/state/routelane/system-primary-v1.json` (XDG state dizinine uyar)
  yalnız eski/yazılan metrikleri, kimliği, ilgisiz ayarların hash'lerini ve
  bekleyen işlem niyetini tutar. Tam profil, IP hedefleri veya sır saklamaz.
  Özel dizin/0600 dosya, symlink reddi, boyut/sayı sınırı ve süreçler arası kilit vardır.
- Her D-Bus yazmasından **önce** niyet kalıcıdır. Zaman aşımı/çökme sonrası
  belirsiz işlem tekrarlanmaz. Önceki durum hâlâ görünüyorsa, geç kalmış yazmanın
  üzerine kontrolsüz Undo gönderilmez; hedef değer gözlenene kadar kayıt korunur.
- **Önceki önceliği geri al** ayrı onay ister. UUID, arayüz/fiziksel kimlik,
  diğer ayarların hash'i ve metrikler hâlâ kayıtla uyuşmalıdır. Kayıtta alan
  başlangıçta yoksa Undo onu kaldırır; otomatik `-1` ile karıştırmaz.
- Geri alma yalnız öncelik alanını düzeltir. Başka bir aracın değişikliği,
  belirsizlik, kapalı bağlantı veya yazılamayan kayıt varsa durur; geniş reset yoktur.

## Uyumluluk

Standart bağımsız Ethernet/Wi-Fi profilleri ve IPv4 içindir. Diğer routing
özellikleri değişmez. Güvenli otomatik ayar için **NetworkManager 1.44+** gerekir:
profil sürümüne bağlı `Update2` bu sürümde sağlanır; dış IP/rota korumalı
`Reapply` 1.42'den itibaren mevcuttur. Eski sürüm/başka yönetici için bu eylem
kullanılamaz olarak açıklanır; sessizce güvensiz yönteme düşülmez.
[Update2 belgesi](https://networkmanager.dev/docs/api/latest/gdbus-org.freedesktop.NetworkManager.Settings.Connection.html),
[Reapply belgesi](https://networkmanager.pages.freedesktop.org/NetworkManager/NetworkManager/gdbus-org.freedesktop.NetworkManager.Device.html).

`route-metric`, yalnız default'a değil profilin varsayılan metriğini kullanan
diğer rotalara da uygulanır; onay metni bu yüzden bu etkiyi açıklar.
[IPv4 ayar sözleşmesi](https://www.networkmanager.dev/docs/api/latest/settings-ipv4.html).

## Kod, doğrulama ve sınırlar

- `src/system_primary/`: dar backend arayüzü, native D-Bus adaptörü, özel kayıt
  ve onay/geri alma motoru. Helper/daemon IPC ve kernel planner değiştirilmedi.
- `src/ui/system_primary.rs`: onay, hata, başarı, meşgul durum ve taze durum
  gösterimi. İşlem sürerken GUI/tepsi yönlendirme mutasyonları ve çıkış kilitlidir.
- Rootless testler: değişmiş onay, cihaz/profil değişimi, metrik çakışması,
  yarım işlem, belirsiz geç yazma, yinelenen onay ve güvenli geri alma.
- `scripts/run-gtk-smoke.sh`: özel bus'ta TR/EN GTK akışları ve sahte
  NetworkManager üzerinde gerçek D-Bus payload/flags/sürüm sözleşmesi.
  Testte system bus da özel bus'a yöneltilir; host NM/Polkit devreye girmez.
- Ayrı opt-in **salt okunur** host testi: `ROUTELANE_NM_READONLY_INTERFACE`
  ile gerçek GetSettings/GetAppliedConnection ve topology gözlemi.

Yeni kodla gerçek NetworkManager üzerinde **yazma/Reapply ve etkileşimli Polkit**
henüz uçtan uca doğrulanmadı. Ubuntu/Debian/Fedora'nın bu eyleme ait ayrı
paket ve canlı ağ kabulü de açık kapıdır; mock testler gerçek trafik kanıtı
değildir. Paket kurmak veya servisi yeniden başlatmak bu geliştirme işine dahil değildir.

UI test verisiyle [TR ekranı](screenshots/overview-tray/system-primary-tr.png)
ve [EN ekranı](screenshots/overview-tray/system-primary-en.png).
