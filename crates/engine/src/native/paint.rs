//! Rasterisasi kotak layout menjadi PNG lewat `tiny-skia`.
//!
//! Ini lapisan **eyes**: yang dilukis adalah latar kotak dari kaskade
//! ([`super::css::Kaskade`]) beserta gradiennya, garis `border` dengan
//! `border-radius`, isi `<img>` yang byte-nya diserahkan pemanggil, dan outline
//! glyph dari face TrueType yang sama dengan pengukur layout ([`super::teks`]).
//! Bayangan, shaping, dan font fallback belum ada; batasnya disebut di
//! [`super::CELAH`], bukan ditutupi dengan warna atau bentuk karangan.
//!
//! Warna yang tidak dikenali **tidak** ditebak: deklarasinya masuk ke
//! [`Raster::diabaikan`] dan pikselnya dibiarkan seperti latar.

use std::collections::HashMap;

use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LinearGradient, Mask, Paint, Path, PathBuilder,
    Pixmap, PixmapPaint, Point, RadialGradient, Rect, Shader, SpreadMode, Stroke, Transform,
};

use super::NativeError;
use super::css::LembarLuar;
use super::dom::{Document, NodeId};
use super::gaya::GayaTerhitung;
use super::layout::{self, Kotak, Viewport};

/// Format keluaran yang bisa dikodekan raster.
///
/// `Format::Png` membawa sendiri pilihan kompresinya, karena
/// `Page.captureScreenshot` punya `optimizeForSpeed`: klien yang mengutamakan
/// waktu meminta PNG yang cepat dikodekan, bukan yang paling kecil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// PNG lossless. `cepat` memilih kompresi cepat alih-alih rapat.
    Png { cepat: bool },
    /// JPEG lossy dengan mutu 0-100. Raster software tidak punya tabel
    /// kuantisasi lain, jadi mutu di sini benar-benar mutu encoder.
    Jpeg { mutu: u8 },
}

impl Format {
    /// Nama MIME yang benar untuk byte hasil [`Format`] ini.
    #[must_use]
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png { .. } => "image/png",
            Self::Jpeg { .. } => "image/jpeg",
        }
    }

    /// Magic bytes yang wajib muncul di awal berkas hasil encode.
    #[must_use]
    pub fn magic(self) -> &'static [u8] {
        match self {
            Self::Png { .. } => b"\x89PNG\r\n\x1a\n",
            Self::Jpeg { .. } => b"\xff\xd8\xff",
        }
    }
}

/// Potongan halaman yang mau diambil, dalam koordinat CSS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Klip {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// `clip.scale` CDP: berapa piksel keluaran per satuan CSS.
    ///
    /// 1.0 berarti satu piksel per piksel. 2.0 merender ulang pada kerapatan
    /// dua kali, bukan memperbesar gambar yang sudah jadi, sehingga glyph tetap
    /// tajam dan ukuran kanvas ikut berlipat.
    pub scale: f32,
}

impl Klip {
    /// Klip pada skala 1: bentuk yang paling sering dipakai tes dan pemanggil.
    #[must_use]
    pub fn baru(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            scale: 1.0,
        }
    }

    /// Kerapatan yang dipakai merender, sudah dijaga di atas nol.
    #[must_use]
    fn kerapatan(&self) -> f32 {
        if self.scale.is_finite() && self.scale > 0.0 {
            self.scale
        } else {
            1.0
        }
    }
}

/// Hasil satu kali rasterisasi.
#[derive(Debug, Clone)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    /// Byte hasil encode lengkap, sesuai [`Raster::format`], siap dikirim
    /// sebagai base64 ke klien CDP.
    pub data: Vec<u8>,
    /// Format byte di [`Raster::data`].
    pub format: Format,
    /// Piksel RGBA premultiplied, sesuai penyimpanan `tiny-skia`.
    pub rgba: Vec<u8>,
    /// Berapa kotak yang benar-benar dilukis.
    pub kotak_dilukis: usize,
    /// Berapa baris teks yang benar-benar dilukis.
    pub baris_dilukis: usize,
    /// Berapa glyph yang benar-benar dilukis.
    pub glyph_dilukis: usize,
    /// Berapa `<img>` yang benar-benar dilukis dari byte yang diserahkan
    /// pemanggil. `0` berarti tidak ada gambar yang berhasil didekode, bukan
    /// berarti halaman tidak punya gambar.
    pub gambar_dilukis: usize,
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

/// Melukis dokumen pada ukuran viewport tertentu, sebagai PNG.
///
/// Latar kanvas putih buram, sama seperti tangkapan layar halaman tanpa latar.
/// Ini bentuk ringkas dari [`lukis_dengan`] untuk pemanggil yang tidak memilih
/// format sendiri.
pub fn lukis(
    doc: &Document,
    viewport: Viewport,
    klip: Option<Klip>,
) -> Result<Raster, NativeError> {
    lukis_dengan(doc, viewport, klip, Format::Png { cepat: false })
}

/// Melukis dokumen dengan pilihan format keluaran.
///
/// `klip.scale` ikut menentukan ukuran kanvas: layout selalu dihitung pada
/// satuan CSS, lalu setiap koordinat dikalikan kerapatan saat digambar. Itu
/// sebabnya skala di sini bukan pembesaran gambar hasil, melainkan render ulang
/// pada kerapatan lebih tinggi.
pub fn lukis_dengan(
    doc: &Document,
    viewport: Viewport,
    klip: Option<Klip>,
    format: Format,
) -> Result<Raster, NativeError> {
    lukis_dengan_lembar(doc, viewport, klip, format, &[])
}

/// Melukis dokumen dengan lembar gaya eksternal yang sudah diambil dari
/// jaringan.
///
/// Lembar itu diteruskan ke layout, bukan diurai di sini: warna yang dilukis
/// harus berasal dari kaskade yang sama dengan yang menghasilkan kotak.
pub fn lukis_dengan_lembar(
    doc: &Document,
    viewport: Viewport,
    klip: Option<Klip>,
    format: Format,
    eksternal: &[LembarLuar],
) -> Result<Raster, NativeError> {
    lukis_dengan_sumber(doc, viewport, klip, format, eksternal, &HashMap::new())
}

