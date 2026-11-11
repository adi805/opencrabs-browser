//! Selektor CSS subset: parser dan pencocok terhadap [`Document`].
//!
//! Yang didukung sengaja dibatasi pada bentuk yang dipakai skrip otomasi
//! sehari-hari, dan **batasnya disebut**, bukan disembunyikan:
//!
//! - tag (`div`), kelas (`.kartu`), id (`#utama`), universal (`*`)
//! - atribut: `[href]`, `[type=text]`, `[href^=https]`, `[src$=.png]`,
//!   `[class*=kartu]`
//! - kombinator keturunan (spasi) dan anak langsung (`>`)
//! - pseudo-kelas `:first-child`, `:nth-child(an+b)`, `:not(sederhana)`
//! - daftar selektor dipisah koma
//!
//! Yang **tidak** ada: kombinator saudara (`+`, `~`), pseudo-elemen (`::`),
//! pemilih berbasis teks (`:contains`), `[attr~=]`/`[attr|=]`, modifier
//! `i`/`s`, dan escape CSS (`\.`). Selektornya mengembalikan galat, bukan
//! diam-diam salah cocok.
//!
//! Pencocokan berjalan dari kanan ke kiri: elemen diuji dulu terhadap syarat
//! terakhir, baru leluhurnya. Itu yang membuat `div > p span` tidak perlu
//! menelusuri seluruh pohon dari akar untuk tiap kandidat.

use super::{Document, Node, NodeId};

/// Galat sintaks selektor. Pesannya menyebut bagian yang gagal diurai supaya
/// pemanggil tahu di mana letak salahnya, bukan sekadar "selektor tidak sah".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("selektor tidak sah: {0}")]
pub struct GalatSelektor(String);

impl GalatSelektor {
    fn baru(pesan: impl Into<String>) -> Self {
        Self(pesan.into())
    }
}

/// Operator pembanding nilai atribut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpAtribut {
    /// `[attr]` — atribut ada, nilainya apa pun.
    Ada,
    /// `[attr=nilai]`
    Sama(String),
    /// `[attr^=nilai]`
    Awalan(String),
    /// `[attr$=nilai]`
    Akhiran(String),
    /// `[attr*=nilai]`
    Mengandung(String),
}

/// Pola `an+b` untuk `:nth-child`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pola {
    pub a: i64,
    pub b: i64,
}

impl Pola {
    /// Posisi anak dihitung 1-based, hanya elemen (node teks tidak dihitung).
    #[must_use]
    pub fn cocok(&self, posisi: i64) -> bool {
        if self.a == 0 {
            return posisi == self.b;
        }
        let selisih = posisi - self.b;
        selisih % self.a == 0 && selisih / self.a >= 0
    }
}

/// Satu syarat yang harus dipenuhi satu elemen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sederhana {
    Tag(String),
    Id(String),
    Kelas(String),
    Atribut { nama: String, op: OpAtribut },
    PertamaAnak,
    AnakKe(Pola),
    Bukan(Box<Sederhana>),
}

/// Hubungan antara satu bagian selektor dan bagian di kirinya.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kombinator {
    /// Spasi: leluhur mana pun.
    Turunan,
    /// `>`: induk langsung.
    Anak,
}

/// Selektor kompleks: rangkaian gabungan yang dihubungkan kombinator.
///
/// `bagian[0].0` tidak dipakai (tidak ada sisi kiri); disimpan supaya bentuk
/// datanya seragam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kompleks {
    bagian: Vec<(Kombinator, Vec<Sederhana>)>,
}

/// Mengurai daftar selektor (dipisah koma).
pub fn urai(teks: &str) -> Result<Vec<Kompleks>, GalatSelektor> {
    let mut hasil = Vec::new();
    for potong in pisah_koma(teks) {
        let bersih = potong.trim();
        if bersih.is_empty() {
            return Err(GalatSelektor::baru("ada bagian kosong di daftar selektor"));
        }
        hasil.push(urai_kompleks(bersih)?);
    }
    if hasil.is_empty() {
        return Err(GalatSelektor::baru("selektor kosong"));
    }
    Ok(hasil)
}

