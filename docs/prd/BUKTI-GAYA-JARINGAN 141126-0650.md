---
document_type: evidence
project: "OpenCrabs Browser: web-opening engine"
subject: "External stylesheet fetching in the native engine"
date: "2026-11-14"
measured_by: "crates/cdp/examples/ukur_gaya.rs"
---

# Evidence: `<link rel=stylesheet>` fetched from the network

The gap this closes was stated in code, not in a document:
`engine::native::CELAH[0]` said the cascade read only `<style>` blocks and
inline `style`, that `<link rel=stylesheet>` was never fetched, and that
at-rules were recorded and not executed. Until 2026-11-13 a page that carried
its layout in an external stylesheet therefore rendered with no CSS at all,
and the raster was honest about it: it painted unstyled boxes.

This document records what was built, the numbers measured from the same code
path `Page.navigate` uses, and what is still not done. Numbers come from
`cargo run -p cdp --example ukur_gaya -- <url>` on the machine that built the
debug profile, on 2026-11-14 between 06:20 and 06:50 WIB.

## What was built

| Layer | File | What it does now |
|---|---|---|
| Cascade | `crates/engine/src/native/css.rs` | `LembarLuar { href, teks }`, `tautan_eksternal()`, `Kaskade::kumpulkan_dengan()`. An external sheet is placed **at its `<link>` position** in the tree, so a later `<style>` still wins by source order. `rel=alternate`, `disabled` and media other than empty/`all`/`screen` are skipped with the reason recorded |
| Layout | `crates/engine/src/native/layout.rs` | `hitung_dengan(doc, viewport, eksternal)`; `hitung` delegates with an empty slice |
| Paint | `crates/engine/src/native/paint.rs` | `lukis_dengan_lembar(...)`; `lukis_dengan` delegates |
| Tab pool | `crates/engine/src/native/tab_pool.rs` | `TabPanas.lembar` and `Snapshot.lembar`; sheets survive eviction and revival |
| Store | `crates/store/src/schema.rs`, `tab.rs` | `SCHEMA_VERSION = 3`, `MIGRASI_V3` adds `tab_snapshot.sheets_enc BLOB`. The sheet list is sealed with the store key, never plaintext |
| Fetch | `crates/cdp/src/native_wire/jaringan.rs` | `muat_css` / `muat_css_blokir`: `text/css` (or empty mime) only, 1 MB per sheet, 5 s per sheet, 16 sheets per document, 2 MB total |
| Resolve | `crates/cdp/src/native_wire/gaya.rs` | `gaya::ambil(url_akhir, html)`: resolves `href` against the **post-redirect** URL and `<base href>`, fetches all links concurrently, then resolves `@import` recursively (depth 3, cycle set, imported rules placed before the parent's own rules) |

## Measured

Five real pages, one run each. `css` is the sum of the sheet bodies actually
handed to the cascade; `ms` is wall clock on this host, single connection pool,
no warm cache between runs.

| Page | HTML | Sheets | CSS | Fetch + resolve | Notes recorded |
|---|---|---|---|---|---|
| `example.com` | 577 B | 0 | 0 B | 274 ms | 0 (page has no stylesheet) |
| `gnu.org` | 32,352 B | 2 | 22,128 B | 2,209 ms | 1: `media="print"` skipped |
| `developer.mozilla.org/en-US/docs/Web/CSS` | 328,385 B | 16 | 62,374 B | 761 ms | 1: cap of 16 sheets hit |
| `en.wikipedia.org/wiki/Rust_(programming_language)` | 1,065,646 B | 2 | 231,156 B | 2,073 ms | 0 |
| `github.com` | 526,066 B | 15 | 2,008,413 B | 970 ms | 16: 14 links with no `href`, 1 cap of 16, 1 total-byte cap |

The two ends are the useful ones. `example.com` fetches nothing and takes
274 ms, so the feature costs nothing on a page that does not use it. `github.com`
spends 715 ms of a 970 ms load on CSS and still hits the 2 MB total cap: one
stylesheet is dropped with the reason naming the byte budget. A page that
heavy is the case the caps exist for, and the caps fired instead of the daemon
growing without bound.

## Two defects this measurement found, both still open

1. **No de-duplication by URL.** `github.com` links
   `primer-react-css.49293c99b99c0da8.module.css` twice (entries 12 and 14,
   307,764 bytes each). Both are fetched and both are applied. That is 307,764
   wasted bytes, 15% of the CSS on the page, and it is part of why the 2 MB
   cap fired and dropped a *different* sheet. A browser de-duplicates identical
   URLs; this one does not.
2. **The cap is per document, not per page load.** A second navigation of the
   same page re-fetches everything. There is no cache, so the 2 MB budget is
   spent again on every reload. This is acceptable for a first cut and is not
   what the PRD asked for, but it should not be mistaken for caching.

Neither is fixed here. Both are recorded in `PROGRESS.md` under open items
rather than quietly left out of this document.

## Tests that prove it

`cargo test -p engine -p store -p browserd -p cdp` on 2026-11-14: **every
suite green, 0 failed**. The ones that would fail if the feature were reverted:

| Test | What it pins |
|---|---|
| `native::css::lembar_eksternal_dipasang_di_posisi_linknya` | A `<style>` after the `<link>` still wins |
| `native::css::lembar_eksternal_sesudah_style_menang` | The reverse order wins the other way |
| `native::css::tautan_media_print_dan_alternate_dilewati_dengan_alasan` | Skipped links are recorded with `media`/`alternate` in the note |
| `native::css::tautan_tanpa_teks_lembar_tetap_dicatat` | A link with no fetched text is recorded as not applied, not silently applied |
| `native::layout::lembar_eksternal_menentukan_lebar_kotak` | An external `width` changes the measured box |
| `native::paint::warna_dari_lembar_eksternal_terlukis` | An external `color` reaches the raster |
| `native::tab_pool::ganti_dokumen_dengan_menyimpan_lembar_baru` | Sheets travel with the tab |
| `native::tab_pool::lembar_selamat_lewat_penyingkiran_dan_dihidupkan_lagi` | Evict then revive keeps the sheets |
| `store::tab::lembar_gaya_tidak_tersimpan_plaintext` | `sheets_enc` holds no readable CSS |
| `store::schema::database_versi_1_naik_ke_versi_terkini_tanpa_kehilangan_data` | v1 databases migrate to v3 and gain `sheets_enc` |
| `browserd::tabstore::lembar_gaya_ikut_pulang_dari_tab_dingin` | The store adapter round-trips sheets |
| `jaringan::muat_css_menerima_text_css_dan_menolak_html` | A `text/html` body is refused, not parsed as CSS |
| `jaringan::muat_css_membatasi_ukuran_lembar` | The 1 MB per-sheet cap |
| `jaringan::gaya_jaringan_mengambil_tautan_relatif_dan_melewati_media_print` | Relative `href` resolved, `print` skipped |
| `jaringan::gaya_jaringan_menyelesaikan_href_relatif_terhadap_url_akhir_pengalihan` | Resolution uses the post-redirect URL |
| `jaringan::gaya_jaringan_ikut_sampai_ke_layar` | Two rasters of the same document differ when sheets are supplied |
| `jaringan::impor_relatif_diambil_dan_ditaruh_di_depan_aturan_induk` | Imported rules precede the parent's own rules |
| `jaringan::impor_siklus_berhenti_dan_dicatat` | A → B → A stops, "siklus" recorded |
| `jaringan::impor_kedalaman_berlebih_dicatat_bukan_diambil` | Depth 4 of imports is skipped with the reason |
| `jaringan::impor_berkondisi_media_dicatat_tidak_dijalankan` | `@import ... print` is recorded, not inserted |
| `native_foundations::celah_dinyatakan_bukan_disembunyikan` | The closed clause cannot return to `CELAH`, and the narrower at-rule limit must stay |

## What is still not done

`CELAH[0]` was replaced, not deleted. It now says: at-rules other than
`@import` are still not executed. `@media` is recorded and its contents
skipped, so a page that keeps its layout rules inside `@media` renders without
them; `@font-face` is not downloaded, so a webfont falls back to the one
embedded face. The other eleven entries are unchanged.
