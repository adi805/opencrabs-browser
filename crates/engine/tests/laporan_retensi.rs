//! Berapa byte yang **ditahan** satu `LaporanLayout` setelah layout selesai?
//!
//! Uji ini tidak mengukur PSS dan tidak menjalankan daemon. Yang diukur adalah
//! isi laporan itu sendiri: kapasitas tiap peta, ukuran tiap entri, dan
//! kapasitas tiap `String` yang ikut tersimpan. Angkanya deterministik, jadi
//! bisa dipakai sebagai pagu yang tidak ikut berubah karena host.
//!
//! Latar belakangnya ada di `docs/prd/BUKTI-PSS-DAEMON 131126-0323.md`:
//! job CI `budget` mengukur daemon 105.3-110.6 MB setelah commit teks, dan
//! sebabnya masih INFERRED. Uji ini mengubah tebakan itu jadi angka.

use std::mem::size_of;

use engine::native::{Document, Kotak, LaporanLayout, NodeId, Viewport, layout};

/// Beban yang sama dengan `crates/browserd/tests/native_daemon_budget.rs`:
/// 1500 `div` berisi teks, satu deklarasi `style` sebaris per elemen.
const JUMLAH_DIV: usize = 1500;

fn dokumen_beban() -> String {
    use std::fmt::Write as _;
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

fn string_heap(s: &String) -> usize {
    s.capacity()
}

/// Perkiraan byte yang masih dipakai laporan ini, termasuk heap `String`.
///
/// Tabel hash memakai kapasitas slot, bukan jumlah isi: yang menentukan RSS
/// adalah kapasitas yang dialokasikan. Overhead kontrol tabel hash tidak ikut
/// dihitung, jadi angka ini batas bawah yang konservatif.
fn retensi(l: &LaporanLayout) -> usize {
    let mut total = 0usize;

    total += l.kotak.capacity() * size_of::<Kotak>();

    total += l.gaya.capacity() * (size_of::<NodeId>() + size_of::<Vec<(String, String)>>());
    for v in l.gaya.values() {
        total += v.capacity() * size_of::<(String, String)>();
        for (k, val) in v {
            total += string_heap(k) + string_heap(val);
        }
    }

    total += l.terhitung.capacity()
        * (size_of::<NodeId>() + size_of::<engine::native::gaya::GayaTerhitung>());
    for g in l.terhitung.values() {
        if let Some(w) = &g.warna {
            total += string_heap(w);
        }
        if let Some(p) = &g.perataan {
            total += string_heap(p);
        }
    }

    total += l.teks.capacity() * (size_of::<NodeId>() + size_of::<String>());
    for t in l.teks.values() {
        total += string_heap(t);
    }

    total += l.teks_tersembunyi.capacity() * size_of::<NodeId>();

    for s in &l.diabaikan {
        total += string_heap(s) + size_of::<String>();
    }
    for s in &l.catatan_kaskade {
        total += string_heap(s) + size_of::<String>();
    }

    total
}

#[test]
fn laporan_beban_dihitung_per_bagian() {
    let html = dokumen_beban();
    let dokumen = Document::parse_html(&html).expect("dokumen beban");
    let laporan = layout::hitung(&dokumen, Viewport::new(1280.0, 800.0)).expect("layout beban");

    let kotak = laporan.kotak.len();
    let gaya = laporan.gaya.len();
    let terhitung = laporan.terhitung.len();
    let teks = laporan.teks.len();
    let total = retensi(&laporan);

    println!(
        "node dokumen={} kotak={kotak} gaya={gaya} terhitung={terhitung} teks={teks}",
        dokumen.jumlah_node()
    );
    // Rincian per bagian: yang menentukan perbaikan mana yang layak.
    let b_kotak = laporan.kotak.capacity() * size_of::<Kotak>();
    let mut b_gaya =
        laporan.gaya.capacity() * (size_of::<NodeId>() + size_of::<Vec<(String, String)>>());
    for v in laporan.gaya.values() {
        b_gaya += v.capacity() * size_of::<(String, String)>();
        for (k, val) in v {
            b_gaya += string_heap(k) + string_heap(val);
        }
    }
    let b_terhitung = laporan.terhitung.capacity()
        * (size_of::<NodeId>() + size_of::<engine::native::gaya::GayaTerhitung>());
    let b_teks = laporan.teks.capacity() * (size_of::<NodeId>() + size_of::<String>())
        + laporan.teks.values().map(string_heap).sum::<usize>();
    println!(
        "rincian: kotak={b_kotak} gaya={b_gaya} terhitung={b_terhitung} teks={b_teks} sisa={}",
        total.saturating_sub(b_kotak + b_gaya + b_terhitung + b_teks)
    );

    println!(
        "retensi laporan = {} byte ({:.3} MB) per tab",
        total,
        total as f64 / 1_048_576.0
    );
    println!(
        "25 tab panas = {:.1} MB, 99 tab = {:.1} MB",
        total as f64 * 25.0 / 1_048_576.0,
        total as f64 * 99.0 / 1_048_576.0
    );

    assert!(
        terhitung >= 3000,
        "dokumen beban harus >= 3000 elemen, dapat {terhitung}"
    );
}
