---
document_type: project-progress
project: "OpenCrabs Browser — mesin buka-web"
prd: "PRD-MESIN-BUKA-WEB 041126-0631.md"
status: draft
current_milestone: 2
total_milestones: 4
last_updated: "2026-11-04 16:39"
---

# OpenCrabs Browser Progress

Live state untuk target ini saja. PRD memegang outcome milestone, Done When, dan
cakupan FR/NFR/AC; berkas ini mencatat bukti dan langkah berikutnya, bukan
menyalin spesifikasi.

## Current State

- PRD version: 0.1.0 (draft)
- Execution mode: native (belum ada build yang diminta untuk requirement baru)
- Build authority: **belum ada**. PRD ini spesifikasi, bukan izin membangun.
- Active milestone / next action: milestone 1 (mata dan tangan) dan milestone 2
  (klasifikasi kegagalan + diagnostik) sudah **Done**, keduanya dibuktikan test
  live. Berikutnya milestone 3 (ketahanan sesi) lalu 4 (audit terpadu).
- Otoritas build: owner memberi izin eksekusi penuh 3 November 2026.
- Required unresolved input: tiga keputusan owner di PRD §2 Review Focus
  (batas anti-bot, tab paralel, `chrome_full`) mempengaruhi FR-001/FR-006/FR-004.

## Milestones And Evidence

| # | PRD milestone reference | Status | Current evidence / remaining gap |
|---|---|---|---|
| 1 | §4 #1 Mata dan tangan | ✅ Done | Runtime `oc-mata.js` + `oc-tangan.js` disuntik ke setiap tab baru (`Page.addScriptToEvaluateOnNewDocument`), dan `crates/browserd/tests/mata_tangan.rs` membuktikan di mesin sungguhan: 14 kotak centang custom diklik sampai status halaman berubah, isi shadow DOM `closed` dan iframe se-asal terbaca, klik yang tertutup overlay dilaporkan gagal tanpa mengubah halaman. `cargo test -p browserd --test mata_tangan` = 2 passed |
| 2 | §4 #2 Diagnostik + eskalasi | ✅ Done | `klasifikasi.rs` (6 kategori, bukti wajib, 12 test), `artefak.rs` (4 berkas + kuota, 6 test), `eskalasi.rs` (tunggu → ulang backoff maks 2 → ganti mesin → handoff, 10 test), routing host→mesin + `validasi()` aturan saat start, metrik `browser_open_failures_total{kategori}`. `crates/browserd/tests/diagnostik.rs` membuktikan di mesin sungguhan: normal → `Terbuka`/200, 403 bertanda tantangan → `BlokAntiBot`, 2FA → `ButuhManusia`, port mati → `Jaringan`. `cargo test -p browserd --test diagnostik` = 1 passed |
| 3 | §4 #3 Ketahanan sesi + mesin | ⬜ Not Started | Keadaan awal terukur: `idle_reap_secs = 60` mematikan mesin tanpa klien, `max_tabs` dibaca saat start sehingga ubah angka = restart |
| 4 | §4 #4 Audit terpadu | ⬜ Not Started | Gate yang sudah ada: `cargo xtask check` exit 0 di HEAD `f13fb01` |

## Resume And Changes

- 2026-11-04 06:31 — PRD dibuat dari percakapan 2–4 Nov 2026 dan bukti repo
  (HEAD `f13fb01`, `max_tabs` sudah ditegakkan; browserd jalan di 9224 dengan
  `max_tabs = 6`; 9222 produksi tidak disentuh).
- Bukti terukur yang dipakai di PRD: mesin kosong 26 MB PSS, 6 tab 187 MB PSS,
  `--single-process` 3 tab 522,8 MB lawan 858,8 MB, Firefox headless 896 MB idle
  tanpa `/json/version`, Lightpanda nol layout.
- 2026-11-04 07:50 WIB — Milestone 1 selesai dan dibuktikan test live, bukan
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
- 2026-11-04 08:05 WIB — Bukti di daemon yang **sedang jalan**, bukan cuma di
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

- 2026-11-04 16:39 WIB — Milestone 2 selesai dengan bukti, bukan klaim.
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

## AI Agent Rules

Baca PRD sekali per sesi, lalu berkas yang berubah. Jangan mengulang discovery
yang tidak berubah, jangan klaim VERIFIED tanpa keluaran alat, dan jangan
memperlakukan PRD ini sebagai izin membangun.