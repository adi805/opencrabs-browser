//! Berapa byte yang ditahan satu `TabPanas` utuh: DOM, laporan layout, dan
//! isolat JavaScript?
//!
//! `laporan_retensi.rs` mengukur laporan layout saja (0.551 MB per tab pada
//! beban budget). Uji ini menambahkan DOM, karena pohon `Vec<Node>` dengan
//! atribut `String` per elemen bisa jauh lebih besar daripada laporannya.
//!
//! Tidak ada PSS di sini: yang diukur kapasitas alokasi yang benar-benar
//! ditahan, supaya angkanya deterministik dan tidak bergantung host.

use std::mem::size_of;

use engine::native::{Document, LaporanLayout, NodeKind, Viewport, layout};

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

/// Byte yang ditahan arena DOM: vektor node, atribut, anak, dan isi `String`.
fn retensi_dom(doc: &Document) -> usize {
    let node = doc.nodes();
    let mut total = std::mem::size_of_val(node);

    for n in node {
        total += n.attrs.capacity() * size_of::<(String, String)>();
        for (k, v) in &n.attrs {
            total += string_heap(k) + string_heap(v);
        }
        total += n.children.capacity() * size_of::<usize>();
        match &n.kind {
            NodeKind::Element(t) | NodeKind::Text(t) => total += string_heap(t),
            NodeKind::Document | NodeKind::Doctype => {}
        }
    }
    total
}

fn retensi_laporan(l: &LaporanLayout) -> usize {
    let mut total = l.kotak.capacity() * size_of::<engine::native::Kotak>();
    for v in l.gaya.values() {
        total += v.capacity() * size_of::<(String, String)>();
        for (k, val) in v {
            total += string_heap(k) + string_heap(val);
        }
    }
    for t in l.teks.values() {
        total += string_heap(t);
    }
    for s in &l.diabaikan {
        total += string_heap(s);
    }
    for s in &l.catatan_kaskade {
        total += string_heap(s);
    }
    total
}

#[test]
fn dom_dan_laporan_satu_tab() {
    let html = dokumen_beban();
    let dokumen = Document::parse_html(&html).expect("dokumen beban");
    let laporan = layout::hitung(&dokumen, Viewport::new(1280.0, 800.0)).expect("layout beban");

    let dom = retensi_dom(&dokumen);
    let lap = retensi_laporan(&laporan);
    let satu = dom + lap;

    println!(
        "node={} dom={} byte ({:.3} MB) laporan={} byte ({:.3} MB) total={:.3} MB",
        dokumen.jumlah_node(),
        dom,
        dom as f64 / 1_048_576.0,
        lap,
        lap as f64 / 1_048_576.0,
        satu as f64 / 1_048_576.0
    );
    println!(
        "25 tab panas = {:.1} MB, 99 tab = {:.1} MB",
        satu as f64 * 25.0 / 1_048_576.0,
        satu as f64 * 99.0 / 1_048_576.0
    );

    assert!(dokumen.jumlah_node() >= 3000, "beban harus >= 3000 node");
}
