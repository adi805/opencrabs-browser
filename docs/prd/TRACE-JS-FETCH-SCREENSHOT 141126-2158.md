# TRACE: `<script>`, `fetch()` dari halaman, dan `Page.captureScreenshot`

**Tanggal:** 14 Nov 2026 21:58 WIB
**Repo:** `opencrabs-browser` @ `ba65cc7`
**Binari:** `/mnt/hdd/OpenCrabs/build/cargo/debug/browserd` 274.147.368 byte (14 Nov 20:59), nol berkas `.rs` lebih baru darinya
**Cara:** baca kode (file:line diverifikasi di turn ini) + 4 daemon terpisah yang benar-benar dijalankan + pengukuran waktu per permintaan CDP

---

## 0. Hasil ringkas, sebelum detail

| Pertanyaan | Jawaban terukur |
|---|---|
| Apakah `<script>` inline jalan saat navigate? | Ya. `document.title` diubah skrip, judul yang dilaporkan `Page.navigate` ikut berubah |
| Apakah `<script src>` jalan? | Ya, lewat `Pengambil` yang **sama** dengan `fetch()`, jadi **melewati gerbang** (beda dari jalur dokumen, lihat §2) |
| Apakah `fetch()` dari halaman kena kebijakan? | Ya, dan penolakannya menyebut alasan yang bisa dibaca skrip |
| Apakah `fetch()` mengembalikan Promise? | **Tidak.** Objek biasa, disengaja (lihat §4) |
| Apakah cukup untuk aplikasi web hari ini? | **Belum.** 4 global yang dipakai hampir semua framework absen (lihat §5) |
| Berapa lama satu screenshot 800x600? | 843 ms untuk 23 node; **25.936 ms untuk 1.206 node**. ~20 ms per simpul, **linear, dan tidak turun saat diulang** |

---

## 1. Rantai eksekusi skrip (inline dan `src`), 9 simpul

| # | Simpul | Lokasi | Yang terjadi |
|---|---|---|---|
| 1 | Kumpulkan `<script>` | `crates/engine/src/native/js.rs:162` `kumpulkan_skrip` | Telusur DFS, **`children.iter().rev()`** (`:198`) supaya urutan dokumen terjaga walau pakai stack. `<script>` tidak ditelusuri anaknya (`:196` `continue`) karena isinya teks, bukan markup |
| 2 | Filter `type` | `js.rs:170-190` | Kosong / `text/javascript` / `application/javascript` / `text/ecmascript` / `application/ecmascript` dijalankan. **`type="module"` dilewati** (`js.rs:171-174`) dengan alasan "pemuat impor belum ada". Tipe lain (mis. `application/json`) dilewati sebagai "bukan MIME JavaScript" |
| 3 | Basis URL | `js.rs:1965-1974` | `<base href>` menang; kalau tidak ada, `Halaman::url()`. Dipakai untuk menyelesaikan `src` relatif |
| 4 | Ambil `src` | `js.rs:1978-2006` | `bingkai::selesaikan(&src, dasar)` lalu `pengambil.ambil(&PermintaanAmbil{metode:"GET"})`. **`Pengambil` = `PengambilJaringan`**, yaitu gerbang kebijakan yang sama dengan `fetch()` |
| 5 | Aturan gagal `src` | `js.rs:1998-2009` | Non-2xx → masuk `laporan.dilewati` dengan `HTTP <status>`, skrip **dilewati**. Galat jaringan → `dilewati`, skrip dilewati. **Tidak fatal**: navigasi tetap sukses |
| 6 | Evaluasi | `js.rs:2016` `self.eval(&sumber)` | Satu skrip satu isolat-eval. Hasil `Ok` → `dijalankan += 1` |
| 7 | Batas waktu | `js.rs:2018-2025` | `NativeError::BatasWaktu` **tetap dihitung `dijalankan += 1`**, pesannya masuk `galat`. Alasannya tertulis di komentar: yang gagal adalah penyelesaiannya, bukan pemanggilannya |
| 8 | Galat lain | `js.rs:2026-2028` | `dijalankan += 1` + pesan ke `galat`. Satu skrip gagal **tidak** menghentikan yang lain |
| 9 | Timer | `tab_pool.rs:490` → `js.rs:2041` `kuras_timer` | `globalThis.__timer.kuras()` dieval sekali sesudah semua skrip. **Tidak ada event loop**: timer hanya jalan kalau dikuras, dan penguras dipanggil pemanggil (navigate, `Runtime.evaluate`) |

**Konsekuensi yang tidak kelihatan dari kode tapi terukur:** `setTimeout(fn, 5000)` yang dipasang saat muat **tidak** akan jalan 5 detik kemudian. Ia jalan di kuras berikutnya, yaitu evaluasi CDP berikutnya. Halaman yang mengisi dirinya sendiri setelah delay tampak kosong sampai klien menyentuhnya lagi.

