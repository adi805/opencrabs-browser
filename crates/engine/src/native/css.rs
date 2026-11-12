//! Kaskade stylesheet: dari teks CSS ke deklarasi pemenang per elemen.
//!
//! Yang diurai di sini adalah subset CSS yang benar-benar dipakai halaman
//! biasa: blok aturan `selektor { deklarasi }`, daftar selektor dipisah koma,
//! `!important`, dan shorthand `padding`/`margin`. At-rule (`@media`,
//! `@import`, `@font-face`) **tidak** dijalankan: setiap at-rule dicatat di
//! [`Kaskade::diabaikan`], bukan diterapkan dengan arti lain.
//!
//! `<link rel="stylesheet">` juga tidak diambil dari jaringan oleh crate ini.
//! URL-nya dicatat sebagai diabaikan supaya lapisan CDP bisa mengambilnya dan
//! menambahkannya lewat [`Kaskade::tambah_lembar`] kalau mau.

use std::collections::HashMap;

use super::dom::{Document, NodeId, NodeKind};
use super::selectors::{Kompleks, urai};

/// Satu deklarasi `nama: nilai` di dalam blok aturan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deklarasi {
    pub nama: String,
    pub nilai: String,
    pub penting: bool,
}

/// Satu aturan dengan **satu** selektor. Daftar selektor dipisah koma
/// dipecah jadi beberapa aturan, karena tiap selektor punya spesifisitasnya
/// sendiri dan pemenangnya ditentukan per selektor, bukan per daftar.
#[derive(Debug, Clone)]
pub struct Aturan {
    pub selektor: Kompleks,
    pub spesifisitas: (u32, u32, u32),
    pub deklarasi: Vec<Deklarasi>,
    /// Urutan kemunculan di seluruh dokumen; dipakai sebagai pemutus seri
    /// terakhir setelah spesifisitas.
    pub urutan: usize,
}

/// Hasil penguraian satu lembar gaya.
#[derive(Debug, Clone, Default)]
pub struct Lembar {
    pub aturan: Vec<Aturan>,
    pub diabaikan: Vec<String>,
}

/// Seluruh lembar gaya yang berlaku untuk satu dokumen, urut dokumen.
#[derive(Debug, Clone, Default)]
pub struct Kaskade {
    lembar: Vec<Lembar>,
    /// Dinaikkan tiap lembar baru supaya urutan tetap naik lintas lembar.
    urutan_berikut: usize,
    diabaikan: Vec<String>,
}

impl Kaskade {
    /// Mengumpulkan `<style>` dari dokumen, urut kemunculan, dan mencatat
    /// `<link rel="stylesheet">` sebagai tidak diambil.
    #[must_use]
    pub fn kumpulkan(doc: &Document) -> Self {
        let mut k = Self::default();
        for (id, node) in doc.nodes().iter().enumerate() {
            let Some(tag) = node.tag() else {
                continue;
            };
            match tag {
                "style" => {
                    let teks = teks_di_dalam(doc, id);
                    k.tambah_lembar(&teks, &format!("<style> node {id}"));
                }
                "link" => {
                    let rel = node.attr("rel").unwrap_or_default().to_ascii_lowercase();
                    if rel.split_whitespace().any(|r| r == "stylesheet") {
                        let href = node.attr("href").unwrap_or("(tanpa href)");
                        k.diabaikan.push(format!(
                            "node {id}: `<link rel=stylesheet>` tidak diambil dari jaringan: {href}"
                        ));
                    }
                }
                _ => {}
            }
        }
        k
    }

    /// Menambah satu lembar gaya dari teks CSS. Dipakai `kumpulkan` untuk
    /// `<style>`, dan tersedia untuk lapisan lain yang sudah punya teks
    /// stylesheet eksternal.
    pub fn tambah_lembar(&mut self, teks: &str, asal: &str) {
        let mut lembar = Lembar::default();
        lembar.aturan = urai_lembar(teks, &mut lembar.diabaikan);
        for aturan in &mut lembar.aturan {
            aturan.urutan += self.urutan_berikut;
        }
        self.urutan_berikut += lembar.aturan.len();
        self.diabaikan
            .extend(lembar.diabaikan.iter().map(|d| format!("{asal}: {d}")));
        self.lembar.push(lembar);
    }

