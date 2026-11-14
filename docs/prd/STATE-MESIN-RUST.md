# STATE: mesin browser Rust, plan 16 unit

Terakhir diperbarui: 2026-11-14 14:55 WIB.

## Selesai (ter-commit)

- W4a..W8, paint (gambar/gradien/border/radius/bold/justify/overflow), CDP events.

## Selesai, BELUM ter-commit

Task 9 bagian **fallback per aksara + shaping**:

| Berkas | Isi |
|---|---|
| `Cargo.toml`, `crates/engine/Cargo.toml`, `Cargo.lock` | `rustybuzz = "0.20"` (MIT, sudah ada di allow-list deny.toml) |
| `crates/engine/assets/NotoSansDevanagari-Regular.ttf` | face kedua, upem 1000, 229 336 byte, OFL-1.1 |
| `crates/engine/assets/OFL-1.1.txt` | teks lisensi, disalin dari `fonts-noto-core` Debian |
| `crates/engine/assets/README.md` | bagian face kedua |
| `crates/engine/src/native/teks.rs` | rantai `TERTANAM`, `bentuk()` + `Glyph{face,id,x_advance,x_offset,y_offset,cluster}`, `pilih()`, `skala_face()` per face, `lebar` dari hasil shaping; 16 tes |
| `crates/engine/src/native/paint.rs` | `lukis_teks` memakai `bentuk()`, skala per glyph, `x_offset`/`y_offset` HarfBuzz, spasi-justify dikenali dari `cluster`; tes `aksara_dari_face_kedua_dilukis_bukan_notdef` |
| `crates/engine/src/native/mod.rs` | entri CELAH teks ditulis ulang (shaping + rantai face ADA; sisa: `@font-face`, face per bobot, CJK/Thai/emoji) |
| `crates/engine/tests/native_foundations.rs` | guard: `@font-face tidak diunduh` wajib ada; `pengukuran teks hanya satu face` + `tidak ada shaping` dilarang kembali |
| `docs/prd/PRD-MESIN-RUST 071126-1413.md` | baris milestone 2 diperbarui |

Bukti: `cargo test -p engine --lib native::teks` 16 passed 0 failed; `cargo test -p engine --lib` 210 passed; `cargo xtask check` -> fmt ok / clippy ok / test ok / EXIT:0.

## BELUM dikerjakan di task 9

**`@font-face` belum diunduh.** Rencana yang sudah dipetakan:

1. `native::css`: urai blok `@font-face` jadi `{keluarga, src url, tebal, gaya}` (sekarang at-rule cuma dicatat lalu dilewati).
2. `cdp::native_wire::gaya`: ambil byte font untuk tiap `src` dengan batas byte/waktu/jumlah, lewat `jaringan`.
3. `engine`: `KumpulanFont` (keluarga -> byte, `Arc`), `teks::bentuk_dengan`/`ukuran_dengan`/`bungkus_dengan` yang memilih keluarga dari `font-family` sebelum jatuh ke rantai tertanam.
4. `native::gaya`: urai `font-family` (waris) jadi `keluarga_font: Vec<String>`.
5. `layout` + `paint`: teruskan `Arc<KumpulanFont>` (KonteksTeks di taffy menyimpan Arc, bukan referensi, supaya tetap `Clone + 'static`).
6. `cdp::native_wire::state`: rakit `KumpulanFont` dari hasil ambil, serahkan ke `lukis_dengan_sumber`.
7. Tes: face "unduhan" (pakai byte Noto Devanagari sebagai fixture) dipakai saat `font-family` menunjuknya, dan tidak dipakai saat tidak disebut.

## Catatan lingkungan

- `export PATH="$HOME/.cargo/bin:$PATH"` wajib (rustc 1.98.1); target dir `/mnt/hdd/OpenCrabs/build/cargo`.
- Build/test berat: jalankan detached (`setsid nohup ... > /tmp/x.log`), jangan blok tool.
- `tail` tidak ada di host ini; pakai `sed -n`.
- Berkas asing yang JANGAN di-commit: `docs/prd/PRD-MESIN-BUKA-WEB 041126-0631.md.bak44`.
