//! AC-009 dan NFR-001: 99 tab terkelola di bawah pagu 100 MB PSS.
//!
//! Tes ini **mengukur**, bukan memperkirakan. Pagu 100 MB dan angka 3,10 MB per
//! tab selama ini berasal dari harness di luar repo dengan dokumen 3006 node
//! (`docs/prd/BUKTI-BUDGET 071126-1413.md`), sedangkan probe daemon native cuma
//! punya RSS pada dokumen 403 node. Keduanya tidak boleh dibandingkan langsung,
//! jadi pagu itu belum pernah diuji terhadap mesin ini.
//!
//! Yang diukur di sini adalah PSS proses tes sendiri lewat
//! `/proc/self/smaps_rollup`: 99 tab dibuka berurutan dengan dokumen seukuran
//! beban harness (3006 node), `TabPool` menyingkirkan LRU setiap kali tab panas
//! penuh, lalu PSS total dibaca dan dibandingkan dengan pagu.
//!
//! Berkas ini sengaja hanya berisi **satu** fungsi tes. `cargo test` menjalankan
//! tes dalam satu proses secara paralel, dan tes kedua di berkas yang sama akan
//! menambah PSS proses sehingga angka yang dilaporkan bukan lagi angka beban
//! yang diukur. Kalau butuh tes lain, taruh di berkas terpisah.

use engine::native::tab_pool::{
    BASELINE_MB, MemoryColdStore, PAGU_MB, PSS_PER_TAB_MB, TabPool, tab_panas_maksimum,
};
use std::fmt::Write as _;

/// Jumlah `div` di dokumen beban. Satu `div` berisi teks menghasilkan dua node
/// (elemen dan teks), jadi 1500 `div` mendekati 3006 node seperti dokumen
/// harness yang melahirkan angka 3,10 MB.
const JUMLAH_DIV: usize = 1500;

/// Batas tab yang dijanjikan PRD, dipakai apa adanya dari `cdp`.
const MAKS_TAB: usize = cdp::native_wire::MAKS_TAB;

fn dokumen_beban() -> String {
    let mut s = String::with_capacity(JUMLAH_DIV * 64);
    s.push_str("<!doctype html><html><head><title>beban</title></head><body>");
    for i in 0..JUMLAH_DIV {
        let _ = write!(
            s,
            "<div id=\"k{i}\" class=\"baris\" style=\"height:20px\">isi baris {i}</div>"
        );
    }
    s.push_str("</body></html>");
    s
}

/// PSS proses ini dalam MB, dibaca dari `/proc/self/smaps_rollup`.
///
/// `smaps_rollup` memuat satu baris `Pss:` berisi total PSS seluruh pemetaan,
/// dalam kB. Dibaca per panggilan, bukan disimpan, supaya angka yang dilaporkan
/// adalah angka saat itu.
fn pss_mb() -> f64 {
    let teks = std::fs::read_to_string("/proc/self/smaps_rollup")
        .expect("smaps_rollup harus terbaca di Linux");
    for baris in teks.lines() {
        if let Some(sisa) = baris.strip_prefix("Pss:") {
            let kb: f64 = sisa
                .split_whitespace()
                .next()
                .expect("nilai Pss")
                .parse()
                .expect("Pss dalam kB");
            return kb / 1024.0;
        }
    }
    panic!("baris Pss tidak ditemukan di smaps_rollup");
}

#[test]
fn sembilan_puluh_sembilan_tab_di_bawah_seratus_mb_pss() {
    let maks_panas = tab_panas_maksimum(BASELINE_MB);
    assert_eq!(maks_panas, 25, "floor((100 - 20) / 3,10) = 25");

    let html = dokumen_beban();
    let node = engine::native::Document::parse_html(&html)
        .expect("dokumen beban harus terparse")
        .jumlah_node();
    assert!(
        node >= 3000,
        "beban harus seukuran harness 3006 node, dapat {node} node"
    );

    let pss_awal = pss_mb();
    let mut pool = TabPool::new(MemoryColdStore::default(), MAKS_TAB);
    let mut panas_tertinggi = 0usize;

    for nomor in 0..MAKS_TAB {
        pool.buka(&format!("https://contoh.test/tab-{nomor}"), &html)
            .unwrap_or_else(|err| panic!("tab ke-{} ditolak: {err:?}", nomor + 1));
        panas_tertinggi = panas_tertinggi.max(pool.jumlah_panas());
        assert!(
            pool.jumlah_panas() <= maks_panas,
            "setelah tab ke-{} ada {} tab panas, pagu {}",
            nomor + 1,
            pool.jumlah_panas(),
            maks_panas
        );
    }

    let pss_akhir = pss_mb();
    let dingin = pool.jumlah_dingin().expect("jumlah tab dingin");

    assert_eq!(pool.jumlah_dialamat(), MAKS_TAB, "99 tab harus terkelola");
    assert_eq!(pool.jumlah_panas(), maks_panas, "tab panas tepat di pagu");
    assert_eq!(dingin, MAKS_TAB - maks_panas, "sisanya dingin");
    assert_eq!(
        panas_tertinggi, maks_panas,
        "tab panas tidak pernah melewati pagu"
    );

    let laporan = format!(
        "PSS {pss_akhir:.1} MB (awal {pss_awal:.1} MB, selisih {:.1} MB) untuk {} tab, \
         {} panas + {} dingin, dokumen {node} node, perkiraan model {:.1} MB",
        pss_akhir - pss_awal,
        pool.jumlah_dialamat(),
        pool.jumlah_panas(),
        dingin,
        BASELINE_MB + pool.jumlah_panas() as f64 * PSS_PER_TAB_MB
    );
    println!("{laporan}");
    assert!(
        pss_akhir <= PAGU_MB,
        "pagu {PAGU_MB} MB terlampaui: {laporan}"
    );
}
