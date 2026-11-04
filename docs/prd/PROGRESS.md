---
document_type: project-progress
project: "OpenCrabs Browser: mesin buka-web"
prd: "PRD-MESIN-BUKA-WEB 041126-0631.md"
status: in-progress
current_milestone: 4
total_milestones: 4
last_updated: "2026-11-04 21:29"
---

# OpenCrabs Browser Progress

Live state untuk target ini saja. PRD memegang outcome milestone, Done When, dan
cakupan FR/NFR/AC; berkas ini mencatat bukti dan langkah berikutnya, bukan
menyalin spesifikasi.

## Current State

- PRD version: 0.1.0 (draft)
- Execution mode: native (belum ada build yang diminta untuk requirement baru)
- Build authority: **belum ada**. PRD ini spesifikasi, bukan izin membangun.
- Active milestone / next action: milestone 1 (mata dan tangan), milestone 2
  (klasifikasi kegagalan + diagnostik), milestone 3 (ketahanan sesi + mesin),
  dan milestone 4 (audit terpadu) sudah **Done**. Audit 26 AC:
  **26 VERIFIED, 0 UNVERIFIED**, gerbang penuh hijau.
  Dua AC yang dulu menunggu bukti otomatis sudah ditutup: AC-007 (routing host
  ke mesin) lewat test integrasi, AC-019 (isolasi port produksi 9222) lewat uji
  Rust yang menyalakan mesin nyata di port acak lalu membandingkan 9222.
- Otoritas build: owner memberi izin eksekusi penuh 3 November 2026.
- Required unresolved input: tiga keputusan owner di PRD §2 Review Focus
  (batas anti-bot, tab paralel, `chrome_full`) mempengaruhi FR-001/FR-006/FR-004.

## Milestones And Evidence

| # | PRD milestone reference | Status | Current evidence / remaining gap |
|---|---|---|---|
| 1 | §4 #1 Mata dan tangan | ✅ Done | Runtime `oc-mata.js` + `oc-tangan.js` disuntik ke setiap tab baru (`Page.addScriptToEvaluateOnNewDocument`), dan `crates/browserd/tests/mata_tangan.rs` membuktikan di mesin sungguhan: 14 kotak centang custom diklik sampai status halaman berubah, isi shadow DOM `closed` dan iframe se-asal terbaca, klik yang tertutup overlay dilaporkan gagal tanpa mengubah halaman. `cargo test -p browserd --test mata_tangan` = 2 passed |
| 2 | §4 #2 Diagnostik + eskalasi | ✅ Done | `klasifikasi.rs` (6 kategori, bukti wajib, 12 test), `artefak.rs` (4 berkas + kuota, 6 test), `eskalasi.rs` (tunggu → ulang backoff maks 2 → ganti mesin → handoff, 10 test), routing host→mesin + `validasi()` aturan saat start, metrik `browser_open_failures_total{kategori}`. `crates/browserd/tests/diagnostik.rs` membuktikan di mesin sungguhan: normal → `Terbuka`/200, 403 bertanda tantangan → `BlokAntiBot`, 2FA → `ButuhManusia`, port mati → `Jaringan`. `cargo test -p browserd --test diagnostik` = 1 passed |
| 3 | §4 #3 Ketahanan sesi + mesin | ✅ Done | `lifecycle::lease` (gudang lease per sesi, TTL wajib ditolak nol, batas dihitung dari lease yang masih hidup, 11 test), `browserd::reload` (SIGHUP menukar peta `max_tabs` tanpa restart; config rusak = batas lama dipertahankan, 4 test), `browserd::anggaran` (supervisor PSS per mesin, pemulihan dijanjikan < 30 detik dan 31 detik dihitung terlambat, 8 test), `cdp` gagal-tertutup untuk mesin tak dikenal + gagal-terbuka untuk metrik gagal dibaca. Bukti live di `crates/browserd/tests/sesi_live.rs` (4 test): handoff dijawab melanjutkan tab yang sama (`window.__sesi` selamat), handoff dibatalkan melepas tab, SIGHUP mengubah `max_tabs` 3 → 1 → 4 dengan pid tetap, config rusak saat reload mempertahankan batas lama. Handoff membawa judul, PNG di `<store_db>/handoff/`, dan selector `#challenge-form` |
| 4 | §4 #4 Audit terpadu | ✅ Done | Audit 26 AC di `docs/prd/AUDIT-AC 041126-1718.md`: 24 VERIFIED, 2 UNVERIFIED (AC-007 routing per host, AC-019 isolasi port produksi), keduanya beralasan. Dua itu sudah ditutup di putaran berikutnya, jadi kini 26 VERIFIED, 0 UNVERIFIED. Satu cacat ditemukan dan diperbaiki (`9ba1129`): default `--port` `browserd` menunjuk 9222, port browser produksi. Gerbang di tree yang sama dengan kode `9ba1129`: fmt rc 0, clippy rc 0 tanpa warning, `cargo test --workspace --no-fail-fast` 32 suite ok / 427 passed / 0 failed rc 0, `cargo xtask check` rc 0 |

