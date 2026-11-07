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

Target: **100 MB total PSS / RSS untuk 99 tab terkelola**.

### Kasus A: 99 Tab Aktif Serentak di Memori (Semua DOM + JS Heap Hidup)
- Baseline runtime engine Rust (tokio runtime, HTTP/WS CDP broker, connection pool): **15 - 20 MB**.
- Sisa anggaran untuk 99 tab: `100 MB - 20 MB = 80 MB`.
- Anggaran per tab: `80 MB / 99 = 808 KB per tab`.
- **Fakta Realitas Web:**
  - Dokumen web modern rata-rata memiliki 1.500 - 3.500 DOM node.
  - Rust DOM node representation (`html5ever` + `taffy` layout): ~200 - 400 byte/node $\rightarrow$ ~700 KB.
  - QuickJS execution context + global scope + script cache: ~800 KB - 1.5 MB.
  - Total 1 tab aktif web riil = **1.5 MB - 3 MB**.
  - 99 tab aktif serentak = `99 * 2 MB = ~198 MB` (+ baseline 20 MB = **~218 MB**).
- **Kesimpulan Kasus A:** 99 tab yang *aktif serentak merender dan menjalankan JavaScript* dalam 100 MB adalah batasan fisik yang mustahil tanpa mengorbankan kelengkapan dokumen.

### Kasus B: 99 Tab Terkelola dengan Arsitektur Suspensi Virtual (Tiered Tab Memory)
Agent berinteraksi dengan browser secara sekuensial atau semi-paralel (1-5 tab aktif dikendalikan dalam satu waktu, sementara 94 tab lainnya adalah konteks tersimpan).

- **Baseline Engine Rust:** 18 MB.
- **Hot Tier (5 Tab Aktif Penuh):**
  - Memiliki DOM tree hidup, layout box, QuickJS context, network cache.
  - Alokasi: $5 \times 12\text{ MB} = 60\text{ MB}$.
- **Cold / Suspended Tier (94 Tab Tertidur):**
  - DOM di-evict ke ring buffer disk / SQLite (`crates/store`).
  - Menyimpan snapshot ringkas di RAM: URL, history stack, cookies, accessibility outline (~30 KB metadata per tab).
  - Alokasi: $94 \times 30\text{ KB} = 2.82\text{ MB}$.
- **CDP Broker & Buffer Cadangan:** 15 MB.
- **Total PSS Terpakai:**
  $$18\text{ MB} + 60\text{ MB} + 2.82\text{ MB} + 15\text{ MB} = \mathbf{95.82\text{ MB}} \le 100\text{ MB}$$

---

## 4. Status Tiap Angka: Terukur atau Diasumsikan

Ini bagian yang memisahkan anggaran dari angan-angan. Yang terukur punya perintah yang bisa
dijalankan ulang di repo ini; yang diasumsikan punya nama uji yang harus lulus sebelum klaim
100 MB boleh diucapkan.

| Angka | Nilai | Status | Cara membuktikan |
|---|---|---|---|
| Baseline `chrome-headless-shell` | 142 MB PSS | **Terukur** | `crates/browserd/tests/biaya_tab.rs`, commit `3c1d5cf` |
| Baseline `chrome-full` di Xvfb | 295 MB PSS | **Terukur** | idem |
| Marjinal per tab Chrome | 12.8 sampai 32.7 MB | **Terukur** | idem |
| Efek `--process-per-site` | 759 turun ke 555 MB | **Terukur** | idem |
| Efek tab freeze | hemat 4 sampai 18 persen | **Terukur** | `Page.setWebLifecycleState`, idem |
| Lightpanda 100 paralel | 410.2 MB | **Terukur pihak lain** | benchmark resmi, 100 proses terpisah |
| Baseline engine Rust | 18 MB | **Diasumsikan** | harus dibuktikan uji `native_baseline_pss` di milestone 1 |
| Biaya satu hot tab Rust | 12 MB | **Diasumsikan** | harus dibuktikan uji `native_hot_tab_cost` di milestone 1 |
| Metadata tab dingin | 30 KB | **Diasumsikan** | harus dibuktikan uji `native_cold_tab_metadata` di milestone 4 |
| Rehidrasi | di bawah 50 ms | **Diasumsikan** | harus dibuktikan `test_tab_rehydration_latency` di milestone 4 |

**Konsekuensi jujur.** Total 95.82 MB itu bukan hasil pengukuran, itu hasil penjumlahan empat
asumsi. Kalau biaya hot tab ternyata 20 MB bukan 12 MB, maka 5 tab panas saja sudah 100 MB dan
anggaran jebol sebelum satu tab dingin pun dihitung. Karena itu milestone 1 wajib mengukur dua
angka pertama sebelum milestone 4 dimulai: mengukur setelah mesinnya selesai berarti membangun
di atas anggaran yang belum pernah diuji. Kalau pengukuran milestone 1 keluar di atas asumsi,
angka yang direvisi adalah **jumlah hot tab**, bukan pagu 100 MB.


---

## 5. Vonis & Strategi Arsitektur v0.2

1. **Ganti Mesin ke Native Rust:** Menghapus ketergantungan pada biner eksternal Chrome (baik shell maupun Xvfb).
2. **Kedaulatan Mutlak Agent (Tangan & Mata):**
   - **Mata:** Parsing DOM berbasis `html5ever`, ekstraksi pohon aksesibilitas (AXTree) instan, rendering visual headless melalui `tiny-skia` / `vello` on-demand (hanya saat agent meminta screenshot).
   - **Tangan:** Event dispatcher langsung ke node target (klik sintetis, keyboard typing, form fills) tanpa lapisan IPC berat.
3. **Penyelia Tab Tiered (Virtual Tab Pool):** Pagu memori dijaga dengan batas keras 100 MB. Tab ke-6 dan seterusnya yang tidak disentuh otomatis masuk ke tier suspensi (rehidrasi instan < 50ms begitu diakses agent).
