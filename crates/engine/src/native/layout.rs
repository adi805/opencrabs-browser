//! Penempatan kotak dari pohon [`Document`] lewat `taffy`.
//!
//! Gaya tiap elemen datang dari [`super::css::Kaskade`]: blok `<style>` di
//! dokumen, diurutkan menurut `!important`, sebaris, spesifisitas, lalu urutan
//! sumber. Atribut `style` tetap dibaca sebagai deklarasi sebaris oleh
//! kaskade, bukan oleh modul ini.
//!
//! Properti yang dikenali `taffy` di sini: `display`, `width`, `height`,
//! `padding-*`, `margin-*`, `flex-direction`. Setiap properti lain dicatat
//! sebagai diabaikan, bukan diam-diam dipakai dengan arti lain.
//!
//! Node teks menghasilkan kotak: ia jadi daun `taffy` yang membawa
//! [`KonteksTeks`], dan `compute_layout_with_measure` mengukurnya lewat
//! [`super::teks`]. Lebar teks dibungkus pada lebar yang tersedia, jadi satu
//! paragraf di dalam kotak sempit menghasilkan beberapa baris, bukan satu
//! baris yang melewati induknya.

use std::collections::HashMap;

use taffy::compute_leaf_layout;
use taffy::prelude::{
    AvailableSpace, Dimension, Display, FlexDirection, LengthPercentageAuto, Size, Style, TaffyTree,
};

use super::NativeError;
use super::css::{Kaskade, LembarLuar};
use super::dom::{Document, NodeId, NodeKind};
use super::gaya::{GayaTerhitung, Konteks, PohonGaya, dimensi, panjang};

/// Ukuran jendela tempat layout dihitung, dalam piksel CSS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
}

impl Viewport {
    #[must_use]
    pub fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

/// Satu kotak hasil layout, koordinatnya absolut terhadap viewport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Kotak {
    pub node: NodeId,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Default)]
pub struct LaporanLayout {
    pub kotak: Vec<Kotak>,
    /// Gaya yang menang per elemen, sudah termasuk `style` sebaris. Disimpan
    /// supaya lapisan lukis membaca angka yang **sama** dengan yang dipakai
    /// layout, bukan mengurai ulang atribut dan bisa berbeda pendapat.
    pub gaya: HashMap<NodeId, Vec<(String, String)>>,
    /// Deklarasi yang bentuknya dikenali tapi nilainya tidak didukung,
    /// mis. `padding: 1em`. Ini laporan jujur, bukan log.
    pub diabaikan: Vec<String>,
    /// Gaya terhitung per elemen: hasil pewarisan dan konversi satuan.
    /// Lapisan lukis dan pengukur teks membaca angka dari sini supaya tidak
    /// menghitung ulang dan berbeda pendapat dengan layout.
    pub terhitung: HashMap<NodeId, GayaTerhitung>,
    /// Teks node setelah perataan spasi, persis seperti yang diukur layout.
    /// Lapisan lukis membungkusnya dengan `teks::bungkus` pada lebar kotaknya,
    /// jadi yang dilukis adalah teks yang sama yang menghasilkan kotak itu.
    pub teks: HashMap<NodeId, String>,
    /// Node teks yang dinyatakan tidak terlihat oleh gaya warisan.
    pub teks_tersembunyi: std::collections::HashSet<NodeId>,
    /// Hal yang sengaja tidak dijalankan oleh kaskade: at-rule dan
    /// `<link rel=stylesheet>`.
    pub catatan_kaskade: Vec<String>,
}

impl LaporanLayout {
    #[must_use]
    pub fn kotak_untuk(&self, node: NodeId) -> Option<Kotak> {
        self.kotak.iter().copied().find(|k| k.node == node)
    }

    /// Deklarasi pemenang untuk satu properti pada satu elemen.
    #[must_use]
    pub fn properti(&self, node: NodeId, nama: &str) -> Option<&str> {
        self.gaya
            .get(&node)?
            .iter()
            .find(|(k, _)| k == nama)
            .map(|(_, v)| v.as_str())
    }

    /// Gaya terhitung satu elemen, sudah termasuk pewarisan dan satuan.
    #[must_use]
    pub fn gaya_terhitung(&self, node: NodeId) -> Option<&GayaTerhitung> {
        self.terhitung.get(&node)
    }

    /// Kotak terbesar menurut luas, berguna untuk memilih elemen utama
    /// sebuah halaman tanpa selektor CSS.
    #[must_use]
    pub fn kotak_terluas(&self) -> Option<Kotak> {
        self.kotak
            .iter()
            .copied()
            .max_by(|a, b| (a.width * a.height).total_cmp(&(b.width * b.height)))
    }
}

