# STATE: mesin browser Rust, plan 16 unit

Terakhir diperbarui: 2026-11-14 15:52 WIB.

## Selesai (ter-commit)

| Unit | Commit | Isi |
|---|---|---|
| W4a..W8 | `aec71d9`, `a3207e6` | DOM arena, JS bridge, pointer events, peristiwa CDP |
| paint | `b7db8b1` | gradien linear/radial, `border: solid` + radius, `<img>` PNG/JPEG, bold sintetis, `justify`, `overflow: hidden` |
| teks A | `292bb4e` | rantai dua face per aksara + shaping `rustybuzz` |
| teks B | `5ab621e` | `@font-face` diunduh, dipasang `KumpulanFont`, dipakai pengukur DAN pelukis |
| `@media` | (commit unit 10) | prelude dievaluasi terhadap `KonteksMedia` (width/height + `min-`/`max-`/`device-`, `orientation`, `prefers-color-scheme`, `prefers-reduced-motion`); blok yang cocok masuk kaskade di posisinya. Kueri ber-`not` atau berfitur di luar daftar dilewati dengan alasan, bukan diterapkan dengan arti karangan |
| `webp` | (commit unit 11) | `paint::Format::Webp` lewat `image-webp` 0.2.4 (VP8L lossless, Rust murni); `quality` dicatat diabaikan karena encoder-nya lossless; format di luar png/jpeg/webp tetap `-32602` |

Task 9 (font) **tuntas**: ketiga bagiannya (`@font-face` unduh, fallback per aksara, shaping) sudah ada tesnya masing-masing.

## Sisa plan

| # | Unit | Status |
|---|---|---|
| 10 | `@media` benar-benar dijalankan | **selesai**, tes `native::css::tests::test_media_query_dievaluasi_terhadap_viewport` |
| 11 | encoder `webp` | **selesai**, tes `native::paint::tests::webp_lossless_bolak_balik` + `cdp` tes WebP |
| 12 | traversal `iframe` + closed shadow root | belum |
| 13 | `cargo xtask e2e` + census proses di `xtask check` | belum |
| 14 | leak-check daemon native | belum |
| 15 | gladi bersih backup ke host kedua | belum |
| 16 | hapus `adi805/opencrabs-web` permanen | belum (butuh `gh auth refresh -s delete_repo`) |

## Celah yang masih dinyatakan `CELAH`

- `@media` sudah dijalankan; yang tersisa `@supports` (dicatat di `diabaikan`, tidak dijalankan).
- `font-weight`/`font-style` tidak memilih antar face sekeluarga; aksara di luar face tertanam dan unduhan (CJK, Thai, emoji berwarna) tetap `.notdef`; byte webfont tidak ikut snapshot tab dingin.
- `Input.dispatchKeyEvent` dan `Input.dispatchTouchEvent` masih method-not-found.
- `box-shadow`/`text-shadow`, border per sisi, `border-style` selain `solid`, `object-fit` selain `fill`, face bold asli.
- Encoder `webp` ada tetapi **lossless** (VP8L): `quality` tidak berpengaruh, jadi berkas WebP dari mesin ini bisa lebih besar daripada Chrome pada halaman berfoto. Format lain (`avif`) tetap `-32602`.

## Catatan lingkungan

- `export PATH="$HOME/.cargo/bin:$PATH"` wajib (rustc 1.98.1); target dir `/mnt/hdd/OpenCrabs/build/cargo`.
- Build/test berat: jalankan detached (`setsid nohup ... > /tmp/x.log`), jangan blok tool.
- `tail` tidak ada di host ini; pakai `sed -n`.
- Berkas asing yang JANGAN di-commit: `docs/prd/PRD-MESIN-BUKA-WEB 041126-0631.md.bak44`.
- Dua `cargo test --workspace` paralel saling kunci di build dir (`Blocking waiting for file lock`); satu rantai saja per pohon.