/// Melukis dokumen dengan lembar gaya eksternal **dan** byte gambar yang sudah
/// diambil pemanggil.
///
/// Peta `gambar` memetakan nilai `src` apa adanya ke byte berkasnya. Mesin ini
/// tidak membuka soket sendiri: pengambilan gambar tunduk pada kebijakan
/// jaringan daemon, sama seperti `fetch()` dan `<link rel=stylesheet>`. `src`
/// yang tidak ada di peta tidak dilukis dan dilaporkan di
/// [`Raster::diabaikan`], bukan diganti kotak abu-abu.
pub fn lukis_dengan_sumber(
    doc: &Document,
    viewport: Viewport,
    klip: Option<Klip>,
    format: Format,
    eksternal: &[LembarLuar],
    gambar: &HashMap<String, Vec<u8>>,
) -> Result<Raster, NativeError> {
    let laporan = layout::hitung_dengan(doc, viewport, eksternal)?;
    let kerapatan = klip.map_or(1.0, |k| k.kerapatan());
    let (lebar, tinggi, geser) = match klip {
        Some(k) => (
            (k.width.max(1.0) * kerapatan).round() as u32,
            (k.height.max(1.0) * kerapatan).round() as u32,
            (-k.x * kerapatan, -k.y * kerapatan),
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
    // Geser sudah dalam piksel; kerapatan dikalikan sebelum terjemahan supaya
    // setiap koordinat CSS mendarat di piksel keluaran yang benar.
    let transform = Transform::from_translate(geser.0, geser.1).pre_scale(kerapatan, kerapatan);

    let mut kotak_dilukis = 0usize;
    let mut baris_dilukis = 0usize;
    let mut glyph_dilukis = 0usize;
    let mut gambar_dilukis = 0usize;
    let mut diabaikan = Vec::new();
    for kotak in &laporan.kotak {
        if kotak.width <= 0.0 || kotak.height <= 0.0 {
            continue;
        }
        let Some(rect) = Rect::from_xywh(kotak.x, kotak.y, kotak.width, kotak.height) else {
            continue;
        };
        let deklarasi: &[(String, String)] =
            laporan.gaya.get(&kotak.node).map_or(&[][..], Vec::as_slice);
        let radius = sudut_radius(deklarasi);
        let jalur = jalur_kotak(rect, radius);

        // Latar: warna solid lebih dulu, lalu gradien. Deklarasi yang bentuknya
        // dikenali tapi tidak bisa dilukis masuk laporan, bukan jadi tebakan.
        match warna_latar(deklarasi) {
            Some((_, Some(warna))) => {
                if warna.alpha() > 0.0 {
                    let mut paint = Paint::default();
                    paint.set_color(warna);
                    pixmap.fill_path(&jalur, &paint, FillRule::Winding, transform, None);
                    kotak_dilukis += 1;
                }
            }
            Some((nama_deklarasi, None)) => match gradien_latar(deklarasi) {
                Some(gradien) => {
                    if let Some(shader) = shader_gradien(&gradien, rect) {
                        let paint = Paint {
                            shader,
                            ..Paint::default()
                        };
                        pixmap.fill_path(&jalur, &paint, FillRule::Winding, transform, None);
                        kotak_dilukis += 1;
                    } else {
                        diabaikan.push(format!(
                            "node {}: `{nama_deklarasi}` tidak bisa dipetakan ke kotaknya",
                            kotak.node
                        ));
                    }
                }
                None => diabaikan.push(format!(
                    "node {}: `{nama_deklarasi}` tidak didukung",
                    kotak.node
                )),
            },
            None => {}
        }

        // Garis tepi dilukis di dalam kotak: jalurnya digeser setengah tebal
        // supaya stroke tidak melebar ke luar batas elemen seperti di CSS.
        if let Some((tebal, warna)) = tepi(deklarasi, &mut diabaikan, kotak.node) {
            let setengah = tebal / 2.0;
            let dalam = Rect::from_xywh(
                kotak.x + setengah,
                kotak.y + setengah,
                kotak.width - tebal,
                kotak.height - tebal,
            );
            if let Some(dalam) = dalam {
                let mut paint = Paint::default();
                paint.set_color(warna);
                paint.anti_alias = true;
                let stroke = Stroke {
                    width: tebal,
                    ..Stroke::default()
                };
                let jalur_tepi = jalur_kotak(dalam, (radius - setengah).max(0.0));
                pixmap.stroke_path(&jalur_tepi, &paint, &stroke, transform, None);
                kotak_dilukis += 1;
            }
        }

        // Isi `<img>`: byte-nya datang dari pemanggil, tidak diambil di sini.
        if let Some(node) = doc.node(kotak.node)
            && node.tag() == Some("img")
        {
            let src = node.attr("src").unwrap_or("");
            match gambar.get(src) {
                Some(bytes) => match dekode_gambar(bytes) {
                    Some(gambar) => {
                        lukis_gambar(&mut pixmap, &gambar, rect, radius, transform);
                        gambar_dilukis += 1;
                    }
                    None => diabaikan.push(format!(
                        "node {}: `src=\"{src}\"` bukan PNG/JPEG yang bisa didekode",
                        kotak.node
                    )),
                },
                None if !src.is_empty() => diabaikan.push(format!(
                    "node {}: `src=\"{src}\"` tidak diserahkan pemanggil",
                    kotak.node
                )),
                None => {}
            }
        }
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
        let potong = potong_teks(doc, &laporan, *node);
        let (baris, glyph) = lukis_teks(&mut pixmap, isi, &gaya, &kotak, warna, transform, potong);
        baris_dilukis += baris;
        glyph_dilukis += glyph;
    }

    let data = match format {
        Format::Png { cepat } => kodekan_png(&pixmap, cepat)?,
        Format::Jpeg { mutu } => kodekan_jpeg(&pixmap, mutu)?,
    };
    Ok(Raster {
        width: lebar,
        height: tinggi,
        data,
        format,
        rgba: pixmap.data().to_vec(),
        kotak_dilukis,
        baris_dilukis,
        glyph_dilukis,
        gambar_dilukis,
        diabaikan,
    })
}

/// Piksel RGBA *straight* (bukan premultiplied) untuk encoder PNG.
///
/// `tiny-skia` menyimpan warna premultiplied; PNG menyimpan warna straight
/// dengan kanal alfa terpisah. Mengirim yang premultiplied membuat piksel
/// semi-transparan terbaca lebih gelap di klien.
fn rgba_lurus(pixmap: &Pixmap) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixmap.data().len());
    for p in pixmap.pixels() {
        let c = p.demultiply();
        out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    out
}

/// Piksel RGB untuk encoder JPEG, dengan latar putih sebagai dasar.
///
/// JPEG tidak punya kanal alfa. Melubangi alfa berarti menebak: piksel yang
/// transparan dikomposit di atas putih, sama seperti kanvas ini sendiri.
fn rgb_di_atas_putih(pixmap: &Pixmap) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixmap.pixels().len() * 3);
    for p in pixmap.pixels() {
        let a = u32::from(p.alpha());
        let atas = |c: u8| {
            // Nilai tersimpan premultiplied: c = straight * a / 255. Sisa ruang
            // ke 255 diisi putih, jadi hasilnya c + (255 - a).
            (u32::from(c) + (255 - a)).min(255) as u8
        };
        out.extend_from_slice(&[atas(p.red()), atas(p.green()), atas(p.blue())]);
    }
    out
}

fn kodekan_png(pixmap: &Pixmap, cepat: bool) -> Result<Vec<u8>, NativeError> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, pixmap.width(), pixmap.height());
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        // `optimizeForSpeed` punya arti konkret di sini: tukar ukuran berkas
        // dengan waktu encode, bukan parameter yang diterima lalu diabaikan.
        enc.set_compression(if cepat {
            png::Compression::Fast
        } else {
            png::Compression::Default
        });
        let mut w = enc
            .write_header()
            .map_err(|e| NativeError::raster(format!("PNG gagal ditulis: {e}")))?;
        w.write_image_data(&rgba_lurus(pixmap))
            .map_err(|e| NativeError::raster(format!("PNG gagal dikodekan: {e}")))?;
        w.finish()
            .map_err(|e| NativeError::raster(format!("PNG gagal ditutup: {e}")))?;
    }
    Ok(out)
}

