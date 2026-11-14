# STATE: mesin browser Rust, plan 16 unit

Terakhir diperbarui: 2026-11-14 17:34 WIB.

## Selesai (ter-commit)

| Unit | Commit | Isi |
|---|---|---|
| W4a..W8 | `aec71d9`, `a3207e6` | DOM arena, JS bridge, pointer events, peristiwa CDP |
| paint | `b7db8b1` | gradien linear/radial, `border: solid` + radius, `<img>` PNG/JPEG, bold sintetis, `justify`, `overflow: hidden` |
| teks A | `292bb4e` | rantai dua face per aksara + shaping `rustybuzz` |
| teks B | `5ab621e` | `@font-face` diunduh, dipasang `KumpulanFont`, dipakai pengukur DAN pelukis |
| `@media` | (commit unit 10) | prelude dievaluasi terhadap `KonteksMedia` (width/height + `min-`/`max-`/`device-`, `orientation`, `prefers-color-scheme`, `prefers-reduced-motion`); blok yang cocok masuk kaskade di posisinya. Kueri ber-`not` atau berfitur di luar daftar dilewati dengan alasan, bukan diterapkan dengan arti karangan |
| iframe + shadow | (commit unit 12) | `Document` punya `bayang`/`bingkai`, `anak_tata` jadi satu jalur anak untuk layout, raster, dan AX; `selectors.rs` dipecah jadi `pilih` (otomasi, menembus) dan `pilih_terang` (skrip halaman, berhenti di batas pohon). `cdp::native_wire::bingkai` mengambil dokumen anak same-origin di jalur `Page.navigate`, `bingkaiCatatan` menjelaskan yang dilewati. `DOM.getDocument` mengirim `shadowRoots` dan dokumen anak sebagai satu node `#document` |
| `webp` | (commit unit 11) | `paint::Format::Webp` lewat `image-webp` 0.2.4 (VP8L lossless, Rust murni); `quality` dicatat diabaikan karena encoder-nya lossless; format di luar png/jpeg/webp tetap `-32602` |

Task 9 (font) **tuntas**: ketiga bagiannya (`@font-face` unduh, fallback per aksara, shaping) sudah ada tesnya masing-masing.

## Sisa plan

| # | Unit | Status |
|---|---|---|
| 10 | `@media` benar-benar dijalankan | **selesai**, tes `native::css::tests::test_media_query_dievaluasi_terhadap_viewport` |
| 11 | encoder `webp` | **selesai**, tes `native::paint::tests::webp_lossless_bolak_balik` + `cdp` tes WebP |
| 12 | traversal `iframe` + closed shadow root | **selesai**, tes `native::dom::tests::bingkai_iframe_menggeser_nomor_tanpa_menggeser_nomor_lama`, `shadow_root_tidak_masuk_pohon_terang_tapi_tercapai_otomasi`, `cdp` tes `iframe_same_origin_dimuat_oleh_navigasi` + `shadow_root_tertutup_terbaca_otomasi_tapi_tidak_oleh_skrip` |
| 13 | `cargo xtask e2e` + census proses di `xtask check` | **selesai**, `xtask/src/e2e.rs` jalan tanpa Chrome dan menghitung pid sebelum/sesudah |
| 14 | leak-check daemon native | **selesai**, skrip sekarang mengenali `browserd` ber-penanda `/tmp/ocb-` dan `--self-test` membuktikan pemeriksanya bisa gagal |
| 15 | gladi bersih backup ke host kedua | **selesai**, drill ke `joyboy` dengan 5 cookie dan sha256 identik (`docs/prd/BUKTI-DRILL-HOST2 141126-1650.md`) |
| 16 | hapus `adi805/opencrabs-web` permanen | **selesai 2026-11-14**, dua backup terverifikasi lalu `gh repo view` exit 1 dan API 404 |

## Celah yang masih dinyatakan `CELAH`

- `@media` sudah dijalankan; yang tersisa `@supports` (dicatat di `diabaikan`, tidak dijalankan).
- `font-weight`/`font-style` tidak memilih antar face sekeluarga; aksara di luar face tertanam dan unduhan (CJK, Thai, emoji berwarna) tetap `.notdef`; byte webfont tidak ikut snapshot tab dingin.
- `Input.dispatchKeyEvent` dan `Input.dispatchTouchEvent` masih method-not-found.
- `box-shadow`/`text-shadow`, border per sisi, `border-style` selain `solid`, `object-fit` selain `fill`, face bold asli.
- Encoder `webp` ada tetapi **lossless** (VP8L): `quality` tidak berpengaruh, jadi berkas WebP dari mesin ini bisa lebih besar daripada Chrome pada halaman berfoto. Format lain (`avif`) tetap `-32602`.
- Iframe: **satu tingkat** (iframe di dalam iframe tidak diambil), hanya same-origin, maksimal 8 per dokumen / 4 MiB per dokumen / 12 MiB total / 8 detik. `sandbox`, `allow`, dan `csp` tidak dibaca. `contentDocument`/`contentWindow` tidak ada di isolat.
- Shadow: `closed` root tidak terlihat skrip halaman dan itu memang spesifikasinya; yang belum `<slot>`/`assignedNodes`, `:host`/`::slotted`/`:host-context`, dan `elementFromPoint` belum menembus bayangan.

## Catatan lingkungan

- `export PATH="$HOME/.cargo/bin:$PATH"` wajib (rustc 1.98.1); target dir `/mnt/hdd/OpenCrabs/build/cargo`.
- Build/test berat: jalankan detached (`setsid nohup ... > /tmp/x.log`), jangan blok tool.
- `tail` tidak ada di host ini; pakai `sed -n`.
- Berkas asing yang JANGAN di-commit: `docs/prd/PRD-MESIN-BUKA-WEB 041126-0631.md.bak44`.
- Dua `cargo test --workspace` paralel saling kunci di build dir (`Blocking waiting for file lock`); satu rantai saja per pohon.
