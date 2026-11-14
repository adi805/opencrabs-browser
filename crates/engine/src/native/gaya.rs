//! Gaya terhitung: pewarisan properti dan satuan non-px.
//!
//! Kaskade di [`super::css`] hanya memilih **pemenang per elemen**. Lapisan ini
//! mengubah pemenang itu jadi angka yang bisa dipakai layout: properti yang
//! memang waris (`color`, `font-size`, `font-weight`, `line-height`,
//! `visibility`) diturunkan dari induk, dan nilai bersatuan `em`/`rem`/`%`
//! dihitung terhadap konteks yang benar.
//!
//! Batasnya sengaja sempit: hanya properti yang dibutuhkan pengukuran teks
//! yang dihitung di sini. Properti lain tetap diteruskan apa adanya ke layout
//! lewat [`PohonGaya::deklarasi`], dan properti yang tidak waris **tidak**
//! diturunkan.

use std::collections::HashMap;

use taffy::prelude::{Dimension, LengthPercentage};

use super::css::Kaskade;
use super::dom::{Document, NodeId};

/// Ukuran font awal bila tidak ada satu pun `font-size` di pohon.
pub const UKURAN_FONT_AWAL: f32 = 16.0;

/// Gaya satu elemen setelah pewarisan dan konversi satuan.
#[derive(Debug, Clone, PartialEq)]
pub struct GayaTerhitung {
    /// `color`, waris.
    pub warna: Option<String>,
    /// `font-size` dalam px, waris.
    pub ukuran_font: f32,
    /// `font-weight` sebagai angka, waris.
    pub tebal_font: u16,
    /// `line-height` dalam px, waris.
    pub tinggi_baris: f32,
    /// `visibility`, waris. `hidden` menyembunyikan elemen tapi kotaknya
    /// tetap ada, beda dengan `display:none`.
    pub terlihat: bool,
    /// `text-align`, waris.
    pub perataan: Option<String>,
    /// `font-family` sebagai daftar nama, waris, urut prioritas.
    ///
    /// Disimpan sudah dipecah dan tanpa tanda kutip: lapisan lukis mencari face
    /// per nama, dan mengurai string yang sama berulang kali di dalam loop
    /// glyph hanya membakar waktu tanpa mengubah hasil. Nama generik
    /// (`serif`, `monospace`) tetap disimpan apa adanya supaya daftar yang
    /// diminta bisa dilaporkan utuh; yang tidak punya face dilewati saat
    /// pencarian.
    pub keluarga_font: Vec<String>,
    /// Menandai `line-height` datang dari deklarasi atau warisan. Tanpa ini,
    /// elemen yang menaikkan `font-size` tetap memakai `line-height` induk,
    /// padahal `normal` di CSS ikut ukuran font elemen itu sendiri.
    tinggi_baris_diatur: bool,
}

impl Default for GayaTerhitung {
    fn default() -> Self {
        let ukuran_font = UKURAN_FONT_AWAL;
        Self {
            warna: None,
            ukuran_font,
            tebal_font: 400,
            tinggi_baris: ukuran_font * 1.2,
            terlihat: true,
            perataan: None,
            keluarga_font: Vec::new(),
            tinggi_baris_diatur: false,
        }
    }
}

/// Konteks yang dibutuhkan untuk menghitung satuan relatif.
#[derive(Debug, Clone, Copy)]
pub struct Konteks {
    /// `em` dihitung terhadap ini: ukuran font elemen induk.
    pub ukuran_font: f32,
    /// `rem` dihitung terhadap ini: ukuran font elemen akar.
    pub ukuran_font_akar: f32,
}

/// Gaya terhitung seluruh elemen satu dokumen.
#[derive(Debug, Clone)]
pub struct PohonGaya {
    gaya: HashMap<NodeId, GayaTerhitung>,
    deklarasi: HashMap<NodeId, Vec<(String, String)>>,
    /// Ukuran font elemen akar, dasar semua `rem`.
    akar: f32,
}

impl Default for PohonGaya {
    fn default() -> Self {
        Self {
            gaya: HashMap::new(),
            deklarasi: HashMap::new(),
            akar: UKURAN_FONT_AWAL,
        }
    }
}

impl PohonGaya {
    /// Menghitung gaya seluruh elemen dokumen, akar ke daun.
    ///
    /// Dua lintasan: lintasan pertama menghitung pohon dengan `rem` masih
    /// menunjuk ukuran font awal, lalu ukuran font elemen akar (`html`)
    /// diambil dari hasilnya dan pohon dihitung ulang. Tanpa lintasan kedua,
    /// `html { font-size: 10px }` tidak berpengaruh ke `rem` mana pun, karena
    /// nilainya baru diketahui setelah akar selesai dihitung.
    #[must_use]
    pub fn hitung(doc: &Document, kaskade: &Kaskade) -> Self {
        Self::hitung_dari(doc, kaskade, doc.root())
    }