fn kodekan_jpeg(pixmap: &Pixmap, mutu: u8) -> Result<Vec<u8>, NativeError> {
    let lebar = u16::try_from(pixmap.width())
        .map_err(|_| NativeError::raster(format!("lebar {} terlalu besar", pixmap.width())))?;
    let tinggi = u16::try_from(pixmap.height())
        .map_err(|_| NativeError::raster(format!("tinggi {} terlalu besar", pixmap.height())))?;
    let mut out = Vec::new();
    let enc = jpeg_encoder::Encoder::new(&mut out, mutu);
    enc.encode(
        &rgb_di_atas_putih(pixmap),
        lebar,
        tinggi,
        jpeg_encoder::ColorType::Rgb,
    )
    .map_err(|e| NativeError::raster(format!("JPEG gagal dikodekan: {e}")))?;
    Ok(out)
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
/// mengikuti `text-align` warisan; `justify` benar-benar meregangkan spasi pada
/// semua baris kecuali baris terakhir, sama seperti CSS. `font-weight` 600 ke
/// atas ditebalkan secara sintetis, karena mesin ini hanya menanam satu face:
/// glyph yang sama dilukis dua kali dengan geseran kecil. Kalau `potong` benar,
/// glyph dipotong ke kotak elemen lewat mask, jadi `overflow: hidden` benar
/// benar menyembunyikan bagian yang keluar, bukan sekadar diterima.
/// Mengembalikan `(baris, glyph)` yang benar-benar dilukis.
fn lukis_teks(
    pixmap: &mut Pixmap,
    isi: &str,
    gaya: &GayaTerhitung,
    kotak: &Kotak,
    warna: Color,
    transform: Transform,
    potong: bool,
) -> (usize, usize) {
    if !super::teks::tersedia() || warna.alpha() == 0.0 || kotak.width <= 0.0 {
        return (0, 0);
    }
    if gaya.ukuran_font <= 0.0 {
        return (0, 0);
    }
    let baris = super::teks::bungkus(isi, gaya.ukuran_font, kotak.width);
    let baseline_baris = super::teks::jarak_baseline(gaya.ukuran_font);
    let mut paint = Paint::default();
    paint.set_color(warna);
    paint.anti_alias = true;
    let mask = if potong {
        mask_kotak(pixmap, kotak, transform)
    } else {
        None
    };

    // Tebal sintetis: satu face hanya punya satu bobot, jadi glyph yang sama
    // dilukis ulang dengan geseran sepersekian em. Ambang 600 sama dengan
    // `font-weight: semibold` ke atas, dan nilainya nol untuk bobot normal
    // supaya tidak ada piksel tambahan yang tidak diminta.
    let tebal_sintetis = if gaya.tebal_font >= 600 {
        (gaya.ukuran_font * 0.03).max(0.4)
    } else {
        0.0
    };
    let perataan = gaya.perataan.as_deref();
    let rata_tengah = perataan == Some("center");
    let rata_kanan = matches!(perataan, Some("right") | Some("end"));
    let rata_justify = perataan == Some("justify");
    let jumlah_baris = baris.len();

    let mut dilukis_baris = 0usize;
    let mut dilukis_glyph = 0usize;
    for (i, isi_baris) in baris.iter().enumerate() {
        // Lebar baris dari hasil bentuk yang sama dengan yang dilukis. Kalau
        // angka ini dihitung dengan cara lain (mis. menjumlah lebar per
        // karakter), baris bersambung akan meleset dari perataan yang diminta.
        let glyph_baris = super::teks::bentuk(isi_baris, gaya.ukuran_font);
        let lebar_baris: f32 = glyph_baris.iter().map(|g| g.x_advance).sum();
        // Baris terakhir `justify` tetap rata kiri: CSS tidak meregangkan baris
        // penutup, dan meregangkannya akan membuat satu kata tampak terpecah.
        let (x_awal, tambahan_spasi) = if rata_justify && i + 1 < jumlah_baris {
            let spasi = isi_baris.chars().filter(|c| *c == ' ').count();
            let sisa = (kotak.width - lebar_baris).max(0.0);
            (kotak.x, if spasi > 0 { sisa / spasi as f32 } else { 0.0 })
        } else if rata_tengah {
            (kotak.x + (kotak.width - lebar_baris) / 2.0, 0.0)
        } else if rata_kanan {
            (kotak.x + kotak.width - lebar_baris, 0.0)
        } else {
            (kotak.x, 0.0)
        };
        let baseline = kotak.y + baseline_baris + i as f32 * gaya.tinggi_baris;
        let mut pena = x_awal;
        let mut ada_glyph = false;
        for glyph in &glyph_baris {
            // Skala per glyph, bukan satu skala untuk seluruh baris: tiap face
            // punya `units_per_em` sendiri, dan satu baris boleh memuat lebih
            // dari satu face.
            let skala_glyph = gaya.ukuran_font / f32::from(glyph.face.units_per_em());
            let mut pembangun = PembangunGlyph::default();
            if glyph.face.outline_glyph(glyph.id, &mut pembangun).is_some()
                && let Some(jalur) = pembangun.jalur.finish()
            {
                // Sumbu Y font ke atas, kanvas ke bawah: skala negatif pada Y.
                // `y_offset` HarfBuzz juga ke atas, jadi ia mengurangi
                // baseline. Pena dan baseline masih satuan CSS; `transform`
                // sudah membawa kerapatan dan geseran klip.
                let t = transform
                    .pre_translate(pena + glyph.x_offset, baseline - glyph.y_offset)
                    .pre_scale(skala_glyph, -skala_glyph);
                pixmap.fill_path(&jalur, &paint, FillRule::Winding, t, mask.as_ref());
                if tebal_sintetis > 0.0 {
                    let t2 = transform
                        .pre_translate(
                            pena + glyph.x_offset + tebal_sintetis,
                            baseline - glyph.y_offset,
                        )
                        .pre_scale(skala_glyph, -skala_glyph);
                    pixmap.fill_path(&jalur, &paint, FillRule::Winding, t2, mask.as_ref());
                }
                dilukis_glyph += 1;
                ada_glyph = true;
            }
            pena += glyph.x_advance;
            // Spasi dikenali dari offset byte-nya di teks asli, bukan dari
            // indeks urut: pada aksara yang menukar posisi, urutan glyph tidak
            // sama dengan urutan karakter.
            if isi_baris.as_bytes().get(glyph.cluster) == Some(&b' ') {
                pena += tambahan_spasi;
            }
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

/// Satu gradien latar yang sudah diurai dari deklarasi CSS.
#[derive(Debug, Clone, PartialEq)]
enum Gradien {
    /// `linear-gradient(<arah>, <stop>...)`. Sudut mengikuti CSS: `0deg` ke
    /// atas, `90deg` ke kanan.
    Linear {
        derajat: f32,
        stop: Vec<(f32, Color)>,
    },
    /// `radial-gradient(<stop>...)`. Pusat di tengah kotak, jari-jari setengah
    /// sisi terpanjang.
    Radial { stop: Vec<(f32, Color)> },
}

impl Gradien {
    fn stop(&self) -> &[(f32, Color)] {
        match self {
            Self::Linear { stop, .. } | Self::Radial { stop } => stop,
        }
    }
}

/// Mencari deklarasi gradien latar di antara deklarasi pemenang kaskade.
///
/// `background-image` didahulukan atas shorthand `background`, sama seperti
/// urutan prioritas di [`warna_latar`]. `None` berarti tidak ada gradien yang
/// bisa diurai; pemanggil yang memutuskan apakah itu dilaporkan.
fn gradien_latar(deklarasi: &[(String, String)]) -> Option<Gradien> {
    for kunci in ["background-image", "background"] {
        let Some((_, nilai)) = deklarasi.iter().find(|(k, _)| k == kunci) else {
            continue;
        };
        if let Some(g) = urai_gradien(nilai) {
            return Some(g);
        }
    }
    None
}

/// Mengurai `linear-gradient(...)` atau `radial-gradient(...)`.
///
/// Hanya bentuk yang benar-benar dilukis yang diterima: dua stop atau lebih,
/// warna yang dikenali [`urai_warna`], dan arah yang bisa dipetakan ke sudut.
/// Bentuk lain menghasilkan `None` supaya dilaporkan, bukan diganti gradien
/// karangan.
fn urai_gradien(nilai: &str) -> Option<Gradien> {
    let v = nilai.trim();
    let (linear, isi) = if let Some(s) = v.strip_prefix("linear-gradient(") {
        (true, s)
    } else {
        (false, v.strip_prefix("radial-gradient(")?)
    };
    let isi = isi.trim_end().strip_suffix(')')?;
    let bagian = pecah_koma(isi);
    if bagian.len() < 2 {
        return None;
    }
    // Argumen pertama hanya arah kalau ia memang arah. `red, blue` tidak punya
    // arah, dan itu berarti gradien ke bawah menurut CSS.
    let (derajat, sisa) = match urai_arah_gradien(&bagian[0]) {
        Some(d) => (d, &bagian[1..]),
        None if linear => (180.0, &bagian[..]),
        None => (0.0, &bagian[..]),
    };
    let mut stop = Vec::with_capacity(sisa.len());
    for (i, mentah) in sisa.iter().enumerate() {
        let (warna_teks, posisi) = pisah_posisi(mentah);
        let warna = urai_warna(&warna_teks)?;
        let p = posisi.unwrap_or_else(|| {
            if sisa.len() <= 1 {
                0.0
            } else {
                i as f32 / (sisa.len() - 1) as f32
            }
        });
        stop.push((p.clamp(0.0, 1.0), warna));
    }
    if stop.len() < 2 {
        return None;
    }
    if linear {
        Some(Gradien::Linear { derajat, stop })
    } else {
        Some(Gradien::Radial { stop })
    }
}

/// Memecah argumen fungsi CSS pada koma di kedalaman nol.
///
/// `rgb(1, 2, 3)` mengandung koma yang bukan pemisah argumen; memakai
/// `split(',')` biasa akan memotong warnanya jadi tiga bagian yang tidak bisa
/// diurai.
fn pecah_koma(isi: &str) -> Vec<String> {
    let mut hasil = Vec::new();
    let mut kini = String::new();
    let mut dalam = 0i32;
    for c in isi.chars() {
        match c {
            '(' => {
                dalam += 1;
                kini.push(c);
            }
            ')' => {
                dalam -= 1;
                kini.push(c);
            }
            ',' if dalam == 0 => {
                if !kini.trim().is_empty() {
                    hasil.push(kini.trim().to_owned());
                }
                kini.clear();
            }
            _ => kini.push(c),
        }
    }
    if !kini.trim().is_empty() {
        hasil.push(kini.trim().to_owned());
    }
    hasil
}

/// Arah gradien linear sebagai sudut CSS, atau `None` kalau bukan arah.
fn urai_arah_gradien(nilai: &str) -> Option<f32> {
    let v = nilai.trim().to_ascii_lowercase();
    if let Some(deg) = v.strip_suffix("deg") {
        return deg.trim().parse::<f32>().ok();
    }
    let sisa = v.strip_prefix("to ")?;
    let kata: Vec<&str> = sisa.split_whitespace().collect();
    match kata.as_slice() {
        ["top"] => Some(0.0),
        ["right"] => Some(90.0),
        ["bottom"] => Some(180.0),
        ["left"] => Some(270.0),
        ["top", "right"] | ["right", "top"] => Some(45.0),
        ["bottom", "right"] | ["right", "bottom"] => Some(135.0),
        ["bottom", "left"] | ["left", "bottom"] => Some(225.0),
        ["top", "left"] | ["left", "top"] => Some(315.0),
        _ => None,
    }
}

/// Memisahkan posisi opsional di ujung satu stop, mis. `#fff 40%`.
///
/// Pencarian dilakukan dari belakang pada kedalaman kurung nol, jadi spasi di
/// dalam `rgb(255, 255, 255)` tidak dianggap pemisah.
fn pisah_posisi(nilai: &str) -> (String, Option<f32>) {
    let mut dalam = 0i32;
    let mut potong = None;
    for (i, c) in nilai.char_indices().rev() {
        match c {
            ')' => dalam += 1,
            '(' => dalam -= 1,
            c if c.is_whitespace() && dalam == 0 => {
                potong = Some(i);
                break;
            }
            _ => {}
        }
    }
    let Some(i) = potong else {
        return (nilai.trim().to_owned(), None);
    };
    let (kiri, kanan) = nilai.split_at(i);
    let kanan = kanan.trim();
    match kanan
        .strip_suffix('%')
        .and_then(|p| p.trim().parse::<f32>().ok())
    {
        Some(p) => (kiri.trim().to_owned(), Some(p / 100.0)),
        None => (nilai.trim().to_owned(), None),
    }
}

/// Shader gradien untuk satu kotak, dalam koordinat CSS kotak itu.
///
/// Transformasi yang dipakai saat melukis ikut diterapkan ke shader, jadi
/// titik di sini cukup ditulis pada satuan CSS yang sama dengan jalurnya.
fn shader_gradien(g: &Gradien, rect: Rect) -> Option<Shader<'static>> {
    let stop: Vec<GradientStop> = g
        .stop()
        .iter()
        .map(|(p, c)| GradientStop::new(*p, *c))
        .collect();
    match g {
        Gradien::Linear { derajat, .. } => {
            let (x0, y0, x1, y1) = titik_linear(*derajat, rect);
            LinearGradient::new(
                Point::from_xy(x0, y0),
                Point::from_xy(x1, y1),
                stop,
                SpreadMode::Pad,
                Transform::identity(),
            )
        }
        Gradien::Radial { .. } => {
            let pusat = Point::from_xy(
                rect.x() + rect.width() / 2.0,
                rect.y() + rect.height() / 2.0,
            );
            let jari = rect.width().max(rect.height()) / 2.0;
            RadialGradient::new(
                pusat,
                pusat,
                jari,
                stop,
                SpreadMode::Pad,
                Transform::identity(),
            )
        }
    }
}

/// Titik awal dan akhir gradien linear pada satu kotak.
///
/// Sudut CSS 0 derajat berarti "ke atas", jadi vektornya `(sin, -cos)`. Panjang
/// garis diambil dari proyeksi kotak pada arah itu supaya seluruh kotak
/// tercakup, bukan cuma separuh.
fn titik_linear(derajat: f32, rect: Rect) -> (f32, f32, f32, f32) {
    let rad = derajat.to_radians();
    let (dx, dy) = (rad.sin(), -rad.cos());
    let cx = rect.x() + rect.width() / 2.0;
    let cy = rect.y() + rect.height() / 2.0;
    let setengah = (rect.width() * dx.abs() + rect.height() * dy.abs()) / 2.0;
    (
        cx - dx * setengah,
        cy - dy * setengah,
        cx + dx * setengah,
        cy + dy * setengah,
    )
}

/// Jalur kotak, dengan sudut membulat kalau `radius` positif.
///
/// Jari-jari dipotong ke setengah sisi terpendek, sama seperti CSS: radius yang
/// lebih besar dari kotaknya tidak boleh membuat jalur terbalik.
fn jalur_kotak(rect: Rect, radius: f32) -> Path {
    let r = radius.min(rect.width().min(rect.height()) / 2.0).max(0.0);
    if r <= 0.0 {
        return PathBuilder::from_rect(rect);
    }
    let (x, y, w, h) = (rect.x(), rect.y(), rect.width(), rect.height());
    // Konstanta busur lingkaran: 4/3 * (sqrt(2) - 1). Nilai ini yang membuat
    // cubic mendekati seperempat lingkaran, bukan kurva yang lebih gepeng.
    let k = 0.552_284_8 * r;
    let mut b = PathBuilder::new();
    b.move_to(x + r, y);
    b.line_to(x + w - r, y);
    b.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    b.line_to(x + w, y + h - r);
    b.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    b.line_to(x + r, y + h);
    b.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    b.line_to(x, y + r);
    b.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    b.close();
    // Satu subpath dengan lima ruas tidak mungkin kosong; kalau `finish`
    // mengembalikan `None`, kotaknya nol dan pemanggil tidak melukis apa pun.
    b.finish().unwrap_or_else(|| PathBuilder::from_rect(rect))
}

/// `border-radius` dari deklarasi pemenang, dalam px.
///
/// Bentuk empat nilai (`10px 10px 0 0`) dan bentuk garis miring
/// (`10px / 20px`) belum dipetakan: yang dipakai nilai pertama, dan sisanya
/// bukan tebakan.
fn sudut_radius(deklarasi: &[(String, String)]) -> f32 {
    deklarasi
        .iter()
        .find(|(k, _)| k == "border-radius")
        .and_then(|(_, v)| v.split_whitespace().next().and_then(panjang_px))
        .unwrap_or(0.0)
        .max(0.0)
}

/// Gaya garis tepi yang dikenali. `solid` satu-satunya yang dilukis; sisanya
/// dilaporkan, bukan diganti garis penuh.
const GAYA_TEPI: &[&str] = &[
    "solid", "dashed", "dotted", "double", "none", "hidden", "groove", "ridge", "inset", "outset",
];

/// Panjang CSS sederhana dalam px. `None` untuk nilai yang bukan panjang.
fn panjang_px(nilai: &str) -> Option<f32> {
    let v = nilai.trim();
    let angka = v.strip_suffix("px").map(str::trim).unwrap_or(v);
    angka.parse::<f32>().ok().map(|n| n.max(0.0))
}

/// Garis tepi yang harus dilukis untuk satu kotak: `(tebal, warna)`.
///
/// `None` berarti tidak ada garis yang dilukis, dan alasannya sudah masuk
/// [`Raster::diabaikan`] kalau bentuknya dikenali tapi tidak didukung.
fn tepi(
    deklarasi: &[(String, String)],
    diabaikan: &mut Vec<String>,
    node: NodeId,
) -> Option<(f32, Color)> {
    let ambil = |nama: &str| {
        deklarasi
            .iter()
            .find(|(k, _)| k == nama)
            .map(|(_, v)| v.trim())
    };
    let mut tebal: Option<f32> = None;
    let mut gaya: Option<String> = None;
    let mut warna: Option<Color> = None;
    if let Some(v) = ambil("border") {
        for bagian in v.split_whitespace() {
            if let Some(px) = panjang_px(bagian) {
                tebal = Some(px);
            } else if GAYA_TEPI.contains(&bagian) {
                gaya = Some(bagian.to_owned());
            } else if let Some(c) = urai_warna(bagian) {
                warna = Some(c);
            }
        }
    }
    if tebal.is_none() {
        tebal = ambil("border-width").and_then(panjang_px);
    }
    if gaya.is_none() {
        gaya = ambil("border-style").map(str::to_owned);
    }
    if warna.is_none() {
        warna = ambil("border-color").and_then(urai_warna);
    }
    let tebal = tebal?;
    let gaya = gaya.unwrap_or_else(|| "none".to_owned());
    if tebal <= 0.0 || gaya == "none" || gaya == "hidden" {
        return None;
    }
    if gaya != "solid" {
        diabaikan.push(format!(
            "node {node}: `border-style: {gaya}` tidak didukung"
        ));
        return None;
    }
    let warna = warna?;
    if warna.alpha() == 0.0 {
        return None;
    }
    Some((tebal, warna))
}

/// Gambar yang sudah didekode, RGBA *straight* (bukan premultiplied).
struct GambarDekode {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

/// Mendekode PNG atau JPEG dari byte berkas, menurut magic bytes-nya.
///
/// Format lain menghasilkan `None` supaya pemanggil melaporkannya. Menebak
/// format dari ekstensi `src` akan salah untuk URL tanpa ekstensi.
fn dekode_gambar(bytes: &[u8]) -> Option<GambarDekode> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return dekode_png(bytes);
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return dekode_jpeg(bytes);
    }
    None
}

fn dekode_png(bytes: &[u8]) -> Option<GambarDekode> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // Semua kedalaman bit dan tipe warna dinormalkan ke 8 bit per kanal dulu,
    // supaya jalur di bawah hanya menangani satu bentuk.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => rgb_ke_rgba(&buf),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        // Palet seharusnya sudah dinormalkan oleh transformasi di atas; kalau
        // masih sampai di sini, PLTE-nya tidak terbaca dan itu bukan gambar
        // yang bisa dilukis dengan benar.
        png::ColorType::Indexed => return None,
    };
    let (width, height) = (info.width, info.height);
    if rgba.len() != (width as usize) * (height as usize) * 4 {
        return None;
    }
    Some(GambarDekode {
        rgba,
        width,
        height,
    })
}

