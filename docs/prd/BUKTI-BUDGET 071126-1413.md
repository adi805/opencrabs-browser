---
document_type: capability-evidence
topic: memory-budget-feasibility
project: "OpenCrabs Browser v0.2: Native Rust Engine"
date: "2026-11-07"
author: "OpenCrabs & Adi"
---

# Analisis Kelayakan Anggaran Memori: 99 Tab dalam 100 MB

Dokumen ini menyajikan audit empiris, perbandingan industri, dan perhitungan aritmetika
sistemik untuk target arsitektur v0.2: **"Engine browser full Rust dengan 99 tab terkelola
dalam pagu 100 MB RAM"**.

---

## 1. Bukti Empiris dari Engine Chromium Saat Ini (crates/browserd)

Berdasarkan pengujian langsung di host laptop pengembang (Lenovo Mint 22.3, 4 core, 7.7 GB RAM)
melalui `crates/browserd/tests/biaya_tab.rs` (commit `3c1d5cf`):

| Konfigurasi | Baseline (1 tab) | 20 Tab Terbuka | Biaya Marjinal / Tab |
|---|---|---|---|
| `chrome-headless-shell` (bawaan) | 142 MB PSS | 796 MB PSS | +32.7 MB / tab |
| `chrome-headless-shell` (`--process-per-site`) | 141 MB PSS | 629 MB PSS | +24.4 MB / tab |
| `chrome-full` di Xvfb (bawaan) | 295 MB PSS | 759 MB PSS | +23.2 MB / tab |
| `chrome-full` (`--process-per-site`) | 299 MB PSS | 555 MB PSS | +12.8 MB / tab |

### Temuan Batas Fisik Chromium:
1. **Baseline terlalu gemuk:** Hanya untuk membalas handshake CDP pertama dengan satu tab kosong, Chromium sudah menyita 141 MB (shell) hingga 295 MB (full). Anggaran 100 MB sudah jebol sebelum halaman pertama dimuat.
2. **Tab freezing tidak membebaskan memori:** Pengujian opsi `Page.setWebLifecycleState(state: "frozen")` membuktikan hanya ada penghematan 4% (full) hingga 18% (shell). Struktur DOM, V8 isolate, dan layout tree tetap menetap di memori fisik.

---

## 2. Pembanding Industri: Lightpanda (Engine Headless Modern Tanpa Chromium)

Lightpanda (ditulis dari nol menggunakan Zig, tanpa grafis Skia) mempublikasikan benchmark
paralelisme terhadap Chrome. Sumber: <https://lightpanda.io/docs/core-concepts/benchmarks>,
diukur dengan `smem` memakai PSS supaya halaman shared tidak dihitung dua kali, sampel tiap
100 ms, di AWS m5.xlarge, Chrome 143.0.7499.169.

| Paralel | Lightpanda RAM peak | Chrome RAM peak | Rasio |
|---|---|---|---|
| 1 | 27.2 MB | 1.3 GB | 48x lebih hemat |
| 2 | 31.7 MB | 1.3 GB | 41x |
| 5 | 43.9 MB | 1.6 GB | 36x |
| 10 | 63.7 MB | 1.7 GB | 27x |
| 25 | 123.0 MB | 2.0 GB | 16x |
| **100** | **410.2 MB** | **4.2 GB** | **10x** |

**Koreksi tafsir yang penting.** Kolom Lightpanda itu **proses terpisah, bukan tab**: dokumentasi
resminya menyatakan Lightpanda tidak bisa membuka banyak tab dalam satu proses, jadi benchmark
100 paralel berarti 100 proses, masing-masing di port sendiri. Artinya 410.2 MB / 100 = 4.1 MB
adalah biaya **satu proses utuh**, bukan biaya satu tab di dalam satu engine. Chrome di kolom
kanan justru satu browser dengan banyak tab, dan 4.2 GB / 100 = 42 MB per tab.