    /// Seperti [`Self::hitung`], tapi dimulai dari `akar_dokumen`.
    ///
    /// Dipakai dokumen `<iframe>`: pohon gaya dihitung dari akar dokumen anak,
    /// bukan dari akar arena. Tanpa parameter ini, layout anak akan mewarisi
    /// gaya dari dokumen induk yang bukan induknya.
    #[must_use]
    pub fn hitung_dari(doc: &Document, kaskade: &Kaskade, akar_dokumen: NodeId) -> Self {
        let mut coba = Self::default();
        coba.turun(
            doc,
            kaskade,
            akar_dokumen,
            GayaTerhitung::default(),
            UKURAN_FONT_AWAL,
        );
        let akar = coba
            .elemen_akar(doc, akar_dokumen)
            .and_then(|id| coba.gaya(id))
            .map_or(UKURAN_FONT_AWAL, |g| g.ukuran_font);

        let mut pohon = Self {
            akar,
            ..Self::default()
        };
        pohon.turun(doc, kaskade, akar_dokumen, GayaTerhitung::default(), akar);
        pohon
    }

    /// Elemen akar dokumen, yaitu elemen pertama di bawah node dokumen
    /// (`html` pada dokumen normal). `rem` diukur terhadap elemen ini.
    fn elemen_akar(&self, doc: &Document, akar_dokumen: NodeId) -> Option<NodeId> {
        doc.node(akar_dokumen)?
            .children
            .iter()
            .copied()
            .find(|&id| doc.node(id).is_some_and(|n| n.tag().is_some()))
    }

    fn turun(
        &mut self,
        doc: &Document,
        kaskade: &Kaskade,
        id: NodeId,
        warisan: GayaTerhitung,
        akar: f32,
    ) {
        let deklarasi = kaskade.menang(doc, id);
        let gaya = terapkan(&deklarasi, &warisan, akar);
        self.gaya.insert(id, gaya.clone());
        self.deklarasi.insert(id, deklarasi);

        // Anak komposit: pohon terang ditambah isi shadow root, kalau ada.
        // Gaya waris menyeberang ke shadow root seperti di browser, dan
        // itulah kenapa jalurnya sama dengan anak biasa.
        for anak in doc.anak_tata(id) {
            self.turun(doc, kaskade, anak, gaya.clone(), akar);
        }
    }

    /// Salinan gaya terhitung seluruh elemen, untuk dilaporkan ke lapisan
    /// lain tanpa memberi mereka akses ke pohon internal.
    #[must_use]
    pub fn salin_terhitung(&self) -> HashMap<NodeId, GayaTerhitung> {
        self.gaya.clone()
    }

    /// Gaya satu elemen.
    #[must_use]
    pub fn gaya(&self, id: NodeId) -> Option<&GayaTerhitung> {
        self.gaya.get(&id)
    }

    /// Ukuran font elemen akar, dasar semua `rem` di pohon ini.
    #[must_use]
    pub fn ukuran_font_akar(&self) -> f32 {
        self.akar
    }

    /// Deklarasi pemenang satu elemen, **tanpa** pewarisan. Dipakai lapisan
    /// layout untuk properti yang tidak waris (padding, margin, display).
    #[must_use]
    pub fn deklarasi(&self, id: NodeId) -> &[(String, String)] {
        self.deklarasi.get(&id).map_or(&[], Vec::as_slice)
    }
}