## Resume And Changes

- 2026-11-04 06:31: PRD dibuat dari percakapan 2-4 Nov 2026 dan bukti repo
  (HEAD `f13fb01`, `max_tabs` sudah ditegakkan; browserd jalan di 9224 dengan
  `max_tabs = 6`; 9222 produksi tidak disentuh).
- Bukti terukur yang dipakai di PRD: mesin kosong 26 MB PSS, 6 tab 187 MB PSS,
  `--single-process` 3 tab 522,8 MB lawan 858,8 MB, Firefox headless 896 MB idle
  tanpa `/json/version`, Lightpanda nol layout.
- 2026-11-04 07:50 WIB: Milestone 1 selesai dan dibuktikan test live, bukan
  klaim. Lima bug ditemukan justru oleh test itu sendiri, semuanya nyata:
  (1) axum menjawab `text/plain` sehingga Chromium membungkus fixture dalam
  `<pre>`; (2) `setAttribute` di dalam constructor custom element membuat
  Chromium **permanen** gagal meng-upgrade elemen (14 kotak jadi
  `HTMLUnknownElement`, 0x0 piksel); (3) konstruktor event dan prototipe input
  diambil dari realm jendela teratas sehingga elemen di iframe melempar
  `Illegal invocation`; (4) `Node.contains()` tidak menembus batas shadow root,
  jadi host dilaporkan "tertutup" oleh isinya sendiri; (5) potret keadaan
  membaca dokumen teratas saja, jadi klik yang berhasil di iframe dilaporkan
  "tidak ada perubahan".
- Teardown test sekarang diaudit: group proses mesin wajib hilang dalam 5 detik
  sesudah `stop()`, dengan `Reaper` yang tetap membunuh group kalau assertion
  gagal di tengah jalan. Sebelum ini, run yang merah meninggalkan 15+ browser
  hantu dan memakan RAM sampai reboot.
- 2026-11-04 08:05 WIB: Bukti di daemon yang **sedang jalan**, bukan cuma di
  test: setelah `install -m755 target/debug/browserd ~/.opencrabs/bin/browserd`
  dan restart `opencrabs-browserd.service` (9224, produksi 9222 tidak disentuh),
  klien CDP biasa membuat tab baru lalu mendapat `typeof window.__oc ===
  "object"`. `__oc.mata()` melihat tombolnya (`peran:"button"`, kotak 43x21,
  `asal:"dom"`), dan `__oc.klik({nama:'Halo'})` menjawab `berubah` dengan jalur
  `klik-native` sementara teks tombol di halaman benar benar menjadi
  "sudah diklik".
- Angka bukti: `cargo test -p browserd --test mata_tangan` 2 passed;
  `cargo test -p browserd --lib` 70 passed.
