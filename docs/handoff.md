# RouteLane — geliştirici devir belgesi

Tarih: 15 Ekim 2026. Bu belge, devralan geliştiricinin geçmiş kararları bulması ve yarım kalan işi güvenli şekilde sürdürmesi içindir. Kalıcı devir kaydı `docs/handoff.md` dosyasıdır; `/tmp` kopyasına veya oturumlar arası asistan hafızasına bağımlı değildir. Ayrıntılı değişiklik geçmişinin kaynağı Git ve aşağıdaki depo belgeleridir.

## En son kayıt — hafıza güncellemesi (15 Ekim 2026)

- Kullanıcının son isteği son yapılanların hatırlanmasıdır; bu turda yalnız bu belge güncellendi. Yeni kod, paket kurulumu, VM işlemi veya canlı ağ değişikliği yapılmadı.
- Başlangıç HEAD'i `d645f44e57d66c17eef44b571d6b92913436a453` (`add AGENTS, CLAUDE`). `rtk git status --short --branch` çıktısında `main...origin/main` ve temiz çalışma ağacı görüldü. Aşağıdaki oturum 3'teki kirli baseline bilgisi artık güncel değildir: kalan değişiklikler bu commit'e alınmış.
- 12 görevin uygulanmış olması bütün yayın kapılarının geçtiği anlamına gelmez. Güncel kabul kaynağı `docs/primary-failover-verification.md` dosyasıdır. Ubuntu 22.04/24.04 kayıtlarında iki root modunda temiz 18/18 var; Ubuntu 26.04'te sayaç/verim eşiği kaynaklı aralıklı başarısızlıklar var, bu satır temiz PASS değil.
- Son runtime düzeltmesi `b34ac27` yalnız rootless testlerle doğrulanmış. Eski VM sonuçları veya 433 testlik tarihsel kayıt, güncel HEAD'in yeniden doğrulandığı şeklinde sunulmamalı. Bu hafıza güncellemesinde testler yeniden çalıştırılmadı.
- Debian 12/13, Fedora 43/44 gerçek kurulum/kernel/Polkit, tüm aarch64 satırları, etkileşimli Polkit ajanı, dağıtımlarda GTK açılışı, firewalld/SELinux ve CI matrisi hâlâ NOT VERIFIED. Fedora container'ında RPM üretmek bunların yerine geçmez.
- Devralan kişi önce bu bölümü ve doğrulama kaydını okumalı. Eski Task 6'dan tekrar başlamamalı; §1–9 tarihsel oturum 1 notlarıdır. Sıradaki teknik iş, yeni kullanıcı talebi doğrultusunda Ubuntu 26.04 aralıklı testlerini teşhis etmek veya eksik dağıtım/architecture kabul altyapısını hazırlamaktır; yeni VM/host yetkilerini önce kapsamla eşleştir.
- Devir kaydını güncellerken her komutu yığmak yerine önemli kararları, commit'i, gerçekten çalıştırılmış doğrulamaları, açık riskleri ve bir sonraki dar adımı kaydet. AGENTS.md bu turda değiştirilmedi.
- Saklama sınırı: `.gitignore` içindeki `docs/*` kuralı bu dosyayı yok sayıyor; `git ls-files -- docs/handoff.md` boş. Kayıt bu çalışma dizininde kalıcıdır fakat yeni klona otomatik gelmez. Bu turda ignore kuralı değiştirilmedi, dosya stage/commit edilmedi.

Güncel duruma dönmek için depo kökünde ilk komutlar:

```bash
rtk git status --short --branch
rtk git log -8 --oneline
rtk proxy sed -n '1,240p' docs/primary-failover-verification.md
```

Bu bölümün altındaki eski oturum durumları, bu güncel kayıtla çeliştiklerinde tarihsel bağlam olarak değerlendirilmelidir.

## 0. Güncel durum (oturum 3, 15 Ekim 2026 — önce bunu oku)