Konsekuensinya untuk PRD v0.2: angka Lightpanda **tidak** membuktikan bahwa 99 tab dalam satu
proses butuh 410 MB. Yang dibuktikan angka itu adalah batas atas biaya per proses, dan engine
Rust satu-proses yang mengelola tab secara virtual tidak membayar overhead per proses itu sama
sekali. Sebaliknya angka Chrome 42 MB per tab konsisten dengan pengukuran sendiri di bagian 1
(23 sampai 33 MB per tab marjinal), jadi dua sumber ini saling menguatkan, bukan saling menggantikan.

---

## 3. Aritmetika Anggaran: 99 Tab dalam 100 MB

Target: **100 MB total PSS untuk 99 tab terkelola**.

### Kasus A: 99 tab aktif serentak. TERUKUR, dan hasilnya menutup jalur ini

Harness di luar repo: `~/.opencrabs/projects/browser-audit/ukur-memori` (kode `src/main.rs`, hasil
mentah `HASIL.txt` 103 baris). Yang diukur bukan perkiraan: `html5ever` 0.40 mem-parse halaman
3000 kartu (3006 elemen), `taffy` 0.15 menghitung layout, satu `rquickjs` 0.14 context memuat 200
objek dan 50 tautan per tab. PSS dibaca dari `/proc/self/smaps_rollup`.

| Jumlah tab | PSS total | Marjinal per tab |
|---|---|---|
| 1 | 5.9 MB | 4.4 MB (warmup tab pertama) |
| 5 | 18.3 MB | 3.1 MB |
| 10 | 33.8 MB | 3.1 MB |
| 20 | 64.7 MB | 3.1 MB |
| 50 | 157.4 MB | 3.1 MB |
| 99 | **308.9 MB** | **3.10 MB** |

Dua fakta mengunci keputusan ini:

1. Marjinal **flat 3.10 MB** dari tab ke-2 sampai tab ke-99. Tidak ada kebocoran akumulatif di
   desain DOM, layout dan JS.
2. 99 tab hidup serentak = **308.9 MB**, yaitu **3.1x** anggaran. Jadi 100 MB tidak bisa dicapai
   dengan membiarkan tab tetap hidup, seberapa pun rapinya kode. Angka ini bahkan lebih buruk dari
   aritmetika lama yang menebak 198 sampai 297 MB.

**Caveat yang wajib ikut dibaca:** 3.10 MB adalah **lower bound**. Yang diukur hanya DOM + layout +
JS heap. Belum ada HTTP/TLS client, CSS computed style, image decode, renderer `tiny-skia`, atau
CDP broker. Engine lengkap akan lebih mahal dari ini, bukan lebih murah.

### Kasus B: tiering. Yang direvisi jumlah hot tab, bukan pagu

Rumusnya diturunkan dari angka terukur, bukan dipilih:

`hot_tab = floor((100 - baseline_engine) / 3.10)`

| Baseline engine | Hot tab | Cold tab | RAM hot tab | Sisa anggaran |
|---|---|---|---|---|
| 10 MB | 29 | 70 | 99.9 MB | 0.1 MB |
| 15 MB | 27 | 72 | 98.7 MB | 1.3 MB |
| 20 MB | **25** | **74** | **97.5 MB** | 2.5 MB |
| 25 MB | 24 | 75 | 99.4 MB | 0.6 MB |
| 30 MB | 22 | 77 | 98.2 MB | 1.8 MB |

Dengan baseline 20 MB (tokio runtime + HTTP/WS client + CDP broker + buffer cadangan):
`25 x 3.10 MB = 77.5 MB`, plus baseline 20 MB = **97.5 MB <= 100 MB**, plus 74 tab dingin dengan
metadata sekitar 1 KB per tab (bukan 30 KB seperti asumsi lama).

**Perbandingan langsung dengan versi lama:** asumsi 12 MB per hot tab memberi 5 hot tab. Terukur
3.10 MB per hot tab memberi **25 hot tab di pagu yang sama**. Asumsi lama 3.9x terlalu pesimis,
dan efeknya besar: 5 tab aktif adalah mainan, 25 tab aktif adalah alat kerja.

---

## 4. Status Tiap Angka: TERUKUR atau Diasumsikan

