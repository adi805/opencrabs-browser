//! Pengukuran teks dari satu face TrueType yang ditanam di dalam biner.
//!
//! Kenapa ditanam, bukan dibaca dari sistem: angka layout dan PNG harus sama
//! di laptop, di runner CI, dan di container. Font sistem berbeda di tiap
//! host, jadi lebar teks yang diukur dari sana membuat kotak teks berubah
//! tanpa satu baris kode berubah. Berkas dan lisensinya:
//! `crates/engine/assets/README.md`.
//!
//! Yang dibaca dari face hanya yang dibutuhkan pengukuran: `units_per_em`,
//! `ascender`/`descender`/`line_gap`, lebar glyph (`hmtx`), dan pemetaan
//! code point ke glyph (`cmap`). Tidak ada shaping, kerning, ligatur, atau
//! font fallback; aksara di luar cakupan face diukur selebar `.notdef`, dan
//! batas itu disebut di [`super::CELAH`], bukan ditutupi.
//!
//! Perataan spasi mengikuti `white-space: normal`: rentetan spasi, tab, dan
//! baris baru diruntuhkan jadi satu spasi, lalu dipangkas di kedua ujung.
//! Lapisan lukis wajib memakai [`rapikan`] yang sama supaya yang dilukis
//! adalah teks yang diukur, bukan teks mentah.

use std::sync::OnceLock;

/// Face yang ditanam. Isinya diperiksa oleh `font_tanam_bisa_dibaca`.
const FONT: &[u8] = include_bytes!("../../assets/DejaVuSans.ttf");

/// Face yang dipakai seluruh mesin, diurai sekali.
///
/// `None` hanya mungkin kalau berkas yang ditanam rusak, dan itu berarti
/// build-nya salah, bukan input pengguna. Pemanggil yang menerima `None`
/// melaporkannya sebagai catatan, bukan menebak lebar karakter.
fn metrik() -> Option<&'static ttf_parser::Face<'static>> {
    static METRIK: OnceLock<Option<ttf_parser::Face<'static>>> = OnceLock::new();
    METRIK
        .get_or_init(|| ttf_parser::Face::parse(FONT, 0).ok())
        .as_ref()
}

/// Apakah metrik font bisa dipakai. Dipakai tes dan laporan layout.
#[must_use]
pub fn tersedia() -> bool {
    metrik().is_some()
}

/// `units_per_em` face yang ditanam. Dipakai tes untuk mengunci identitas font.
#[must_use]
pub fn units_per_em() -> Option<u16> {
    metrik().map(ttf_parser::Face::units_per_em)
}

/// Tinggi satu baris menurut metrik font, dalam piksel.
///
/// Rumusnya `(ascender - descender + line_gap) / units_per_em * ukuran_font`.
/// Ini nilai `line-height: normal` yang sebenarnya, bukan 1.2 x font-size yang
/// cuma perkiraan.
#[must_use]
pub fn tinggi_baris_normal(ukuran_font: f32) -> f32 {
    let Some(face) = metrik() else {
        return 0.0;
    };
    let naik = f32::from(face.ascender());
    let turun = f32::from(face.descender());
    let celah = f32::from(face.line_gap());
    (naik - turun + celah) / f32::from(face.units_per_em()) * ukuran_font
}

/// Lebar satu karakter dalam piksel, termasuk fallback `.notdef`.
fn lebar_char(face: &ttf_parser::Face<'_>, c: char, skala: f32) -> f32 {
    let glyph = face.glyph_index(c).unwrap_or(ttf_parser::GlyphId(0));
    face.glyph_hor_advance(glyph)
        .map_or(0.0, |advance| f32::from(advance) * skala)
}

/// Lebar satu baris teks dalam piksel, tanpa membungkus.
///
/// Teks diruntuhkan spasinya lebih dulu, jadi `"a   b"` selebar `"a b"`.
#[must_use]
pub fn lebar(teks: &str, ukuran_font: f32) -> f32 {
    let Some(face) = metrik() else {
        return 0.0;
    };
    let skala = ukuran_font / f32::from(face.units_per_em());
    rapikan(teks)
        .chars()
        .map(|c| lebar_char(face, c, skala))
        .sum()
}