- Yang **belum** terukur dan tidak diklaim: laju deteksi anti-bot pada host
  nyata, konsumsi `chrome_full` di xvfb, dan efektivitas lease tab.

- 2026-11-04 16:39 WIB: Milestone 2 selesai dengan bukti, bukan klaim.
  Tiga situs yang dulu gagal dibuka ulang lewat daemon 9224 dan hasilnya
  mengubah desain klasifikasi (detail di
  `docs/prd/BUKTI-BUKA-ULANG 041126-0857.md`): Google membalas **429** di
  `/sorry/index` dengan teks halaman kosong sehingga status + pola URL + frame
  reCAPTCHA yang jadi bukti, `nowsecure.nl` memasang hCaptcha tanpa satu pun
  event dokumen karena tab lahir sudah membawa URL (navigasi terjadi sebelum
  `Network.enable`), dan host mati mendarat di `chrome-error://chromewebdata/`
  yang ternyata bukti jaringan paling kuat.
- Kode yang lahir dari bukti itu: `klasifikasi.rs` (6 kategori, `Bukti` wajib
  membawa penyebab dan minimal satu kenyataan pendukung, 12 test),
  `artefak.rs` (4 berkas per kegagalan plus kuota 20 percobaan / 64 MB, menolak
  menulis di dalam work tree, 6 test), `eskalasi.rs` (tunggu jaringan tenang →
  ulang dengan backoff maksimal 2 → ganti mesin → handoff, berhenti di langkah
  pertama yang berhasil, 10 test), serta `validasi()` di `policy::select` yang
  menolak aturan default ke mesin yang tidak terpasang.
- Angka gerbang terakhir: `cargo xtask check` → fmt ok, clippy ok, test ok;
  `cargo test --workspace` = 15 suite, 0 failed.
- Yang **belum** terukur di milestone 2: efektivitas `stealth_hosts` per host,
  dan laju deteksi anti-bot pada host nyata selain tiga situs di atas.

- 2026-11-04 11:54 WIB: Milestone 3 selesai, dan tiga dari empat jalurnya
  lahir dari kegagalan yang sama: daemon yang harus dimatikan untuk mengubah
  satu angka adalah daemon yang akan dimatikan pada waktu yang salah.
- Lease menahan reap (AC-009, AC-012, AC-013). `lifecycle::lease` menyimpan
  lease murni di memori dengan waktu disuntik, jadi aturan kedaluwarsa diuji
  tanpa menunggu. Tiga keputusan yang menentukan perilakunya: `pegang` menolak
  TTL nol (lease tanpa batas waktu tidak bisa dibedakan dari kebocoran), batas
  per sesi dihitung dari lease yang masih HIDUP bukan total sepanjang masa, dan
  `ganti_batas` tidak mencabut lease yang sudah ada. `Lease::hidup` inklusif di
  detik kedaluwarsa supaya "15 menit" berarti 15 menit. Bukti di reap:
  `crates/browserd/tests/idle_reap.rs::lease_hidup_menahan_reap_dan_kedaluwarsa_tidak`.
- SIGHUP mengganti batas tanpa restart (AC-010, AC-011). `browserd::reload`
  memuat config, memvalidasi, lalu menukar peta `max_tabs` di `EnginePool` yang
  dipakai bersama semua klon (`Arc<RwLock<..>>`), dan `loop_reap` membaca
  `idle_reap` ulang tiap tick 5 detik sehingga SIGHUP bisa MENYALAKAN reap yang
  tadinya mati. Config rusak tidak mengubah apa pun: batas lama dipertahankan
  dan alasannya dicatat, tidak jatuh ke tanpa batas. `--idle-reap-secs` dari
  CLI tetap menang bahkan sesudah reload, karena flag itu perintah operator untuk
  proses ini.