Bagian ini memisahkan anggaran dari angan-angan. Yang TERUKUR punya perintah yang bisa dijalankan
ulang; yang masih diasumsikan punya nama uji yang harus lulus sebelum angka 100 MB boleh diucapkan.

| Angka | Nilai | Status | Cara membuktikan |
|---|---|---|---|
| Baseline `chrome-headless-shell` | 142 MB PSS | **TERUKUR** | `crates/browserd/tests/biaya_tab.rs`, commit `3c1d5cf` |
| Baseline `chrome-full` di Xvfb | 295 MB PSS | **TERUKUR** | idem |
| Marjinal per tab Chrome | 12.8 sampai 32.7 MB | **TERUKUR** | idem |
| Efek `--process-per-site` | 759 turun ke 555 MB | **TERUKUR** | idem |
| Efek tab freeze | hemat 4 sampai 18 persen | **TERUKUR** | `Page.setWebLifecycleState`, idem |
| Lightpanda 100 paralel | 410.2 MB | **TERUKUR pihak lain** | benchmark resmi, 100 proses terpisah |
| Marjinal satu tab Rust (DOM+layout+JS) | **3.10 MB PSS, flat ke 99 tab** | **TERUKUR** | `ukur_memori 99`, hasil di `HASIL.txt`; harness mem-parse 3006 elemen, layout taffy, context rquickjs |
| 99 tab Rust hidup serentak | **308.9 MB PSS** | **TERUKUR** | idem, baris `TAB99` dan `RINGKAS` |
| Baseline engine Rust (tokio+HTTP+CDP) | 20 MB | **Diasumsikan** | harus dibuktikan uji `native_baseline_pss` di milestone 1 |
| Biaya satu hot tab di engine lengkap | lebih dari 3.10 MB | **Diasumsikan** | uji `native_hot_tab_cost` di milestone 1, karena 3.10 MB belum memuat HTTP, CSS, renderer |
| Metadata tab dingin | 1 KB | **Diasumsikan** | uji `native_cold_tab_metadata` di milestone 4 |
| Rehidrasi | di bawah 50 ms | **Diasumsikan** | uji `test_tab_rehydration_latency` di milestone 4 |

**Konsekuensi jujur.** Yang sekarang berdiri di atas pengukuran adalah biaya per tab (3.10 MB) dan
konsekuensinya (99 tab hidup = 308.9 MB). Yang masih asumsi adalah baseline engine 20 MB, biaya tab
di engine lengkap, metadata tab dingin, dan latency rehidrasi. Karena itu 97.5 MB di Kasus B adalah
**proyeksi**, bukan hasil pengukuran, dan belum boleh ditulis sebagai bukti bahwa target tercapai.

Milestone 1 wajib mengukur baseline engine sebelum milestone 4 memakai rumus ini. Kalau baseline
keluar 30 MB bukan 20 MB, yang turun adalah jumlah hot tab (25 menjadi 22), bukan pagu 100 MB.
Kalau biaya tab di engine lengkap ternyata 6 MB, hot tab jatuh ke 13. Keduanya masih di atas angka
lama 5, jadi revisi ini tetap menang, tapi angkanya harus datang dari uji, bukan dari dokumen ini.

---

## 5. Vonis & Strategi Arsitektur v0.2

1. **Ganti Mesin ke Native Rust:** Menghapus ketergantungan pada biner eksternal Chrome (baik shell maupun Xvfb).
2. **Kedaulatan Mutlak Agent (Tangan & Mata):**
   - **Mata:** Parsing DOM berbasis `html5ever`, ekstraksi pohon aksesibilitas (AXTree) instan, rendering visual headless melalui `tiny-skia` / `vello` on-demand (hanya saat agent meminta screenshot).
   - **Tangan:** Event dispatcher langsung ke node target (klik sintetis, keyboard typing, form fills) tanpa lapisan IPC berat.
3. **Penyelia Tab Tiered (Virtual Tab Pool):** Pagu memori dijaga dengan batas keras 100 MB. Tab ke-6 dan seterusnya yang tidak disentuh otomatis masuk ke tier suspensi (rehidrasi instan < 50ms begitu diakses agent).