/// Memisah pada koma yang berada di luar `[...]`/`(...)` dan di luar kutip.
fn pisah_koma(teks: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut dalam = 0usize;
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
            '[' | '(' => {
                dalam += 1;
                buf.push(c);
            }
            ']' | ')' => {
                dalam = dalam.saturating_sub(1);
                buf.push(c);
            }
            ',' if dalam == 0 => out.push(std::mem::take(&mut buf)),
            _ => buf.push(c),
        }
    }
    out.push(buf);
    out
}

fn urai_kompleks(teks: &str) -> Result<Kompleks, GalatSelektor> {
    let chars: Vec<char> = teks.chars().collect();
    let mut bagian: Vec<(Kombinator, Vec<Sederhana>)> = Vec::new();
    let mut buf = String::new();
    let mut komb_berikut = Kombinator::Turunan;
    let mut gantung = false;
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c == '[' || c == '(' {
            // Salin utuh sampai penutupnya, termasuk koma/spasi di dalamnya.
            let tutup = if c == '[' { ']' } else { ')' };
            let (isi, lanjut) = urai_sampai(&chars, i + 1, tutup)?;
            buf.push(c);
            buf.push_str(&isi);
            buf.push(tutup);
            i = lanjut;
            continue;
        }
        if c.is_whitespace() || c == '>' {
            if !buf.is_empty() {
                bagian.push((komb_berikut, urai_gabungan(&buf)?));
                buf.clear();
                komb_berikut = Kombinator::Turunan;
                gantung = false;
            }
            if c == '>' {
                if bagian.is_empty() {
                    return Err(GalatSelektor::baru("kombinator '>' tanpa sisi kiri"));
                }
                komb_berikut = Kombinator::Anak;
                gantung = true;
            }
            i += 1;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            continue;
        }
        buf.push(c);
        i += 1;
    }

    if !buf.is_empty() {
        bagian.push((komb_berikut, urai_gabungan(&buf)?));
        gantung = false;
    }
    if gantung {
        return Err(GalatSelektor::baru("kombinator '>' tanpa sisi kanan"));
    }
    if bagian.is_empty() {
        return Err(GalatSelektor::baru("selektor kosong"));
    }
    if let Some(pertama) = bagian.first_mut() {
        pertama.0 = Kombinator::Turunan;
    }
    Ok(Kompleks { bagian })
}

/// Mengurai satu gabungan (`div.kartu#utama[href]`) jadi daftar syarat.
fn urai_gabungan(teks: &str) -> Result<Vec<Sederhana>, GalatSelektor> {
    let chars: Vec<char> = teks.chars().collect();
    if chars.is_empty() {
        return Err(GalatSelektor::baru("gabungan kosong"));
    }
    let mut out = Vec::new();
    let mut universal = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '*' => {
                // `*` tidak menambah syarat, tapi juga bukan gabungan kosong.
                universal = true;
                i += 1;
            }
            '.' => {
                let (nama, lanjut) = urai_ident(&chars, i + 1)?;
                out.push(Sederhana::Kelas(nama));
                i = lanjut;
            }
            '#' => {
                let (nama, lanjut) = urai_ident(&chars, i + 1)?;
                out.push(Sederhana::Id(nama));
                i = lanjut;
            }
            '[' => {
                let (isi, lanjut) = urai_sampai(&chars, i + 1, ']')?;
                out.push(urai_atribut(&isi)?);
                i = lanjut;
            }
            ':' => {
                let (nama, lanjut) = urai_ident(&chars, i + 1)?;
                let kecil = nama.to_ascii_lowercase();
                if chars.get(lanjut) == Some(&'(') {
                    let (isi, sesudah) = urai_sampai(&chars, lanjut + 1, ')')?;
                    match kecil.as_str() {
                        "nth-child" => out.push(Sederhana::AnakKe(urai_nth(&isi)?)),
                        "not" => {
                            let dalam = urai_gabungan(isi.trim())?;
                            if dalam.len() != 1 {
                                return Err(GalatSelektor::baru(
                                    ":not() hanya menerima satu syarat sederhana",
                                ));
                            }
                            if let Some(satu) = dalam.into_iter().next() {
                                out.push(Sederhana::Bukan(Box::new(satu)));
                            }
                        }
                        lain => {
                            return Err(GalatSelektor::baru(format!(
                                "pseudo-kelas ':{lain}()' belum didukung"
                            )));
                        }
                    }
                    i = sesudah;
                } else {
                    match kecil.as_str() {
                        "first-child" => out.push(Sederhana::PertamaAnak),
                        lain => {
                            return Err(GalatSelektor::baru(format!(
                                "pseudo-kelas ':{lain}' belum didukung"
                            )));
                        }
                    }
                    i = lanjut;
                }
            }
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => {
                let (nama, lanjut) = urai_ident(&chars, i)?;
                out.push(Sederhana::Tag(nama.to_ascii_lowercase()));
                i = lanjut;
            }
            lain => {
                return Err(GalatSelektor::baru(format!(
                    "karakter '{lain}' tidak dikenal di selektor"
                )));
            }
        }
    }
    if out.is_empty() && !universal {
        return Err(GalatSelektor::baru("gabungan tanpa syarat"));
    }
    Ok(out)
}