/// Konteks satu daun teks: isi yang sudah dirapikan, dan metrik yang
/// dibutuhkan untuk mengukurnya.
///
/// Disimpan di pohon `taffy` sebagai node context, bukan dihitung ulang di
/// dalam measure callback: callback itu bisa dipanggil berkali-kali untuk
/// simpul yang sama selama satu lintasan, dan mengurai string setiap kali
/// hanya membakar waktu tanpa mengubah hasil.
#[derive(Debug, Clone)]
pub struct KonteksTeks {
    /// Isi node setelah [`super::teks::rapikan`].
    pub teks: String,
    /// `font-size` terhitung, dalam piksel.
    pub ukuran_font: f32,
    /// `line-height` terhitung, dalam piksel.
    pub tinggi_baris: f32,
}

impl KonteksTeks {
    /// Ukur daun teks untuk ruang yang diberikan `taffy`.
    ///
    /// Lebar pembungkusan diambil dari lebar yang sudah pasti (kalau induk
    /// memaksanya) atau dari ruang yang tersedia. `MaxContent` dan
    /// `MinContent` tidak membungkus: itu permintaan lebar intrinsik, dan
    /// membungkus di sana akan melaporkan lebar yang lebih kecil dari isinya.
    fn ukur(&self, dikenal: Size<Option<f32>>, tersedia: Size<AvailableSpace>) -> Size<f32> {
        let lebar_maks = dikenal.width.or(match tersedia.width {
            AvailableSpace::Definite(w) => Some(w),
            AvailableSpace::MaxContent | AvailableSpace::MinContent => None,
        });
        let hasil =
            super::teks::ukuran(&self.teks, self.ukuran_font, self.tinggi_baris, lebar_maks);
        Size {
            width: dikenal.width.unwrap_or(hasil.lebar),
            height: dikenal.height.unwrap_or(hasil.tinggi),
        }
    }
}

/// Gaya `taffy` untuk daun teks.
///
/// Ukuran dan posisi sepenuhnya datang dari measure callback, jadi tidak ada
/// satu pun properti ukuran yang dipatok di sini. `flex-shrink` dimatikan
/// supaya taffy tidak memotong lebar teks yang sudah diukur agar muat di
/// baris: teks yang dibungkus lebih baik turun ke baris berikutnya daripada
/// dipepetkan jadi tidak terbaca.
fn taffy_style_teks() -> Style {
    Style {
        flex_shrink: 0.0,
        ..Default::default()
    }
}

/// Menghitung layout seluruh dokumen untuk satu viewport.
///
/// Lembar gaya eksternal tidak ikut: pemanggil yang sudah mengambil CSS dari
/// jaringan memakai [`hitung_dengan`]. Versi ini yang dipakai tes dan jalur
/// tanpa jaringan, jadi halaman dengan `<link rel=stylesheet>` di sini
/// dilaporkan tanpa lembar itu, bukan diterapkan sebagian.
pub fn hitung(doc: &Document, viewport: Viewport) -> Result<LaporanLayout, NativeError> {
    hitung_dengan(doc, viewport, &[])
}

