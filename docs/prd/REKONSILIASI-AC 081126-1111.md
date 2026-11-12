# Rekonsiliasi AC PRD v0.2 dengan bukti nyata

Cap waktu: 08 Nov 2026 11:11 WIB. Sumber kontrak: `docs/prd/PRD-MESIN-RUST 071126-1413.md` baris 135-154.

Dokumen ini tidak menilai kualitas kode. Isinya satu hal: untuk tiap AC, apakah perintah yang PRD sebut **benar-benar ada dan lulus**, atau tidak.

## Cara mengukurnya

1. Seluruh workspace dijalankan sekali: `cargo test --workspace`, 44 binary tes, **557 lulus, 0 gagal, 0 panik**, `EXIT:0`.
2. Nama tes yang PRD tuntut dicocokkan ke daftar nama yang benar-benar jalan di log itu, bukan ke ingatan.
3. Untuk AC yang bentuknya perintah luar (`curl`, skrip), dijalankan proses nyata.

Perintah pencocokan:

```
for t in test_dom_parsing_dan_styling test_malformed_html_recovery ... ; do
  grep -q "test $t \.\.\. ok" ws-test.log && echo "ADA $t" || echo "HILANG $t"
done
```

## Hasil

### Lulus dengan nama tes yang persis

| AC | Perintah PRD | Bukti |
|---|---|---|
| AC-001 | `cargo test -p engine --lib test_dom_parsing_dan_styling` | target ada, `1 passed; 0 failed` (2026-11-12). Gaya terhitung datang dari `<style>` lewat `native::css::Kaskade` dan warisan lewat `native::gaya::PohonGaya`, bukan dari atribut `style` |
| AC-010 | `cargo test -p browserd --test native_tab_capacity_cap` | target ada, lulus di 557 tes |
| AC-019 | `cargo test -p browserd --test native_action_receipt_diff` | target ada, lulus |
| AC-020 | `cargo test -p browserd --test native_no_observable_change` | target ada, lulus |

Empat dari dua puluh pada 2026-11-12, saat AC-001 mendapat tes bernama persis. Itu angka yang jujur, bukan yang enak dibaca.

### Nama tesnya tidak ada (13 AC)

Dicari di seluruh log 557 tes, nol kecocokan:

`test_malformed_html_recovery`, `test_js_runtime_dom_mutations`, `test_js_timeout_termination`, `test_screenshot_png_magic_bytes`, `test_axtree_hierarchy`, `test_mouse_click_dispatch`, `test_overlay_obstruction_detection`, `test_tab_rehydration_latency`, `test_suspended_tab_memory_release`, `test_iframe_element_traversal`, `test_shadow_root_piercing`, `test_navigator_webdriver_false`, `test_no_automation_globals`.

`test_dom_parsing_dan_styling` left this list on 2026-11-12 with the receipt in the table above; the test exists at `crates/engine/src/native/mod.rs`.

`native_99_tabs_budget` left this list on 2026-11-12: `crates/browserd/tests/native_99_tabs_budget.rs` now exists and passes with 49.4 MB PSS over 99 tabs of 3006-node documents (25 hot + 74 cold) against the 100 MB ceiling, so AC-009 moves from absent to met at the acceptance workload.

### Yang punya cakupan lain, bukan yang diminta

| AC | Yang PRD minta | Yang benar-benar ada | Selisih |
|---|---|---|---|
| AC-002 | HTML rusak tidak panik | `native::dom::tests::html_rusak_tetap_diurai` | **setara**, hanya beda nama |
| AC-003 | aritmetika **dan** mutasi DOM dari skrip | `native::js::tests::{mengevaluasi_ekspresi,state_yang_sama_terlihat_antar_panggilan,console_log_tertangkap}` | mutasi DOM dari dalam skrip belum diuji |
| AC-004 | `while(true){}` dihentikan | `native::js::tests::pagu_memori_menghentikan_alokasi_besar` | pagu memori ada, batas waktu/gas tidak ada |

### Bergantung proses hidup

| AC | Perintah | Status |
|---|---|---|
| AC-013 | `curl -s http://127.0.0.1:9224/json/version` | daemon yang hidup di 9224 menjawab `HeadlessChrome/156`, jadi perintah PRD apa adanya **belum** memenuhi AC di port itu. Instance native terpisah di 9234 sekarang **lulus** dengan biner hasil GitHub Actions: `{"Browser":"RustNativeEngine/0.2.0",...}`, plus satu putaran CDP penuh yang keluar 0. Bukti: `docs/prd/BUKTI-NATIVE-CDP 081126-1124.md` |
| AC-014 | `bash ~/.opencrabs/projects/oc-browser-leak-check.sh` | skrip ada (1062 byte). Setelah SIGTERM: pid hilang, 0 listener di 9234, 0 proses instance, jumlah `chrome` host 53 sebelum dan 53 sesudah (delta 0), log `semua mesin dimatikan`. Skripnya sendiri keluar **1** karena satu orphan yang sudah ada di baseline (pid `598510`, lahir 2026-11-07 21:46, reparent ke `systemd --user`), bukan dari run ini |

## Cacat yang ditemukan saat verifikasi

Daemon menolak menyalakan mesin native dengan:

```
browserd: [policy] tidak sah untuk mesin yang terpasang (rust-native):
  - nama mesin 'rust-native' tidak dikenal
```

Sebabnya `EngineKind` di `crates/policy/src/select.rs` hanya punya `Shell`, `Full`, `Oxi`. `Catalog::dari_mesin` membuang id yang tidak dikenali, jadi katalog kosong dan validasi policy gagal walau mesinnya sudah menyala. Diperbaiki di commit `e618729` dengan nilai kemampuan yang mengikuti bukti, bukan harapan:

| kemampuan | nilai | alasan |
|---|---|---|
| `script` | true | isolat rquickjs menjalankan ekspresi, menjaga state, menangkap console, menolak alokasi besar |
| `layout` | false | taffy menghitung kotak, tapi `Page.printToPDF` belum ada; field ini menuntut dua-duanya |
| `cookies` | false | belum ada cookie jar, nol tes |
| `input` | false | `hands` belum dibangun |

Varian `Native` sengaja **tidak** dimasukkan ke `choose_with_layout` / `cheapest_script`: mesin ini belum punya tumpukan jaringan, jadi memilihnya otomatis berarti menukar mesin yang bisa membuka situs dengan mesin yang tidak bisa.

## Batas dokumen ini

- Verifikasi AC-013/AC-014 dengan biner hasil GitHub Actions: run `42445740212` gagal di langkah paket (biner jadi, `target/dev/` dicari padahal cargo menulis `target/debug/`), diperbaiki `2633095`, run `42445976171` hijau penuh. Bukti di `docs/prd/BUKTI-NATIVE-CDP 081126-1124.md`.
- Angka 557 berasal dari satu kali jalan penuh di laptop, bukan dari CI. CI run `42441056817` (head `2aece46`) hijau untuk fmt+clippy+test, tapi angka per-tes di sana tidak dibaca.
- Yang masih belum diperiksa: perilaku di bawah beban, ketahanan anti-bot di host nyata, dan **PSS** (yang diukur pada 8 Nov adalah RSS: 20 MB baseline, 37.6 MB untuk 99 tab berisi dokumen 403 node, 0.08 MB/tab marginal; tab `about:blank` 99 buah = 30 MB). RSS satu proses native sudah mewakili mesinnya karena mesinnya in-process, tapi PSS akan berbeda begitu ada proses anak.