//! Pengukuran dan pembentukan (shaping) teks dari face TrueType yang ditanam
//! di dalam biner.
//!
//! Kenapa ditanam, bukan dibaca dari sistem: angka layout dan PNG harus sama
//! di laptop, di runner CI, dan di container. Font sistem berbeda di tiap
//! host, jadi lebar teks yang diukur dari sana membuat kotak teks berubah
//! tanpa satu baris kode berubah. Berkas dan lisensinya:
//! `crates/engine/assets/README.md`.
//!
//! Yang dibaca dari satu face: `units_per_em`, `ascender`/`descender`/
//! `line_gap`, lebar glyph (`hmtx`), pemetaan code point ke glyph (`cmap`),
//! dan tabel layout (`GSUB`/`GPOS`) lewat `rustybuzz`.
//!
//! Dua hal yang membedakan modul ini dari sekadar penjumlahan lebar karakter:
//!
//! - **Rantai face.** Satu face tidak punya semua aksara. Setiap code point
//!   dicari di face pertama yang benar-benar memetakannya, dan teks dipecah
//!   jadi rentetan se-face sebelum dibentuk. Tanpa rantai ini, aksara di luar
//!   DejaVu Sans jatuh ke `.notdef` dan lebarnya jadi lebar kotak kosong.
//! - **Shaping.** Aksara Arab bersambung dan aksara Indic menukar urutan
//!   glyph: matra `i` digambar **sebelum** konsonan yang dibubuhinya. Satu
//!   glyph per code point tidak bisa menyatakan itu, jadi rentetan dibentuk
//!   dengan HarfBuzz (port Rust) dan hasilnya yang dipakai, bukan tebakan
//!   per karakter.
//!
//! Batas yang masih ada, dan disebut di [`super::CELAH`] alih-alih ditutupi:
//! tidak ada `@font-face` (jadi `font-family` diabaikan), tidak ada pemilihan
//! face per bobot (satu face per aksara), dan `white-space: pre` belum
//! dihormati.
//!
//! Perataan spasi mengikuti `white-space: normal`: rentetan spasi, tab, dan
//! baris baru diruntuhkan jadi satu spasi, lalu dipangkas di kedua ujung.
//! Lapisan lukis wajib memakai [`rapikan`] yang sama supaya yang dilukis
//! adalah teks yang diukur, bukan teks mentah.

use std::sync::OnceLock;

/// Face yang ditanam, urut prioritas: face pertama yang memetakan sebuah code
/// point yang dipakai untuk code point itu.
///
/// Urutannya sengaja "yang paling luas dulu": DejaVu Sans memegang Latin,
/// Yunani, Kiril, Arab, dan Ibrani, jadi teks campuran tetap didominasi satu
/// face dan shaping-nya tidak terpecah tanpa perlu.
const TERTANAM: &[(&str, &[u8])] = &[
    ("DejaVu Sans", include_bytes!("../../assets/DejaVuSans.ttf")),
    (
        "Noto Sans Devanagari",
        include_bytes!("../../assets/NotoSansDevanagari-Regular.ttf"),
    ),
];

/// Satu face yang berhasil diurai, beserta byte asalnya.
///
/// Byte-nya disimpan karena `rustybuzz` mengurai ulang tabel layout dari slice
/// yang sama: memakai dua salinan akan membuka kemungkinan metrik dan bentuk
/// glyph berasal dari berkas yang berbeda.
struct Tanam {
    nama: &'static str,
    byte: &'static [u8],
    face: ttf_parser::Face<'static>,
}