/// Menerapkan deklarasi pemenang satu elemen di atas gaya induknya.
///
/// Hanya properti yang memang waris di CSS yang dibawa dari `warisan`.
/// `padding`, `margin`, `display`, `width`, `height` sengaja **tidak**
/// dibawa, karena di CSS properti itu tidak waris: anak yang tidak punya
/// deklarasi sendiri kembali ke nilai awalnya, bukan meniru induk.
fn terapkan(deklarasi: &[(String, String)], warisan: &GayaTerhitung, akar: f32) -> GayaTerhitung {
    let mut gaya = warisan.clone();

    for (nama, nilai) in deklarasi {
        match nama.as_str() {
            "color" => gaya.warna = Some(nilai.clone()),
            "font-size" => {
                // `em` pada font-size diukur terhadap font-size induk, bukan
                // terhadap dirinya sendiri, jadi konteksnya `warisan`.
                let konteks = Konteks {
                    ukuran_font: warisan.ukuran_font,
                    ukuran_font_akar: akar,
                };
                if let Some(v) = px_relatif(nilai, konteks) {
                    gaya.ukuran_font = v;
                }
            }
            "font-weight" => {
                if let Some(w) = tebal_font(nilai) {
                    gaya.tebal_font = w;
                }
            }
            "line-height" => {
                let konteks = Konteks {
                    ukuran_font: gaya.ukuran_font,
                    ukuran_font_akar: akar,
                };
                if let Some(v) = px_relatif(nilai, konteks) {
                    gaya.tinggi_baris = v;
                    gaya.tinggi_baris_diatur = true;
                } else if let Ok(kali) = nilai.trim().parse::<f32>() {
                    // `line-height: 1.5` itu pengali, bukan 1.5px.
                    gaya.tinggi_baris = kali * gaya.ukuran_font;
                    gaya.tinggi_baris_diatur = true;
                }
            }
            "visibility" => {
                gaya.terlihat = !nilai.trim().eq_ignore_ascii_case("hidden");
            }
            "text-align" => gaya.perataan = Some(nilai.clone()),
            "font-family" => {
                let daftar = keluarga_font(nilai);
                if !daftar.is_empty() {
                    gaya.keluarga_font = daftar;
                }
            }
            _ => {}
        }
    }

    if !gaya.tinggi_baris_diatur {
        gaya.tinggi_baris = gaya.ukuran_font * 1.2;
    }
    gaya
}