fn dekode_jpeg(bytes: &[u8]) -> Option<GambarDekode> {
    let mut decoder =
        zune_jpeg::JpegDecoder::new(zune_jpeg::zune_core::bytestream::ZCursor::new(bytes));
    let pixels = decoder.decode().ok()?;
    let info = decoder.info()?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    let jumlah = (width as usize) * (height as usize);
    if jumlah == 0 {
        return None;
    }
    // Dekoder bisa mengeluarkan RGB atau RGBA menurut warna sumbernya; yang
    // menentukan adalah panjang buffer, bukan asumsi.
    let rgba = match pixels.len() / jumlah {
        3 => rgb_ke_rgba(&pixels),
        4 => pixels,
        _ => return None,
    };
    if rgba.len() != jumlah * 4 {
        return None;
    }
    Some(GambarDekode {
        rgba,
        width,
        height,
    })
}

fn rgb_ke_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for p in rgb.as_chunks::<3>().0 {
        out.extend_from_slice(&[p[0], p[1], p[2], 255]);
    }
    out
}

/// `tiny-skia` menyimpan warna premultiplied; dekoder mengeluarkan straight.
fn premultiply(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for p in rgba.as_chunks::<4>().0 {
        let a = u32::from(p[3]);
        out.push((u32::from(p[0]) * a / 255) as u8);
        out.push((u32::from(p[1]) * a / 255) as u8);
        out.push((u32::from(p[2]) * a / 255) as u8);
        out.push(p[3]);
    }
    out
}