/// Seluruh face yang ditanam, diurai sekali.
///
/// Face yang gagal diurai dilewati, bukan menggagalkan seluruh modul: satu
/// berkas rusak tidak boleh membuat teks hilang sama sekali. Face pertama
/// yang gagal tetap terlihat lewat [`tersedia`] yang jadi `false`, dan itu
/// kegagalan build yang ditangkap tes, bukan kondisi runtime.
fn semua() -> &'static [Tanam] {
    static SEMUA: OnceLock<Vec<Tanam>> = OnceLock::new();
    SEMUA.get_or_init(|| {
        TERTANAM
            .iter()
            .filter_map(|(nama, byte)| {
                let face = ttf_parser::Face::parse(byte, 0).ok()?;
                Some(Tanam { nama, byte, face })
            })
            .collect()
    })
}

/// Face utama: yang dipakai untuk metrik baris dan sebagai jalan terakhir
/// ketika tidak ada face yang memetakan sebuah code point.
fn metrik() -> Option<&'static ttf_parser::Face<'static>> {
    semua().first().map(|t| &t.face)
}

/// Apakah metrik font bisa dipakai. Dipakai tes dan laporan layout.
#[must_use]
pub fn tersedia() -> bool {
    metrik().is_some()
}

/// Face utama, untuk pemanggil yang butuh outline tanpa lewat [`bentuk`].
///
/// Yang dikembalikan adalah face yang **sama** dengan yang dipakai mengukur
/// baris, jadi tinggi baris dan glyph yang dilukis tidak bisa berbeda
/// pendapat. `None` berarti berkas yang ditanam rusak, dan itu kesalahan
/// build, bukan input pengguna.
#[must_use]
pub fn face() -> Option<&'static ttf_parser::Face<'static>> {
    metrik()
}

/// Daftar face yang benar-benar terpakai: nama dan `units_per_em`-nya.
///
/// Dipakai tes dan laporan supaya "rantai face" bisa diperiksa dari luar tanpa
/// membuka isi biner. Face kedua yang hilang akan terlihat di sini sebelum
/// terlihat sebagai teks yang salah.
#[must_use]
pub fn rantai() -> Vec<(&'static str, u16)> {
    semua()
        .iter()
        .map(|t| (t.nama, t.face.units_per_em()))
        .collect()
}

/// Batas total byte face runtime yang boleh dipasang satu proses.
///
/// Byte webfont tidak pernah dibebaskan: `ttf_parser::Face` meminjam slice-nya,
/// dan mesin ini tidak memakai struktur self-referential hanya untuk itu. Batas
/// ini yang membuat "tidak dibebaskan" berhenti jadi kebocoran: setelah 24 MB,
/// face baru ditolak dan penolakannya dilaporkan, bukan ditelan.
pub const BATAS_BYTE_FONT: usize = 24 * 1024 * 1024;

/// Face yang dipasang saat runtime dari `@font-face`.
///
/// Dipisah dari [`TERTANAM`] karena isinya datang dari jaringan: nama keluarga
/// ditentukan halaman, byte-nya tidak dikenal saat build, dan jumlahnya harus
/// dibatasi. Yang **tidak** berubah: pencarian tetap per code point, jadi
/// keluarga yang tidak memetakan sebuah aksara tidak menghalangi aksara itu
/// jatuh ke rantai tertanam.
#[derive(Default)]
pub struct KumpulanFont {
    face: Vec<&'static Tanam>,
    byte: usize,
}

impl std::fmt::Debug for KumpulanFont {
    /// Ditulis tangan karena `ttf_parser::Face` tidak `Debug`: yang berguna
    /// saat log justru nama face dan total byte, bukan isi tabel font.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KumpulanFont")
            .field(
                "face",
                &self.face.iter().map(|t| t.nama).collect::<Vec<_>>(),
            )
            .field("byte", &self.byte)
            .finish()
    }
}

impl KumpulanFont {
    /// Kumpulan kosong, untuk pemanggil yang tidak punya webfont.
    #[must_use]
    pub fn baru() -> Self {
        Self::default()
    }