/// Rentetan spasi, tab, dan baris baru jadi satu spasi, lalu dipangkas.
///
/// Ini aturan `white-space: normal` yang paling dasar. Ia tidak menangani
/// `white-space: pre` atau `&nbsp;`, dan itu batas yang dicatat di
/// [`super::CELAH`].
#[must_use]
pub fn rapikan(teks: &str) -> String {
    let mut hasil = String::with_capacity(teks.len());
    let mut spasi_terakhir = true;
    for c in teks.chars() {
        if c.is_whitespace() {
            if !spasi_terakhir {
                hasil.push(' ');
                spasi_terakhir = true;
            }
        } else {
            hasil.push(c);
            spasi_terakhir = false;
        }
    }
    if hasil.ends_with(' ') {
        hasil.pop();
    }
    hasil
}

/// Bungkus teks jadi baris yang muat dalam `lebar_maks` piksel.
///
/// Pembungkusannya greedy pada batas kata, seperti `word-wrap: normal`: satu
/// kata yang lebih lebar dari batas dibiarkan melebar sendirian di barisnya,
/// bukan dipotong. Pemotongan kata dan tanda hubung belum ada, dan itu batas
/// yang dicatat di [`super::CELAH`].
///
/// `lebar_maks <= 0.0` berarti tidak ada batas: seluruh teks jadi satu baris.
#[must_use]
pub fn bungkus(teks: &str, ukuran_font: f32, lebar_maks: f32) -> Vec<String> {
    let rapi = rapikan(teks);
    if rapi.is_empty() {
        return Vec::new();
    }
    let Some(face) = metrik() else {
        return vec![rapi];
    };
    if lebar_maks <= 0.0 {
        return vec![rapi];
    }
    let skala = ukuran_font / f32::from(face.units_per_em());
    let lebar_spasi = lebar_char(face, ' ', skala);

    let mut baris: Vec<String> = Vec::new();
    let mut sekarang = String::new();
    let mut lebar_sekarang = 0.0_f32;

    for kata in rapi.split(' ') {
        let lebar_kata = lebar_kata(face, kata, skala);
        if sekarang.is_empty() {
            sekarang.push_str(kata);
            lebar_sekarang = lebar_kata;
        } else if lebar_sekarang + lebar_spasi + lebar_kata <= lebar_maks {
            sekarang.push(' ');
            sekarang.push_str(kata);
            lebar_sekarang += lebar_spasi + lebar_kata;
        } else {
            baris.push(std::mem::take(&mut sekarang));
            sekarang.push_str(kata);
            lebar_sekarang = lebar_kata;
        }
    }
    if !sekarang.is_empty() {
        baris.push(sekarang);
    }
    baris
}

fn lebar_kata(face: &ttf_parser::Face<'_>, kata: &str, skala: f32) -> f32 {
    kata.chars().map(|c| lebar_char(face, c, skala)).sum()
}

/// Hasil pengukuran satu blok teks setelah dibungkus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UkuranTeks {
    /// Lebar baris terlebar, dalam piksel.
    pub lebar: f32,
    /// `baris` dikali tinggi baris yang diberikan pemanggil.
    pub tinggi: f32,
    /// Jumlah baris setelah pembungkusan.
    pub baris: usize,
}