- Anggaran memori memulihkan mesin, bukan mematikan daemon (AC-016, AC-017).
  Supervisor membaca PSS per grup proses (bukan RSS), dan saat melewati ambang
  keras mesin dimatikan lalu dinyalakan lagi lewat `spawn_blocking` sementara
  daemon tetap melayani. Janji 30 detik dicatat jujur: pemulihan 31 detik
  dihitung TERLAMBAT lewat counter `pulih_terlambat`, bukan berhasil.
  Pengukuran yang gagal dibiarkan terbuka dengan peringatan, karena supervisor
  yang mematikan mesin karena tidak bisa membaca `/proc` lebih berbahaya
  daripada tidak ada supervisor. `[budget]` yang hanya menulis sebagian ambang
  ditolak dengan nama bagian dan kunci yang kurang, bukan diterima dengan nilai
  karangan.
- Arah gagal dipilih sadar (AC-020): tujuan tak dikenal = gagal-TERTUTUP
  (galat -32602 menyebut mesin yang diminta; menebak mesin lain akan mengirim
  perintah ke tab yang bukan milik pemanggil), metrik gagal dibaca =
  gagal-TERBUKA (menolak tab baru hanya karena metrik gagal dibaca akan
  mematikan daemon yang sehat).
- Handoff manusia membawa konteks siap jawab (AC-014): judul lewat
  `Runtime.evaluate` dibatasi 200 karakter, layar PNG asli di
  `<store_db>/handoff/` dengan nama berkas yang menyebut detik dan id target
  yang sudah disanitasi (id berisi `/` tidak boleh menulis di luar direktori
  itu), dan selector hanya ekspresi yang benar-benar dicari `JS_DETECT`;
  sinyal teks menjawab `None` daripada mengarang selector. Base64 didekode
  sendiri tanpa dependensi baru karena masukan tidak sah harus jadi error,
  bukan PNG rusak yang ditampilkan sebagai bukti. Semua pengambilan
  gagal-terbuka: handoff tetap dibuat tanpa field yang gagal, dan berkas layar
  yang tidak jadi dirujuk dihapus lagi.
- Bukti live di `crates/browserd/tests/sesi_live.rs` (4 test, 2,91 detik):
  `handoff_dijawab_melanjutkan_tab_yang_sama` membuktikan `window.__sesi`
  selamat dari handoff ke jawaban operator (tab yang SAMA, bukan tab baru),
  `handoff_dibatalkan_melepas_tab_dan_mesin_siap_lagi`,
  `sighup_mengubah_batas_tab_tanpa_restart` (3 → 1 → 4 dengan pid tidak
  berubah), dan `config_rusak_saat_reload_mempertahankan_batas_lama`.
- Satu bug nyata ditemukan gerbang penuh, bukan oleh milestone ini:
  `engine::slot::pastikan` menulis `terakhir` dengan `sekarang` yang diambil
  SEBELUM mesin dinyalakan, jadi mesin yang baru selesai boot langsung terlihat
  menganggur selama seluruh waktu boot-nya dan bisa di-reap tepat setelah
  dipakai. Diperbaiki di `fix(engine)` dengan uji penggerak boot 1,2 detik.
- Satu test live sempat flaky, dan penyebab pertamanya nyata:
  `m4_proof::dua_mesin_nyata_tidak_saling_mengganggu` membaca `document.title`
  sekali tepat saat `frameNavigated` datang, padahal event itu tanda navigasi
  COMMIT bukan dokumen selesai diparse. Diganti retry berpagar waktu 5 detik.
- Retry itu belum cukup, dan kegagalannya memang bukan satu. Di bawah
  `cargo test --workspace` tiga uji berbeda gagal dengan bentuk yang sama
  ("perintah CDP tidak dijawab", `Connection reset without closing handshake`)
  padahal ketiganya lolos sendirian (`m4_proof` menunggu `Page.enable` 90 detik,
  `mata_tangan` menunggu `Runtime.evaluate` 10 detik, `sesi_live` tidak
  menemukan tangkapan layar). Log daemon yang disimpan saat gagal menunjuk
  sebabnya: mesin `kiri` berhenti pada `06:11:09` padahal baru lahir `06:11:04`,
  artinya prosesnya dibunuh, bukan lambat menjawab.