    /// Kumpulan kosong yang berumur `'static`, dipakai [`bentuk`] dan kawan-kawan.
    #[must_use]
    pub fn kosong() -> &'static Self {
        static KOSONG: OnceLock<KumpulanFont> = OnceLock::new();
        KOSONG.get_or_init(KumpulanFont::default)
    }

    /// Apakah ada face runtime yang terpasang.
    #[must_use]
    pub fn kosong_mutlak(&self) -> bool {
        self.face.is_empty()
    }

    /// Jumlah byte face runtime yang sudah dipasang.
    #[must_use]
    pub fn byte(&self) -> usize {
        self.byte
    }

    /// Memasang `byte` sebagai face untuk `keluarga`.
    ///
    /// Mengembalikan `false` kalau byte-nya bukan TrueType/OpenType yang bisa
    /// diurai, atau kalau batas [`BATAS_BYTE_FONT`] sudah lewat. Face yang gagal
    /// diurai tidak dipasang sama sekali: face setengah jadi akan membuat
    /// `font-family` yang menyebutnya menghasilkan teks kosong.
    pub fn pasang(&mut self, keluarga: &str, byte: Vec<u8>) -> bool {
        let nama = keluarga.trim();
        if nama.is_empty() || byte.is_empty() || self.byte + byte.len() > BATAS_BYTE_FONT {
            return false;
        }
        // Byte dan nama dilepas ke `'static` karena `ttf_parser::Face` meminjam
        // slice-nya. Batas di atas yang menjaga ini tetap terbatas.
        let bocor: &'static [u8] = Box::leak(byte.into_boxed_slice());
        let Ok(face) = ttf_parser::Face::parse(bocor, 0) else {
            return false;
        };
        let nama: &'static str = Box::leak(nama.to_lowercase().into_boxed_str());
        self.byte += bocor.len();
        self.face.push(Box::leak(Box::new(Tanam {
            nama,
            byte: bocor,
            face,
        })));
        true
    }

    /// Face runtime pertama untuk `keluarga` yang memetakan `c`.
    ///
    /// Nama dibandingkan tanpa peduli huruf besar-kecil, sama seperti CSS.
    /// Face tertanam ikut dicari lewat nama, supaya `font-family: "Noto Sans
    /// Devanagari"` menunjuk face tertanam itu dan bukan jatuh ke face utama.
    fn cari(&self, keluarga: &str, c: char) -> Option<&'static Tanam> {
        let nama = keluarga.trim().trim_matches('"').trim_matches('\'');
        if nama.is_empty() {
            return None;
        }
        if let Some(t) = self
            .face
            .iter()
            .find(|t| t.nama.eq_ignore_ascii_case(nama) && t.face.glyph_index(c).is_some())
        {
            return Some(*t);
        }
        semua()
            .iter()
            .find(|t| t.nama.eq_ignore_ascii_case(nama) && t.face.glyph_index(c).is_some())
    }
}

/// Face pertama yang memetakan `c`, atau face utama kalau tidak ada.
fn pilih(c: char) -> &'static Tanam {
    semua()
        .iter()
        .find(|t| t.face.glyph_index(c).is_some())
        .unwrap_or_else(|| &semua()[0])
}

/// Skala dari unit font satu face ke piksel.
///
/// Dihitung per face, bukan sekali untuk seluruh rantai: `units_per_em` tiap
/// face berbeda (DejaVu 2048, Noto Sans Devanagari 1000), dan memakai pembagi
/// face pertama untuk semua face membuat aksara dari face kedua 2x terlalu
/// besar.
fn skala_face(face: &ttf_parser::Face<'_>, ukuran_font: f32) -> f32 {
    ukuran_font / f32::from(face.units_per_em())
}

