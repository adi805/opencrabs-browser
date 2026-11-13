//! Rasterisasi kotak layout menjadi PNG lewat `tiny-skia`.
//!
//! Ini lapisan **eyes** versi minimum yang bisa dipertanggungjawabkan: yang
//! dilukis adalah latar kotak dari kaskade ([`super::css::Kaskade`]) dan outline
//! glyph dari face TrueType yang sama dengan pengukur layout ([`super::teks`]).
//! Gambar, gradien, bayangan, border, radius, shaping, dan font fallback belum
//! dilukis atau dihitung; batasnya disebut di [`super::CELAH`], bukan ditutupi
//! dengan warna atau bentuk karangan.
//!
//! Warna yang tidak dikenali **tidak** ditebak: deklarasinya masuk ke
//! [`Raster::diabaikan`] dan pikselnya dibiarkan seperti latar.

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};

use super::NativeError;
use super::dom::{Document, NodeId};
use super::gaya::GayaTerhitung;
use super::layout::{self, Kotak, Viewport};

/// Potongan halaman yang mau diambil, dalam koordinat CSS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Klip {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Hasil satu kali rasterisasi.
#[derive(Debug, Clone)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    /// Byte PNG lengkap, siap dikirim sebagai base64 ke klien CDP.
    pub png: Vec<u8>,
    /// Piksel RGBA premultiplied, sesuai penyimpanan `tiny-skia`.
    pub rgba: Vec<u8>,
    /// Berapa kotak yang benar-benar dilukis.
    pub kotak_dilukis: usize,
    /// Berapa baris teks yang benar-benar dilukis.
    pub baris_dilukis: usize,
    /// Berapa glyph yang benar-benar dilukis.
    pub glyph_dilukis: usize,
    /// Deklarasi warna yang bentuknya dikenali tapi nilainya tidak didukung.
    pub diabaikan: Vec<String>,
}

impl Raster {
    /// Warna satu piksel, `None` kalau koordinatnya di luar kanvas.
    #[must_use]
    pub fn piksel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 4) as usize;
        let s = self.rgba.get(i..i + 4)?;
        Some([s[0], s[1], s[2], s[3]])
    }
}

/// Melukis dokumen pada ukuran viewport tertentu.
///
/// Latar kanvas putih buram, sama seperti tangkapan layar halaman tanpa latar.
pub fn lukis(
    doc: &Document,
    viewport: Viewport,
    klip: Option<Klip>,
) -> Result<Raster, NativeError> {
    let laporan = layout::hitung(doc, viewport)?;
    let (lebar, tinggi, geser) = match klip {
        Some(k) => (
            k.width.max(1.0).round() as u32,
            k.height.max(1.0).round() as u32,
            (-k.x, -k.y),
        ),
        None => (
            viewport.width.max(1.0).round() as u32,
            viewport.height.max(1.0).round() as u32,
            (0.0, 0.0),
        ),
    };
    let mut pixmap = Pixmap::new(lebar, tinggi)
        .ok_or_else(|| NativeError::raster(format!("kanvas {lebar}x{tinggi} tidak bisa dibuat")))?;
    pixmap.fill(Color::WHITE);
    let transform = Transform::from_translate(geser.0, geser.1);

    let mut kotak_dilukis = 0usize;
    let mut baris_dilukis = 0usize;
    let mut glyph_dilukis = 0usize;
    let mut diabaikan = Vec::new();
    for kotak in &laporan.kotak {
        // Warna dibaca dari gaya hasil kaskade, bukan dari atribut `style`:
        // aturan `<style>` harus ikut terpakai, dan layout serta lukis wajib
        // memakai angka yang sama.
        let Some(deklarasi) = laporan.gaya.get(&kotak.node) else {
            continue;
        };
        let Some((nama_deklarasi, warna)) = warna_latar(deklarasi) else {
            continue;
        };
        let Some(warna) = warna else {
            diabaikan.push(format!(
                "node {}: `{nama_deklarasi}` tidak didukung",
                kotak.node
            ));
            continue;
        };
        if warna.alpha() == 0.0 || kotak.width <= 0.0 || kotak.height <= 0.0 {
            continue;
        }
        let Some(rect) = Rect::from_xywh(kotak.x, kotak.y, kotak.width, kotak.height) else {
            continue;
        };
        let mut paint = Paint::default();
        paint.set_color(warna);
        pixmap.fill_rect(rect, &paint, transform, None);
        kotak_dilukis += 1;
    }

    // Teks dilukis setelah latar, supaya glyph tidak tertimpa kotak yang
    // dilukis sesudahnya. Urutan ini sama dengan cat browser: latar dulu,
    // isi di atasnya.
    let kotak_teks: Vec<(&NodeId, &String)> = laporan
        .teks
        .iter()
        .filter(|(node, _)| !laporan.teks_tersembunyi.contains(*node))
        .collect();
    for (node, isi) in kotak_teks {
        let Some(kotak) = laporan.kotak_untuk(*node) else {
            continue;
        };
        let gaya = laporan.terhitung.get(node).cloned().unwrap_or_default();
        let warna = warna_teks(&gaya, &mut diabaikan, *node);
        let (baris, glyph) = lukis_teks(&mut pixmap, isi, &gaya, &kotak, warna, geser);
        baris_dilukis += baris;
        glyph_dilukis += glyph;
    }

    let png = pixmap
        .encode_png()
        .map_err(|e| NativeError::raster(format!("PNG gagal dikodekan: {e}")))?;
    Ok(Raster {
        width: lebar,
        height: tinggi,
        png,
        rgba: pixmap.data().to_vec(),
        kotak_dilukis,
        baris_dilukis,
        glyph_dilukis,
        diabaikan,
    })
}