- **Planın 12 görevi uygulandı** (main, inline). Task 6–10 commit'leri: `7ad1766`, `b4e82f5`, `46f1e9a`, `98159b6`, `d336502`, `bb8667a`, `be9982b`, `9d3b49d`.
- **Task 11:** `33aa274` (carrier kaybında `linkdown` route'larla failover), `18eb413` (kernel kabulü + gerçek system-bus/Polkit betiği `tests/root/system_bus_acceptance.sh`, `docs/primary-failover-verification.md`).
- **Task 12:** `7a89648` (kullanıcının paketleme güvenilirlik hunk'ları, kullanıcı onayıyla ayrı commit), `d8949e7` (nft 1.0.2/1.1.6 dump uyumu), `333c323` (rpm, paket sözleşmesi, `polkitd | policykit-1`, Ubuntu matrisi).
- **Son inceleme (self-review, alt ajan yok):** `0203fab` başarısız geçişin kendi rollback bildirimleriyle sonsuz tekrarını ve tutulan journal’ın sürekli replay’ini üstel beklemeyle durdurdu; `b34ac27` tekrar başarısız kurtarmanın Degraded durumunu ve sonraki deneme zamanlayıcısını kaybetmemesini sağladı. 26.04’te sayaç tabanlı iki testte aralıklı hata var (doğrulama belgesi).
- **Kanıt:** Ubuntu 22.04/24.04/26.04 (x86_64) root süiti iki modda 18/18, system bus + silahlı yükseltme, reboot ve kaldırma PASS. Fedora rpm yalnız container'da üretildi.
- **Yayın kapısı AÇIK DEĞİL:** Debian 12/13, Fedora 43/44, aarch64, etkileşimli Polkit ajanı, GTK'nın dağıtımlarda başlatılması, firewalld/SELinux, CI matrisi NOT VERIFIED.
- **VM'ler:** onaylı test VM'i `routelane-test-u2404-20261014` duruyor; `routelane-test-u2204-20261015` ve `routelane-test-u2604-20261015` bu oturumda kullanıcı onayıyla açıldı ve iş bitince silindi.
- **Kirli ağaç:** kullanıcının kalan hunk'ları unstaged (`.gitignore`, `README.md` (+ schema v3 düzenlemesi), `src/application/process.rs`, `src/backend/inspect.rs`, `src/backend/privileged.rs` baseline hunk'ları, `src/daemon/service.rs`, `tests/ipc_contract.rs`, `docs/reliability-audit.md`). VM koşuları bu hunk'lar dahil çalışma ağacından yapıldı.
- **Sıradaki dar adım:** Debian/Fedora/aarch64 için hipervizör/imaj; kalan kullanıcı hunk'larının kendi commit'i; ertelenen küçükler `progress.md` içinde `Final: minor (deferred)`.
- Aşağıdaki §1–9 oturum 1 sonundaki durumu anlatır; "yarım/bekliyor" ifadeleri artık geçerli değildir.

## 1. Önce bunu bil

**Ana bağlantı/failover özelliği henüz tamamlanmadı.** Onaylı 12 görevden 1–5 commit edildi; 6. görev çalışma ağacında devam ediyor; 7–12 bekliyor. Ara derlemeleri tamamlanmış ürün gibi kurma, yayınlama veya kullanıcının canlı ağına uygulama.

Son kullanıcı talebi geliştirmeyi sürdürmek değil, `AGENTS.md` ve bu devir belgesini oluşturmaktır. Bu turda kaynak koduna dokunulmadı. Devam etmeden önce güncel kullanıcı talebini doğrula.

Çalışma dizini: RouteLane deposunun kökü. Dal: `main`; bu belgenin hazırlanmasında `origin/main` karşısında 11 commit ilerideydi. HEAD:

```text
a14fffd6a5d1f6e09d923995a9a4f9e9721ec45e
```

Kullanıcı önceki uygulamada **main üzerinde, inline; yeni worktree ve alt ajan olmadan** ilerlemeyi seçti. Bağımsız inceleme yapılmış gibi raporlama. Multipass'ta ayrı test VM'i kullanımı onaylı; host kurulumu/canlı ağ mutasyonu için bu onayı genelleme.

## 2. Nereden okunmalı?

Depo köküne geçtikten sonra sırayla:

1. `AGENTS.md`: kısa katkı, komut ve güvenlik kuralları.
2. `docs/superpowers/specs/2026-10-14-primary-connection-failover-design.md`: onaylı gereksinimler ve güvenlik sınırı.
3. `docs/superpowers/plans/2026-10-14-primary-connection-failover.md`: görevler, kabul testleri ve bağımlılıklar. Sıradaki başlık **Task 6**.
4. `.superpowers/sdd/2026-10-14-primary-connection-failover/progress.md`: en ayrıntılı yerel karar/test günlüğü. **Git tarafından yok sayılıyor; yeni klonda gelmeyebilir.**
5. `docs/research/primary-failover-kernel.md`: kernel/nft ABI araştırması ve kanıtlar.
6. `README.md`, `docs/reliability-audit.md`, `docs/ui-ux-verification.md`: ürün/mimari ve önceki doğrulamalar. Tarihsel sonuçlar güncel kabul kanıtı değildir.
7. `docs/commercial-roadmap.md`: ticari/açık kaynak kapsamı; teknik görevlere alternatif spesifikasyon değildir.

Yerel çalışma alanındaki `baseline.patch`, `task-*-brief.md`, `task-*-tests.log`, `task*-feature.patch` dosyaları geçmişi yeniden kurmaya yardımcıdır. Bunlar da klona taşınmayabilir. Plan kutucukları tek başına güncel durum kaynağı değildir; commit, diff ve test günlüğüyle karşılaştır.

## 3. Ürün amacı ve sabit kararlar

- Başlangıçta işletim sisteminin varsayılan bağlantısını ana hat kabul et; kullanıcı başka ana hat seçerse önce uyarı/onay al.
- Örnek: Ethernet ana, VINN yardımcı; kurallı siteler/uygulamalar VINN'den, diğer yeni internet bağlantıları ana hattan çıksın.
- Ana hat kesilince yalnızca kullanıcı tarafından onaylanmış yedeklere geç. Ana hat geri geldiğinde 5 saniyelik kararlılık süresini uygula; mevcut sahiplenilmiş yedek bağlantıları zorla koparma, yeni akışları ana hatta yönelt.
- IPv4/IPv6 kararları ayrı. Uygun çıkış yoksa fail-closed; OS default'una sessiz kaçış olmasın.
- Route/Direct/Block ve uygulama bağlılığını koru. V3 migrasyonu salt okunur yüklemede ağı etkinleştirmesin; ilk aktivasyonda semantik değişikliği açıkla.
- OS ana routing tablosunu, DNS'i, yabancı firewall/policy kurallarını ve global sysctl değerlerini yeniden yazma. Global NAT, conntrack flush, IPv6 kapatma veya proxy ile testleri geçiştirme yok.
- Onay, bilinmeyen kaynağın silinmesine izin vermez. Root helper sadece doğrulanmış sahiplik ve dar, tipli sözleşmelerle mutasyon yapar. `Inspect` salt okunur kalır.
- İlk resmi sürümden Ubuntu, Debian, Fedora destek kapsamında; tek Ubuntu testi veya `.deb` üretimi bu taahhüdü doğrulamaz.

Domain kuralları şu anda uygulama anındaki sistem resolver IP snapshot'una dayanır; sürekli DNS/TTL/CDN keşfi yok. DoH/farklı DNS cevapları kaçırılabilir. Desteklenen host uygulamasının süreç ağacını aynı lane'e bağlamak yeni üçüncü taraf bağlantılarını kapsar; aynı browser sürecinin başka sekmelerini de etkileyebilir. Snap/Flatpak bağlılığı ve mevcut socket'leri sonradan taşıma garantisi yok. Bağlantı Sağlığı yerel plana uygunluğu gösterir, sitenin açıldığını kanıtlamaz.

## 4. Tamamlanan commit'ler

| Commit | Teslim edilen parça |
| --- | --- |
| `d64cb96` | Ana bağlantı/failover tasarımı |
| `fd2e705` | 12 adımlı uygulama planı |
| `545d1d3` | Task 1: ConfigV3, routing policy, eski kayıt migrasyonu |
| `aa29c4d` | Task 2: gözlenen topology, sistem default'u/owned route ayrımı |
| `4ad41a4` | Task 3: deterministik aile bazlı failover/failback seçici |
| `6c1a249` | Task 4: explicit primary/source/identity planner modeli |
| `6d25b6a` | Kernel-canonical nft reject ifadeleri; exact ownership düzeltmesi |
| `a14fffd` | Task 5: ortak nft JSON/text semantiği, akış bağlılığı, dar kaynak düzeltmesi |

Task 5 öncesi kernel kanıtı: A lane'i doğru mark alırken ARP isteği B'nin `10.10.2.2` kaynağıyla çıkıyordu; cevap yoktu. Düzeltilmiş disposable VM testinde A kaynağı `10.10.1.2` oldu ve cevap alındı. Mark-only route relookup kaynak adresi tek başına düzeltmiyor.

Ortak nft belgesi yerel akış/yerel orijinal kaynak kanıtı, conntrack bağlılığı, kontrol/LAN istisnaları ve scoped egress koruması içerir. `rules_v2_<hash>` zinciri tutarlılık mührüdür; kriptografik yetki imzası değildir. Mühür geçici guard'ı dışlar; pending/guarded journal fingerprint'leri guard'ın tamamını doğrular. Journal kaybolmuşken guard bulunan modern tabloyu otomatik temizlemek şu an güvenli kabul edilmiyor.

## 5. Çalışma ağacı: kullanıcı değişikliklerini koru

Bu belge öncesi index boştu; **hiçbir dosya staged değildi**. Aşağıdaki eski değişiklikler bu failover çalışmasından önce vardı; kullanıcıya ait kabul et:

```text
.gitignore
README.md
data/routelane-helper.service
packaging/deb/build-deb.sh
src/application/process.rs
src/backend/inspect.rs
src/backend/privileged.rs
src/daemon/service.rs
tests/ipc_contract.rs
```

Önceden untracked: `data/routelane-tmpfiles.conf`, `docs/reliability-audit.md`, `packaging/deb/postinst`.

**`src/backend/privileged.rs` hem eski değişiklik hem yeni Task 6 hunks içerir.** Dosyayı topluca commit etme, baseline'a restore etme veya başka dosyadan kopyalama. `baseline.patch` ve Git diff ile hunk bazında ayır. `git add .`, hard reset/clean, tüm dosyayı checkout etme yok.

Yeni Task 6 değişiklikleri:

- `src/backend/interface_settings.rs` (untracked): `InterfaceSettingReceipt`, `MarkAwareRpf`, fixed-purpose backend/system adapter. Yalnız strict effective RPF gerekiyorsa arayüz `src_valid_mark=1`; intent journal callback'i başarılı olmadan yazma yok. Aynı tam cihaz kimliği ve beklenen değerle compare-and-restore; all/default/diğer tunable yazımı yok. **Üretim transaction/journal akışına henüz bağlı değil.**
- `tests/primary_transaction_contract.rs` (untracked): 6 fake-backend scope/lifecycle testi. Bunlar gerçek sysctl yazımı kanıtı değil.
- `src/backend/{nft_document,topology,privileged}.rs`, `tests/nftables_contract.rs`, `tests/fixtures/nftables/three-lane.nft`: değişmez sayısal iif/oif indeksleri, canlı nft dump çözümlemesi. Sembolik çıktıyı eski ad→index ile değil, bounded gerçek Links snapshot'ının kararlı önce/sonra tam kimlik haritasıyla çöz. Sadece indeks tipli operandlar; sayısal-only interface adları belirsiz olduğu için reddediliyor.
- `src/backend/mod.rs`: yeni modül export'u.
- `tests/root/namespace_routing.rs`: `ROUTELANE_TEST_STRICT_RPF=1` sadece testin sahip olduğu client namespace'i strict yapar.
- `src/backend/privileged.rs`: `FakeAppliedStateStore.fail_commit_generation` ve `pending_commit_failure_restores_exact_previous_state`; committed journal save hatasında önceki journal/NFT/plan geri geliyor mu kontrolü.
- `docs/research/primary-failover-kernel.md` ve ignored ilerleme günlüğü: yeni ABI kanıtları/kararlar.

Bu dokümantasyon turunda ayrıca yeni `AGENTS.md` oluşturuldu. Handoff `/tmp` altındadır, Git diff'e girmez.

## 6. Doğrulama: geçmiş ile bugün ayrımı

| Kanıt | Sonuç ve sınırı |
| --- | --- |
| Task 5 son rootless turu | 360 passed, 2 açıkça ignored; fmt/clippy temizdi |
| Task 6 son önceki full rootless turu | Oturum kaydına göre 370 passed, 2 ignored; son rollback testi eklenmeden önce. Bu belge turunda full suite tekrar çalıştırılmadı |
| **Bu tur:** `rtk cargo test --lib pending_commit_failure_restores_exact_previous_state` | **1 passed**, 143 filtered out, exit 0 |
| **Bu tur:** `rtk proxy cargo fmt --all -- --check` | **FAIL**, exit 1; `privileged.rs` yeni fake-store/test bloğu biçimlendirilmemiş. Kod değiştirilmedi |
| Task 5 gerçek helper namespace suite | Ayrı Ubuntu VM'de 6/6 passed; TCP/UDP/IPv6/failback kapsamının tamamı değil |
| Task 6 strict-RPF ek fixture | **5 passed / 1 failed**; A-lane ilk yanıtta %100 kayıp. Journal/settings bağlantısı yapılmadan GREEN değil |
| Gerçek nft 1.0.9 JSON/text gate | Her iki biçimde EXACT 86 obje; eski ifindex2 silinip aynı ad ifindex9 olunca eski literal indeks değişmiyor |
| `root-integration --no-run` | Derleme geçmişte başarılı; kernel testi veya Polkit geçişi değil |
| Gerçek system-bus/Polkit ve Debian/Fedora | **Henüz doğrulanmadı** |

Sayısal-index hardening sonrası yeni helper/root test ikilisiyle standart gerçek root suite yeniden koşulmalı. Önceki 6/6'yı güncel dirty kodun kapsamlı geçişi diye sunma. Exit77, derleme başarısı veya izin hatası beklenen davranış RED'i yerine geçmez. Son test sayısını yeniden koşmadan artırıp “371 geçti” deme.

## 7. İzole VM ve tekrar üretme notları

Onaylı tek yeni VM: `routelane-test-u2404-20261014`; Ubuntu 24.04.5, kernel 6.8.0-142-generic, nft 1.0.9; 2 vCPU/2 GiB/12 GiB. Repo/home mount veya ek fiziksel bridged interface kullanılmadı. `slave-01`, `master-01`, `master-02`, `central-lab-01` kapsam dışı; dokunma.

Önceki oturum sonunda test namespace listesi boştu; host routing/DNS/sysctl/kurulu servisler değiştirilmedi. Bu dokümantasyon turunda VM durumu yeniden sorgulanmadı.

Ignored çalışma alanında VM scriptleri: `vm-baseline-run.sh`, `vm-capture-baseline.sh`, `vm-nft-roundtrip.py`; fixture'lar: `primary-policy.{json,nft}`, `expected-primary-policy.json`.

Guest çalışma dosyaları `/home/ubuntu/` altında: `routelane-root-baseline`, `routelane-helper-candidate`, dondurulmuş `routelane-helper-baseline`, `vm-baseline-run.sh`, `vm-nft-roundtrip.py`. Scriptin beklediği helper yolu ve CARGO_MANIFEST_DIR kaynak script yolu guest içinde oluşturulmuştu; gerçek yolları runner/kaynak koddan doğrula. Guest'e kopyalanan artifact'ler yeni çalışma ağacından eski olabilir; tarih/hash kontrolü yap.

**Önemli build tuzağı:** `cargo test --all-targets`, `target/debug/routelane-helper` dosyasını root-integration özelliği olmayan binary ile değiştirebilir. Feature'lı helper'ı root tests ile seri olarak yeniden derle ve hemen guest'e aktar; taşınan artifact kimliğini doğrula. Test stdio yolu gerçek system-bus/Polkit yetkilendirme testi değildir; bypass'u üretime taşıma.

Frozen eski nft fixture: `tests/fixtures/nftables/legacy-primary-baseline.nft`, kaynak `aa29c4d`, SHA256 `f581c1775f73d2742a70bea46a68343a26e5bac0260eaf16db9e753203fbe2a3`.

## 8. Sıradaki uygulama adımları ve kabul sınırı

1. Yeni talep uygulamaya devam etmekse önce dirty diff'i/Task 6'yı oku, son test bloğunu rustfmt ile biçimlendir; baseline'ı koruduğunu kontrol et. Aşağıdaki odaklı testleri tekrar çalıştır.
2. Journal'ı explicit version'la; gerçek eski V1 `StoredAppliedState`/`KernelPlan` DTO ve legacy doğrulayıcıyı **cleanup-only** dondur. Eski state'e uydurma primary default ekleyip runtime yetkisi verme. Boyut limitlerini kontrollü güncelle.
3. Setting receipts'i durable pending/commit/rollback/cleanup'a bağla. Journal/fsync hatası sıfır ayar yazımı yapmalı; committed save hatası receipt kaybettirmemeli; kimlik/değer değişmişse yabancı ayarı geri yazma. Aynı-değer admin yazımı compare-and-restore ile ayırt edilemez; bu sınırı belgele.
4. Terminal unreachable route/rule komutlarını exact ownership/crash-prefix validator'larıyla **birlikte** bağla; planner builder var diye runtime'ı tamamlanmış sayma. Tam kimliği mutasyondan hemen önce yeniden doğrula.
5. Servis `ProtectKernelTunables=yes` kalsın; gerekiyorsa yalnız belgelenmiş arayüz src_valid_mark yazımını sağlayan dar exception. Gerçek systemd sandbox doğrulaması ayrıca gerekir.
6. Task 6 unit/contracts + standart/strict izole helper testleri GREEN olmadan Task 7'yi geçmiş veya Task 8 otomasyonunu etkin sayma.
7. Task 7: gerçek TCP, UDP request/reply, IPv6/no-family escape, strict/loose RPF, uzun yedek oturumunun failback sonrası devamı, source/address kaybı, yerel kontrol ve foreign sentinel cleanup kanıtları.
8. Task 8–10: bounded root runtime/olaylar ve grant; IPC/restart correlation; GTK primary/backup seçimi, onay ve gerçek effective durum. Helper planını root topology'den üret; daemon preview'ını etkin gerçek sanma.
9. Task 11–12: fault/restart/system-bus Polkit ve Ubuntu/Debian/Fedora install/upgrade/remove/sandbox/kernel/GTK kabul matrisi. Ürün ancak bütün release kapılarıyla tamamlanır.

### Açık güvenlik riskleri

- Link kaybında kernel owned route'u silebilir. Auto refresh yalnız **bilinen eksik route + taze loss/identity kanıtı + değişmemiş tam NFT + ek/değişmiş reserved kaynak yok** ise bunu açıklayabilir. Keyfi subset kabulü veya hazır lane'deki admin route silinmesini link kaybı sayma yok.
- Manual lane-ID değişiklikleri slot→mark eşleşmesini değiştirebilir; eski conntrack/NAT kaynağı yeni lane'den kaçabilir. Gerçek kernel reproducer ve gerekirse dar **post-SNAT source+identity guard** gerekir. Mevcut pre-SNAT guard bunu kanıtlamaz. Global CT flush veya varsayımsal session survival vaat etme.
- Root runtime derleyicisi topology'yi dondurulmuş grant kimlikleriyle filtrelemeli; yalnız `PrimarySelector` kontrolü, planner'ın onaysız yeniden kullanılan cihazı görmesini engellemez.
- Modern guard var/journal yok durumunun otomatik recovery yolu henüz güvenli kanıtlanmadı; adında routelane geçiyor diye temizleme yok.
- Sayısal interface adının nft dump ABI belirsizliğini normalize ederek gizleme; fail-closed kal.

## Suggested Skills

- `superpowers:using-superpowers`: başlangıçta ilgili yönergeleri yükle; kullanıcı kapsamı ve no-agent tercihi öncelikli.
- `matt-skills-curated:implement` veya `superpowers:executing-plans`: onaylı planın Task 6 sınırını uygula.
- `superpowers:test-driven-development`: yeni journal/legacy/rollback davranışında önce gerçek RED assertion.
- `superpowers:systematic-debugging`: strict-RPF ve kernel ABI sorunlarında kanıt→hipotez→dar test.
- `superpowers:verification-before-completion`: geçmiş sonucu yeni başarı diye sunma; gerçek komut çıktısını doğrula.
- `matt-skills-curated:handoff`: oturum sonunda yeni durum ve bir sonraki adımı aktar.

## 9. Tam devam komutları

Depo kökünde **ilk komut**:

```bash
rtk git status --short --branch
```

Sonra yalnız okuma/odaklı rootless kontrol:

```bash
rtk git diff -- src/backend/privileged.rs src/backend/interface_settings.rs
rtk proxy sed -n '236,269p' docs/superpowers/plans/2026-10-14-primary-connection-failover.md
rtk cargo test --test primary_transaction_contract
rtk cargo test --lib pending_commit_failure_restores_exact_previous_state
rtk proxy cargo fmt --all -- --check
```

`git diff` untracked dosyanın içeriğini göstermez; `interface_settings.rs` için ayrıca `rtk proxy sed -n '1,300p' src/backend/interface_settings.rs` kullan. Şu an fmt kontrolünün yukarıdaki test bloğunda başarısız olması beklenir; belge isteği bunu düzeltme yetkisi değildir.

Uygulamaya devam talebi geldikten sonra değişikliği tamamla; `rtk cargo test --test transaction_contract`, `rtk cargo test --lib backend::privileged`, `rtk cargo test --all-targets`, `rtk cargo clippy --all-targets -- -D warnings` ve fmt gate'ini çalıştır. Kernel testlerini yalnız onaylı guest içinde root olarak `bash scripts/run-root-integration.sh` ile veya doğrulanmış prebuilt guest runner üzerinden çalıştır; workstation'da sudo ile başlatma.

Yeni geliştiriciye verilebilecek başlangıç istemi:

> Bu handoff'u, AGENTS.md'yi ve ana bağlantı/failover spec/planını oku. Kullanıcıya ait dirty baseline'ı koru. Özellik tamamlanmış değil: Task 6 journal/legacy cleanup/receipt/terminal ownership entegrasyonundan devam et. Rootless ve gerçek izole kernel kanıtını ayır; host ağına/kurulumuna dokunma. Ana dalda inline ve alt ajansız çalış. Önce durumu ve sıradaki dar RED testini bildir.