/// Satu glyph hasil pembentukan, sudah dalam piksel.
#[derive(Debug, Clone, Copy)]
pub struct Glyph {
    /// Face asal glyph ini. Lapisan lukis mengambil outline dari sini.
    pub face: &'static ttf_parser::Face<'static>,
    /// Indeks glyph di dalam face-nya.
    pub id: ttf_parser::GlyphId,
    /// Maju pena setelah glyph ini, dalam piksel.
    pub x_advance: f32,
    /// Geseran glyph terhadap pena, dalam piksel. Mark dan matra memakainya.
    pub x_offset: f32,
    /// Geseran vertikal terhadap baseline, dalam piksel.
    pub y_offset: f32,
    /// Offset byte glyph ini di dalam teks masukan.
    ///
    /// Dipakai pemanggil untuk tahu glyph mana yang berasal dari spasi saat
    /// meregangkan baris `text-align: justify`. Urutannya **tidak** menaik
    /// pada aksara yang menukar posisi (matra Indic mendahului konsonannya),
    /// jadi jangan dipakai sebagai indeks urut.
    pub cluster: usize,
}

/// Membentuk `teks` jadi glyph yang siap dilukis, dalam piksel.
///
/// Teks dipecah per face lebih dulu (lihat [`pilih`]), lalu tiap rentetan
/// dibentuk dengan HarfBuzz. Pemecahan memotong kerning dan ligatur yang
/// melintasi batas face; itu konsekuensi wajar dari rantai face dan tidak
/// mengubah lebar yang dilaporkan, karena rentetan yang sama dipakai untuk
/// mengukur dan melukis.
#[must_use]
pub fn bentuk(teks: &str, ukuran_font: f32) -> Vec<Glyph> {
    bentuk_dengan(teks, ukuran_font, &[], KumpulanFont::kosong())
}

/// Sama dengan [`bentuk`], tapi `font-family` yang diminta ikut dipertimbangkan.
///
/// `keluarga` adalah daftar nama dari `font-family`, urut prioritas. Nama yang
/// punya face terpasang dari `@font-face` dipakai lebih dulu; nama yang tidak
/// punya face (termasuk keluarga generik seperti `serif`) dilewati, dan teks
/// yang tidak tercakup jatuh ke rantai face tertanam. Tidak ada tebakan
/// "mirip": keluarga yang tidak terpasang tidak dipetakan ke face lain.
#[must_use]
pub fn bentuk_dengan(
    teks: &str,
    ukuran_font: f32,
    keluarga: &[String],
    kumpulan: &KumpulanFont,
) -> Vec<Glyph> {
    let daftar = semua();
    if daftar.is_empty() || ukuran_font <= 0.0 || teks.is_empty() {
        return Vec::new();
    }
    let mut hasil = Vec::new();
    let mut mulai = 0usize;
    let mut face_kini: Option<&'static Tanam> = None;
    for (i, c) in teks.char_indices() {
        let face = pilih_dengan(c, keluarga, kumpulan);
        match face_kini {
            None => face_kini = Some(face),
            Some(kini) if !std::ptr::eq(kini, face) => {
                bentuk_satu(&teks[mulai..i], mulai, kini, ukuran_font, &mut hasil);
                mulai = i;
                face_kini = Some(face);
            }
            Some(_) => {}
        }
    }
    if let Some(kini) = face_kini {
        bentuk_satu(&teks[mulai..], mulai, kini, ukuran_font, &mut hasil);
    }
    hasil
}