/// Menghitung layout dengan lembar gaya eksternal yang sudah diambil.
pub fn hitung_dengan(
    doc: &Document,
    viewport: Viewport,
    eksternal: &[LembarLuar],
) -> Result<LaporanLayout, NativeError> {
    let mut taffy: TaffyTree<KonteksTeks> = TaffyTree::new();
    let mut peta: HashMap<NodeId, taffy::NodeId> = HashMap::new();
    let mut laporan = LaporanLayout::default();

    let akar_dokumen = doc.root();
    // Kaskade dibangun SEKALI per dokumen. Mengurai stylesheet per elemen
    // akan mengulang pekerjaan yang sama sebanyak jumlah node.
    let kaskade = Kaskade::kumpulkan_dengan(doc, eksternal);
    laporan.catatan_kaskade = kaskade.diabaikan().to_vec();
    // Gaya terhitung juga dibangun sekali: pewarisan turun dari akar, dan
    // `em`/`rem` butuh ukuran font induk yang baru diketahui setelah induk
    // dihitung.
    let pohon = PohonGaya::hitung(doc, &kaskade);
    laporan.terhitung = pohon.salin_terhitung();

    let mut anak_teratas = Vec::new();
    for &anak in &doc
        .node(akar_dokumen)
        .map(|n| n.children.clone())
        .unwrap_or_default()
    {
        if let Some(id) = tempatkan(&mut taffy, doc, &pohon, anak, &mut peta, &mut laporan)? {
            anak_teratas.push(id);
        }
    }

    if anak_teratas.is_empty() {
        return Ok(laporan);
    }

    // Akar SELALU dibungkus wadah selebar viewport, termasuk saat dokumen
    // cuma punya satu elemen teratas. Tanpa wadah, `taffy` tidak memberi akar
    // berlebar `auto` ukuran penuh ruang yang tersedia, jadi `<html>` (dan
    // semua blok di bawahnya) menyusut selebar isinya. Akibatnya `text-align`
    // tidak punya ruang untuk menggeser apa pun: rata kiri, tengah, dan kanan
    // menghasilkan raster yang identik. Di browser, blok `width: auto`
    // mengisi containing block, dan wadah ini yang menjadi containing block
    // itu.
    let gaya_wadah = Style {
        display: Display::Flex,
        flex_direction: FlexDirection::Column,
        size: Size {
            width: Dimension::percent(1.0),
            height: Dimension::auto(),
        },
        ..Default::default()
    };
    let akar = taffy.new_leaf(gaya_wadah).map_err(NativeError::layout)?;
    for anak in anak_teratas {
        taffy.add_child(akar, anak).map_err(NativeError::layout)?;
    }

    // Daun tanpa konteks (elemen kosong) tetap diukur seperti sebelumnya:
    // nol, kecuali gayanya menentukan ukuran. Hanya daun teks yang punya
    // konteks, dan hanya daun yang dipanggil `taffy` di sini.
    taffy
        .compute_layout_with_measure(
            akar,
            Size {
                width: AvailableSpace::Definite(viewport.width),
                height: AvailableSpace::Definite(viewport.height),
            },
            |inputs, _node, konteks, gaya| match konteks {
                Some(teks) => compute_leaf_layout(
                    inputs,
                    gaya,
                    |_, _| 0.0,
                    |dikenal, tersedia| teks.ukur(dikenal, tersedia),
                ),
                None => compute_leaf_layout(inputs, gaya, |_, _| 0.0, |_, _| Size::ZERO),
            },
        )
        .map_err(NativeError::layout)?;

    let mut kotak = Vec::new();
    kumpulkan(&taffy, &peta, doc, akar_dokumen, 0.0, 0.0, &mut kotak)?;
    laporan.kotak = kotak;
    Ok(laporan)
}

fn tempatkan(
    taffy: &mut TaffyTree<KonteksTeks>,
    doc: &Document,
    pohon: &PohonGaya,
    id: NodeId,
    peta: &mut HashMap<NodeId, taffy::NodeId>,
    laporan: &mut LaporanLayout,
) -> Result<Option<taffy::NodeId>, NativeError> {
    let Some(node) = doc.node(id) else {
        return Ok(None);
    };
    if let NodeKind::Text(isi) = &node.kind {
        // Isi elemen raw-text (`style`, `script`, `title`) adalah data,
        // bukan teks halaman: CSS di dalam `<style>` tidak boleh mengambil
        // ruang layout, kalau tidak seluruh isi `body` tergeser ke bawah
        // sebesar satu tinggi baris.
        let ortu_mentah = node
            .parent
            .and_then(|p| doc.node(p))
            .and_then(|ortu| ortu.tag())
            .is_some_and(|tag| matches!(tag, "style" | "script" | "title"));
        if ortu_mentah {
            return Ok(None);
        }
        // Node teks jadi daun berkonteks. Gaya yang dipakai adalah gaya
        // terhitungnya sendiri, yang sudah mewarisi `font-size` dan
        // `line-height` dari induk lewat `PohonGaya`. Teks kosong setelah
        // perataan spasi tidak menghasilkan kotak sama sekali, supaya
        // indentasi HTML tidak menambah tinggi halaman.
        let gaya = pohon.gaya(id).cloned().unwrap_or_default();
        if !gaya.terlihat {
            laporan.teks_tersembunyi.insert(id);
            return Ok(None);
        }
        let rapi = super::teks::rapikan(isi);
        let konteks = KonteksTeks {
            teks: rapi.clone(),
            ukuran_font: gaya.ukuran_font,
            tinggi_baris: gaya.tinggi_baris,
        };
        if konteks.teks.is_empty() {
            return Ok(None);
        }
        laporan.teks.insert(id, rapi);
        let daun = taffy
            .new_leaf_with_context(taffy_style_teks(), konteks)
            .map_err(NativeError::layout)?;
        peta.insert(id, daun);
        return Ok(Some(daun));
    }
    if !matches!(node.kind, NodeKind::Element(_)) {
        // Doctype dan komentar tidak menghasilkan kotak.
        return Ok(None);
    }

    // Deklarasi dan gaya terhitung sama-sama datang dari pohon yang dibangun
    // sekali, bukan diurai ulang di sini. `em` pada padding/lebar diukur
    // terhadap font-size elemen ini sendiri, sesuai CSS.
    let deklarasi = pohon.deklarasi(id).to_vec();
    let konteks = Konteks {
        ukuran_font: pohon
            .gaya(id)
            .map_or(super::gaya::UKURAN_FONT_AWAL, |g| g.ukuran_font),
        ukuran_font_akar: pohon.ukuran_font_akar(),
    };
    let gaya = baca_gaya(&deklarasi, konteks, id, laporan);
    laporan.gaya.insert(id, deklarasi);
    if !gaya.tampil {
        return Ok(None);
    }

    let daun = taffy.new_leaf(gaya.taffy).map_err(NativeError::layout)?;
    peta.insert(id, daun);

    for anak in node.children.clone() {
        if let Some(c) = tempatkan(taffy, doc, pohon, anak, peta, laporan)? {
            taffy.add_child(daun, c).map_err(NativeError::layout)?;
        }
    }
    Ok(Some(daun))
}

