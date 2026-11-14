# Bukti: celah `<script>` dan API DOM ditutup (14 Nov 2026 18:38 WIB)

Dokumen ini menutup dua temuan di `BUKTI-PAKAI-NYATA 141126-1728.md` §3:

1. `<script>` di halaman **tidak pernah dijalankan** mesin native.
2. Sebelas API DOM/window hilang tanpa disebut satu baris pun di `CELAH`.

Angka di bawah dibaca dari output perintah di sesi yang sama, bukan dari ingatan.

---

## 1. `<script>` sekarang berjalan

Mesin sebelumnya mem-parse HTML, membangun DOM, menghitung layout, lalu menggambar.
Skrip halaman diabaikan total: `Document` selesai dibangun tanpa satu pun evaluasi JS.
Akibatnya halaman yang mengandalkan JS (hampir semua halaman nyata) tampil sebagai
kerangka kosong, dan tidak ada satu baris pun di `CELAH` yang mengakuinya.

Yang ditambahkan:

| Bagian | Isi |
|---|---|
| `native::js::Halaman` | Konteks halaman untuk isolat: `url`, `Viewport`, `Lembar` (stylesheet), cache layout malas ber-versi, font opsional |
| `native::js::SumberSkrip` | Sumber skrip: sebaris atau eksternal (`src`), hasil resolusi `bingkai::selesaikan` terhadap `<base href>` atau URL halaman |
| `native::js::kumpulkan_skrip` | Mengumpulkan `<script>` sesuai urutan dokumen, melewati `type="module"` dan MIME bukan-JS **dengan alasan tercatat** |
| `native::js::LaporanSkrip` | `{ dijalankan, dilewati, galat }` — skrip yang melempar galat tetap dihitung `dijalankan` |
| `Isolat::jalankan_skrip_halaman` | Menjalankan skrip di isolat yang **sama** dengan DOM, jadi mutasi `document.*` benar-benar mengubah pohon yang dilayout |
| `Isolat::kuras_timer` | Menjalankan `setTimeout`/`setInterval` yang sudah jatuh tempo |

Skrip eksternal diambil lewat `Pengambil` yang sama dengan jalur jaringan biasa, jadi
tidak ada klien HTTP kedua yang perlu diaudit terpisah.

Tes yang membuktikan (semua lulus, lihat §5):

- `skrip_sebaris_berjalan_saat_halaman_dibuka`
- `skrip_luar_diambil_lewat_jalur_jaringan_lalu_berjalan`
- `skrip_yang_melempar_galat_tidak_menghentikan_skrip_lain`
- `skrip_bukan_javascript_dilewati_dengan_alasan` (mencakup `type="module"` dan `application/json`)
- `skrip_tanpa_akhir_dibatalkan_bukan_membekukan_tab`
- `skrip_tidak_punya_akses_berkas_atau_modul`

---

## 2. Sebelas API DOM/window yang hilang

| API | Tes |
|---|---|
| `location.href` / `pathname` / `search` / `hash` | `location_membaca_url_halaman` |
| `innerWidth` / `innerHeight` / `outerWidth` / `outerHeight` / `devicePixelRatio` | `inner_width_dan_inner_height_membaca_viewport` |
| `setTimeout` / `clearTimeout` | `settimeout_dijalankan_saat_timer_dikuras`, `clear_timeout_membatalkan_callback` |
| `setInterval` / `clearInterval` | `setinterval_dan_clearinterval_mengatur_antrean` |
| `getComputedStyle` | `get_computed_style_membaca_kaskade_bukan_atribut` |
| `getBoundingClientRect` | `get_bounding_client_rect_memakai_layout` |
| `innerHTML` (getter) | `inner_html_getter_mengembalikan_isi_elemen` |
| `getElementsByTagName` | `get_elements_by_tag_name_mengembalikan_semua` |
| `firstElementChild` / `lastElementChild` | `first_element_child_dan_last_element_child_melewati_teks` |
| `childElementCount` | `first_element_child_dan_last_element_child_melewati_teks` |
| `addEventListener` / `removeEventListener` di `document` | `add_event_listener_di_document_menerima_click_yang_menggelembung` |

Satu catatan jujur: `getComputedStyle` membaca kaskade hasil `css::Kaskade`, bukan
`window.getComputedStyle` penuh. Properti yang tidak dihitung layout tetap kosong.

---

## 3. Jalur produksi, bukan cuma tes