/// Membentuk satu rentetan yang seluruhnya milik `tanam`.
fn bentuk_satu(
    rentetan: &str,
    offset_byte: usize,
    tanam: &'static Tanam,
    ukuran_font: f32,
    keluar: &mut Vec<Glyph>,
) {
    let skala = skala_face(&tanam.face, ukuran_font);
    // `rustybuzz` mengurai ulang tabel dari byte yang sama dengan `ttf-parser`.
    // Kalau itu gagal, glyph tetap bisa diambil per karakter: hasilnya lebih
    // buruk (tanpa sambungan dan tanpa penukaran matra) tapi tidak kosong, dan
    // lebar yang dilaporkan tetap konsisten dengan yang dilukis.
    let Some(rb) = rustybuzz::Face::from_slice(tanam.byte, 0) else {
        for (i, c) in rentetan.char_indices() {
            let id = tanam.face.glyph_index(c).unwrap_or(ttf_parser::GlyphId(0));
            let maju = tanam
                .face
                .glyph_hor_advance(id)
                .map_or(0.0, |a| f32::from(a) * skala);
            keluar.push(Glyph {
                face: &tanam.face,
                id,
                x_advance: maju,
                x_offset: 0.0,
                y_offset: 0.0,
                cluster: offset_byte + i,
            });
        }
        return;
    };
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(rentetan);
    // Skrip dan arah ditebak dari isi rentetan. Tanpa ini aksara Arab dibentuk
    // sebagai huruf terpisah dalam urutan kiri-ke-kanan.
    buffer.guess_segment_properties();
    let hasil = rustybuzz::shape(&rb, &[], buffer);
    for (info, pos) in hasil.glyph_infos().iter().zip(hasil.glyph_positions()) {
        keluar.push(Glyph {
            face: &tanam.face,
            id: ttf_parser::GlyphId(u16::try_from(info.glyph_id).unwrap_or(0)),
            x_advance: pos.x_advance as f32 * skala,
            x_offset: pos.x_offset as f32 * skala,
            y_offset: pos.y_offset as f32 * skala,
            cluster: offset_byte + info.cluster as usize,
        });
    }
}

/// Face untuk satu code point, dengan `font-family` dipertimbangkan lebih dulu.
fn pilih_dengan(c: char, keluarga: &[String], kumpulan: &KumpulanFont) -> &'static Tanam {
    for nama in keluarga {
        if let Some(tanam) = kumpulan.cari(nama, c) {
            return tanam;
        }
    }
    pilih(c)
}

/// Skala dari unit font face utama ke piksel untuk satu `font-size`.
///
/// Dipakai lapisan lukis untuk tinggi baris, yang selalu dihitung dari face
/// utama: satu baris tidak punya satu face, jadi metrik baris tidak bisa
/// diambil dari face mana pun yang kebetulan muncul di dalamnya.
#[must_use]
pub fn skala(ukuran_font: f32) -> f32 {
    metrik().map_or(0.0, |f| skala_face(f, ukuran_font))
}

/// Jarak dari tepi atas baris ke garis dasar (baseline), dalam piksel.
///
/// Dipakai lapisan lukis untuk menempatkan glyph di dalam kotak teks: baseline
/// ada `ascender` di bawah tepi atas kotak, karena kotak setinggi satu baris
/// penuh (`ascender - descender + line_gap`).
#[must_use]
pub fn jarak_baseline(ukuran_font: f32) -> f32 {
    metrik().map_or(0.0, |f| {
        f32::from(f.ascender()) / f32::from(f.units_per_em()) * ukuran_font
    })
}

/// `units_per_em` face utama. Dipakai tes untuk mengunci identitas font.
#[must_use]
pub fn units_per_em() -> Option<u16> {
    metrik().map(ttf_parser::Face::units_per_em)
}

/// Tinggi satu baris menurut metrik font utama, dalam piksel.
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

/// Lebar `teks` apa adanya, tanpa meruntuhkan spasinya.
fn lebar_mentah_dengan(
    teks: &str,
    ukuran_font: f32,
    keluarga: &[String],
    kumpulan: &KumpulanFont,
) -> f32 {
    bentuk_dengan(teks, ukuran_font, keluarga, kumpulan)
        .iter()
        .map(|g| g.x_advance)
        .sum()
}

/// Lebar satu baris teks dalam piksel, tanpa membungkus.
///
/// Teks diruntuhkan spasinya lebih dulu, jadi `"a   b"` selebar `"a b"`.
/// Angkanya berasal dari hasil shaping yang sama dengan yang dilukis, bukan
/// penjumlahan lebar per karakter: pada aksara bersambung keduanya berbeda.
#[must_use]
pub fn lebar(teks: &str, ukuran_font: f32) -> f32 {
    lebar_dengan(teks, ukuran_font, &[], KumpulanFont::kosong())
}