    /// Deklarasi yang menang per properti untuk satu elemen, sudah termasuk
    /// atribut `style` sebaris.
    ///
    /// Kuncinya `(penting, sebaris, spesifisitas, urutan)`: deklarasi
    /// `!important` mengalahkan deklarasi normal, `style` sebaris mengalahkan
    /// aturan selektor, lalu spesifisitas, lalu urutan sumber.
    #[must_use]
    pub fn menang(&self, doc: &Document, id: NodeId) -> Vec<(String, String)> {
        let mut pemenang = Pemenang::new();

        for lembar in &self.lembar {
            for aturan in &lembar.aturan {
                if !doc.cocok_terurai(id, &aturan.selektor) {
                    continue;
                }
                let dasar = (false, false, aturan.spesifisitas, aturan.urutan);
                for d in &aturan.deklarasi {
                    let kunci = (d.penting, dasar.1, dasar.2, dasar.3);
                    masukkan(&mut pemenang, &d.nama, &d.nilai, kunci);
                }
            }
        }

        if let Some(node) = doc.node(id)
            && let Some(style) = node.attr("style")
        {
            let mut diabaikan = Vec::new();
            for d in urai_deklarasi(style, &mut diabaikan) {
                let kunci = (d.penting, true, (0, 0, 0), usize::MAX);
                masukkan(&mut pemenang, &d.nama, &d.nilai, kunci);
            }
        }

        let mut out: Vec<(String, String)> = pemenang
            .into_iter()
            .map(|(nama, (_, nilai))| (nama, nilai))
            .collect();
        out.sort();
        out
    }

    /// Catatan hal yang sengaja tidak dijalankan: at-rule, stylesheet
    /// eksternal, dan deklarasi yang bentuknya tidak dikenali.
    #[must_use]
    pub fn diabaikan(&self) -> &[String] {
        &self.diabaikan
    }
}

/// Kunci pembanding kaskade: `(penting, sebaris, spesifisitas, urutan)`.
///
/// Diurutkan sebagai tuple, jadi urutannya persis prioritas CSS: `!important`
/// dulu, lalu deklarasi sebaris, lalu spesifisitas, lalu urutan sumber.
type Kunci = (bool, bool, (u32, u32, u32), usize);

/// Pemenang sementara per properti: kunci pembanding dan nilai yang menang.
type Pemenang = HashMap<String, (Kunci, String)>;

fn masukkan(peta: &mut Pemenang, nama: &str, nilai: &str, kunci: Kunci) {
    match peta.get(nama) {
        Some((kunci_lama, _)) if *kunci_lama >= kunci => {}
        _ => {
            peta.insert(nama.to_string(), (kunci, nilai.to_string()));
        }
    }
}

/// Teks dari seluruh node teks di dalam satu elemen, digabung tanpa pemisah.
fn teks_di_dalam(doc: &Document, id: NodeId) -> String {
    let mut out = String::new();
    let Some(node) = doc.node(id) else {
        return out;
    };
    if let NodeKind::Text(t) = &node.kind {
        out.push_str(t);
    }
    for &anak in &node.children {
        out.push_str(&teks_di_dalam(doc, anak));
    }
    out
}

