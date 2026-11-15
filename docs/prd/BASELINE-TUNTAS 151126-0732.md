# BASELINE TUNTAS 151126-0732

Inventaris terukur sebelum menyentuh sisa pekerjaan. Semua angka di bawah dibaca
dari keluaran perintah pada 15 November 2026, 07:05-07:32 WIB. Yang tidak diukur
disebut "belum diukur", bukan dikira-kira.

## Pohon yang diukur

| Hal | Nilai |
|---|---|
| HEAD | `48d8372 fix(cdp): gate navigation and sub-resources through the network policy` |
| Berkas berubah belum commit | 6 (`js.rs`, `tab_pool.rs`, `state.rs`, `native_wire/mod.rs`, `tests/native_fetch.rs`, PRD v0.3) |
| Berkas untracked | 1 (`PRD-MESIN-BUKA-WEB 041126-0631.md.bak44`, jangan di-stage) |
| Host | laptop, target-dir `/mnt/hdd/OpenCrabs/build/cargo` |
| Jam ukur | 07:05 (check), 07:09 (engine js), 07:29:55 (workspace test selesai) |

## Probe

| # | Yang diukur | Perintah | Hasil | Bukti |
|---|---|---|---|---|
| 1 | Kompilasi seluruh workspace | `cargo check --workspace --all-targets` | `Finished dev` 38,33 s, 4 crate, nol `error` | `/tmp/ocb-tuntas/check1.log` |
| 2 | Test seluruh workspace | `cargo test --workspace --no-fail-fast` | **867 passed, 0 failed, 0 ignored**, 55 baris `test result` | `/tmp/ocb-tuntas/ws-test3.log` |
| 3 | Unit crate `engine` | (bagian dari #2) | 264 passed, 0 failed | `ws-test3.log:1059` |
| 4 | Unit crate `cdp` | (bagian dari #2) | 82 passed, 0 failed | `ws-test3.log:583` |
| 5 | Permukaan JS isolat | `cargo test -p engine --lib native::js` | **60 passed, 0 failed**, 204 filtered | `/tmp/ocb-tuntas/js-test2.log` |
| 6 | `fetch` lewat CDP | (bagian dari #2) `crates/cdp/tests/native_fetch.rs` | 3 passed, 0 failed, termasuk `daftar_tolak_menang_lewat_runtime_evaluate` | `ws-test3.log:687` |
| 7 | Daemon hidup? | `ss -ltnp` lalu saring `:92xx` dan `:93xx` | 0 listener | keluaran kosong |
| 8 | Skrip bukti AC-010 | `ls /tmp/ocb-audit/promise.py` | tidak ada | keluaran `ls` tanpa berkas itu |
| 9 | Skrip bukti AC-012 | `ls /tmp/ocb-audit/timer.py` | tidak ada | idem |
| 10 | Skrip bukti AC-014/AC-015 | `ls /tmp/ocb-audit/p95.py` | tidak ada | idem |
| 11 | Skrip bukti AC-017 | `ls /tmp/ocb-audit/gate-audit.py` | tidak ada | idem |
| 12 | Skrip bukti AC-009 | pemeriksa kesegaran binary | tidak ada di pohon | `ls` tanpa berkas itu |

## Status per FR/AC

| ID | Kriteria | Status hari ini | Bukti atau alasan |
|---|---|---|---|
| FR-009 / AC-010 | `fetch` mengembalikan Promise asli, `.then` dan `await` setara | kode + tes lulus, **bukti AC belum ada** | `js.rs:1258` (Promise asli), `js.rs:1807` (batas microtask); tes `fetch_mengembalikan_promise_dengan_then` dan `await_fetch_selesai_dalam_satu_evaluasi` lulus (#5). Probe CDP mentah (AC-010) belum dijalankan |
| FR-011 / AC-012 | Timer jatuh tempo tanpa pemicu klien | kode ada, **AC belum diukur** | pemompa 20 ms `native_wire/mod.rs:177`, `kuras_timer_semua` `state.rs:315`; tes `settimeout_dijalankan_saat_timer_dikuras` lulus. Pasangan (penanda berubah, kolom `timer` = 0) belum diukur |
| FR-013 / AC-022 | `window.addEventListener`, `readyState`, `DOMContentLoaded` lalu `load` | kode + tes lulus, **bukti AC belum ada** | `js.rs:2140` `selesaikan_pemuatan`; tes `window_add_event_listener_tidak_lagi_galat`, `ready_state_loading_lalu_complete`, `dom_content_loaded_dan_load_dipanggil_berurutan` lulus (#5) |
| G10-A | `matches`, `closest`, sibling, `ownerDocument`, `isConnected`, `classList`, `dataset` | kode + tes lulus | 5 tes terkait lulus (#5) |
| G10-B | setter `innerHTML`, `outerHTML`, `insertAdjacentHTML`, `replaceChildren`, `cloneNode` | kode + tes lulus | 4 tes terkait lulus (#5) |
| FR-010 / AC-011 | Parameter CDP tak dikenal ditolak bernama | belum diukur | tidak ada probe di pohon |
| FR-012 / AC-013 | Log tanpa nilai kosong | belum diukur | tidak ada probe di pohon |
| NFR-001 / AC-014 | p95 tangkapan 1.206 simpul < 4.000 ms | belum diukur | `p95.py` tidak ada |
| NFR-002 / AC-015 | p95 `/json/list` < 50 ms saat tangkapan | belum diukur | `p95.py` tidak ada |
| NFR-004 / AC-017 | Nol jalur jaringan tanpa gerbang | belum diukur | `gate-audit.py` tidak ada |
| AC-009 | Pemeriksa kesegaran binary gagal saat binary basi | belum ada alatnya | tidak ada berkas pemeriksa |
| AC-021 | `cargo test --workspace` nol gagal pada pohon bersih | angka nol gagal **ada**, pohon belum bersih | #2: 867 passed / 0 failed; tetapi 6 berkas belum di-commit, jadi syarat "pohon kerja bersih" belum terpenuhi |

## Pertentangan CELAH vs kode hari ini

`CELAH` di `crates/engine/src/native/mod.rs` adalah kontrak: satu entri keluar
hanya bersama tes yang **gagal sebelum** kemampuan itu ada. Tiga entri sudah
bertentangan dengan pohon ini.

| Baris | Klaim CELAH | Keadaan kode sekarang |
|---|---|---|
| 118 | "Jawabannya objek biasa, bukan `Promise`" | `js.rs:1258` mengembalikan Promise asli dan menguras microtask; tes `.then` lulus |
| 168 | "tidak ada fase `DOMContentLoaded`/`load`" | `js.rs:2140` `selesaikan_pemuatan`; tes urutan DCL lalu load lulus |
| 172 | "Timer hanya berjalan saat mesin memompa" | pemompa 20 ms `native_wire/mod.rs:177` |

Entri DOM (baris 135) sudah diperbarui untuk G10-A/G10-B, jadi yang tersisa pada
baris itu hanya kemampuan yang memang belum ada (`NodeList` hidup,
`document.write`, CSSOM tulis).

## Yang belum diukur sama sekali

- p95 sepuluh tangkapan berturut-turut pada halaman 1.206 simpul (AC-014).
- p95 sepuluh `GET /json/list` selama tangkapan berjalan (AC-015).
- Penanda timer pada halaman menganggur 1 detik beserta kolom `timer` (AC-012).
- `typeof fetch(...).then` dan kesetaraan `await` lewat CDP mentah (AC-010).
- Audit mekanis pemanggil pembuka koneksi (AC-017).
- Kesegaran binary release terhadap commit terakhir (AC-009).

## Konsekuensi untuk rencana

Baseline ini menutup Task #1. Pekerjaan yang benar-benar tersisa: verifikasi
G10 dengan tes yang merah-sebelum-hijau, penyelarasan `CELAH` (3 baris di atas),
pembuatan skrip bukti AC-010/012/014/015/017/009, pengukuran p95, lalu commit
bertahap sampai pohon bersih sehingga AC-021 bisa diklaim dengan bukti.