/// Warna teks dari gaya terhitung. Warna bawaan CSS adalah hitam.
fn warna_teks(gaya: &GayaTerhitung, diabaikan: &mut Vec<String>, node: NodeId) -> Color {
    let Some(nilai) = gaya.warna.as_deref() else {
        return Color::BLACK;
    };
    match urai_warna(nilai) {
        Some(w) => w,
        None => {
            diabaikan.push(format!("node {node}: color: {nilai} tidak didukung"));
            Color::BLACK
        }
    }
}

/// Pembangun path glyph dari outline TrueType.
///
/// `ttf-parser` memberi koordinat dalam unit font dengan sumbu Y ke atas,
/// sementara kanvas punya sumbu Y ke bawah. Pembalikan itu dikerjakan oleh
/// transformasi di [`lukis_teks`], bukan di sini, supaya path-nya tetap bisa
/// dipakai ulang untuk ukuran apa pun.
#[derive(Default)]
struct PembangunGlyph {
    jalur: PathBuilder,
}

impl ttf_parser::OutlineBuilder for PembangunGlyph {
    fn move_to(&mut self, x: f32, y: f32) {
        self.jalur.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.jalur.line_to(x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.jalur.quad_to(x1, y1, x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.jalur.cubic_to(x1, y1, x2, y2, x, y);
    }

    fn close(&mut self) {
        self.jalur.close();
    }
}

/// Melukis satu blok teks di dalam kotaknya.
///
/// Pembungkusan memakai [`super::teks::bungkus`] pada lebar kotak, jadi baris
/// yang dilukis adalah baris yang menghasilkan tinggi kotak itu. Perataan
/// mengikuti `text-align` warisan; `justify` diperlakukan seperti `left`
/// karena peregangan spasi belum ada, dan itu dicatat di [`super::CELAH`].
/// Mengembalikan `(baris, glyph)` yang benar-benar dilukis.
fn lukis_teks(
    pixmap: &mut Pixmap,
    isi: &str,
    gaya: &GayaTerhitung,
    kotak: &Kotak,
    warna: Color,
    geser: (f32, f32),
) -> (usize, usize) {
    let Some(face) = super::teks::face() else {
        return (0, 0);
    };
    if warna.alpha() == 0.0 || kotak.width <= 0.0 || gaya.ukuran_font <= 0.0 {
        return (0, 0);
    }
    let baris = super::teks::bungkus(isi, gaya.ukuran_font, kotak.width);
    let skala = super::teks::skala(gaya.ukuran_font);
    if skala <= 0.0 {
        return (0, 0);
    }
    let baseline_baris = super::teks::jarak_baseline(gaya.ukuran_font);
    let mut paint = Paint::default();
    paint.set_color(warna);
    paint.anti_alias = true;

    let mut dilukis_baris = 0usize;
    let mut dilukis_glyph = 0usize;
    for (i, isi_baris) in baris.iter().enumerate() {
        let lebar_baris = super::teks::lebar(isi_baris, gaya.ukuran_font);
        let x_awal = match gaya.perataan.as_deref() {
            Some("center") => kotak.x + (kotak.width - lebar_baris) / 2.0,
            Some("right") | Some("end") => kotak.x + kotak.width - lebar_baris,
            // `left`, `start`, `justify`, dan nilai tak dikenal: rata kiri.
            _ => kotak.x,
        };
        let baseline = kotak.y + baseline_baris + i as f32 * gaya.tinggi_baris;
        let mut pena = x_awal;
        let mut ada_glyph = false;
        for c in isi_baris.chars() {
            let glyph = face.glyph_index(c).unwrap_or(ttf_parser::GlyphId(0));
            let mut pembangun = PembangunGlyph::default();
            if face.outline_glyph(glyph, &mut pembangun).is_some()
                && let Some(jalur) = pembangun.jalur.finish()
            {
                // Sumbu Y font ke atas, kanvas ke bawah: skala negatif pada
                // Y. Terjemahan dilakukan setelah skala supaya pena berada
                // di koordinat piksel.
                let transform = Transform::from_translate(pena + geser.0, baseline + geser.1)
                    .pre_scale(skala, -skala);
                pixmap.fill_path(&jalur, &paint, FillRule::Winding, transform, None);
                dilukis_glyph += 1;
                ada_glyph = true;
            }
            pena += super::teks::lebar_karakter(c, gaya.ukuran_font);
        }
        if ada_glyph {
            dilukis_baris += 1;
        }
    }
    (dilukis_baris, dilukis_glyph)
}

/// Mencari deklarasi latar di antara deklarasi pemenang kaskade.
///
/// `background-color` didahulukan atas shorthand `background`, sesuai urutan
/// prioritas CSS. `None` pada warna berarti nilainya dikenali bentuknya tapi
/// tidak didukung, dan itu dilaporkan pemanggil, bukan diganti warna default.
fn warna_latar(deklarasi: &[(String, String)]) -> Option<(String, Option<Color>)> {
    let ambil = |nama: &str| {
        deklarasi
            .iter()
            .find(|(k, _)| k == nama)
            .map(|(_, v)| v.as_str())
    };
    for kunci in ["background-color", "background"] {
        let Some(nilai) = ambil(kunci) else {
            continue;
        };
        let nilai = nilai.trim();
        if nilai.is_empty() {
            continue;
        }
        let deklarasi = format!("{kunci}: {nilai}");
        // `background` boleh berisi gambar atau gradien; itu bukan warna.
        if nilai.contains("url(") || nilai.contains("gradient(") {
            return Some((deklarasi, None));
        }
        return Some((deklarasi, urai_warna(nilai)));
    }
    None
}

/// Mengurai warna CSS yang bentuknya sederhana.
///
/// Nilai yang tidak dikenal menghasilkan `None`. Tidak ada tebakan warna.
#[must_use]
pub fn urai_warna(nilai: &str) -> Option<Color> {
    let v = nilai.trim().to_ascii_lowercase();
    if v == "transparent" {
        return Some(Color::TRANSPARENT);
    }
    if let Some(hex) = v.strip_prefix('#') {
        return warna_hex(hex);
    }
    if let Some(isi) = v
        .strip_prefix("rgb(")
        .or_else(|| v.strip_prefix("rgba("))
        .and_then(|s| s.strip_suffix(')'))
    {
        return warna_rgb(isi);
    }
    warna_nama(&v)
}

fn warna_hex(hex: &str) -> Option<Color> {
    let digit: Vec<u8> = hex
        .chars()
        .map(|c| c.to_digit(16).map(|d| d as u8))
        .collect::<Option<_>>()?;
    let (r, g, b, a) = match digit.len() {
        3 => (digit[0] * 17, digit[1] * 17, digit[2] * 17, 255),
        4 => (digit[0] * 17, digit[1] * 17, digit[2] * 17, digit[3] * 17),
        6 => (
            digit[0] * 16 + digit[1],
            digit[2] * 16 + digit[3],
            digit[4] * 16 + digit[5],
            255,
        ),
        8 => (
            digit[0] * 16 + digit[1],
            digit[2] * 16 + digit[3],
            digit[4] * 16 + digit[5],
            digit[6] * 16 + digit[7],
        ),
        _ => return None,
    };
    Some(Color::from_rgba8(r, g, b, a))
}

fn warna_rgb(isi: &str) -> Option<Color> {
    let bagian: Vec<&str> = isi
        .split([',', '/', ' '])
        .filter(|s| !s.is_empty())
        .collect();
    if bagian.len() < 3 || bagian.len() > 4 {
        return None;
    }
    let kanal = |s: &str| -> Option<u8> {
        if let Some(p) = s.strip_suffix('%') {
            let n: f32 = p.parse().ok()?;
            return Some((n.clamp(0.0, 100.0) / 100.0 * 255.0).round() as u8);
        }
        Some(s.parse::<f32>().ok()?.clamp(0.0, 255.0).round() as u8)
    };
    let r = kanal(bagian[0])?;
    let g = kanal(bagian[1])?;
    let b = kanal(bagian[2])?;
    let a = if bagian.len() == 4 {
        let n: f32 = bagian[3].parse().ok()?;
        (n.clamp(0.0, 1.0) * 255.0).round() as u8
    } else {
        255
    };
    Some(Color::from_rgba8(r, g, b, a))
}

/// Warna bernama yang paling sering dipakai. Daftar sengaja pendek: nama di
/// luar daftar ini dilaporkan sebagai tidak didukung, bukan ditebak.
fn warna_nama(nama: &str) -> Option<Color> {
    let (r, g, b) = match nama {
        "white" => (255, 255, 255),
        "black" => (0, 0, 0),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "lime" => (0, 255, 0),
        "blue" => (0, 0, 255),
        "yellow" => (255, 255, 0),
        "orange" => (255, 165, 0),
        "gray" | "grey" => (128, 128, 128),
        "silver" => (192, 192, 192),
        "navy" => (0, 0, 128),
        "teal" => (0, 128, 128),
        "purple" => (128, 0, 128),
        "maroon" => (128, 0, 0),
        "olive" => (128, 128, 0),
        "aqua" | "cyan" => (0, 255, 255),
        "fuchsia" | "magenta" => (255, 0, 255),
        "pink" => (255, 192, 203),
        "brown" => (165, 42, 42),
        _ => return None,
    };
    Some(Color::from_rgba8(r, g, b, 255))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ukuran_png(png: &[u8]) -> (u32, u32) {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "harus PNG sungguhan");
        let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
        (w, h)
    }

    #[test]
    fn png_memuat_magic_bytes_dan_ukuran_viewport() {
        let doc = Document::parse_html("<body><p>x</p></body>").unwrap();
        let raster = lukis(&doc, Viewport::new(320.0, 200.0), None).unwrap();
        assert_eq!(ukuran_png(&raster.png), (320, 200));
        assert_eq!(raster.width, 320);
        assert_eq!(raster.height, 200);
        assert_eq!(raster.rgba.len(), (320 * 200 * 4) as usize);
    }

    #[test]
    fn kotak_berwarna_mengubah_piksel() {
        let doc = Document::parse_html(
            "<body><div style=\"width:60px;height:40px;background-color:#ff0000\"></div></body>",
        )
        .unwrap();
        let raster = lukis(&doc, Viewport::new(100.0, 100.0), None).unwrap();
        assert_eq!(raster.kotak_dilukis, 1, "satu kotak berlatar dilukis");
        assert_eq!(raster.piksel(10, 10), Some([255, 0, 0, 255]));
        assert_eq!(
            raster.piksel(90, 90),
            Some([255, 255, 255, 255]),
            "di luar kotak tetap putih"
        );
    }

    #[test]
    fn warna_rgb_dan_nama_dikenali() {
        let doc = Document::parse_html(
            "<body><div style=\"width:20px;height:20px;background:rgb(0, 0, 255)\"></div>\
             <div style=\"width:20px;height:20px;background-color:lime\"></div></body>",
        )
        .unwrap();
        let raster = lukis(&doc, Viewport::new(80.0, 80.0), None).unwrap();
        assert_eq!(raster.kotak_dilukis, 2);
        let ada_biru = (0..80u32)
            .flat_map(|y| (0..80u32).map(move |x| (x, y)))
            .filter_map(|(x, y)| raster.piksel(x, y))
            .any(|p| p == [0, 0, 255, 255]);
        assert!(ada_biru, "harus ada piksel biru dari rgb()");
    }

    #[test]
    fn warna_dari_stylesheet_terlukis() {
        let doc = Document::parse_html(
            "<html><head><style>#kotak{width:30px;height:30px;background-color:#0000ff}</style>\
             </head><body><div id=\"kotak\"></div></body></html>",
        )
        .unwrap();
        let raster = lukis(&doc, Viewport::new(60.0, 60.0), None).unwrap();
        assert_eq!(
            raster.kotak_dilukis, 1,
            "warna dari <style> harus ikut dilukis"
        );
        assert_eq!(raster.piksel(5, 5), Some([0, 0, 255, 255]));
    }

    #[test]
    fn warna_tak_dikenal_dilaporkan_bukan_ditebak() {
        let doc = Document::parse_html(
            "<body><div style=\"width:10px;height:10px;background-color:chartreuse\"></div></body>",
        )
        .unwrap();
        let raster = lukis(&doc, Viewport::new(40.0, 40.0), None).unwrap();
        assert_eq!(raster.kotak_dilukis, 0);
        assert_eq!(raster.diabaikan.len(), 1, "chartreuse harus dilaporkan");
        assert_eq!(raster.piksel(5, 5), Some([255, 255, 255, 255]));
    }

    #[test]
    fn klip_memotong_dan_menggeser_kanvas() {
        let doc = Document::parse_html(
            "<body><div style=\"width:40px;height:40px;background-color:#00ff00\"></div></body>",
        )
        .unwrap();
        let klip = Klip {
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
        };
        let raster = lukis(&doc, Viewport::new(100.0, 100.0), Some(klip)).unwrap();
        assert_eq!(ukuran_png(&raster.png), (20, 20));
        assert_eq!(raster.piksel(0, 0), Some([0, 255, 0, 255]));
    }

    /// Menghitung piksel yang lebih gelap dari ambang, di dalam kotak tertentu.
    fn piksel_gelap_di(raster: &Raster, x: u32, y: u32, w: u32, h: u32) -> usize {
        let mut n = 0;
        for py in y..(y + h).min(raster.height) {
            for px in x..(x + w).min(raster.width) {
                if let Some([r, g, b, a]) = raster.piksel(px, py)
                    && a > 0
                    && (u16::from(r) + u16::from(g) + u16::from(b)) < 400
                {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    fn teks_tanpa_latar_tetap_terlukis() {
        // Halaman yang isinya hanya teks: sebelum glyph dilukis, kanvas ini
        // keluar kosong seluruhnya.
        let doc = Document::parse_html("<body><p>Halo</p></body>").unwrap();
        let raster = lukis(&doc, Viewport::new(200.0, 80.0), None).unwrap();
        assert!(raster.glyph_dilukis >= 4, "empat huruf harus jadi glyph");
        assert_eq!(raster.baris_dilukis, 1, "satu baris");
        assert!(
            piksel_gelap_di(&raster, 0, 0, 200, 80) > 10,
            "glyph hitam harus meninggalkan piksel gelap"
        );
    }

    #[test]
    fn warna_teks_dari_stylesheet_dipakai() {
        let doc = Document::parse_html(
            "<html><head><style>p{color:#ff0000}</style></head>\
             <body><p>Merah</p></body></html>",
        )
        .unwrap();
        let raster = lukis(&doc, Viewport::new(200.0, 80.0), None).unwrap();
        let ada_merah = (0..80u32)
            .flat_map(|y| (0..200u32).map(move |x| (x, y)))
            .filter_map(|(x, y)| raster.piksel(x, y))
            .any(|[r, g, b, a]| a > 0 && r > 120 && g < 90 && b < 90);
        assert!(ada_merah, "warna dari <style> harus dipakai melukis teks");
    }

    #[test]
    fn teks_tersembunyi_tidak_dilukis() {
        let doc =
            Document::parse_html("<body><p style=\"visibility:hidden\">Tidak terlihat</p></body>")
                .unwrap();
        let raster = lukis(&doc, Viewport::new(200.0, 80.0), None).unwrap();
        assert_eq!(raster.glyph_dilukis, 0);
        assert_eq!(raster.baris_dilukis, 0);
        assert_eq!(
            piksel_gelap_di(&raster, 0, 0, 200, 80),
            0,
            "tidak boleh ada piksel teks"
        );
    }

    /// Kolom x pertama yang memuat piksel gelap, atau `u32::MAX` kalau kosong.
    fn awal_kolom_gelap(raster: &Raster) -> u32 {
        (0..raster.width)
            .find(|&x| piksel_gelap_di(raster, x, 0, 1, raster.height) > 0)
            .unwrap_or(u32::MAX)
    }

    /// Jumlah seluruh piksel gelap di kanvas.
    fn jumlah_piksel_gelap(raster: &Raster) -> usize {
        (0..raster.width)
            .map(|x| piksel_gelap_di(raster, x, 0, 1, raster.height))
            .sum()
    }

    #[test]
    fn perataan_menggeser_awal_baris() {
        // Rata tengah dan rata kanan harus benar-benar memindahkan glyph di
        // dalam kotak. Sebelum akar layout dibungkus wadah selebar viewport,
        // setiap blok menyusut selebar isinya, jadi ketiga perataan
        // menghasilkan raster yang identik dan tidak ada yang bisa dibedakan.
        let lukis_perataan = |perataan: &str| {
            let html = format!("<body><p style=\"text-align:{perataan}\">tengah</p></body>");
            let doc = Document::parse_html(&html).unwrap();
            let raster = lukis(&doc, Viewport::new(300.0, 80.0), None).unwrap();
            assert_eq!(
                raster.glyph_dilukis, 6,
                "{perataan}: enam glyph kata `tengah` harus terlukis"
            );
            (awal_kolom_gelap(&raster), jumlah_piksel_gelap(&raster))
        };
        let (x_kiri, n_kiri) = lukis_perataan("left");
        let (x_tengah, n_tengah) = lukis_perataan("center");
        let (x_kanan, n_kanan) = lukis_perataan("right");

        assert_eq!(x_kiri, 0, "rata kiri harus mulai di tepi kiri");
        assert!(
            x_tengah > x_kiri,
            "rata tengah harus mulai lebih ke kanan: {x_tengah} vs {x_kiri}"
        );
        assert!(
            x_kanan > x_tengah,
            "rata kanan harus mulai lebih ke kanan lagi: {x_kanan} vs {x_tengah}"
        );
        assert!(
            x_kanan > 240,
            "rata kanan di kanvas 300px harus mendekati tepi kanan, dapat {x_kanan}"
        );
        // Glyph yang sama digeser, bukan diganti: jumlah piksel gelap harus
        // hampir sama. Kalau perataan sampai mengubah teks, ini yang menangkap.
        for (nama, n) in [("tengah", n_tengah), ("kanan", n_kanan)] {
            assert!(
                n.abs_diff(n_kiri) * 10 <= n_kiri,
                "{nama}: jumlah piksel gelap harus dekat dengan rata kiri, {n} vs {n_kiri}"
            );
        }
    }

    #[test]
    fn teks_panjang_dibungkus_dan_tetap_di_dalam_kotak() {
        let doc = Document::parse_html(
            "<body><p style=\"width:80px\">kata kata kata kata kata</p></body>",
        )
        .unwrap();
        let raster = lukis(&doc, Viewport::new(120.0, 200.0), None).unwrap();
        assert!(
            raster.baris_dilukis > 1,
            "teks 80px harus jadi lebih dari satu baris, dapat {}",
            raster.baris_dilukis
        );
        let kolom_paling_kanan = (0..120u32)
            .rev()
            .find(|&x| (0..200u32).any(|y| piksel_gelap_di(&raster, x, y, 1, 1) > 0))
            .unwrap_or(0);
        assert!(
            kolom_paling_kanan < 100,
            "glyph tidak boleh melewati kotak 80px, dapat kolom {kolom_paling_kanan}"
        );
    }

    #[test]
    fn urai_warna_menolak_sampah() {
        assert!(urai_warna("#zzz").is_none());
        assert!(urai_warna("rgb(1,2)").is_none());
        assert!(urai_warna("bukanwarna").is_none());
        assert_eq!(urai_warna("#fff").map(|c| c.to_color_u8().red()), Some(255));
    }
}