/// Membaca `[A-Za-z0-9_-]+` mulai `mulai`, mengembalikan nama dan indeks lanjut.
fn urai_ident(chars: &[char], mulai: usize) -> Result<(String, usize), GalatSelektor> {
    let mut i = mulai;
    let mut nama = String::new();
    while i < chars.len()
        && (chars[i].is_ascii_alphanumeric() || chars[i] == '-' || chars[i] == '_')
    {
        nama.push(chars[i]);
        i += 1;
    }
    if nama.is_empty() {
        return Err(GalatSelektor::baru("nama tag/kelas/id kosong"));
    }
    Ok((nama, i))
}

/// Menyalin isi sampai `tutup`, menghormati kurung bersarang dan tanda kutip.
fn urai_sampai(
    chars: &[char],
    mulai: usize,
    tutup: char,
) -> Result<(String, usize), GalatSelektor> {
    let buka = if tutup == ']' { '[' } else { '(' };
    let mut dalam = 0usize;
    let mut kutip: Option<char> = None;
    let mut out = String::new();
    let mut i = mulai;
    while i < chars.len() {
        let c = chars[i];
        if let Some(k) = kutip {
            if c == k {
                kutip = None;
            }
            out.push(c);
            i += 1;
            continue;
        }
        if c == '"' || c == '\'' {
            kutip = Some(c);
            out.push(c);
            i += 1;
            continue;
        }
        if c == buka {
            dalam += 1;
            out.push(c);
            i += 1;
            continue;
        }
        if c == tutup {
            if dalam == 0 {
                return Ok((out, i + 1));
            }
            dalam -= 1;
            out.push(c);
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    Err(GalatSelektor::baru(format!("'{buka}' tidak ditutup")))
}

fn urai_atribut(isi: &str) -> Result<Sederhana, GalatSelektor> {
    let isi = isi.trim();
    if isi.is_empty() {
        return Err(GalatSelektor::baru("kurung siku kosong"));
    }
    let Some(pos) = isi.find('=') else {
        return Ok(Sederhana::Atribut {
            nama: isi.to_ascii_lowercase(),
            op: OpAtribut::Ada,
        });
    };
    let kiri = &isi[..pos];
    let kanan = isi[pos + 1..].trim();
    let (nama, jenis) = match kiri.chars().last() {
        Some('^') => (&kiri[..kiri.len() - 1], '^'),
        Some('$') => (&kiri[..kiri.len() - 1], '$'),
        Some('*') => (&kiri[..kiri.len() - 1], '*'),
        _ => (kiri, '='),
    };
    let nama = nama.trim().to_ascii_lowercase();
    if nama.is_empty() {
        return Err(GalatSelektor::baru("nama atribut kosong"));
    }
    let nilai = buka_kutip(kanan);
    let op = match jenis {
        '^' => OpAtribut::Awalan(nilai),
        '$' => OpAtribut::Akhiran(nilai),
        '*' => OpAtribut::Mengandung(nilai),
        _ => OpAtribut::Sama(nilai),
    };
    Ok(Sederhana::Atribut { nama, op })
}

fn buka_kutip(teks: &str) -> String {
    let b = teks.as_bytes();
    if b.len() >= 2 {
        let awal = b[0];
        let akhir = b[b.len() - 1];
        if (awal == b'"' || awal == b'\'') && awal == akhir {
            return teks[1..teks.len() - 1].to_owned();
        }
    }
    teks.to_owned()
}

fn urai_nth(isi: &str) -> Result<Pola, GalatSelektor> {
    let t: String = isi
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    match t.as_str() {
        "odd" => return Ok(Pola { a: 2, b: 1 }),
        "even" => return Ok(Pola { a: 2, b: 0 }),
        "" => return Err(GalatSelektor::baru(":nth-child() kosong")),
        _ => {}
    }
    if let Some(pos) = t.find('n') {
        let a = match &t[..pos] {
            "" | "+" => 1,
            "-" => -1,
            lain => lain
                .parse::<i64>()
                .map_err(|_| GalatSelektor::baru(format!("koefisien '{lain}' bukan angka")))?,
        };
        let sisa = &t[pos + 1..];
        let b = if sisa.is_empty() {
            0
        } else {
            let (tanda, angka) = match sisa.strip_prefix('+') {
                Some(x) => (1, x),
                None => match sisa.strip_prefix('-') {
                    Some(x) => (-1, x),
                    None => {
                        return Err(GalatSelektor::baru(format!(
                            "':nth-child({t})' butuh tanda + atau - sebelum b"
                        )));
                    }
                },
            };
            tanda
                * angka
                    .parse::<i64>()
                    .map_err(|_| GalatSelektor::baru(format!("'{angka}' bukan angka")))?
        };
        return Ok(Pola { a, b });
    }
    let b = t
        .parse::<i64>()
        .map_err(|_| GalatSelektor::baru(format!("':nth-child({t})' tidak bisa diurai")))?;
    Ok(Pola { a: 0, b })
}

impl Document {
    /// Seluruh elemen yang cocok dengan selektor, urut dokumen.
    pub fn pilih(&self, selektor: &str) -> Result<Vec<NodeId>, GalatSelektor> {
        let daftar = urai(selektor)?;
        let mut out = Vec::new();
        for (id, node) in self.nodes().iter().enumerate() {
            if node.tag().is_none() {
                continue;
            }
            if daftar.iter().any(|k| cocok_kompleks(self, id, &k.bagian)) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Elemen yang cocok **di dalam** subtree `akar`, urut dokumen.
    ///
    /// Dipakai `DOM.querySelector` yang membatasi pencarian ke satu node,
    /// bukan seluruh dokumen.
    pub fn pilih_dalam(&self, akar: NodeId, selektor: &str) -> Result<Vec<NodeId>, GalatSelektor> {
        if self.node(akar).is_none() {
            return Err(GalatSelektor::baru(format!("node {akar} tidak ada")));
        }
        let daftar = urai(selektor)?;
        let mut out = Vec::new();
        for (id, node) in self.nodes().iter().enumerate() {
            if node.tag().is_none() || !di_bawah(self, akar, id) {
                continue;
            }
            if daftar.iter().any(|k| cocok_kompleks(self, id, &k.bagian)) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Elemen pertama yang cocok, atau `None`.
    pub fn pilih_satu(&self, selektor: &str) -> Result<Option<NodeId>, GalatSelektor> {
        let daftar = urai(selektor)?;
        for (id, node) in self.nodes().iter().enumerate() {
            if node.tag().is_none() {
                continue;
            }
            if daftar.iter().any(|k| cocok_kompleks(self, id, &k.bagian)) {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Apakah satu node memenuhi selektor.
    pub fn cocok(&self, id: NodeId, selektor: &str) -> Result<bool, GalatSelektor> {
        let daftar = urai(selektor)?;
        Ok(daftar.iter().any(|k| cocok_kompleks(self, id, &k.bagian)))
    }
}

fn cocok_kompleks(doc: &Document, id: NodeId, bagian: &[(Kombinator, Vec<Sederhana>)]) -> bool {
    let Some((komb, syarat)) = bagian.last() else {
        return false;
    };
    if !cocok_gabungan(doc, id, syarat) {
        return false;
    }
    if bagian.len() == 1 {
        return true;
    }
    let sisa = &bagian[..bagian.len() - 1];
    match komb {
        Kombinator::Anak => doc
            .node(id)
            .and_then(|n| n.parent)
            .is_some_and(|p| cocok_kompleks(doc, p, sisa)),
        Kombinator::Turunan => {
            let mut kini = doc.node(id).and_then(|n| n.parent);
            while let Some(p) = kini {
                if cocok_kompleks(doc, p, sisa) {
                    return true;
                }
                kini = doc.node(p).and_then(|n| n.parent);
            }
            false
        }
    }
}

fn cocok_gabungan(doc: &Document, id: NodeId, syarat: &[Sederhana]) -> bool {
    let Some(n) = doc.node(id) else {
        return false;
    };
    if n.tag().is_none() {
        return false;
    }
    syarat.iter().all(|s| cocok_satu(doc, id, n, s))
}

fn cocok_satu(doc: &Document, id: NodeId, n: &Node, s: &Sederhana) -> bool {
    match s {
        Sederhana::Tag(t) => n.tag().is_some_and(|tag| tag.eq_ignore_ascii_case(t)),
        Sederhana::Id(v) => n.attr("id") == Some(v.as_str()),
        Sederhana::Kelas(k) => n
            .attr("class")
            .is_some_and(|c| c.split_ascii_whitespace().any(|x| x == k)),
        Sederhana::Atribut { nama, op } => cocok_atribut(n, nama, op),
        Sederhana::PertamaAnak => posisi_anak(doc, id) == Some(1),
        Sederhana::AnakKe(pola) => posisi_anak(doc, id).is_some_and(|p| pola.cocok(p)),
        Sederhana::Bukan(dalam) => !cocok_satu(doc, id, n, dalam),
    }
}

fn cocok_atribut(n: &Node, nama: &str, op: &OpAtribut) -> bool {
    let Some(nilai) = n.attr(nama) else {
        return false;
    };
    match op {
        OpAtribut::Ada => true,
        OpAtribut::Sama(v) => nilai == v,
        OpAtribut::Awalan(v) => nilai.starts_with(v.as_str()),
        OpAtribut::Akhiran(v) => nilai.ends_with(v.as_str()),
        OpAtribut::Mengandung(v) => nilai.contains(v.as_str()),
    }
}

/// Apakah `id` berada di dalam subtree `akar` (termasuk `akar` sendiri).
fn di_bawah(doc: &Document, akar: NodeId, id: NodeId) -> bool {
    if akar == id {
        return true;
    }
    let mut kini = doc.node(id).and_then(|n| n.parent);
    while let Some(p) = kini {
        if p == akar {
            return true;
        }
        kini = doc.node(p).and_then(|n| n.parent);
    }
    false
}

/// Posisi 1-based di antara saudara **elemen**; node teks tidak dihitung.
fn posisi_anak(doc: &Document, id: NodeId) -> Option<i64> {
    let induk = doc.node(id)?.parent?;
    let p = doc.node(induk)?;
    let mut pos = 0i64;
    for &c in &p.children {
        if doc.node(c).is_some_and(|n| n.tag().is_some()) {
            pos += 1;
            if c == id {
                return Some(pos);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dok(html: &str) -> Document {
        Document::parse_html(html).expect("html uji harus terurai")
    }

    fn tag_dari(doc: &Document, ids: &[NodeId]) -> Vec<String> {
        ids.iter()
            .map(|id| {
                doc.node(*id)
                    .and_then(|n| n.tag())
                    .unwrap_or("?")
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn memilih_tag_kelas_dan_id() {
        let doc = dok(
            "<div class=\"kartu utama\"><p class=\"isi\">a</p><span id=\"target\">b</span></div>",
        );
        assert_eq!(tag_dari(&doc, &doc.pilih("p").unwrap()), vec!["p"]);
        assert_eq!(tag_dari(&doc, &doc.pilih(".kartu").unwrap()), vec!["div"]);
        assert_eq!(tag_dari(&doc, &doc.pilih(".utama").unwrap()), vec!["div"]);
        assert_eq!(tag_dari(&doc, &doc.pilih("#target").unwrap()), vec!["span"]);
        // Kelas dicocokkan utuh, bukan potongan kata.
        assert!(doc.pilih(".kart").unwrap().is_empty());
        // Gabungan menuntut semuanya sekaligus.
        assert_eq!(
            tag_dari(&doc, &doc.pilih("div.kartu.utama").unwrap()),
            vec!["div"]
        );
        assert!(doc.pilih("p.kartu").unwrap().is_empty());
    }

    #[test]
    fn memilih_lewat_operator_atribut() {
        let doc = dok("<a href=\"https://contoh.id/a.png\" data-x=\"1\">a</a>\
             <a href=\"http://lain.id/b.jpg\">b</a>\
             <img src=\"gambar.png\">");
        let satu = |sel: &str| doc.pilih(sel).map(|v| tag_dari(&doc, &v)).unwrap();
        assert_eq!(satu("[data-x]"), vec!["a"]);
        assert_eq!(satu("[href^=https]"), vec!["a"]);
        assert_eq!(satu("[href$=.png]"), vec!["a"]);
        assert_eq!(satu("[href*=.id]"), vec!["a", "a"]);
        assert_eq!(satu("[href=\"https://contoh.id/a.png\"]"), vec!["a"]);
        // Nama atribut tidak membedakan besar kecil huruf; nilai iya.
        assert_eq!(satu("[DATA-X=\"1\"]"), vec!["a"]);
        assert!(doc.pilih("[DATA-X=\"2\"]").unwrap().is_empty());
    }

    #[test]
    fn kombinator_keturunan_dan_anak() {
        let doc = dok("<div id=\"luar\"><section><p>dalam</p></section></div>\
             <div><p>saudara</p></div>");
        let keturunan = doc.pilih("#luar p").unwrap();
        assert_eq!(keturunan.len(), 1, "hanya p di dalam #luar");
        assert_eq!(doc.teks(keturunan[0]), "dalam");

        let anak = doc.pilih("section > p").unwrap();
        assert_eq!(anak.len(), 1);

        // `div > p` di sini harus match SATU node: p "saudara" memang anak
        // langsung div. Kalau `>` keliru jatuh jadi keturunan, hasilnya dua
        // dan tes ini tetap merah.
        let langsung = doc.pilih("div > p").unwrap();
        assert_eq!(langsung.len(), 1, "hanya p saudara, bukan p di section");
        assert_eq!(doc.teks(langsung[0]), "saudara");
        assert_eq!(doc.pilih("div section > p").unwrap().len(), 1);
    }

    #[test]
    fn pseudo_kelas_anak() {
        let doc = dok("<ul><li>1</li><li>2</li><li>3</li><li>4</li><li>5</li></ul>");
        let teks = |sel: &str| -> Vec<String> {
            doc.pilih(sel)
                .unwrap()
                .into_iter()
                .map(|id| doc.teks(id))
                .collect()
        };
        assert_eq!(teks("li:first-child"), vec!["1"]);
        assert_eq!(teks("li:nth-child(2)"), vec!["2"]);
        assert_eq!(teks("li:nth-child(odd)"), vec!["1", "3", "5"]);
        assert_eq!(teks("li:nth-child(even)"), vec!["2", "4"]);
        assert_eq!(teks("li:nth-child(2n+1)"), vec!["1", "3", "5"]);
        assert_eq!(teks("li:nth-child(3n)"), vec!["3"]);
        assert_eq!(teks("li:nth-child(-n+2)"), vec!["1", "2"]);
    }

    #[test]
    fn pseudo_not_dan_daftar_selektor() {
        let doc = dok("<div><p class=\"a\">x</p><p>y</p><span class=\"a\">z</span></div>");
        let teks = |sel: &str| -> Vec<String> {
            doc.pilih(sel)
                .unwrap()
                .into_iter()
                .map(|id| doc.teks(id))
                .collect()
        };
        assert_eq!(teks("p:not(.a)"), vec!["y"]);
        assert_eq!(teks(".a, span"), vec!["x", "z"]);
        assert_eq!(teks("p.a, p:not(.a)"), vec!["x", "y"]);
    }

    #[test]
    fn universal_dan_urutan_dokumen() {
        let doc = dok("<div><b>1</b><i>2</i><u>3</u></div>");
        let semua = doc.pilih("*").unwrap();
        // html, head, body, div, b, i, u
        assert_eq!(
            tag_dari(&doc, &semua),
            vec!["html", "head", "body", "div", "b", "i", "u"]
        );
        assert_eq!(
            tag_dari(&doc, &doc.pilih("div > *").unwrap()),
            vec!["b", "i", "u"]
        );
    }

    #[test]
    fn pilih_satu_dan_cocok() {
        let doc = dok("<div><p id=\"satu\">a</p><p id=\"dua\">b</p></div>");
        let satu = doc.pilih_satu("p").unwrap().expect("harus ada");
        assert_eq!(doc.node(satu).unwrap().attr("id"), Some("satu"));
        assert!(doc.cocok(satu, "p:first-child").unwrap());
        assert!(
            doc.cocok(satu, "p:last-child").is_err(),
            ":last-child belum didukung, jadi harus galat bukan false"
        );
        assert!(doc.pilih_satu("article").unwrap().is_none());
    }

    #[test]
    fn selektor_salah_dilaporkan_bukan_diam() {
        let doc = dok("<p>x</p>");
        for buruk in [
            "",
            "   ",
            "p,",
            "div >",
            "> p",
            "[href",
            "p:nth-child(",
            "p:hover",
            "p:nth-child(2n+)",
            "p::before",
            "p + p",
        ] {
            assert!(
                doc.pilih(buruk).is_err(),
                "selektor {buruk:?} seharusnya ditolak"
            );
        }
    }

    #[test]
    fn spasi_di_sekitar_kombinator_tidak_mengubah_arti() {
        let doc = dok("<div><section><p>x</p></section></div>");
        assert_eq!(doc.pilih("div>section>p").unwrap().len(), 1);
        assert_eq!(doc.pilih("div > section > p").unwrap().len(), 1);
        assert_eq!(doc.pilih("  div   section   p  ").unwrap().len(), 1);
    }

    #[test]
    fn kurung_di_dalam_nilai_atribut_tidak_memotong_selektor() {
        let doc = dok("<a href=\"x(1)\">a</a><b data-y=\"[z]\">b</b>");
        assert_eq!(doc.pilih("[href=\"x(1)\"]").unwrap().len(), 1);
        assert_eq!(doc.pilih("[data-y=\"[z]\"]").unwrap().len(), 1);
    }

    #[test]
    fn pencarian_dibatasi_ke_subtree() {
        let doc = dok("<div id=\"a\"><p>x</p></div><div id=\"b\"><p>y</p></div>");
        let a = doc.pilih_satu("#a").unwrap().expect("#a harus ada");
        let dalam = doc.pilih_dalam(a, "p").unwrap();
        assert_eq!(dalam.len(), 1, "hanya p di dalam #a");
        assert_eq!(doc.teks(dalam[0]), "x");
        // Akar sendiri ikut dihitung, seperti `Element.querySelector`.
        assert_eq!(doc.pilih_dalam(a, "div").unwrap(), vec![a]);
        assert!(
            doc.pilih_dalam(9999, "p").is_err(),
            "node di luar arena harus galat, bukan senyap kosong"
        );
    }
}