/// Melukis satu gambar ke dalam kotaknya, diskalakan mengikuti ukuran kotak.
///
/// `object-fit` bawaan CSS untuk `<img>` adalah `fill`: gambar meregang mengisi
/// kotak tanpa menjaga rasio. Itu yang dilakukan di sini, bukan `contain`.
fn lukis_gambar(
    pixmap: &mut Pixmap,
    gambar: &GambarDekode,
    rect: Rect,
    radius: f32,
    transform: Transform,
) {
    let Some(size) = tiny_skia::IntSize::from_wh(gambar.width, gambar.height) else {
        return;
    };
    let Some(sumber) = Pixmap::from_vec(premultiply(&gambar.rgba), size) else {
        return;
    };
    let sx = rect.width() / gambar.width as f32;
    let sy = rect.height() / gambar.height as f32;
    let t = transform
        .pre_translate(rect.x(), rect.y())
        .pre_scale(sx, sy);
    let mask = if radius > 0.0 {
        let mut mask = match Mask::new(pixmap.width(), pixmap.height()) {
            Some(m) => m,
            None => return,
        };
        mask.fill_path(
            &jalur_kotak(rect, radius),
            FillRule::Winding,
            true,
            transform,
        );
        Some(mask)
    } else {
        None
    };
    let paint = PixmapPaint {
        quality: FilterQuality::Bilinear,
        ..Default::default()
    };
    pixmap.draw_pixmap(0, 0, sumber.as_ref(), &paint, t, mask.as_ref());
}