---

## 2. `fetch()` dari halaman: rantai gerbang, 6 simpul

| # | Simpul | Lokasi | Yang terjadi |
|---|---|---|---|
| 1 | Jembatan JS | `js.rs:1221` `JEMBATAN_AMBIL` | `globalThis.__ambil` diambil lalu **`delete`** (`:1224`), supaya skrip halaman tidak bisa memanggil fungsi Rust mentah dengan bentuk berbeda |
| 2 | Bentuk `fetch` | `js.rs:1261` | `fetch(url, init)` sinkron. Metode dari `init.method`, badan dari `init.body`. Hasil `JSON.parse(ambil(...))` |
| 3 | Galat → lempar | `js.rs:1268-1270` | `data.__galat` → `throw new TypeError(...)`. **Sinkron, bukan promise yang ditolak.** Komentar `:1256-1260` menyebut konsekuensinya sendiri: `.catch()` tidak bekerja, hanya `try`/`await` |
| 4 | Pasang ke `globalThis` | `js.rs:1277-1282` | `enumerable: true`, `configurable`, `writable`, sama seperti `window.fetch` di browser sungguhan |
| 5 | Fungsi Rust | `js.rs:2092` `pasang_ambil` → `:2110` set `__ambil` → `:2113` `eval(JEMBATAN_AMBIL)` | Dipasang **sesudah** jembatan DOM dan **sebelum** skrip halaman (`tab_pool.rs:473` lalu `:486`) |
| 6 | Gerbang | `ambil.rs:266-267` `impl Pengambil for PengambilJaringan` / `fn ambil` | `kebijakan.putuskan(permintaan)` → `alasan()` → `tracing::warn!` + `Err(alasan)`. Lolos → `ambil_blokir` (`:291`) dengan `ureq`, batas dari `kebijakan.batas` |

**Bukti hidup** (daemon `izin_host = ["contoh.invalid"]`, port loopback tidak diizinkan):

```
Runtime.evaluate  try{fetch('http://127.0.0.1:8099/tes.html').status}catch(e){e.message}
  -> "alamat '127.0.0.1' menunjuk ke jaringan internal host ini; tulis alamat itu sendiri
      di [fetch] izin_host kalau memang itu yang dimaksud"
```

Pesan itu **dibaca skrip**, bukan dikubur di log. Log daemon juga mencatatnya:
`WARN fetch dari halaman ditolak gerbang jaringan url=… metode=GET alasan=…` (`ambil.rs:273`).

**Asimetri yang harus dicatat:** `<script src>` (§1 simpul 4) memakai `Pengambil` yang sama, jadi **skrip eksternal melewati gerbang**. Bedanya dengan `Page.navigate` hanya di siapa yang memutuskan URL-nya: dokumen dipilih klien CDP, `src` dipilih halaman. Untuk halaman berbahaya keduanya sama saja, tapi untuk halaman lokal yang jahat, `src` bisa menunjuk alamat internal dan gerbangnya menolak. Jadi yang masih bocor cuma jalur dokumen (§1 dokumen di `TRACE-NAVIGATE`).

---

## 3. Screenshot: rantai sampai byte, 6 simpul

| # | Simpul | Lokasi | Yang terjadi |
|---|---|---|---|
| 1 | Method | `crates/cdp/src/native_wire/state.rs:905` `tangkap_layar` | `format_dari(params)` lalu `klip_dari(params)` |
| 2 | Normalisasi klip | `state.rs:930-966` | **`captureBeyondViewport` default `false`**: klip yang melewati 800x600 **dipotong** ke batas viewport, dan pemotongan dilaporkan di `receipt.clipDipotong`. Klip yang seluruhnya di luar viewport → `-32602` dengan saran mengirim `captureBeyondViewport: true` |
| 3 | Hidupkan tab dingin | `state.rs:970-972` | `pool.tab(tab).is_none()` → `pool.hidupkan(tab)`. Tab dari penyimpanan dingin **tidak** menjalankan ulang skrip (`tab_pool.rs:561-573`, `bangun_dengan(..., false)` di `:573`), karena snapshot sudah post-skrip |
| 4 | Lukis | `state.rs:979` → `paint.rs:224` `lukis_dengan_font` → `:233` `lukis_penuh` | Layout dihitung **di dalam** paint (`layout.rs:227` `hitung_dengan_font`), bukan disimpan |
| 5 | Encode | `paint.rs:400-402` | `Format::Png{cepat}` → `kodekan_png` (`:450`), `Jpeg{mutu}` → `kodekan_jpeg`, `Webp` → `kodekan_webp` |
| 6 | Balas | `state.rs:992-1007` | `data` base64 + `receipt` (`width`, `height`, `format`, `scale`, `kotakDilukis`, `clipDipotong`, `diabaikan`) |