- Pembunuhnya `earlyoom`. Sejak 12:00 tercatat 221 kill `chrome-headless` dan 71
  kill `opencrabs` (yang terakhir ini yang membuat sesi agent restart
  berulang-ulang di tengah gerbang). Pemicunya `oc-serve-embed.service`
  (embedding server OpenCrabs) yang berputar di loop restart: `NRestarts=641`,
  `Restart=always`, tiap siklus memuat model 200-800 MB lalu kena SIGTERM saat
  swap menyentuh 0%. Loop itu menekan memori host, dan earlyoom memilih korban
  berikutnya dari proses terbesar yang sedang jalan: mesin uji gue.
- Dua langkah, karena sebabnya dua. Pertama, jejak memori uji live dikecilkan:
  uji live sekarang diserialkan lewat kunci `flock` host-wide di
  `crates/browserd/tests/common/mod.rs`, dipegang selama satu uji live
  berjalan, jadi bukan lagi lima binary melahirkan Chromium bersamaan. Kuncinya
  `std::fs::File::try_lock` (Rust 1.98): nol dependency baru, dan kernel
  melepasnya kalau proses mati sehingga tidak ada kunci basi. Berkas kunci
  ditaruh di direktori `target` build itu sendiri supaya checkout lain di host
  yang sama tidak ikut mengantre. Kedua, loop `oc-serve-embed` dilaporkan ke
  owner sebagai masalah host, bukan ditambal dari sisi uji.
- Angka gerbang sesudah keduanya: `cargo fmt --all -- --check` rc 0,
  `cargo clippy --workspace --all-targets -- -D warnings` rc 0, lalu
  `cargo test --workspace --no-fail-fast`: 32 suite ok, 426 passed, 0 failed,
  rc 0, dan **delta 0 kill earlyoom** selama run. Sebelum ini, run yang sama
  dengan serialisasi aktif tapi host masih di tengah spiral tetap gagal tiga
  target; jadi hijau ini datang dari memori yang sehat, bukan dari kebetulan.

- Yang **belum** terukur di milestone 3, dan tidak diklaim: (1) `Gudang::pegang`
  BELUM dipanggil dari jalur produksi mana pun; lease vs reap terbukti di test
  (`idle_reap.rs`), tapi belum ada kode daemon yang benar-benar memegang lease
  atas nama sesi. (2) TTL handoff 900 detik diuji dengan jam semu di
  `control::registry`, tidak live. (3) `loop_anggaran` belum diuji end-to-end
  dengan mesin sungguhan; pemulihan terbukti dengan mesin palsu.

### Milestone 4: audit terpadu 26 AC

- Audit seluruh FR/NFR/AC ada di `docs/prd/AUDIT-AC 041126-1718.md`, disusun
  di atas HEAD `7dffb6c`. Aturan statusnya satu, dan itu yang membuat angkanya
  bisa dipercaya: VERIFIED berarti ada perintah yang bisa dijalankan ulang
  orang lain dan lulus di gerbang itu; semua yang lain ditulis UNVERIFIED
  dengan alasannya. Hasil: **24 VERIFIED, 2 UNVERIFIED**. Keduanya sudah
  ditutup setelah putaran itu, jadi angka terkini ada di bagian Routing host
  dan Isolasi port produksi di bawah.
- Dua yang tidak dinaikkan bukan karena kurang diperiksa, tapi karena
  buktinya memang belum ada. AC-007 (routing host ke mesin) logikanya lengkap
  dan teruji unit, tetapi nol pemanggil di luar `crates/policy/src/select.rs`;
  pembuatan target selalu memakai mesin bawaan (`crates/cdp/src/targets.rs:270`).
  AC-019 (isolasi port produksi) buktinya masih prosedur manual: `/json/list`
  9222 menunjukkan 0 target sebelum dan sesudah uji live, sementara uji live
  itu sendiri benar-benar menyalakan Chromium di port dev.