| Berkas | Perubahan |
|---|---|
| `crates/engine/src/native/tab_pool.rs` | `TabPanas.skrip: LaporanSkrip`; `TabPool.viewport` + `dengan_viewport` (bawaan 800x600); `bangun` dipecah jadi `bangun_dengan(..., jalankan_skrip: bool)`; `hidupkan` (rehidrasi cold) mengirim `false` karena snapshot sudah memuat HTML pasca-skrip, menjalankannya ulang akan menduplikasi mutasi |
| `crates/cdp/src/native_wire/state.rs` | `NativeState::bangun` memakai `.dengan_viewport(...)`; helper `laporan_skrip(tab)` menyuntik `{dijalankan,dilewati,galat}` ke hasil `Page.navigate` **dan** `Page.setDocumentContent`; `Runtime.evaluate` menguras timer dan mengembalikan kolom `timer` |
| `crates/engine/src/native/dom.rs` | `Document::dalam_html(id)` untuk `innerHTML` |

Viewport sengaja satu sumber: `VIEWPORT_BAWAAN` di `state.rs` dipakai baik oleh pool
maupun oleh laporan, supaya `getBoundingClientRect` di skrip tidak meleset dari
tangkapan layar tanpa satu pun galat.

---

## 4. `CELAH` sekarang jujur

`crates/engine/src/native/mod.rs`: entri **12 → 13**.

Entri baru menyebut batas eksekusi skrip apa adanya:

- tidak ada `defer` / `async` (urutan dokumen, sinkron)
- tidak ada `document.write`
- tidak ada fase `DOMContentLoaded` / `load` sebagai event
- `type="module"` dilewati (pemuat impor belum ada)
- timer hanya jalan saat mesin memompa (`Runtime.evaluate` atau sesudah navigasi)
- `getComputedStyle` terbatas pada properti hasil kaskade
- `innerText` setara `textContent`
- galat per skrip dikumpulkan, tidak menghentikan skrip lain

Entri `navigator` diperbarui: skrip berjalan sejak W5, sisa celah DOM-nya disebut
eksplisit (`classList`, setter `innerHTML`, `dataset`, `insertAdjacentHTML`, `outerHTML`).

Tes kontrak: `native::celah::celah_eksekusi_skrip_disebut_beserta_batasnya` gagal kalau
entri yang menyebut `<script>` tidak memuat kata "dijalankan"/"berjalan" **dan** ketiga
batas (`module`, `defer`, `timer`) sekaligus.

Komentar menyesatkan di `test_js_runtime_dom_mutations` juga diperbaiki: tes itu memanggil
`Runtime.evaluate` langsung, jadi ia **tidak** membuktikan `<script>` halaman berjalan.

---

## 5. Angka gate

Dijalankan di laptop (operator), bukan build berat. Build berat tetap di GitHub Actions.

| Pemeriksaan | Hasil |
|---|---|
| `cargo fmt --all -- --check` | bersih, exit 0 |
| `cargo test -p engine --lib` | **248 passed**, 0 failed (3,46 s) |
| `cargo test -p cdp` (11 suite) | **181 passed**, 0 failed |
| `cargo test -p engine --lib -- native::js` | **45 passed**, 0 failed (203 filtered) |
| `cargo test -p engine --lib -- native::celah` | **3 passed**, 0 failed |
| `cargo test -p cdp --test native_state` | **27 passed**, 0 failed |
| `bash ~/.opencrabs/projects/oc-sanity.sh` | **22 PASS**, 0 FAIL, RC=0 |

Perubahan: 6 berkas, **+1369 / -16** baris (`crates/engine/src/native/js.rs` sendiri +1070).

---

## 6. Yang belum ditutup (jangan dibaca sebagai selesai)

- Belum ada tes end-to-end dengan halaman nyata (bukan fixture) yang membuktikan skrip
  mengubah **tampilan** hasil paint. Bukti saat ini: unit tes isolat + 2 tes CDP.
- `hidupkan` (rehidrasi dari penyimpanan dingin) sengaja mengirim `jalankan_skrip=false`
  karena snapshot sudah memuat HTML pasca-skrip. Yang ada:
  `tab_dingin_bisa_dihidupkan_dengan_isi_utuh` membuktikan jumlah simpul sama, bukan
  membuktikan mutasi tidak berganda pada halaman yang benar-benar berskrip.
- `document.write` belum ada, jadi halaman lama yang memakainya akan tampil berbeda.
- 15 tes bernama di PRD masih belum ada; `native_99_tabs_budget` yang disebut PRD tidak
  pernah dibuat (kapasitas dibuktikan `native_tab_capacity_cap`).
- `engine::oxi` masih belum terukur.

Kalau salah satu di atas mau diklaim selesai, ia butuh tes yang **gagal sebelum** perubahan.