fn kumpulkan(
    taffy: &TaffyTree<KonteksTeks>,
    peta: &HashMap<NodeId, taffy::NodeId>,
    doc: &Document,
    id: NodeId,
    ofset_x: f32,
    ofset_y: f32,
    out: &mut Vec<Kotak>,
) -> Result<(), NativeError> {
    let Some(node) = doc.node(id) else {
        return Ok(());
    };

    let (mut x, mut y) = (ofset_x, ofset_y);
    if let Some(tid) = peta.get(&id) {
        let l = taffy.layout(*tid).map_err(NativeError::layout)?;
        x = ofset_x + l.location.x;
        y = ofset_y + l.location.y;
        out.push(Kotak {
            node: id,
            x,
            y,
            width: l.size.width,
            height: l.size.height,
        });
    }

    for anak in node.children.clone() {
        kumpulkan(taffy, peta, doc, anak, x, y, out)?;
    }
    Ok(())
}

struct Gaya {
    taffy: Style,
    tampil: bool,
}

fn baca_gaya(
    deklarasi: &[(String, String)],
    konteks: Konteks,
    id: NodeId,
    laporan: &mut LaporanLayout,
) -> Gaya {
    let mut gaya = Style {
        display: Display::Flex,
        flex_direction: FlexDirection::Column,
        ..Default::default()
    };
    let mut tampil = true;

    for (nama, nilai) in deklarasi {
        let nama = nama.as_str();
        let nilai = nilai.as_str();
        match nama {
            "display" => match nilai {
                "none" => tampil = false,
                "flex" | "block" | "inline-block" | "inline" | "contents" => {}
                lain => laporan.diabaikan.push(format!("node {id}: display:{lain}")),
            },
            "flex-direction" => match nilai {
                "row" => gaya.flex_direction = FlexDirection::Row,
                "column" => gaya.flex_direction = FlexDirection::Column,
                lain => laporan
                    .diabaikan
                    .push(format!("node {id}: flex-direction:{lain}")),
            },
            // px, em, rem, dan `%` semuanya diterima: `%` diteruskan ke taffy
            // sebagai persen supaya dihitung terhadap kotak induk, bukan
            // diubah jadi px dengan tebakan.
            "width" => match dimensi(nilai, konteks) {
                Some(v) => gaya.size.width = v,
                None => laporan.diabaikan.push(format!("node {id}: width:{nilai}")),
            },
            "height" => match dimensi(nilai, konteks) {
                Some(v) => gaya.size.height = v,
                None => laporan.diabaikan.push(format!("node {id}: height:{nilai}")),
            },
            "padding-top" | "padding-right" | "padding-bottom" | "padding-left" => {
                let Some(lp) = panjang(nilai, konteks) else {
                    laporan.diabaikan.push(format!("node {id}: {nama}:{nilai}"));
                    continue;
                };
                match nama {
                    "padding-top" => gaya.padding.top = lp,
                    "padding-right" => gaya.padding.right = lp,
                    "padding-bottom" => gaya.padding.bottom = lp,
                    _ => gaya.padding.left = lp,
                }
            }
            "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
                let Some(v) = panjang_auto(nilai, konteks) else {
                    laporan.diabaikan.push(format!("node {id}: {nama}:{nilai}"));
                    continue;
                };
                match nama {
                    "margin-top" => gaya.margin.top = v,
                    "margin-right" => gaya.margin.right = v,
                    "margin-bottom" => gaya.margin.bottom = v,
                    _ => gaya.margin.left = v,
                }
            }
            // Properti waris sudah dipakai lapisan gaya; mencatatnya sebagai
            // "tidak didukung" akan jadi laporan palsu.
            "color" | "font-size" | "font-weight" | "line-height" | "visibility" | "text-align"
            | "background" | "background-color" => {}
            lain => laporan
                .diabaikan
                .push(format!("node {id}: properti `{lain}` tidak didukung")),
        }
    }

    Gaya {
        taffy: gaya,
        tampil,
    }
}