/// Ukur satu blok teks untuk kotak berlebar `lebar_maks`.
///
/// `tinggi_baris` datang dari gaya terhitung (`line-height`), bukan dihitung
/// ulang di sini: lapisan lukis dan layout harus memakai angka yang sama.
/// `lebar_maks` `None` berarti teks tidak dibungkus.
#[must_use]
pub fn ukuran(
    teks: &str,
    ukuran_font: f32,
    tinggi_baris: f32,
    lebar_maks: Option<f32>,
) -> UkuranTeks {
    let baris = match lebar_maks {
        Some(maks) => bungkus(teks, ukuran_font, maks),
        None => {
            let rapi = rapikan(teks);
            if rapi.is_empty() {
                Vec::new()
            } else {
                vec![rapi]
            }
        }
    };
    let lebar = baris
        .iter()
        .map(|b| lebar(b, ukuran_font))
        .fold(0.0_f32, f32::max);
    let jumlah = baris.len();
    UkuranTeks {
        lebar,
        tinggi: jumlah as f32 * tinggi_baris,
        baris: jumlah,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_tanam_bisa_dibaca() {
        assert!(tersedia(), "face yang ditanam harus terurai");
        assert_eq!(units_per_em(), Some(2048), "identitas DejaVu Sans");
    }

    #[test]
    fn teks_kosong_tanpa_lebar() {
        assert_eq!(lebar("", 16.0), 0.0);
        assert!(bungkus("", 16.0, 100.0).is_empty());
        let ukuran = ukuran("", 16.0, 18.0, Some(100.0));
        assert_eq!(
            ukuran,
            UkuranTeks {
                lebar: 0.0,
                tinggi: 0.0,
                baris: 0
            }
        );
    }

    #[test]
    fn lebar_bertambah_dengan_panjang_teks() {
        let satu = lebar("a", 16.0);
        let dua = lebar("aa", 16.0);
        assert!(satu > 0.0, "satu huruf punya lebar");
        assert!(dua > satu, "dua huruf lebih lebar dari satu");
    }

    #[test]
    fn lebar_skala_lurus_dengan_ukuran_font() {
        let kecil = lebar("Hello", 16.0);
        let besar = lebar("Hello", 32.0);
        assert!(
            (besar - kecil * 2.0).abs() < 0.01,
            "32px harus dua kali 16px, dapat {kecil} dan {besar}"
        );
    }

    #[test]
    fn spasi_punya_lebar_positif() {
        // Spasi tunggal dipangkas oleh `rapikan` (benar: blok teks tidak
        // dimulai dengan spasi), jadi lebarnya diuji di dalam teks.
        let dengan = lebar("a b", 16.0);
        let tanpa = lebar("ab", 16.0);
        assert!(dengan > tanpa, "spasi di tengah harus menambah lebar");
    }

    #[test]
    fn spasi_diruntuhkan_dan_dipangkas() {
        assert_eq!(rapikan("  a \n\t b  "), "a b");
        assert_eq!(rapikan("a   b"), "a b");
        assert_eq!(lebar("a   b", 16.0), lebar("a b", 16.0));
    }

    #[test]
    fn teks_dibungkus_pada_batas_kata() {
        let satu_kata = lebar("aaaa", 16.0);
        // Dua kata + spasi tidak muat dalam satu kata, jadi harus jadi dua baris.
        let baris = bungkus("aaaa bbbb cccc", 16.0, satu_kata + 1.0);
        assert_eq!(
            baris.len(),
            3,
            "tiap kata turun ke baris sendiri: {baris:?}"
        );
        assert_eq!(baris, vec!["aaaa", "bbbb", "cccc"]);
    }

    #[test]
    fn dua_kata_muat_dalam_satu_baris() {
        let muat = lebar("aaaa bbbb", 16.0);
        let baris = bungkus("aaaa bbbb cccc", 16.0, muat + 0.5);
        assert_eq!(baris.len(), 2, "dua kata pertama muat: {baris:?}");
        assert_eq!(baris[0], "aaaa bbbb");
    }

    #[test]
    fn kata_lebih_lebar_dari_kotak_tidak_dipotong() {
        let baris = bungkus("sangatpanjangsekali", 16.0, 10.0);
        assert_eq!(baris, vec!["sangatpanjangsekali"]);
    }

    #[test]
    fn ukuran_memakai_tinggi_baris_yang_diberikan() {
        let baris = bungkus("aaaa bbbb cccc", 16.0, lebar("aaaa", 16.0) + 1.0);
        let ukuran = ukuran(
            "aaaa bbbb cccc",
            16.0,
            18.0,
            Some(lebar("aaaa", 16.0) + 1.0),
        );
        assert_eq!(ukuran.baris, baris.len());
        assert!((ukuran.tinggi - baris.len() as f32 * 18.0).abs() < 0.01);
        assert!(ukuran.lebar > 0.0);
    }

    #[test]
    fn tinggi_baris_normal_dari_metrik_font() {
        let tinggi = tinggi_baris_normal(16.0);
        // DejaVu Sans: (1901 + 483 + 0) / 2048 * 16 = 18.625
        assert!((tinggi - 18.625).abs() < 0.01, "dapat {tinggi}");
    }

    #[test]
    fn karakter_di_luar_face_diukur_selebar_notdef() {
        // Dua aksara CJK, dua-duanya di luar cakupan DejaVu Sans: harus
        // jatuh ke `.notdef` yang sama, bukan 0 dan bukan lebar karangan.
        // (U+FFFD tidak dipakai sebagai pembanding: face ini memang punya
        // glyph untuknya.)
        let ni = lebar("\u{4f60}", 16.0);
        let hao = lebar("\u{597d}", 16.0);
        assert!(ni > 0.0, "harus punya lebar .notdef, bukan 0");
        assert!((ni - hao).abs() < 0.01, "dua aksara asing selebar .notdef");
    }
}