/// Mask kotak untuk memotong glyph ke batas elemennya.
fn mask_kotak(pixmap: &Pixmap, kotak: &Kotak, transform: Transform) -> Option<Mask> {
    let rect = Rect::from_xywh(kotak.x, kotak.y, kotak.width, kotak.height)?;
    let mut mask = Mask::new(pixmap.width(), pixmap.height())?;
    mask.fill_path(&jalur_kotak(rect, 0.0), FillRule::Winding, true, transform);
    Some(mask)
}

/// Apakah teks node ini harus dipotong ke kotaknya.
///
/// Diputuskan dari `overflow` elemen pemilik teks dan leluhurnya, karena
/// `overflow: hidden` pada wadah memotong isi anaknya juga. `visible` (bawaan
/// CSS) berarti glyph boleh keluar kotak, dan itu yang terjadi sebelumnya.
fn potong_teks(doc: &Document, laporan: &layout::LaporanLayout, node: NodeId) -> bool {
    let mut kini = Some(node);
    let mut kedalaman = 0;
    while let Some(id) = kini {
        if let Some(deklarasi) = laporan.gaya.get(&id)
            && let Some((_, nilai)) = deklarasi.iter().find(|(k, _)| k == "overflow")
        {
            let v = nilai.trim();
            if v.starts_with("hidden")
                || v.starts_with("clip")
                || v.starts_with("scroll")
                || v.starts_with("auto")
            {
                return true;
            }
        }
        kini = doc.node(id).and_then(|n| n.parent);
        kedalaman += 1;
        // Batas kedalaman menjaga siklus parent yang rusak (kalau ada) tidak
        // berubah jadi loop tak berujung di jalur lukis.
        if kedalaman > 64 {
            break;
        }
    }
    false
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
        assert_eq!(ukuran_png(&raster.data), (320, 200));
        assert_eq!(raster.format, Format::Png { cepat: false });
        assert_eq!(raster.format.mime(), "image/png");
        assert_eq!(&raster.data[..8], raster.format.magic());
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
    fn warna_dari_lembar_eksternal_terlukis() {
        let doc = Document::parse_html(
            "<html><head><link rel=\"stylesheet\" href=\"warna.css\"></head>\
             <body><div id=\"kotak\"></div></body></html>",
        )
        .unwrap();
        let lembar = [LembarLuar::baru(
            "warna.css",
            "#kotak{width:30px;height:30px;background-color:#0000ff}",
        )];
        let raster = lukis_dengan_lembar(
            &doc,
            Viewport::new(60.0, 60.0),
            None,
            Format::Png { cepat: false },
            &lembar,
        )
        .unwrap();
        assert_eq!(
            raster.kotak_dilukis, 1,
            "warna dari lembar eksternal harus ikut dilukis"
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
        let klip = Klip::baru(10.0, 10.0, 20.0, 20.0);
        let raster = lukis(&doc, Viewport::new(100.0, 100.0), Some(klip)).unwrap();
        assert_eq!(ukuran_png(&raster.data), (20, 20));
        assert_eq!(raster.piksel(0, 0), Some([0, 255, 0, 255]));
    }

    /// `clip.scale` melipatgandakan ukuran keluaran dengan merender ulang pada
    /// kerapatan lebih tinggi, bukan memperbesar gambar yang sudah jadi.
    #[test]
    fn skala_klip_melipatgandakan_piksel() {
        let doc = Document::parse_html(
            "<body><div style=\"width:40px;height:40px;background-color:#00ff00\"></div></body>",
        )
        .unwrap();
        let mut klip = Klip::baru(10.0, 10.0, 20.0, 20.0);
        klip.scale = 2.0;
        let raster = lukis(&doc, Viewport::new(100.0, 100.0), Some(klip)).unwrap();
        assert_eq!(ukuran_png(&raster.data), (40, 40), "20 px CSS pada skala 2");
        assert_eq!(raster.width, 40);
        assert_eq!(raster.piksel(0, 0), Some([0, 255, 0, 255]));
        assert_eq!(
            raster.piksel(39, 39),
            Some([0, 255, 0, 255]),
            "kotak 40x40 CSS menutup seluruh klip 20x20 CSS"
        );
    }

    /// Skala juga memperbesar glyph: teks yang sama pada skala 2 menutup dua
    /// kali lipat piksel, jadi tidak ada alfa yang hilang saat diperbesar.
    #[test]
    fn skala_klip_memperbesar_glyph() {
        let doc = Document::parse_html("<body><p>WWWW</p></body>").unwrap();
        let satu = lukis(&doc, Viewport::new(200.0, 100.0), None).unwrap();
        let mut klip = Klip::baru(0.0, 0.0, 200.0, 100.0);
        klip.scale = 2.0;
        let dua = lukis(&doc, Viewport::new(200.0, 100.0), Some(klip)).unwrap();
        assert_eq!((dua.width, dua.height), (400, 200));
        let gelap_satu = jumlah_piksel_gelap(&satu);
        let gelap_dua = jumlah_piksel_gelap(&dua);
        assert!(gelap_satu > 0, "teks harus terlukis pada skala 1");
        assert!(
            gelap_dua > gelap_satu * 3,
            "skala 2 harus menutup sekitar empat kali piksel: {gelap_satu} -> {gelap_dua}"
        );
    }

    /// Format JPEG harus mengeluarkan JPEG sungguhan, bukan PNG yang dilabeli.
    #[test]
    fn jpeg_memuat_magic_bytes_jpeg() {
        let doc = Document::parse_html(
            "<body><div style=\"width:30px;height:30px;background-color:#0000ff\"></div></body>",
        )
        .unwrap();
        let raster = lukis_dengan(
            &doc,
            Viewport::new(60.0, 60.0),
            None,
            Format::Jpeg { mutu: 80 },
        )
        .unwrap();
        assert_eq!(&raster.data[..3], b"\xff\xd8\xff", "harus JPEG sungguhan");
        assert_eq!(raster.format.mime(), "image/jpeg");
        assert_eq!(raster.kotak_dilukis, 1);
        assert!(
            !raster.data.starts_with(b"\x89PNG"),
            "JPEG tidak boleh berupa PNG"
        );
    }

    /// `optimizeForSpeed` mengubah hasil encode, bukan diterima lalu diabaikan:
    /// kompresi cepat menghasilkan berkas yang tidak lebih kecil daripada
    /// kompresi rapat untuk isi yang sama.
    #[test]
    fn optimize_for_speed_memilih_kompresi_cepat() {
        let doc = Document::parse_html(
            "<body><div style=\"width:120px;height:120px;background-color:#123456\">\
             <p>teks untuk mengisi kanvas</p></div></body>",
        )
        .unwrap();
        let lambat = lukis_dengan(
            &doc,
            Viewport::new(120.0, 120.0),
            None,
            Format::Png { cepat: false },
        )
        .unwrap();
        let cepat = lukis_dengan(
            &doc,
            Viewport::new(120.0, 120.0),
            None,
            Format::Png { cepat: true },
        )
        .unwrap();
        assert_eq!(&lambat.data[..8], Format::Png { cepat: false }.magic());
        assert_eq!(&cepat.data[..8], Format::Png { cepat: false }.magic());
        assert!(
            cepat.data.len() >= lambat.data.len(),
            "kompresi cepat tidak boleh lebih kecil: {} vs {}",
            cepat.data.len(),
            lambat.data.len()
        );
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
    fn aksara_dari_face_kedua_dilukis_bukan_notdef() {
        // Rantai face harus sampai ke raster, bukan cuma ke pengukuran: kalau
        // `paint.rs` masih mengambil outline dari face pertama, aksara
        // Devanagari keluar sebagai `.notdef` dan kanvasnya jadi IDENTIK
        // dengan kanvas tiga karakter CJK yang juga `.notdef`.
        //
        // Tiga aksara, bukan satu: satu `क` dan satu `.notdef` kebetulan
        // sama-sama tipis (46 lawan 47 piksel gelap), jadi jumlah piksel saja
        // tidak membedakan keduanya. Tiga glyph mengakumulasi selisih maju
        // (12.192 px lawan 9.602 px per glyph), dan bitmap yang identik hanya
        // mungkin kalau outline-nya memang sama.
        let deva = Document::parse_html("<body><p>\u{0915}\u{0916}\u{0917}</p></body>").unwrap();
        let notdef = Document::parse_html("<body><p>\u{4f60}\u{597d}\u{4f60}</p></body>").unwrap();
        let r_deva = lukis(&deva, Viewport::new(160.0, 60.0), None).unwrap();
        let r_notdef = lukis(&notdef, Viewport::new(160.0, 60.0), None).unwrap();
        assert_eq!(r_deva.glyph_dilukis, 3, "tiga glyph Devanagari");
        assert_eq!(r_notdef.glyph_dilukis, 3, "tiga glyph .notdef");
        let bitmap = |r: &Raster| -> Vec<[u8; 4]> {
            (0..60u32)
                .flat_map(|y| (0..160u32).map(move |x| (x, y)))
                .filter_map(|(x, y)| r.piksel(x, y))
                .collect()
        };
        assert_ne!(
            bitmap(&r_deva),
            bitmap(&r_notdef),
            "aksara Devanagari tidak boleh dilukis dengan outline .notdef"
        );
        let kanan = |r: &Raster| kolom_gelap_maks(r);
        assert!(
            kanan(&r_deva) > kanan(&r_notdef),
            "tiga glyph dari face kedua lebih lebar (12.192 px/glyph) dari \
             .notdef face pertama (9.602 px/glyph): {} lawan {}",
            kanan(&r_deva),
            kanan(&r_notdef)
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

    /// Kolom paling kanan yang memuat piksel gelap di seluruh kanvas.
    fn kolom_gelap_maks(raster: &Raster) -> u32 {
        (0..raster.width)
            .rev()
            .find(|&x| piksel_gelap_di(raster, x, 0, 1, raster.height) > 0)
            .unwrap_or(0)
    }

    /// PNG kecil satu warna, dibuat di tes supaya tidak ada berkas biner yang
    /// harus ikut masuk repo.
    fn png_satu_warna(w: u32, h: u32, warna: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().expect("header PNG");
            let data: Vec<u8> = (0..w * h).flat_map(|_| warna).collect();
            writer.write_image_data(&data).expect("data PNG");
        }
        out
    }

    fn jpeg_satu_warna(w: u16, h: u16, warna: [u8; 3]) -> Vec<u8> {
        let mut out = Vec::new();
        let enc = jpeg_encoder::Encoder::new(&mut out, 90);
        let data: Vec<u8> = (0..w as usize * h as usize).flat_map(|_| warna).collect();
        enc.encode(&data, w, h, jpeg_encoder::ColorType::Rgb)
            .expect("data JPEG");
        out
    }

    /// `linear-gradient(180deg, ...)` harus berjalan dari atas ke bawah: dua
    /// ujung kotak berwarna berbeda, bukan satu warna datar.
    #[test]
    fn gradien_linear_melukis_dua_ujung() {
        let doc = Document::parse_html(
            "<body><div style=\"width:40px;height:40px;\
             background:linear-gradient(180deg,#ff0000,#0000ff)\"></div></body>",
        )
        .unwrap();
        let raster = lukis(&doc, Viewport::new(60.0, 60.0), None).unwrap();
        assert_eq!(
            raster.kotak_dilukis, 1,
            "gradien harus dilukis sebagai latar"
        );
        let atas = raster.piksel(20, 2).unwrap();
        let bawah = raster.piksel(20, 37).unwrap();
        assert!(
            atas[0] > 150 && atas[2] < 100,
            "ujung atas harus merah, dapat {atas:?}"
        );
        assert!(
            bawah[2] > 150 && bawah[0] < 100,
            "ujung bawah harus biru, dapat {bawah:?}"
        );
    }

    /// `radial-gradient` memusatkan stop pertama di tengah kotak: tengah
    /// berbeda dari sudut, dan arahnya tidak boleh tertukar.
    #[test]
    fn gradien_radial_memusat_di_tengah() {
        let doc = Document::parse_html(
            "<body><div style=\"width:60px;height:60px;\
             background:radial-gradient(#ff0000,#0000ff)\"></div></body>",
        )
        .unwrap();
        let raster = lukis(&doc, Viewport::new(80.0, 80.0), None).unwrap();
        let tengah = raster.piksel(30, 30).unwrap();
        let sudut = raster.piksel(2, 2).unwrap();
        assert!(
            tengah[0] > 150 && tengah[2] < 100,
            "pusat harus memakai stop pertama, dapat {tengah:?}"
        );
        assert!(
            sudut[2] > 150 && sudut[0] < 100,
            "sudut harus memakai stop terakhir, dapat {sudut:?}"
        );
    }

    /// `border: 4px solid` dilukis di dalam kotak, jadi piksel di tepi berwarna
    /// garis dan piksel di tengah tetap warna latar.
    #[test]
    fn border_dilukis_di_dalam_kotak() {
        let doc = Document::parse_html(
            "<body><div style=\"width:40px;height:40px;background:#ffffff;\
             border:4px solid #00ff00\"></div></body>",
        )
        .unwrap();
        let raster = lukis(&doc, Viewport::new(60.0, 60.0), None).unwrap();
        let tepi = raster.piksel(1, 20).unwrap();
        let tengah = raster.piksel(20, 20).unwrap();
        assert!(
            tepi[1] > 150 && tepi[0] < 100 && tepi[2] < 100,
            "piksel tepi harus hijau, dapat {tepi:?}"
        );
        assert_eq!(tengah, [255, 255, 255, 255], "tengah tetap latar putih");
    }

    /// `border-radius` memotong sudut latar: pojok kotak kembali putih.
    #[test]
    fn border_radius_memotong_sudut() {
        let doc = Document::parse_html(
            "<body><div style=\"width:40px;height:40px;background:#ff0000;\
             border-radius:20px\"></div></body>",
        )
        .unwrap();
        let raster = lukis(&doc, Viewport::new(60.0, 60.0), None).unwrap();
        assert_eq!(
            raster.piksel(0, 0),
            Some([255, 255, 255, 255]),
            "sudut membulat harus tembus ke latar kanvas"
        );
        assert_eq!(
            raster.piksel(20, 20),
            Some([255, 0, 0, 255]),
            "tengah kotak tetap terlukis"
        );
    }

    /// `font-weight: 700` menebalkan glyph secara sintetis: piksel gelap harus
    /// bertambah, bukan sama dengan bobot normal.
    #[test]
    fn tebal_sintetis_menambah_piksel_gelap() {
        let lukis_tebal = |berat: u16| {
            let html = format!("<body><p style=\"font-weight:{berat}\">Tebal</p></body>");
            let doc = Document::parse_html(&html).unwrap();
            let raster = lukis(&doc, Viewport::new(200.0, 80.0), None).unwrap();
            assert_eq!(raster.glyph_dilukis, 5, "lima huruf harus jadi glyph");
            jumlah_piksel_gelap(&raster)
        };
        let normal = lukis_tebal(400);
        let tebal = lukis_tebal(700);
        assert!(normal > 0, "bobot normal tetap harus terlukis");
        assert!(
            tebal > normal,
            "bobot 700 harus menutup lebih banyak piksel: {tebal} vs {normal}"
        );
    }

    /// `text-align: justify` meregangkan spasi sampai tepi kanan kotak, bukan
    /// berhenti seperti rata kiri.
    #[test]
    fn justify_meregangkan_sampai_tepi_kanan() {
        let lukis_perataan = |perataan: &str| {
            let html = format!(
                "<body><p style=\"width:100px;text-align:{perataan}\">\
                 kata kata kata kata kata kata</p></body>"
            );
            let doc = Document::parse_html(&html).unwrap();
            let raster = lukis(&doc, Viewport::new(160.0, 120.0), None).unwrap();
            assert!(raster.baris_dilukis > 1, "teks harus dibungkus");
            kolom_gelap_maks(&raster)
        };
        let kiri = lukis_perataan("left");
        let justify = lukis_perataan("justify");
        assert!(
            justify >= 95,
            "justify harus mendekati tepi kanan 100px, dapat {justify}"
        );
        assert!(kiri < 95, "rata kiri tidak meregangkan spasi, dapat {kiri}");
    }

    /// `overflow: hidden` memotong glyph ke kotak elemen; tanpa itu baris yang
    /// lebih lebar tetap tergambar penuh seperti di CSS `visible`.
    #[test]
    fn overflow_hidden_memotong_glyph() {
        let lukis_dengan_overflow = |tambahan: &str| {
            let html = format!("<body><p style=\"width:30px;{tambahan}\">WWWWWWWWWWWW</p></body>");
            let doc = Document::parse_html(&html).unwrap();
            let raster = lukis(&doc, Viewport::new(200.0, 80.0), None).unwrap();
            kolom_gelap_maks(&raster)
        };
        let terbuka = lukis_dengan_overflow("");
        let terpotong = lukis_dengan_overflow("overflow:hidden;");
        assert!(
            terbuka > 50,
            "tanpa overflow, kata panjang harus meluber, dapat {terbuka}"
        );
        assert!(
            terpotong <= 31,
            "overflow:hidden harus memotong di batas 30px, dapat {terpotong}"
        );
    }

    /// `<img>` dilukis dari byte PNG yang diserahkan pemanggil, bukan dari
    /// jaringan: yang diuji adalah dekode dan penempatan pikselnya.
    #[test]
    fn gambar_png_dilukis_dari_byte() {
        let doc = Document::parse_html(
            "<body><img src=\"a.png\" style=\"width:20px;height:20px\"></body>",
        )
        .unwrap();
        let mut gambar = HashMap::new();
        gambar.insert("a.png".to_owned(), png_satu_warna(4, 4, [255, 0, 0, 255]));
        let raster = lukis_dengan_sumber(
            &doc,
            Viewport::new(60.0, 60.0),
            None,
            Format::Png { cepat: false },
            &[],
            &gambar,
        )
        .unwrap();
        assert_eq!(raster.gambar_dilukis, 1, "satu <img> harus terlukis");
        assert_eq!(
            raster.piksel(5, 5),
            Some([255, 0, 0, 255]),
            "isi gambar harus mendarat di dalam kotaknya"
        );
    }

    /// JPEG didekode lewat jalur yang berbeda dari PNG; kalau jalur itu tidak
    /// ada, `<img src="x.jpg">` akan tampil kosong tanpa laporan.
    #[test]
    fn gambar_jpeg_dilukis_dari_byte() {
        let doc = Document::parse_html(
            "<body><img src=\"b.jpg\" style=\"width:24px;height:24px\"></body>",
        )
        .unwrap();
        let mut gambar = HashMap::new();
        gambar.insert("b.jpg".to_owned(), jpeg_satu_warna(8, 8, [0, 0, 255]));
        let raster = lukis_dengan_sumber(
            &doc,
            Viewport::new(60.0, 60.0),
            None,
            Format::Png { cepat: false },
            &[],
            &gambar,
        )
        .unwrap();
        assert_eq!(raster.gambar_dilukis, 1, "JPEG harus ikut terlukis");
        let p = raster.piksel(6, 6).unwrap();
        assert!(
            p[2] > 150 && p[0] < 100 && p[1] < 100,
            "piksel harus biru dari JPEG, dapat {p:?}"
        );
    }

    /// `src` tanpa byte tidak dilukis dan dilaporkan, bukan diganti kotak
    /// abu-abu yang terlihat seperti gambar rusak.
    #[test]
    fn gambar_tanpa_byte_dilaporkan() {
        let doc = Document::parse_html(
            "<body><img src=\"hilang.png\" style=\"width:20px;height:20px\"></body>",
        )
        .unwrap();
        let raster = lukis(&doc, Viewport::new(60.0, 60.0), None).unwrap();
        assert_eq!(raster.gambar_dilukis, 0);
        assert!(
            raster
                .diabaikan
                .iter()
                .any(|s| s.contains("hilang.png") && s.contains("tidak diserahkan")),
            "src yang tidak diserahkan harus dilaporkan: {:?}",
            raster.diabaikan
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
