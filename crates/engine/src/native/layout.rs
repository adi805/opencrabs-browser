//! Penempatan kotak dari pohon [`Document`] lewat `taffy`.
//!
//! Yang dibaca hari ini hanya atribut `style` inline: `display`, `width`,
//! `height`, `padding`, `margin`, `flex-direction`. Setiap properti lain
//! dicatat sebagai diabaikan, bukan diam-diam dipakai dengan arti lain.
//! Stylesheet belum ada di daftar [`super::CELAH`].

use std::collections::HashMap;

use taffy::prelude::{
    AvailableSpace, Dimension, Display, FlexDirection, LengthPercentage, LengthPercentageAuto,
    Rect, Size, Style, TaffyTree,
};

use super::NativeError;
use super::dom::{Document, NodeId, NodeKind};

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
    /// Deklarasi yang bentuknya dikenali tapi nilainya tidak didukung,
    /// mis. `padding: 1em`. Ini laporan jujur, bukan log.
    pub diabaikan: Vec<String>,
}

impl LaporanLayout {
    #[must_use]
    pub fn kotak_untuk(&self, node: NodeId) -> Option<Kotak> {
        self.kotak.iter().copied().find(|k| k.node == node)
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

/// Menghitung layout seluruh dokumen untuk satu viewport.
pub fn hitung(doc: &Document, viewport: Viewport) -> Result<LaporanLayout, NativeError> {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut peta: HashMap<NodeId, taffy::NodeId> = HashMap::new();
    let mut laporan = LaporanLayout::default();

    let akar_dokumen = doc.root();
    let mut anak_teratas = Vec::new();
    for &anak in &doc
        .node(akar_dokumen)
        .map(|n| n.children.clone())
        .unwrap_or_default()
    {
        if let Some(id) = tempatkan(&mut taffy, doc, anak, &mut peta, &mut laporan)? {
            anak_teratas.push(id);
        }
    }

    if anak_teratas.is_empty() {
        return Ok(laporan);
    }

    let akar = if anak_teratas.len() == 1 {
        anak_teratas[0]
    } else {
        let gaya = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            ..Default::default()
        };
        let wadah = taffy.new_leaf(gaya).map_err(NativeError::layout)?;
        for anak in anak_teratas {
            taffy.add_child(wadah, anak).map_err(NativeError::layout)?;
        }
        wadah
    };

    taffy
        .compute_layout(
            akar,
            Size {
                width: AvailableSpace::Definite(viewport.width),
                height: AvailableSpace::Definite(viewport.height),
            },
        )
        .map_err(NativeError::layout)?;

    let mut kotak = Vec::new();
    kumpulkan(&taffy, &peta, doc, akar_dokumen, 0.0, 0.0, &mut kotak)?;
    laporan.kotak = kotak;
    Ok(laporan)
}

fn tempatkan(
    taffy: &mut TaffyTree<()>,
    doc: &Document,
    id: NodeId,
    peta: &mut HashMap<NodeId, taffy::NodeId>,
    laporan: &mut LaporanLayout,
) -> Result<Option<taffy::NodeId>, NativeError> {
    let Some(node) = doc.node(id) else {
        return Ok(None);
    };
    if !matches!(node.kind, NodeKind::Element(_)) {
        // Node teks tidak menghasilkan kotak: pengukuran teks belum ada.
        return Ok(None);
    }

    let gaya = baca_gaya(node.attr("style"), id, laporan);
    if !gaya.tampil {
        return Ok(None);
    }

    let daun = taffy.new_leaf(gaya.taffy).map_err(NativeError::layout)?;
    peta.insert(id, daun);

    for anak in node.children.clone() {
        if let Some(c) = tempatkan(taffy, doc, anak, peta, laporan)? {
            taffy.add_child(daun, c).map_err(NativeError::layout)?;
        }
    }
    Ok(Some(daun))
}

fn kumpulkan(
    taffy: &TaffyTree<()>,
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

fn baca_gaya(style: Option<&str>, id: NodeId, laporan: &mut LaporanLayout) -> Gaya {
    let mut gaya = Style {
        display: Display::Flex,
        flex_direction: FlexDirection::Column,
        ..Default::default()
    };
    let mut tampil = true;

    let Some(style) = style else {
        return Gaya {
            taffy: gaya,
            tampil,
        };
    };

    for deklarasi in style.split(';') {
        let deklarasi = deklarasi.trim();
        if deklarasi.is_empty() {
            continue;
        }
        let Some((nama, nilai)) = deklarasi.split_once(':') else {
            laporan.diabaikan.push(format!(
                "node {id}: deklarasi tanpa titik dua `{deklarasi}`"
            ));
            continue;
        };
        let nama = nama.trim().to_ascii_lowercase();
        let nilai = nilai.trim();

        match nama.as_str() {
            "display" => match nilai {
                "none" => tampil = false,
                "flex" | "block" | "inline-block" => {}
                lain => laporan.diabaikan.push(format!("node {id}: display:{lain}")),
            },
            "flex-direction" => match nilai {
                "row" => gaya.flex_direction = FlexDirection::Row,
                "column" => gaya.flex_direction = FlexDirection::Column,
                lain => laporan
                    .diabaikan
                    .push(format!("node {id}: flex-direction:{lain}")),
            },
            "width" => match px(nilai) {
                Some(v) => gaya.size.width = Dimension::length(v),
                None => laporan.diabaikan.push(format!("node {id}: width:{nilai}")),
            },
            "height" => match px(nilai) {
                Some(v) => gaya.size.height = Dimension::length(v),
                None => laporan.diabaikan.push(format!("node {id}: height:{nilai}")),
            },
            "padding" => match px(nilai) {
                Some(v) => {
                    gaya.padding = Rect {
                        left: LengthPercentage::length(v),
                        right: LengthPercentage::length(v),
                        top: LengthPercentage::length(v),
                        bottom: LengthPercentage::length(v),
                    };
                }
                None => laporan
                    .diabaikan
                    .push(format!("node {id}: padding:{nilai}")),
            },
            "margin" => match px(nilai) {
                Some(v) => {
                    gaya.margin = Rect {
                        left: LengthPercentageAuto::length(v),
                        right: LengthPercentageAuto::length(v),
                        top: LengthPercentageAuto::length(v),
                        bottom: LengthPercentageAuto::length(v),
                    };
                }
                None => laporan.diabaikan.push(format!("node {id}: margin:{nilai}")),
            },
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

fn px(nilai: &str) -> Option<f32> {
    let tanpa = nilai.strip_suffix("px")?;
    tanpa.trim().parse::<f32>().ok()
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
        let d = doc("<html><body><div style=\"height:2em\"></div></body></html>");
        let laporan = hitung(&d, Viewport::new(800.0, 600.0)).unwrap();
        assert!(
            laporan.diabaikan.iter().any(|s| s.contains("height:2em")),
            "deklarasi yang diabaikan harus muncul di laporan: {:?}",
            laporan.diabaikan
        );
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