- Audit menemukan satu cacat nyata, dan cacat itu jenis yang paling tidak
  enak: kode dan komentarnya saling bertentangan. `browserd serve` tanpa flag
  dan tanpa berkas config akan mencoba bind 9222, yaitu port browser produksi,
  padahal komentar di atas argumen itu sudah mengklaim 9224 sejak semula.
  Selama ini tertutupi karena unit systemd mengirim `--bind` eksplisit, jadi
  yang selama ini melindungi adalah berkas konfigurasi, bukan default yang
  benar. Diperbaiki di `9ba1129`, plus test pengunci
  `default_port_adalah_9224_bukan_port_produksi`.
- Angka gerbang di tree yang sama dengan kode `9ba1129`: `cargo fmt --all
  -- --check` rc 0, `cargo clippy --workspace --all-targets -- -D warnings`
  rc 0 dengan 0 baris warning, `cargo test --workspace --no-fail-fast` 32
  suite ok, 0 suite FAILED, 427 passed, 0 failed, rc 0, lalu `cargo xtask
  check` rc 0 (fmt ok, clippy ok, test ok). Run pertama fmt merah pada
  `crates/browserd/src/cli.rs` karena mod test baru belum diformat; yang
  diperbaiki bentuknya, lalu seluruh gerbang dijalankan ulang. Gerbangnya
  tidak dilonggarkan dan tidak ada allowed-warning yang ditambah.
- Yang **belum** terukur di milestone 4, dan tidak diklaim: tiga butir dari
  milestone 3 masih berlaku tanpa perubahan (`Gudang::pegang` belum dipanggil
  jalur produksi, TTL handoff 900 detik masih jam semu, `loop_anggaran` belum
  end-to-end mesin nyata), ditambah dua yang baru: AC-007 dan AC-019 belum punya
  bukti otomatis. Run gerbang ini juga tidak mengukur memori awal dan akhir,
  jadi tidak ada klaim apa pun soal pembunuhan earlyoom di putaran ini.

### Gerbang ulang: reap tidak boleh jalan sebelum ambang

- Gerbang ulang di HEAD `03fca2e` memunculkan regresi nyata, dan regresinya
  duduk di jalur produksi, bukan di test: `cargo test --workspace
  --no-fail-fast` keluar 101 dengan `panicked at
  crates/browserd/tests/idle_proof.rs:343: reap terjadi sebelum ambang:
  9.852608104s < 10 s`.
- Akar penyebabnya satu tempat pemotongan satuan. `unix_seconds()`
  (`crates/browserd/src/daemon.rs:327`) membulatkan cap waktu ke detik ke
  bawah, `Slot.terakhir` menyimpan detik, dan `diam()` menghitung
  `Duration::from_secs(sekarang - terakhir)`. Dua cap waktu yang sama-sama
  dipotong ke bawah membuat selisihnya **melebihkan** waktu menganggur sampai
  satu detik penuh, jadi reap bisa jalan satu detik sebelum ambangnya. Ambang
  uji 10 detik berarti reap boleh jalan di 9 detik; ambang produksi 60 detik
  (`crates/browserd/src/config.rs:344`) berarti mesin yang baru diam 59 detik
  bisa dimatikan.
