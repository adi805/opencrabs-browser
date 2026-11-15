//! Pengukur fase: berapa milidetik yang pergi ke parse, layout, dan lukis.
//!
//! Dipakai untuk NFR-001: tanpa angka per fase, "cache layout" cuma tebakan
//! soal bagian mana yang mahal. Jalankan dengan satu argumen berisi path HTML.

use std::time::Instant;

use engine::native::dom::Document;
use engine::native::layout::{self, Viewport};
use engine::native::paint::{self, Format, Klip};

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/ocb-audit/www/b600.html".to_string());
    let html = std::fs::read_to_string(&path).expect("baca html");
    let viewport = Viewport::new(1280.0, 800.0);

    let t = Instant::now();
    let doc = Document::parse_html(&html).expect("parse");
    let parse_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("simpul      : {}", doc.jumlah_node());
    println!("parse       : {parse_ms:8.1} ms");

    // Layout berkali-kali: panggilan pertama ikut memanaskan font, yang kedua
    // dan seterusnya adalah biaya yang benar-benar diulang tiap tangkapan.
    for i in 1..=3 {
        let t = Instant::now();
        let laporan = layout::hitung(&doc, viewport).expect("layout");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "layout #{i}   : {ms:8.1} ms  ({} kotak)",
            laporan.kotak.len()
        );
    }

    for i in 1..=3 {
        let t = Instant::now();
        let klip = Klip::baru(0.0, 0.0, 1280.0, 800.0);
        let raster = paint::lukis_dengan(&doc, viewport, Some(klip), Format::Png { cepat: true })
            .expect("lukis");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "lukis #{i}    : {ms:8.1} ms  ({} byte, {} kotak)",
            raster.data.len(),
            raster.kotak_dilukis
        );
    }
}