/// `font-family` dipecah per koma, tanda kutip pembungkus dibuang.
///
/// Nama yang mengandung spasi boleh tanpa kutip di CSS (`font-family: Times New
/// Roman`), dan memotongnya per kata akan menghasilkan tiga keluarga yang tidak
/// ada. Karena itu pemisahnya koma saja, bukan spasi.
#[must_use]
pub fn keluarga_font(nilai: &str) -> Vec<String> {
    nilai
        .split(',')
        .map(|bagian| {
            let t = bagian.trim();
            let t = t
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| t.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(t);
            t.trim().to_string()
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// `font-weight`: angka biasa, atau kata kunci yang punya padanan pasti.
/// `bolder`/`lighter` ditolak karena hasilnya bergantung nilai induk.
fn tebal_font(nilai: &str) -> Option<u16> {
    match nilai.trim().to_ascii_lowercase().as_str() {
        "normal" => Some(400),
        "bold" => Some(700),
        lain => lain.parse::<u16>().ok(),
    }
}

/// `em` terhadap [`Konteks::ukuran_font`], `rem` terhadap
/// [`Konteks::ukuran_font_akar`]. Selain px/em/rem ditolak, bukan ditebak.
#[must_use]
pub fn px_relatif(nilai: &str, konteks: Konteks) -> Option<f32> {
    let nilai = nilai.trim();
    if let Some(n) = nilai.strip_suffix("px") {
        return n.trim().parse::<f32>().ok();
    }
    if let Some(n) = nilai.strip_suffix("rem") {
        return n
            .trim()
            .parse::<f32>()
            .ok()
            .map(|v| v * konteks.ukuran_font_akar);
    }
    if let Some(n) = nilai.strip_suffix("em") {
        return n
            .trim()
            .parse::<f32>()
            .ok()
            .map(|v| v * konteks.ukuran_font);
    }
    None
}

/// `width`/`height`: px dan `em`/`rem` jadi panjang tetap, `%` tetap persen
/// supaya `taffy` yang menghitungnya terhadap kotak induk.
#[must_use]
pub fn dimensi(nilai: &str, konteks: Konteks) -> Option<Dimension> {
    let nilai = nilai.trim();
    if let Some(n) = nilai.strip_suffix('%') {
        let persen = n.trim().parse::<f32>().ok()?;
        return Some(Dimension::percent(persen / 100.0));
    }
    px_relatif(nilai, konteks).map(Dimension::length)
}

/// `padding`/`margin`: px, `em`/`rem`, dan `%` sebagai [`LengthPercentage`].
#[must_use]
pub fn panjang(nilai: &str, konteks: Konteks) -> Option<LengthPercentage> {
    let nilai = nilai.trim();
    if let Some(n) = nilai.strip_suffix('%') {
        let persen = n.trim().parse::<f32>().ok()?;
        return Some(LengthPercentage::percent(persen / 100.0));
    }
    px_relatif(nilai, konteks).map(LengthPercentage::length)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::Document;

    fn doc(html: &str) -> Document {
        Document::parse_html(html).expect("html harus terurai")
    }

    fn pohon(html: &str) -> (Document, PohonGaya) {
        let d = doc(html);
        let kaskade = Kaskade::kumpulkan(&d);
        let p = PohonGaya::hitung(&d, &kaskade);
        (d, p)
    }

    #[test]
    fn properti_teks_yang_waris_turun_ke_anak() {
        let (d, p) = pohon(
            "<html><head><style>\
             body { color: #ff0000; font-size: 20px; font-weight: 700; line-height: 30px; \
             visibility: hidden }\
             </style></head><body><div id=\"a\"><span id=\"b\">x</span></div></body></html>",
        );
        let a = p
            .gaya(d.cari_id("a").unwrap())
            .expect("anak harus punya gaya");
        let b = p
            .gaya(d.cari_id("b").unwrap())
            .expect("cucu harus punya gaya");
        for g in [a, b] {
            assert_eq!(g.warna.as_deref(), Some("#ff0000"), "color harus waris");
            assert_eq!(g.ukuran_font, 20.0, "font-size harus waris");
            assert_eq!(g.tebal_font, 700, "font-weight harus waris");
            assert_eq!(g.tinggi_baris, 30.0, "line-height harus waris");
            assert!(!g.terlihat, "visibility harus waris");
        }
    }

    #[test]
    fn em_dihitung_dari_ukuran_font_induk() {
        let (d, p) = pohon(
            "<html><head><style>\
             body { font-size: 20px }\
             #a { width: 2em; font-size: 1.5em }\
             </style></head><body><div id=\"a\"></div></body></html>",
        );
        let a = p.gaya(d.cari_id("a").unwrap()).unwrap();
        assert_eq!(a.ukuran_font, 30.0, "1.5em dari induk 20px harus jadi 30px");
        let konteks = Konteks {
            ukuran_font: 20.0,
            ukuran_font_akar: UKURAN_FONT_AWAL,
        };
        assert_eq!(
            dimensi("2em", konteks),
            Some(Dimension::length(40.0)),
            "2em dari induk 20px harus jadi 40px"
        );
    }

    #[test]
    fn rem_dihitung_dari_ukuran_font_akar() {
        let (d, p) = pohon(
            "<html><head><style>\
             html { font-size: 10px }\
             #a { font-size: 40px }\
             #b { width: 2rem }\
             </style></head><body><div id=\"a\"><div id=\"b\"></div></div></body></html>",
        );
        let a = p.gaya(d.cari_id("a").unwrap()).unwrap();
        let b = p.gaya(d.cari_id("b").unwrap()).unwrap();
        assert_eq!(a.ukuran_font, 40.0, "font-size induk 40px");
        assert_eq!(b.ukuran_font, 40.0, "font-size tetap waris");
        assert_eq!(
            p.ukuran_font_akar(),
            10.0,
            "akar harus diambil dari deklarasi font-size pada `html`, bukan 16 bawaan"
        );
        let konteks = Konteks {
            ukuran_font: b.ukuran_font,
            ukuran_font_akar: p.ukuran_font_akar(),
        };
        assert_eq!(
            dimensi("2rem", konteks),
            Some(Dimension::length(20.0)),
            "2rem dari akar 10px harus jadi 20px, bukan 80px"
        );
    }

    #[test]
    fn persen_lebar_dan_tinggi_tetap_persen_untuk_taffy() {
        let konteks = Konteks {
            ukuran_font: UKURAN_FONT_AWAL,
            ukuran_font_akar: UKURAN_FONT_AWAL,
        };
        assert_eq!(
            dimensi("50%", konteks),
            Some(Dimension::percent(0.5)),
            "persen harus diteruskan ke taffy, bukan diubah jadi px"
        );
        assert_eq!(dimensi("100%", konteks), Some(Dimension::percent(1.0)));
        assert_eq!(
            panjang("10%", konteks),
            Some(LengthPercentage::percent(0.1))
        );
    }

    #[test]
    fn properti_tidak_waris_tidak_bocor_ke_anak() {
        let (d, p) = pohon(
            "<html><head><style>\
             #a { padding: 8px; margin-top: 4px; display: flex }\
             </style></head><body><div id=\"a\"><span id=\"b\"></span></div></body></html>",
        );
        let a = p.deklarasi(d.cari_id("a").unwrap());
        let b = p.deklarasi(d.cari_id("b").unwrap());
        assert!(
            a.iter().any(|(n, _)| n == "padding-top"),
            "induk harus punya padding hasil shorthand: {a:?}"
        );
        assert!(
            b.iter().all(|(n, _)| !n.starts_with("padding")
                && !n.starts_with("margin")
                && n != "display"),
            "padding/margin/display tidak boleh waris: {b:?}"
        );
    }
}