- Perbaikannya resolusi milidetik, bukan pelonggaran ambang:
  `detik_sekarang()` jadi `ms_sekarang()` di `crates/engine/src/slot.rs`,
  `as_millis` dan `Duration::from_millis` menggantikan `as_secs` dan
  `from_secs`, lalu perubahannya dirambatkan ke `crates/cdp/src/lib.rs`,
  `crates/cdp/src/engines.rs`, `crates/browserd/src/wiring.rs`, dan
  `crates/browserd/src/daemon.rs`. Di daemon ada dua jam dengan dua satuan dan
  itu disengaja: `loop_reap` memakai milidetik, sedangkan TTL handoff dan lease
  tetap detik, dengan alasannya ditulis di kode. Arah keliru yang tersisa
  sekarang berbalik jadi aman: reap paling cepat terlambat satu milidetik,
  bukan terlalu cepat satu detik.
- Bahwa hasilnya bukan kebetulan dibuktikan dengan pengulangan, bukan sekali
  jalan: `idle_proof` dijalankan 3 kali berturut-turut, 1 passed 0 failed tiap
  run (55,95 s / 16,36 s / 15,60 s), dan `idle_reap` 3 kali berturut-turut, 6
  passed 0 failed tiap run. Gerbang penuh di tree yang sama: `cargo fmt --all
  -- --check` rc 0, `cargo clippy --workspace --all-targets -- -D warnings`
  rc 0 dengan 0 baris warning, `cargo test --workspace --no-fail-fast` rc 0
  dengan 32 suite ok, 427 passed, 0 failed, 0 ignored, lalu `cargo xtask check`
  rc 0 (fmt ok, clippy ok, test ok).
- Gerbang tidak dilonggarkan dan itu disengaja: tick reap tetap 5 detik, ambang
  uji tetap 10 detik, tidak ada allowed-warning baru. Satu-satunya yang
  ditambahkan adalah kelonggaran ukur 250 ms di test, dengan arah dan alasannya
  ditulis di kode (cap waktu "terakhir dipakai" ditulis daemon sebelum
  responsnya sampai ke stopwatch test, jadi stopwatch selalu mulai sedikit
  telat). Angka itu dikurung dari dua sisi: jauh di atas latensi localhost dan
  jauh di bawah tick reap 5 detik, jadi tidak ada reap-terlalu-cepat yang bisa
  lolos lewat celah itu.

### Routing host ke mesin tersambung di jalur nyata (AC-007)

- Temuan awalnya bukan bug kode, tapi pemanggil yang hilang: `policy::select`,
  `rule_for`, dan `host_matches` di `crates/policy/src/select.rs` nol pemanggil
  di luar berkasnya sendiri, dan pembuatan target selalu jatuh ke mesin bawaan.
  Logika aturannya benar, tapi tidak ada satu pun jalur nyata yang membacanya.
- Tiga sambungan dipasang, satu per lapisan. Di `policy` ditambah
  `select_untuk_host(host, &PolicySection, &Catalog)` yang mengembalikan
  `Ok(None)` kalau tidak ada aturan yang cocok, plus 4 test unit. Di `cdp`
  ditambah kontrak `PemilihMesin` dan `host_dari_url`, lalu cabang
  `Target.createTarget` di `crates/cdp/src/mux/peta.rs` membaca aturan saat
  klien tidak menyebut mesin eksplisit. Di `browserd` ada
  `crates/browserd/src/routing.rs` berisi `PemilihPolicy` yang membungkus
  `PolicySection` + katalog mesin, dan `crates/browserd/src/daemon.rs`
  menyambungkannya lewat `ProxyState::pool(..).with_observer(..).with_pemilih(..)`.
- Tiga sifat sengaja dipertahankan karena itu inti AC-007. Permintaan eksplisit
  klien (`openCrabsOptions.engine`) menang atas aturan host; jenis yang dipilih
  aturan tapi tidak terpasang dijawab `-32000` yang menyebut jenisnya, bukan
  diam-diam diganti mesin bawaan; dan aturan yang menang dicatat lewat
  `tracing::info!` supaya bisa dibaca dari log, bukan cuma benar di test.
