# RouteLane güvenilirlik denetimi

Tarih: 13 Ekim 2026. Bu rapor kaynak kodu ve yerel doğrulama sonuçlarını kapsar;
kurulu uygulamanın, gerçek trafiğin veya tüm desteklenen dağıtımların sorunsuz
çalıştığı anlamına gelmez.

## Düzeltilen sorunlar

| Sorun | Değişiklik | Doğrulama |
| --- | --- | --- |
| `ip -j -N` çıktısındaki sayısal route alanları yabancı kaynak sayılıyordu | `boot/3`, `unicast/1`, `global/0`, `link/253` eşdeğerlerini dar kapsamlı kabul et | Snapshot ve sahiplik regresyonları |
| Helper yeniden başlatıldığında kernel kaynakları kalırken sahiplik günlüğü silinebiliyordu | `RuntimeDirectoryPreserve=yes` | systemd birim doğrulaması; gerçek servis restart testi henüz yapılmadı |
| Eksik bir RouteLane route'u yeniden başlatma sonrası kapatmayı engelliyordu | Geçerli günlükteki planın doğrulanmış canlı alt kümesini temizlemeye izin ver | Backend ve daemon restart/drift testleri |
| Devam eden yetim temizliği nft sahipliğini kontrol etmeden route silebiliyordu | İlk silmeden önce nft fingerprint ve routing alt kümesini doğrula | Değiştirilmiş nft durumunda hiçbir route/rule silinmediğini doğrulayan test |
| Durum sorgusu yarım kalmış işlemi sürdürebiliyordu | `Inspect` yalnızca günlüğü ve kernel kanıtını okur; replay yetkilendirilmiş mutasyon çağrısına ertelenir | Tekrarlanan sorgularda günlük ve mutasyon sayısı değişmez; ardından yetkili cleanup/recovery tamamlanır |
| Devam ettirilmiş yetim temizliği başarıya rağmen `cleaned=false` dönebiliyordu | Gerçekleştirilen replay'i sonuçta hesaba kat | Yetim günlük recovery regresyonu |
| Masaüstü uygulamasının başlatıcı alias'ı ve argümanları kayboluyordu | Süreç eşleştirme kimliğini başlatıcı yolu/argümanlarından ayır; eski JSON alanlarını uyumlu tut | Symlink ve tırnaklı argüman keşif testi, mevcut yapılandırma testleri |
| `include_children` farklı executable kullanan çocukları kapsamıyordu | Parent PID/start-time kanıtıyla süreç ağacını takip et | Ters sıralı parent/child/grandchild ve children kapalı testi |
| Etkin profil yokken uygulama cgroup işlemi denenebiliyordu | Uygulama işlemlerini uygulanmış profil ve etkin kural üzerinden doğrula | Etkin olmayan profil başlatma regresyonu |
| Snap/Flatpak ortak başlatıcısı farklı uygulamaların süreçlerini seçebilirdi | Başlatma ve süreç taşıma yollarında aynı destek kontrolünü uygula | Her iki launcher ve her iki işlem için regresyon |
| Polkit onayı kısa durum sorgusu timeout'una takılıyordu | Salt okunur sorguları 3 s, mutasyonları 180 s zaman aşımına ayır | Gecikmeli onay/yanıt testi |
| Helper'ın cgroup koruması kendi uygulama alt ağacına yazmayı da engelliyordu | Yalnızca `routelane.apps` için yazma istisnası; kurulum/boot için tmpfiles kaydı | Birim ve paket içeriği doğrulaması; gerçek servis sandbox testi henüz yapılmadı |
| GTK akışları yalnızca model testleriyle kontrol ediliyordu | Özel D-Bus/Broadway oturumunda gerçek pencere, ekleme diyalogları ve profil yenileme smoke testi | Root gerektirmeyen izole GTK testi |

## Yerel doğrulama

- `cargo test -- --test-threads=1`: 291 geçti; 1 GTK testi normal turda hariç.
- `bash scripts/run-gtk-smoke.sh`: GTK testi ayrıca çalıştırılır; gerçek
  diyalog görünürlüğü/modal sahipliği, profil yenilemesi ve mutasyon gönderilmemesi kontrol edilir.
- `cargo clippy --all-targets --all-features -- -D warnings`: başarılı.
- `cargo fmt --check` ve `git diff --check`: başarılı.
- `systemd-analyze verify`: helper ve daemon birimleri doğrulandı; bu bir runtime sandbox testi değildir.
- `cargo test --test root --features root-integration --no-run`: derlendi; testler çalıştırılmadı.
- `.deb` release paketi üretildi ve içerik/izin kontrollerinden geçti; sisteme kurulmadı.
- `bash scripts/run-root-integration.sh`: UID 0 olmadığı için exit 77; **DOĞRULANMADI**.

## Sistem etkisi sınırı

Yeni kernel modülü veya Rust bağımlılığı eklenmedi. Sistem DNS'i, ana routing
tablosu ve başka uygulamaların firewall kuralları bu değişikliklerle yeniden
yazılmıyor. Yönlendirme yine nftables/policy routing ve uygulama cgroup'ları
kullandığından “kernel'e hiç dokunmuyor” garantisi verilemez.

Durum sorgusu otomatik onarım başlatmaz. Sahiplik doğrulaması başarısızsa temizlik
genişletilmez; bilinmeyen kaynaklar kullanıcı onayı olsa bile silinmez.

## Açık sınırlar ve release kapıları

1. Gerçek root namespace apply/rollback/cleanup testi ve systemd altında cgroup
   yazma istisnası henüz doğrulanmadı. İzole test betiği yönetici tarafından
   `sudo bash scripts/run-root-integration.sh` ile çalıştırılmalıdır. Betiğin
   exit 77 vermesi geçiş değildir. Bu da ayrıca gerçek masaüstü Polkit testinin
   yerine geçmez.
2. Ubuntu, Debian ve Fedora ilk sürüm destek kapsamındadır; her biri için gerçek
   kurulum, yükseltme, kaldırma, D-Bus/Polkit ve ağ testleri geçmeden resmi sürüm
   doğrulaması tamamlanmış sayılmaz. `.deb` üretimi Fedora/RPM doğrulaması değildir.
3. Domain IP'leri profil uygulandığında sistem resolver'ından alınır. Sürekli
   DNS/TTL yenilemesi yoktur. Browser DoH, değişen CDN IP'leri ve üçüncü taraf
   domain'leri için domain kuralı tek başına garantili oturum kapsamı sağlamaz.
   Desteklenen host uygulamasına süreç bağlılığı bu IP listesine bağımlı değildir.
4. Snap/Flatpak uygulama bağlılığı desteklenmiyor. Paylaşılan launcher/interpreter
   üzerinden bütün sandbox uygulamalarını taşımak güvenli bir alternatif değildir.
5. Uygulama başlatıcısının argümanları parser testinde doğrulandı; gerçek browser
   başlatma, mevcut browser oturumuna delegasyon, cgroup taşıma ve trafiğin lane
   üzerinden çıktığı ayrıca runtime test gerektirir. Mevcut bağlantıların sonradan
   taşınması garanti edilmez.
6. Bağlantı Sağlığı yerel kaynakların plana uygunluğunu gösterir; bir web sitesinin
   uçtan uca yüklendiğini kanıtlamaz. Wildcard domain ve service pack planlayıcıda
   henüz desteklenmez.