**Tidak ada cache layout dan tidak ada cache shaping glyph.** `layout.rs:60-71` hanya memegang `HashMap` per-simpul untuk simpul itu; tidak ada yang bertahan antar-permintaan. Setiap `captureScreenshot` mengulang seluruh tata letak dari akar.

### 3.1 Waktu yang terukur (bukan perkiraan)

Satu tab, satu page load, satu `Page.captureScreenshot{format:"png"}`, viewport bawaan 800x600:

| Halaman | Simpul DOM | Waktu | Byte PNG |
|---|---|---|---|
| `tes.html` (judul + 1 kotak + 2 paragraf + tombol) | 23 | **843 ms** | 8.566 |
| `b100.html` (100 baris) | 206 | **5.815 ms** | 22.317 |
| `b300.html` (300 baris) | 606 | **14.058 ms** | 22.369 |
| `b600.html` (600 baris) | 1.206 | **25.936 ms** | 22.379 |

Regresi linear antar titik: **(25.936 − 5.815) / (1.206 − 206) = 20,1 ms per simpul**, intersep ≈ 1,7 detik. Byte PNG **hampir tidak berubah** (22,3 KB) karena isi di luar 600 px terpotong: yang tumbuh adalah waktu, bukan gambar.

### 3.2 Diulang tidak lebih cepat

Tiga `captureScreenshot` berturut-turut pada tab yang sama, halaman `b300`, tanpa navigasi di antaranya:

```
shot #1:  14.798,6 ms
shot #2:  13.178,2 ms
shot #3:  17.391,3 ms
```

Tidak ada penurunan sistematis. Ini mengonfirmasi §3 simpul 4: layout dan shaping dihitung ulang setiap kali. Browser sungguhan menyimpan layout tree dan hanya me-reflow yang berubah; di sini tidak ada yang disimpan.

### 3.3 Efek samping yang lebih serius dari lambatnya

Pada `b600` (1.206 simpul, 25 detik), permintaan itu **menahan satu worker tokio**. `tangkap_layar` adalah `fn` sinkron yang dipanggil dari loop `layani` (`mod.rs:474`). Selama 25 detik itu, `GET /json/list` dari klien lain **tidak dijawab** (terukur: `curl` timeout 5 detik, `HTTP 000`), padahal kesehatan daemon biasanya 7 ms. Daemon terlihat mati bagi apa pun yang memeriksanya. Dua worker lain masih hidup, jadi bukan deadlock, tapi cukup untuk membuat health check gagal.

---

## 4. Di mana absennya Promise/`objectId` berpengaruh

`Runtime.evaluate` menerima `awaitPromise` tapi lapisan `fetch()` tidak pernah menghasilkan promise, jadi `await promise` tidak punya arti di sini. Konsekuensi konkret, bukan teoretis:

| Pola kode | Di browser sungguhan | Di mesin ini |
|---|---|---|
| `await fetch(u).then(r=>r.text())` | jalan | **jalan**, karena `await` pada nilai non-thenable mengembalikan nilai itu (`js.rs:1217-1220`) |
| `fetch(u).catch(e => fallback)` | jalan | **tidak jalan**: galat dilempar sinkron, `.catch()` tidak pernah menerimanya (`js.rs:1256-1260`) |
| `Promise.all([fetch(a), fetch(b)])` | paralel | `Promise.all` menerima nilai non-promise, tapi `ambil()` dipanggil **saat argumen dievaluasi**, jadi serial, dan galat satu melempar sebelum `Promise.all` terbentuk |
| `fetch(u).then(...)` tanpa `await` | async | **melempar sinkron** di tempat pemanggilan |

Ini keputusan desain yang **didokumentasikan**, bukan bug yang tersembunyi: komentarnya menyebut alasannya (menjalankan mesin promise di dalam isolat berarti satu state mesin lagi yang harus diuji terhadap batas waktu) dan menyebut batasnya di `CELAH`. Yang perlu diputuskan di PRD: apakah batas ini diterima permanen, atau `fetch` harus mengembalikan promise sungguhan begitu isolat punya loop microtask.

---

## 5. Titik yang memblokir halaman aplikasi modern (diuji, bukan diduga)

26 probe `Runtime.evaluate` pada halaman ber-JS, daemon hidup:

| Fitur | Status |
|---|---|
| `Promise`, `new Promise` | **ada** |
| `class`, `let/const`, arrow fn, template literal | **ada** |
| `setTimeout` | ada (tapi hanya jalan saat dikuras, §1 simpul 9) |
| `Array.from`, `Object.assign`, `JSON.parse`, `Symbol`, `Map`/`Set`, `Object.defineProperty`, `queueMicrotask`, `Error` | **ada** |
| `document.querySelector(All)`, `.forEach` pada NodeList, `innerHTML` setter | **ada** |
| `getComputedStyle`, `requestAnimationFrame`, `performance` | **ada** |
| **`localStorage`** | **`undefined`** |
| **`XMLHttpRequest`** | **`undefined`** |
| **`WebSocket`** | **`undefined`** |
| **`customElements`** | **`undefined`** |
| `Element.prototype.click` / `dispatchEvent` | ada (`js.rs:967`, `:973`), `click` menggelembung sampai `document` (tes `js.rs:3086`) |
| `Input.dispatchKeyEvent` / `dispatchTouchEvent` | **dijawab galat** (`CELAH` entri 3) |
| `<script type="module">` | **dilewati** (`js.rs:172`) |

**Pembacaan yang jujur:** empat global yang absen itu bukan detail. `localStorage` dipakai hampir semua app untuk sesi; `XMLHttpRequest` dipakai pustaka lama dan sebagian polyfill; `WebSocket` wajib untuk app realtime; `customElements` wajib untuk Web Components. Ditambah `type="module"` yang dilewati, **bundel Vite/webpack/esbuild modern yang mengirim ESM tidak akan jalan**. Yang bisa dibuka hari ini adalah situs dokumen/artikel dan app ber-skrip klasik, bukan SPA hasil build hari ini.

---

## 6. Gerbang kebijakan: yang belum diuji (jangan diklaim)

- **Belum diuji** `fetch()` ke `169.254.169.254` di host yang benar-benar punya metadata service. Di laptop ini gerbangnya menolak sebelum socket (`ambil.rs:214-217` untuk link-local), jadi yang terbukti adalah **penolakan**, bukan penerimaan.
- **Belum diukur** biaya shaping per glyph secara terpisah dari tata letak: yang terukur adalah gabungan keduanya (20 ms/simpul), bukan pembagiannya. Klaim "shaping yang mahal" karena itu **belum divalidasi**; yang bisa diklaim hanya "tata letak + lukis linear terhadap jumlah simpul".
- **Belum diuji** apakah `captureBeyondViewport: true` pada halaman panjang menghasilkan gambar yang benar dan berapa biayanya. Yang terbukti: klip di luar viewport dipotong, dan pemotongan dilaporkan.

## 7. Cara mereproduksi

```bash
PID=$(ss -ltnp | grep ':9250 ' | sed -n 's/.*pid=\([0-9]*\).*/\1/p')
/tmp/ocb-audit/mem.sh                       # tabs/pid/rss/pss
python3 /tmp/ocb-audit/shot3.py             # 4 format + waktu, tab b300
python3 /tmp/ocb-audit/scaling.py           # 3 halaman, 3 daemon terpisah
python3 /tmp/ocb-audit/twoshot.py           # 3 screenshot berturut, tab sama
python3 /tmp/ocb-audit/modern.py            # 26 probe fitur JS
```

Skrip-skrip itu di `/tmp` dan akan hilang; angka di dokumen ini yang mengikat. Kalau mau dijadikan tes regresi, isinya harus pindah ke `crates/cdp/tests/`.

---

## 8. Dua koreksi yang muncul saat verifikasi baris ini

Dua hal yang awalnya saya tulis sebagai temuan, lalu berubah setelah kode dibaca ulang. Dicatat supaya tidak diulang di PRD sebagai klaim baru:

1. **`awaitPromise` bukan fitur yang hilang diam-diam, tapi hilang yang tidak dilaporkan.** Saya mengirim `Runtime.evaluate {"awaitPromise": true}` dan tidak menerima `-32602`; permintaannya diterima, parameternya diabaikan tanpa sepatah kata. Padahal `CELAH` (`crates/engine/src/native/mod.rs:123-126`) sudah menyatakan terang-terangan bahwa `awaitPromise`, `objectId`, dan `Runtime.callFunctionOn` belum ada. Jadi yang bocor bukan kapabilitasnya, tapi **pelaporan parameter yang diabaikan**: `captureScreenshot` punya `receipt.diabaikan`, sedangkan `Runtime.evaluate` tidak. Klien yang mengandalkan `awaitPromise` akan menyimpulkan skripnya gagal, bukan menyimpulkan parameter itu tidak dipakai.

2. **`CELAH` sudah menulis batas `fetch`, jadi itu bukan temuan baru.** `mod.rs:116-122` menyebut `fetch(...).then(...)`, `Promise.all`, `AbortController`, `arrayBuffer()`, stream, dan cookie sebagai yang belum ada. Bagian §4 di dokumen ini **mengonfirmasi** pernyataan itu dengan eksekusi, bukan menemukannya. Bedanya penting untuk PRD: ini keputusan yang sudah didokumentasikan dan perlu diputuskan ulang secara sadar, bukan cacat yang tersembunyi.