- Buktinya di dua tingkat. `cargo test -p browserd --test routing_host` = 4
  passed, jalan lewat proxy axum nyata dengan dua mesin websocket palsu beda
  jenis: aturan `toko.test -> chrome-full` mendarat di instance `full`,
  permintaan eksplisit mendarat di mesin yang diminta klien, jenis tak terpasang
  dijawab `-32000`, dan nol aturan memakai mesin bawaan. Test unit di
  `crates/cdp/src/mux/peta/uji.rs` mengunci jalur penolakan dan jalur penugasan
  di tingkat mux tanpa perlu soket.
- Satu kemungkinan yang sengaja tidak ditutup: `/json/new` tetap selalu memakai
  mesin bawaan, karena endpoint itu tidak membawa URL yang bisa dipetakan ke
  host. Kalau nanti dibutuhkan, jalurnya lewat parameter host eksplisit, bukan
  tebakan dari header.
- Status audit berubah: AC-007 dari UNVERIFIED jadi VERIFIED dengan perintah
  yang bisa dijalankan ulang.

### Isolasi port produksi jadi uji otomatis (AC-019)

- AC-019 satu-satunya yang tersisa UNVERIFIED, dan alasannya bukan kode hilang
  tapi bukti yang cuma manual: di putaran sebelumnya isolasi diukur dengan
  `/json/list` 9222 sebelum dan sesudah uji live, dicatat sebagai tabel di
  dokumen audit, tanpa perintah yang bisa diulang orang lain.
- Sekarang jadi `crates/browserd/tests/isolasi_port.rs`. Rantainya sama seperti
  prosedur tangan: baca jumlah target 9222, nyalakan `browserd serve` di port
  acak dengan profil sementara, buat satu tab sehingga mesin benar-benar lahir,
  lalu baca ulang 9222 dan bandingkan. Port debug mesin dibaca dari
  `DevToolsActivePort` yang mesin tulis sendiri, bukan dari angka yang diharap.
- Satu aturan keras dipegang: 9222 hanya dibaca. Kalau browser produksi sedang
  memakai port itu, browser tersebut tidak boleh diganggu, jadi yang
  dibandingkan keadaannya, bukan isinya. Perubahan `None` menjadi `Some` tetap
  tertangkap, karena daemon pengembangan yang menyerobot 9222 akan menyalakan
  listener di sana.
- Rantai `Resolved::resolve` dikunci duluan di tingkat unit, empat test di
  `crates/browserd/src/daemon.rs`: bind bawaan mustahil berakhir `:9222`,
  `[daemon]` tanpa `bind` tetap 9224, 9222 hanya tercapai lewat `bind` yang
  ditulis operator, dan profil bawaan mesin ada di `.cache/opencrabs-browser`,
  terpisah dari profil 9222. Test unit menahan regresi cepat; uji live menahan
  regresi yang cuma kelihatan saat mesin sungguhan nyala.
- Bukti: `cargo test -p browserd --lib daemon::tests` = 16 passed, 0 failed;
  `cargo test -p browserd --test isolasi_port` = 1 passed, 0 failed, exit 0.
  Uji live memakai `chrome-headless-shell` nyata, mesin dapat port 37107, bukan
  9222, dan dilewati dengan pesan jelas kalau biner mesin tidak ada.
- Gerbang penuh dijalankan di satu run (`cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo xtask check`,
  `cargo test --workspace --no-fail-fast`): semuanya exit 0, 450 test passed, 0
  failed, 0 panic, 34 suite 'test result: ok'. Tidak ada gerbang yang
  dilonggarkan dan tidak ada allowed-warning baru.
- Status audit berubah: AC-019 dari UNVERIFIED jadi VERIFIED. `docs/prd/AUDIT-AC`
  sekarang 26 VERIFIED, 0 UNVERIFIED.

## AI Agent Rules

Baca PRD sekali per sesi, lalu berkas yang berubah. Jangan mengulang discovery
yang tidak berubah, jangan klaim VERIFIED tanpa keluaran alat, dan jangan
memperlakukan PRD ini sebagai izin membangun.