/// `auto` dan `0` diterima untuk margin; selain px/em/rem/% ditolak dan
/// dilaporkan.
fn panjang_auto(nilai: &str, konteks: Konteks) -> Option<LengthPercentageAuto> {
    let nilai = nilai.trim();
    if nilai.eq_ignore_ascii_case("auto") {
        return Some(LengthPercentageAuto::auto());
    }
    if nilai == "0" {
        return Some(LengthPercentageAuto::length(0.0));
    }
    panjang(nilai, konteks).map(LengthPercentageAuto::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::Document;

    fn doc(html: &str) -> Document {
        Document::parse_html(html).expect("html harus terurai")
    }

    #[test]
    fn dua_div_bertumpuk() {
        let d = doc(
            "<html><body><div id=\"a\" style=\"width:100px;height:40px\"></div>\
             <div id=\"b\" style=\"width:100px;height:40px\"></div></body></html>",
        );
        let laporan = hitung(&d, Viewport::new(1280.0, 800.0)).unwrap();
        let a = laporan.kotak_untuk(d.cari_id("a").unwrap()).unwrap();
        let b = laporan.kotak_untuk(d.cari_id("b").unwrap()).unwrap();
        assert_eq!(a.height, 40.0);
        assert_eq!(b.y, 40.0, "div kedua mulai tepat di bawah yang pertama");
    }

    #[test]
    fn flex_row_menyamping() {
        let d = doc(
            "<html><body><div id=\"baris\" style=\"display:flex;flex-direction:row\">\
             <div id=\"kiri\" style=\"width:50px;height:10px\"></div>\
             <div id=\"kanan\" style=\"width:50px;height:10px\"></div></div></body></html>",
        );
        let laporan = hitung(&d, Viewport::new(1000.0, 600.0)).unwrap();
        let kiri = laporan.kotak_untuk(d.cari_id("kiri").unwrap()).unwrap();
        let kanan = laporan.kotak_untuk(d.cari_id("kanan").unwrap()).unwrap();
        assert_eq!(kiri.y, kanan.y);
        assert_eq!(kanan.x, kiri.x + 50.0);
    }

    #[test]
    fn display_none_tidak_menghasilkan_kotak() {
        let d = doc(
            "<html><body><div id=\"a\" style=\"display:none;height:40px\"></div>\
             <div id=\"b\" style=\"height:40px\"></div></body></html>",
        );
        let laporan = hitung(&d, Viewport::new(800.0, 600.0)).unwrap();
        assert!(laporan.kotak_untuk(d.cari_id("a").unwrap()).is_none());
        let b = laporan.kotak_untuk(d.cari_id("b").unwrap()).unwrap();
        assert_eq!(b.y, 0.0, "elemen tersembunyi tidak memakan ruang");
    }

    #[test]
    fn nilai_tidak_didukung_dilaporkan() {
        // `vh` belum didukung: bukan px/em/rem/%, jadi harus dilaporkan,
        // bukan ditebak jadi px.
        let d = doc("<html><body><div style=\"height:2vh\"></div></body></html>");
        let laporan = hitung(&d, Viewport::new(800.0, 600.0)).unwrap();
        assert!(
            laporan.diabaikan.iter().any(|s| s.contains("height:2vh")),
            "deklarasi yang diabaikan harus muncul di laporan: {:?}",
            laporan.diabaikan
        );
    }

    #[test]
    fn em_dan_rem_menghasilkan_kotak_sungguhan() {
        // `em` diukur terhadap font-size elemen itu, `rem` terhadap font-size
        // `html`. Kalau satuan ini hanya benar di tes gaya.rs tapi tidak
        // dipakai layout, kotaknya akan nol dan tes ini gagal.
        let d = doc("<html><head><style>\
             html { font-size: 10px }\
             body { font-size: 20px }\
             #a { width: 2em; height: 3rem }\
             </style></head><body><div id=\"a\"></div></body></html>");
        let laporan = hitung(&d, Viewport::new(800.0, 600.0)).unwrap();
        let a = laporan.kotak_untuk(d.cari_id("a").unwrap()).unwrap();
        assert_eq!(a.width, 40.0, "2em dari font-size 20px harus 40px");
        assert_eq!(a.height, 30.0, "3rem dari font-size akar 10px harus 30px");
        assert!(
            !laporan.diabaikan.iter().any(|s| s.contains("width:2em")),
            "em sudah didukung, tidak boleh dilaporkan diabaikan: {:?}",
            laporan.diabaikan
        );
    }

    #[test]
    fn persen_lebar_dihitung_terhadap_induk() {
        // `%` diteruskan ke taffy sebagai persen, bukan diubah jadi px.
        let d = doc(
            "<html><body><div id=\"induk\" style=\"width:200px;height:100px\">\
             <div id=\"anak\" style=\"width:50%;height:10px\"></div></div></body></html>",
        );
        let laporan = hitung(&d, Viewport::new(800.0, 600.0)).unwrap();
        let anak = laporan.kotak_untuk(d.cari_id("anak").unwrap()).unwrap();
        assert_eq!(anak.width, 100.0, "50% dari induk 200px harus 100px");
    }

    #[test]
    fn properti_waris_terbaca_dari_laporan() {
        // Pewarisan harus terlihat di laporan, bukan cuma di tes internal
        // gaya.rs: anak yang tidak punya deklarasi sendiri mewarisi warna
        // dan ukuran font induknya.
        let d = doc("<html><head><style>\
             body { color: #00ff00; font-size: 18px }\
             </style></head><body><div id=\"a\"><span id=\"b\"></span></div></body></html>");
        let laporan = hitung(&d, Viewport::new(800.0, 600.0)).unwrap();
        let b = d.cari_id("b").unwrap();
        let g = laporan
            .gaya_terhitung(b)
            .expect("anak harus punya gaya terhitung");
        assert_eq!(g.warna.as_deref(), Some("#00ff00"), "color harus waris");
        assert_eq!(g.ukuran_font, 18.0, "font-size harus waris");
        assert_eq!(
            laporan.properti(b, "color"),
            None,
            "deklarasi pemenang anak memang kosong; nilainya datang dari warisan"
        );
    }

    #[test]
    fn stylesheet_menentukan_kotak_bukan_hanya_style_sebaris() {
        let d = doc(
            "<html><head><style>.kartu{width:70px;height:25px}</style></head>\
             <body><div id=\"a\" class=\"kartu\"></div>\
             <div id=\"b\" class=\"kartu\" style=\"height:40px\"></div></body></html>",
        );
        let laporan = hitung(&d, Viewport::new(800.0, 600.0)).unwrap();
        let a = laporan.kotak_untuk(d.cari_id("a").unwrap()).unwrap();
        assert_eq!(a.width, 70.0, "lebar dari <style> harus terpakai");
        assert_eq!(a.height, 25.0, "tinggi dari <style> harus terpakai");
        let b = laporan.kotak_untuk(d.cari_id("b").unwrap()).unwrap();
        assert_eq!(b.width, 70.0, "lebar tetap dari stylesheet");
        assert_eq!(b.height, 40.0, "style sebaris mengalahkan stylesheet");
        assert_eq!(
            laporan.properti(d.cari_id("a").unwrap(), "width"),
            Some("70px"),
            "gaya pemenang disimpan untuk lapisan lukis"
        );
    }

    #[test]
    fn lembar_eksternal_menentukan_lebar_kotak() {
        let d = doc(
            "<html><head><link rel=\"stylesheet\" href=\"kartu.css\"></head>\
             <body><div id=\"a\" class=\"kartu\"></div></body></html>",
        );
        let lembar = [LembarLuar::baru(
            "kartu.css",
            ".kartu{width:70px;height:25px}",
        )];
        let laporan = hitung_dengan(&d, Viewport::new(800.0, 600.0), &lembar).unwrap();
        let a = laporan.kotak_untuk(d.cari_id("a").unwrap()).unwrap();
        assert_eq!(a.width, 70.0, "lebar dari lembar eksternal harus terpakai");
        assert_eq!(
            a.height, 25.0,
            "tinggi dari lembar eksternal harus terpakai"
        );
        let catatan = laporan.catatan_kaskade.join(" | ");
        assert!(
            !catatan.contains("tidak diambil"),
            "lembar yang disertakan tidak boleh dilaporkan terlewat: {catatan}"
        );
    }

    #[test]
    fn tanpa_lembar_eksternal_lebar_tidak_terpakai_dan_tercatat() {
        let d = doc(
            "<html><head><link rel=\"stylesheet\" href=\"kartu.css\"></head>\
             <body><div id=\"a\" class=\"kartu\"></div></body></html>",
        );
        let laporan = hitung(&d, Viewport::new(800.0, 600.0)).unwrap();
        let catatan = laporan.catatan_kaskade.join(" | ");
        assert!(
            catatan.contains("kartu.css") && catatan.contains("tidak diambil"),
            "tautan yang tidak diambil harus tercatat: {catatan}"
        );
    }

    #[test]
    fn catatan_kaskade_diteruskan_ke_laporan() {
        let d = doc("<html><head><link rel=\"stylesheet\" href=\"a.css\">\
             <style>@media print {div{width:1px}}</style></head>\
             <body><div></div></body></html>");
        let laporan = hitung(&d, Viewport::new(800.0, 600.0)).unwrap();
        let catatan = laporan.catatan_kaskade.join(" | ");
        assert!(catatan.contains("a.css"), "link harus tercatat: {catatan}");
        assert!(
            catatan.contains("@media"),
            "at-rule harus tercatat: {catatan}"
        );
    }

    #[test]
    fn node_teks_menghasilkan_kotak() {
        // Sebelum pengukuran teks ada, node teks tidak menghasilkan kotak dan
        // tinggi paragraf selalu nol. Tes ini gagal di pohon lama.
        let d = doc("<html><body><p id=\"p\">Halo</p></body></html>");
        let laporan = hitung(&d, Viewport::new(800.0, 600.0)).unwrap();
        let p = d.cari_id("p").unwrap();
        let kotak_p = laporan.kotak_untuk(p).expect("paragraf punya kotak");
        assert!(
            kotak_p.height > 0.0,
            "paragraf berisi teks harus punya tinggi, dapat {}",
            kotak_p.height
        );
        let teks = d
            .node(p)
            .unwrap()
            .children
            .iter()
            .copied()
            .find(|&c| matches!(d.node(c).unwrap().kind, NodeKind::Text(_)))
            .expect("paragraf punya anak teks");
        let kotak_teks = laporan
            .kotak_untuk(teks)
            .expect("node teks punya kotak sendiri");
        assert!(kotak_teks.width > 0.0, "lebar teks harus positif");
        assert!(
            kotak_teks.height > 0.0,
            "tinggi teks harus satu baris, bukan nol"
        );
        assert!(
            (kotak_p.height - kotak_teks.height).abs() < 1.0,
            "tinggi paragraf mengikuti tinggi teks: {} vs {}",
            kotak_p.height,
            kotak_teks.height
        );
    }

    #[test]
    fn teks_panjang_di_kotak_sempit_jadi_beberapa_baris() {
        // Pembungkusan harus nyata: satu baris selebar 40px tidak mungkin
        // memuat kalimat ini, jadi tingginya harus kelipatan tinggi baris.
        let d = doc(
            "<html><body><div id=\"k\" style=\"width:40px\">aaaa bbbb cccc dddd</div></body></html>",
        );
        let laporan = hitung(&d, Viewport::new(800.0, 600.0)).unwrap();
        let k = d.cari_id("k").unwrap();
        let kotak = laporan.kotak_untuk(k).unwrap();
        let teks = d
            .node(k)
            .unwrap()
            .children
            .iter()
            .copied()
            .find(|&c| matches!(d.node(c).unwrap().kind, NodeKind::Text(_)))
            .unwrap();
        let kotak_teks = laporan.kotak_untuk(teks).unwrap();
        let tinggi_baris = laporan.gaya_terhitung(teks).unwrap().tinggi_baris;
        let baris = (kotak_teks.height / tinggi_baris).round();
        assert!(
            baris >= 2.0,
            "empat kata di kotak 40px harus lebih dari satu baris, dapat {baris} baris ({} px)",
            kotak_teks.height
        );
        assert!(
            kotak_teks.width <= 40.5,
            "lebar teks tidak boleh melewati induk 40px, dapat {}",
            kotak_teks.width
        );
        assert!(
            kotak.height >= kotak_teks.height - 1.0,
            "induk ikut tinggi teks: {} vs {}",
            kotak.height,
            kotak_teks.height
        );
    }

    #[test]
    fn teks_mewarisi_ukuran_font_induk() {
        // font-size 32px harus membuat teks dua kali lebih lebar dan lebih
        // tinggi daripada 16px. Kalau layout membaca gaya teks sendiri dan
        // bukan warisan induk, angka ini akan sama.
        //
        // Yang diukur lebar ISI teksnya, bukan lebar kotak: kotak daun teks
        // melebar selebar containing block (sama seperti blok di browser),
        // jadi lebarnya tidak lagi mencerminkan lebar tulisannya.
        let kecil = doc("<html><body><p id=\"p\">Halo dunia</p></body></html>");
        let besar =
            doc("<html><body><p id=\"p\" style=\"font-size:32px\">Halo dunia</p></body></html>");
        let l1 = hitung(&kecil, Viewport::new(800.0, 600.0)).unwrap();
        let l2 = hitung(&besar, Viewport::new(800.0, 600.0)).unwrap();
        let teks1 = teks_dari(&kecil, "p");
        let teks2 = teks_dari(&besar, "p");
        let g1 = l1.gaya_terhitung(teks1).unwrap();
        let g2 = l2.gaya_terhitung(teks2).unwrap();
        assert_eq!(g1.ukuran_font, 16.0, "ukuran font awal 16px");
        assert_eq!(g2.ukuran_font, 32.0, "font-size induk harus diwarisi teks");
        let isi = l1.teks.get(&teks1).expect("teks p harus tercatat");
        let w1 = crate::native::teks::lebar(isi, g1.ukuran_font);
        let w2 = crate::native::teks::lebar(isi, g2.ukuran_font);
        assert!(
            w2 > w1 * 1.8,
            "32px harus hampir dua kali 16px: {w2} vs {w1}"
        );
        let k1 = l1.kotak_untuk(teks1).unwrap();
        let k2 = l2.kotak_untuk(teks2).unwrap();
        assert!(k2.height > k1.height, "baris 32px lebih tinggi");
    }

    #[test]
    fn teks_kosong_tidak_menambah_tinggi() {
        // Indentasi HTML menghasilkan node teks berisi spasi saja. Itu tidak
        // boleh jadi kotak, kalau tidak setiap dokumen berlekuk punya tinggi
        // halaman yang salah.
        let d = doc(
            "<html><body>\n  <div id=\"a\" style=\"height:10px\"></div>\n  <div id=\"b\" style=\"height:10px\"></div>\n</body></html>",
        );
        let laporan = hitung(&d, Viewport::new(800.0, 600.0)).unwrap();
        let a = laporan.kotak_untuk(d.cari_id("a").unwrap()).unwrap();
        let b = laporan.kotak_untuk(d.cari_id("b").unwrap()).unwrap();
        assert_eq!(
            b.y,
            a.y + 10.0,
            "spasi di antara elemen tidak memakan ruang"
        );
        let kotak_teks: Vec<_> = laporan
            .kotak
            .iter()
            .filter(|k| matches!(d.node(k.node).map(|n| &n.kind), Some(NodeKind::Text(_))))
            .collect();
        assert!(
            kotak_teks.is_empty(),
            "tidak boleh ada kotak teks spasi: {kotak_teks:?}"
        );
    }

    #[test]
    fn teks_css_di_style_bukan_konten_halaman() {
        // Isi <style> adalah data CSS, bukan teks halaman. Tanpa pengecualian
        // ini, aturan CSS mengambil satu tinggi baris di <head> dan seluruh
        // isi <body> tergeser ke bawah.
        let d = doc(
            "<html><head><style>#a{width:30px;height:30px;background-color:#00f}</style></head>\
             <body><div id=\"a\"></div></body></html>",
        );
        let laporan = hitung(&d, Viewport::new(800.0, 600.0)).unwrap();
        let kotak_teks: Vec<_> = laporan
            .kotak
            .iter()
            .filter(|k| matches!(d.node(k.node).map(|n| &n.kind), Some(NodeKind::Text(_))))
            .collect();
        assert!(
            kotak_teks.is_empty(),
            "CSS tidak boleh jadi kotak teks: {kotak_teks:?}"
        );
        let a = laporan.kotak_untuk(d.cari_id("a").unwrap()).unwrap();
        assert_eq!(a.y, 0.0, "isi body mulai dari atas, tidak tergeser head");
    }

    fn teks_dari(d: &Document, id: &str) -> NodeId {
        let induk = d.cari_id(id).unwrap();
        d.node(induk)
            .unwrap()
            .children
            .iter()
            .copied()
            .find(|&c| matches!(d.node(c).unwrap().kind, NodeKind::Text(_)))
            .expect("harus punya anak teks")
    }

    #[test]
    fn dokumen_kosong_tidak_panik() {
        // html5ever selalu membentuk html/head/body, jadi hasilnya bukan nol
        // kotak. Yang diuji di sini: tidak ada panik dan semua angka masuk akal.
        let d = doc("");
        let laporan = hitung(&d, Viewport::new(800.0, 600.0)).unwrap();
        assert!(!laporan.kotak.is_empty(), "html/head/body tetap terbentuk");
        for k in &laporan.kotak {
            assert!(k.width.is_finite() && k.width >= 0.0, "lebar: {k:?}");
            assert!(k.height.is_finite() && k.height >= 0.0, "tinggi: {k:?}");
        }
    }
}