/// Mengurai teks CSS menjadi aturan. Kesalahan sintaks tidak menggagalkan
/// seluruh lembar: bagian yang tidak bisa diurai dicatat di `diabaikan` dan
/// penguraian lanjut, karena satu aturan rusak di halaman nyata tidak boleh
/// membatalkan sisa stylesheet.
pub fn urai_lembar(teks: &str, diabaikan: &mut Vec<String>) -> Vec<Aturan> {
    let chars: Vec<char> = teks.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    let mut aturan = Vec::new();

    while i < n {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        if chars[i] == '/' && i + 1 < n && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < n && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        if chars[i] == '@' {
            let mulai = i;
            i += 1;
            while i < n && (chars[i].is_alphanumeric() || chars[i] == '-') {
                i += 1;
            }
            let nama: String = chars[mulai + 1..i].iter().collect();
            // At-rule berakhir di `;` (mis. `@import`) atau di blok `{}`
            // (mis. `@media`). Isinya TIDAK dijalankan: menjalankan
            // `@media` tanpa informasi media yang benar akan menerapkan
            // aturan yang seharusnya tidak berlaku.
            let mut kedalaman = 0i32;
            while i < n {
                match chars[i] {
                    '{' => kedalaman += 1,
                    '}' => {
                        kedalaman -= 1;
                        if kedalaman <= 0 {
                            i += 1;
                            break;
                        }
                    }
                    ';' if kedalaman == 0 => {
                        i += 1;
                        break;
                    }
                    _ => {}
                }
                i += 1;
            }
            diabaikan.push(format!("at-rule `@{nama}` tidak dijalankan"));
            continue;
        }

        let mulai = i;
        let mut kedalaman = 0i32;
        let mut kutip: Option<char> = None;
        while i < n {
            let c = chars[i];
            if let Some(k) = kutip {
                if c == k {
                    kutip = None;
                }
                i += 1;
                continue;
            }
            match c {
                '"' | '\'' => kutip = Some(c),
                '(' | '[' => kedalaman += 1,
                ')' | ']' => kedalaman -= 1,
                '{' | '}' if kedalaman == 0 => break,
                _ => {}
            }
            i += 1;
        }
        let prelude: String = chars[mulai..i].iter().collect();
        if i >= n || chars[i] != '{' {
            if !prelude.trim().is_empty() {
                diabaikan.push(format!("aturan tanpa blok `{}`", prelude.trim()));
            }
            i = (i + 1).min(n);
            continue;
        }

        i += 1;
        let isi_mulai = i;
        let mut kedalaman_blok = 1i32;
        let mut kutip_isi: Option<char> = None;
        while i < n {
            let c = chars[i];
            if let Some(k) = kutip_isi {
                if c == k {
                    kutip_isi = None;
                }
                i += 1;
                continue;
            }
            match c {
                '"' | '\'' => kutip_isi = Some(c),
                '{' => kedalaman_blok += 1,
                '}' => {
                    kedalaman_blok -= 1;
                    if kedalaman_blok == 0 {
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let isi: String = chars[isi_mulai..i.min(n)].iter().collect();
        i = (i + 1).min(n);

        let deklarasi = urai_deklarasi(&isi, diabaikan);
        if deklarasi.is_empty() {
            continue;
        }
        match urai(prelude.trim()) {
            Ok(daftar) => {
                for k in daftar {
                    aturan.push(Aturan {
                        spesifisitas: k.spesifisitas(),
                        selektor: k,
                        deklarasi: deklarasi.clone(),
                        urutan: 0,
                    });
                }
            }
            Err(e) => diabaikan.push(format!("selektor `{}` ditolak: {e}", prelude.trim())),
        }
    }
    aturan
}

/// Mengurai isi atribut `style` atau blok deklarasi menjadi daftar deklarasi.
///
/// Shorthand `padding`/`margin` diperluas jadi empat longhand di sini, supaya
/// kaskade tinggal membandingkan nama properti yang sama, bukan menebak mana
/// yang lebih spesifik antara `padding` dan `padding-left`.
pub fn urai_deklarasi(teks: &str, diabaikan: &mut Vec<String>) -> Vec<Deklarasi> {
    let mut out = Vec::new();
    for potong in pisah_deklarasi(teks) {
        let bersih = potong.trim();
        if bersih.is_empty() {
            continue;
        }
        let Some((nama, nilai)) = bersih.split_once(':') else {
            diabaikan.push(format!("deklarasi tanpa titik dua `{bersih}`"));
            continue;
        };
        let nama = nama.trim().to_ascii_lowercase();
        let (nilai, penting) = pisah_penting(nilai.trim());
        if nama.is_empty() || nilai.is_empty() {
            diabaikan.push(format!("deklarasi kosong `{bersih}`"));
            continue;
        }

        match nama.as_str() {
            "padding" | "margin" => {
                let bagian: Vec<&str> = nilai.split_whitespace().collect();
                let Some(empat) = empat_sisi(&bagian) else {
                    diabaikan.push(format!("`{nama}: {nilai}` tidak bisa diperluas"));
                    continue;
                };
                for (sisi, v) in ["top", "right", "bottom", "left"].iter().zip(empat.iter()) {
                    out.push(Deklarasi {
                        nama: format!("{nama}-{sisi}"),
                        nilai: v.clone(),
                        penting,
                    });
                }
            }
            _ => out.push(Deklarasi {
                nama,
                nilai,
                penting,
            }),
        }
    }
    out
}

/// Memisah deklarasi pada `;` yang berada di luar kurung dan di luar kutip.
fn pisah_deklarasi(teks: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut kedalaman = 0i32;
    let mut kutip: Option<char> = None;
    for c in teks.chars() {
        if let Some(k) = kutip {
            if c == k {
                kutip = None;
            }
            buf.push(c);
            continue;
        }
        match c {
            '"' | '\'' => {
                kutip = Some(c);
                buf.push(c);
            }
            '(' => {
                kedalaman += 1;
                buf.push(c);
            }
            ')' => {
                kedalaman -= 1;
                buf.push(c);
            }
            ';' if kedalaman == 0 => {
                out.push(std::mem::take(&mut buf));
            }
            _ => buf.push(c),
        }
    }
    if !buf.trim().is_empty() {
        out.push(buf);
    }
    out
}

/// Melepas penanda `!important` dari nilai, mengembalikan nilai dan tandanya.
///
/// Pencocokan memakai huruf kecil, tapi nilai yang dikembalikan tetap huruf
/// aslinya: nama font dan `url()` tidak boleh diubah besar kecilnya.
fn pisah_penting(nilai: &str) -> (String, bool) {
    let v = nilai.trim_end();
    let rendah = v.to_ascii_lowercase();
    let Some(idx) = rendah.rfind("important") else {
        return (v.to_string(), false);
    };
    if idx + "important".len() != rendah.len() {
        return (v.to_string(), false);
    }
    let sisa = v[..idx].trim_end();
    let Some(sisa) = sisa.strip_suffix('!') else {
        return (v.to_string(), false);
    };
    (sisa.trim_end().to_string(), true)
}

/// Memperluas 1 sampai 4 nilai shorthand jadi urutan atas, kanan, bawah, kiri.
fn empat_sisi(bagian: &[&str]) -> Option<Vec<String>> {
    let v: Vec<String> = bagian.iter().map(|s| (*s).to_string()).collect();
    Some(match v.len() {
        1 => vec![v[0].clone(); 4],
        2 => vec![v[0].clone(), v[1].clone(), v[0].clone(), v[1].clone()],
        3 => vec![v[0].clone(), v[1].clone(), v[2].clone(), v[1].clone()],
        4 => v,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dokumen(html: &str) -> Document {
        Document::parse_html(html).expect("html harus terurai")
    }

    fn peta(k: &Kaskade, doc: &Document, id: NodeId) -> HashMap<String, String> {
        k.menang(doc, id).into_iter().collect()
    }

    fn satu(html: &str, selektor: &str) -> HashMap<String, String> {
        let doc = dokumen(html);
        let k = Kaskade::kumpulkan(&doc);
        let id = doc
            .pilih_satu(selektor)
            .expect("selektor sah")
            .expect("elemen ada");
        peta(&k, &doc, id)
    }

    #[test]
    fn test_kaskade_style_block_menang_atas_bawaan() {
        let g = satu(
            "<html><head><style>div{width:120px}</style></head>\
             <body><div id=\"a\"></div></body></html>",
            "#a",
        );
        assert_eq!(g.get("width").map(String::as_str), Some("120px"));
    }

    #[test]
    fn test_kaskade_spesifisitas_lalu_urutan() {
        let g = satu(
            "<html><head><style>\
             div{width:10px;height:10px}\
             .k{width:20px;height:20px}\
             #a{width:30px}\
             </style></head><body><div id=\"a\" class=\"k\"></div></body></html>",
            "#a",
        );
        // id menang atas kelas dan tag
        assert_eq!(g.get("width").map(String::as_str), Some("30px"));
        // dua deklarasi tanpa id: yang muncul belakangan menang
        assert_eq!(g.get("height").map(String::as_str), Some("20px"));
    }

    #[test]
    fn test_kaskade_sebaris_menang_atas_aturan_biasa() {
        let g = satu(
            "<html><head><style>#a{width:30px}</style></head>\
             <body><div id=\"a\" style=\"width:40px\"></div></body></html>",
            "#a",
        );
        assert_eq!(g.get("width").map(String::as_str), Some("40px"));
    }

    #[test]
    fn test_kaskade_penting_menang_atas_sebaris_biasa() {
        let g = satu(
            "<html><head><style>#a{width:30px !important}</style></head>\
             <body><div id=\"a\" style=\"width:40px\"></div></body></html>",
            "#a",
        );
        assert_eq!(g.get("width").map(String::as_str), Some("30px"));
    }

    #[test]
    fn test_kaskade_sebaris_penting_menang_atas_aturan_penting() {
        let g = satu(
            "<html><head><style>#a{width:30px !important}</style></head>\
             <body><div id=\"a\" style=\"width:40px !important\"></div></body></html>",
            "#a",
        );
        assert_eq!(g.get("width").map(String::as_str), Some("40px"));
    }

    #[test]
    fn test_kaskade_shorthand_padding_dan_margin() {
        let g = satu(
            "<html><head><style>#a{padding:1px 2px 3px 4px;margin:0 auto}</style></head>\
             <body><div id=\"a\"></div></body></html>",
            "#a",
        );
        assert_eq!(g.get("padding-top").map(String::as_str), Some("1px"));
        assert_eq!(g.get("padding-right").map(String::as_str), Some("2px"));
        assert_eq!(g.get("padding-bottom").map(String::as_str), Some("3px"));
        assert_eq!(g.get("padding-left").map(String::as_str), Some("4px"));
        assert_eq!(g.get("margin-top").map(String::as_str), Some("0"));
        assert_eq!(g.get("margin-right").map(String::as_str), Some("auto"));
        assert_eq!(g.get("margin-bottom").map(String::as_str), Some("0"));
        assert_eq!(g.get("margin-left").map(String::as_str), Some("auto"));
    }

    #[test]
    fn test_kaskade_at_rule_dan_link_dicatat_bukan_dijalankan() {
        let doc = dokumen(
            "<html><head>\
             <link rel=\"stylesheet\" href=\"https://contoh.test/x.css\">\
             <style>@media screen { div{width:1px} } div{height:2px}</style>\
             </head><body><div id=\"a\"></div></body></html>",
        );
        let k = Kaskade::kumpulkan(&doc);
        let id = doc.pilih_satu("#a").unwrap().unwrap();
        let g = peta(&k, &doc, id);
        // aturan di dalam @media tidak dijalankan
        assert!(!g.contains_key("width"), "aturan @media tidak boleh jalan");
        // aturan sesudahnya tetap jalan
        assert_eq!(g.get("height").map(String::as_str), Some("2px"));
        let catatan = k.diabaikan().join(" | ");
        assert!(catatan.contains("x.css"), "href harus tercatat: {catatan}");
        assert!(
            catatan.contains("@media"),
            "at-rule harus tercatat: {catatan}"
        );
    }
}