/// Sama dengan [`lebar`], tapi `font-family` ikut menentukan face.
///
/// Dipakai lapisan tata letak: kotak teks yang memakai webfont harus diukur
/// dengan face yang sama dengan yang dilukis, kalau tidak barisnya meluap atau
/// menyisakan celah tanpa satu pun galat.
#[must_use]
pub fn lebar_dengan(
    teks: &str,
    ukuran_font: f32,
    keluarga: &[String],
    kumpulan: &KumpulanFont,
) -> f32 {
    lebar_mentah_dengan(&rapikan(teks), ukuran_font, keluarga, kumpulan)
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
    bungkus_dengan(teks, ukuran_font, lebar_maks, &[], KumpulanFont::kosong())
}

/// Sama dengan [`bungkus`], tapi pembungkusan memakai face `font-family`.
#[must_use]
pub fn bungkus_dengan(
    teks: &str,
    ukuran_font: f32,
    lebar_maks: f32,
    keluarga: &[String],
    kumpulan: &KumpulanFont,
) -> Vec<String> {
    let rapi = rapikan(teks);
    if rapi.is_empty() {
        return Vec::new();
    }
    if metrik().is_none() || lebar_maks <= 0.0 {
        return vec![rapi];
    }
    let lebar_spasi = lebar_mentah_dengan(" ", ukuran_font, keluarga, kumpulan);

    let mut baris: Vec<String> = Vec::new();
    let mut sekarang = String::new();
    let mut lebar_sekarang = 0.0_f32;

    for kata in rapi.split(' ') {
        let lebar_kata = lebar_mentah_dengan(kata, ukuran_font, keluarga, kumpulan);
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
    ukuran_dengan(
        teks,
        ukuran_font,
        tinggi_baris,
        lebar_maks,
        &[],
        KumpulanFont::kosong(),
    )
}

/// Sama dengan [`ukuran`], tapi pengukuran memakai face `font-family`.
#[must_use]
pub fn ukuran_dengan(
    teks: &str,
    ukuran_font: f32,
    tinggi_baris: f32,
    lebar_maks: Option<f32>,
    keluarga: &[String],
    kumpulan: &KumpulanFont,
) -> UkuranTeks {
    let baris = match lebar_maks {
        Some(maks) => bungkus_dengan(teks, ukuran_font, maks, keluarga, kumpulan),
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
        .map(|b| lebar_dengan(b, ukuran_font, keluarga, kumpulan))
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
    fn rantai_face_berisi_dua_berkas_dengan_upem_berbeda() {
        // Dua face dengan `units_per_em` berbeda itu sengaja: kalau skalanya
        // dihitung sekali dari face pertama, aksara dari face kedua akan
        // tergambar 2x terlalu besar dan tes lebar di bawah menangkapnya.
        assert_eq!(
            rantai(),
            vec![("DejaVu Sans", 2048), ("Noto Sans Devanagari", 1000)]
        );
    }

    #[test]
    fn teks_kosong_tanpa_lebar() {
        assert_eq!(lebar("", 16.0), 0.0);
        assert!(bungkus("", 16.0, 100.0).is_empty());
        assert!(bentuk("", 16.0).is_empty());
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
    fn aksara_latin_memakai_face_pertama() {
        let glyph = bentuk("a", 16.0);
        assert_eq!(glyph.len(), 1);
        assert_eq!(
            glyph[0].face.units_per_em(),
            2048,
            "Latin harus tetap di DejaVu Sans, bukan jatuh ke face kedua"
        );
        assert_ne!(glyph[0].id, ttf_parser::GlyphId(0));
    }

    #[test]
    fn aksara_devanagari_memakai_face_kedua() {
        // DejaVu Sans tidak punya aksara Devanagari. Kalau rantai face tidak
        // dipakai, glyph ini jatuh ke `.notdef` face pertama dan lebarnya
        // 1229/2048*16 = 9.602 px. Dengan face kedua, `क` maju 762/1000*16 =
        // 12.192 px: angka itu yang membuktikan face-nya benar-benar berganti,
        // bukan cuma "tidak nol".
        let ka = bentuk("\u{0915}", 16.0);
        assert_eq!(ka.len(), 1);
        assert_eq!(ka[0].face.units_per_em(), 1000, "Noto Sans Devanagari");
        assert_ne!(ka[0].id, ttf_parser::GlyphId(0), "bukan .notdef");
        assert!(
            (ka[0].x_advance - 12.192).abs() < 0.01,
            "lebar `क` dari face kedua, dapat {}",
            ka[0].x_advance
        );

        let notdef = bentuk("\u{4f60}", 16.0);
        assert_eq!(notdef.len(), 1);
        assert_eq!(
            notdef[0].id,
            ttf_parser::GlyphId(0),
            "CJK di luar semua face harus .notdef, bukan glyph karangan"
        );
        assert!(
            (notdef[0].x_advance - 9.602).abs() < 0.01,
            "lebar .notdef face pertama, dapat {}",
            notdef[0].x_advance
        );
        assert!(
            (ka[0].x_advance - notdef[0].x_advance).abs() > 0.01,
            "aksara dari face kedua tidak boleh selebar .notdef"
        );
    }

    #[test]
    fn shaping_menukar_urutan_matra() {
        // `कि` = konsonan `क` (U+0915) lalu matra `i` (U+093F) di teks
        // masukan. HarfBuzz mengeluarkan matra **sebelum** konsonannya, jadi
        // glyph pertama adalah matra yang sempit dan glyph kedua konsonan yang
        // lebar. Tanpa shaping urutannya persis urutan masukan, dan teksnya
        // tergambar dengan matra di sisi yang salah.
        //
        // Bukti yang dipakai bukan `cluster`: HarfBuzz menyatukan cluster
        // matra ke konsonannya (keduanya 0) karena keduanya satu gugus, jadi
        // `cluster` tidak bisa membedakan dua urutan ini. Yang membedakan
        // adalah maju glyph, dan id matra yang sudah digantikan GSUB.
        let glyph = bentuk("\u{0915}\u{093F}", 16.0);
        assert_eq!(glyph.len(), 2, "satu konsonan dan satu matra: {glyph:?}");
        assert!(
            (glyph[0].x_advance - 4.144).abs() < 0.01,
            "glyph pertama harus matra sempit, dapat {}",
            glyph[0].x_advance
        );
        assert!(
            (glyph[1].x_advance - 12.192).abs() < 0.01,
            "glyph kedua harus konsonan lebar, dapat {}",
            glyph[1].x_advance
        );
        let face = glyph[0].face;
        let konsonan = face
            .glyph_index('\u{0915}')
            .expect("`क` di face Devanagari");
        let matra = face
            .glyph_index('\u{093F}')
            .expect("matra `i` di face Devanagari");
        assert_eq!(glyph[1].id, konsonan, "konsonan tetap glyph aslinya");
        assert_ne!(
            glyph[0].id, matra,
            "matra harus bentuk hasil GSUB, bukan glyph cmap mentah"
        );
    }

    #[test]
    fn lebar_memakai_hasil_shaping() {
        // Lebar satu baris harus sama dengan jumlah maju glyph yang dilukis,
        // bukan penjumlahan lebar per karakter: pada aksara bersambung kedua
        // angka itu berbeda, dan yang benar adalah yang dilukis.
        for teks in [
            "Hello",
            "\u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064a}\u{0629}",
            "\u{0915}\u{093F}",
        ] {
            let jumlah: f32 = bentuk(teks, 16.0).iter().map(|g| g.x_advance).sum();
            assert!(
                (lebar(teks, 16.0) - jumlah).abs() < 0.001,
                "lebar `{teks}` harus sama dengan jumlah maju glyph"
            );
        }
    }
    /// Byte face Devanagari yang ditanam, dipakai sebagai fixture webfont.
    ///
    /// Fixture-nya sengaja berkas yang sudah ada di repo: tes ini menguji
    /// JALUR pemasangan face runtime (byte datang dari luar, dipasang dengan
    /// nama keluarga yang ditentukan halaman), bukan isi berkas fontnya.
    fn byte_webfont() -> Vec<u8> {
        TERTANAM
            .iter()
            .find(|(nama, _)| nama.contains("Devanagari"))
            .map(|(_, byte)| byte.to_vec())
            .expect("fixture Noto Sans Devanagari tertanam")
    }

    #[test]
    fn face_unduhan_dipakai_saat_keluarga_disebut() {
        // `font-family: "WebDeva"` harus menang atas rantai tertanam, walau
        // aksaranya bisa dilayani face kedua yang tertanam. Tanpa ini,
        // `@font-face` cuma dekorasi: halaman mengunduh fontnya, menyebutnya
        // di CSS, dan yang tergambar tetap face bawaan mesin.
        let mut kumpulan = KumpulanFont::baru();
        assert!(kumpulan.pasang("WebDeva", byte_webfont()), "byte dipasang");
        assert!(kumpulan.byte() > 0, "byte terhitung di anggaran");

        let tertanam = semua()
            .iter()
            .find(|t| t.nama.contains("Devanagari"))
            .expect("face Devanagari tertanam");
        let glyph = bentuk_dengan("\u{0915}", 16.0, &["WebDeva".to_string()], &kumpulan);
        assert_eq!(glyph.len(), 1, "satu konsonan: {glyph:?}");
        assert!(
            !std::ptr::eq(glyph[0].face, &tertanam.face),
            "glyph harus dari face unduhan, bukan face tertanam"
        );
        // Berkasnya sama, jadi majunya harus sama persis. Angka ini yang
        // membuktikan face yang menang memang font yang sama, bukan face lain
        // yang kebetulan punya glyph untuk aksara itu.
        assert!(
            (glyph[0].x_advance - 12.192).abs() < 0.01,
            "maju `\u{0915}` dari face unduhan, dapat {}",
            glyph[0].x_advance
        );
    }

    #[test]
    fn face_unduhan_diabaikan_saat_keluarga_lain() {
        // Keluarga yang tidak dipasang tidak boleh meminjam face unduhan:
        // `font-family: "WebLain"` yang tidak punya `@font-face` berarti
        // mesin jatuh ke rantai tertanam, sama seperti browser yang tidak
        // menemukan keluarga itu.
        let mut kumpulan = KumpulanFont::baru();
        assert!(kumpulan.pasang("WebDeva", byte_webfont()));

        let tertanam = semua()
            .iter()
            .find(|t| t.nama.contains("Devanagari"))
            .expect("face Devanagari tertanam");
        let glyph = bentuk_dengan("\u{0915}", 16.0, &["WebLain".to_string()], &kumpulan);
        assert_eq!(glyph.len(), 1, "satu konsonan: {glyph:?}");
        assert!(
            std::ptr::eq(glyph[0].face, &tertanam.face),
            "keluarga yang tidak dipasang harus jatuh ke face tertanam"
        );
    }

    #[test]
    fn pasang_menolak_byte_yang_bukan_font() {
        // Server yang menyajikan halaman galat di URL `.woff2` mengirim byte
        // yang bukan font. Menerimanya berarti `font-family` yang menyebut
        // face itu menghasilkan teks kosong tanpa satu pun galat.
        let mut kumpulan = KumpulanFont::baru();
        assert!(!kumpulan.pasang("Rusak", b"<html>404</html>".to_vec()));
        assert!(
            kumpulan.kosong_mutlak(),
            "face rusak tidak boleh terpasang separuh"
        );
        assert_eq!(kumpulan.byte(), 0, "byte yang ditolak tidak masuk anggaran");
    }
}
