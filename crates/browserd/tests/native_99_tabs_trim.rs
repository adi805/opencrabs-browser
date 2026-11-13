//! Sebab regresi PSS: **data yang ditahan** atau **halaman yang tidak
//! dikembalikan allocator**?
//!
//! `docs/prd/BUKTI-PSS-DAEMON 131126-0323.md` mencatat bahwa job CI `budget`
//! naik dari 70.0 MB (hijau, `ae02432`) ke 105.3-110.6 MB (merah,
//! `3614fcd`..`c61cbd0`) setelah dua commit teks. Sebabnya di sana ditulis
//! INFERRED. Uji ini memisahkan dua kemungkinan itu dengan satu angka:
//!
//! 1. buka 99 tab seperti uji budget, baca PSS;
//! 2. minta allocator mengembalikan halaman kosong (`malloc_trim(0)`), baca PSS
//!    lagi.
//!
//! Kalau PSS turun setelah trim, selisihnya adalah halaman bebas yang ditahan
//! allocator, bukan data yang masih dipakai. Kalau tidak turun, data itu memang
//! masih hidup dan harus dicari pemiliknya.
//!
//! Berkas ini sengaja hanya berisi satu fungsi tes, alasannya sama dengan
//! `native_99_tabs_budget.rs`: tes kedua di berkas yang sama akan menambah PSS
//! proses sehingga angka yang dilaporkan bukan lagi angka beban.

use engine::native::tab_pool::{MemoryColdStore, TabPool};
use std::fmt::Write as _;

const JUMLAH_DIV: usize = 1500;

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

/// `malloc_trim` hanya ada di glibc. Di luar itu uji ini melaporkan bahwa
/// pemisahan tidak bisa dilakukan, bukan berpura-pura sudah mengukurnya.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn trim() -> bool {
    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    // SAFETY: `malloc_trim` dari glibc, tanpa argumen pointer, dan hanya
    // mengembalikan halaman bebas ke kernel. Dipanggil saat tidak ada alokasi
    // lain yang sedang berjalan di proses ini.
    unsafe { malloc_trim(0) == 1 }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn trim() -> bool {
    false
}

#[test]
fn penyingkiran_99_tab_dipisahkan_dari_trim_allocator() {
    let html = dokumen_beban();
    let mut pool = TabPool::new(MemoryColdStore::default(), cdp::native_wire::MAKS_TAB);
    let pss_awal = pss_mb();

    for nomor in 0..cdp::native_wire::MAKS_TAB {
        pool.buka(&format!("https://contoh.test/tab-{nomor}"), &html)
            .unwrap_or_else(|err| panic!("tab ke-{} ditolak: {err:?}", nomor + 1));
    }

    let pss_sebelum = pss_mb();
    let dikembalikan = trim();
    let pss_sesudah = pss_mb();

    println!(
        "awal {pss_awal:.1} MB -> 99 tab {pss_sebelum:.1} MB (+{:.1}) -> trim {pss_sesudah:.1} MB \
         (allocator mengembalikan {:.1} MB, trim={dikembalikan})",
        pss_sebelum - pss_awal,
        pss_sebelum - pss_sesudah
    );
    println!(
        "{} panas + {} dingin",
        pool.jumlah_panas(),
        pool.jumlah_dingin().unwrap_or(0)
    );

    assert_eq!(pool.jumlah_panas(), 25, "tab panas tepat di pagu");
    assert!(
        dikembalikan,
        "malloc_trim harus tersedia di glibc Linux untuk uji ini"
    );
}
