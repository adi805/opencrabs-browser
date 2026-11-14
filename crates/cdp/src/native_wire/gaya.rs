//! Pengambilan stylesheet eksternal untuk satu dokumen.
//!
//! `jaringan` tahu cara mengambil satu badan HTTP; modul ini yang tahu **apa**
//! yang harus diambil setelah dokumennya sampai: mengumpulkan
//! `<link rel="stylesheet">`, menyelesaikan `href` relatif terhadap URL akhir
//! dokumen dan `<base href>`, lalu mengambil semuanya dengan batas yang
//! eksplisit.
//!
//! Yang dijaga di sini, dan diuji satu per satu:
//!
//! - `href` yang disimpan di [`LembarLuar`] adalah teks atribut **apa adanya**,
//!   bukan URL absolut: kaskade mencocokkannya dengan node `<link>` di pohon,
//!   dan URL absolut tidak akan pernah cocok
//! - resolusi memakai URL **setelah pengalihan**, karena halaman yang
//!   dialihkan ke `/artikel/` punya `href="gaya.css"` yang berarti
//!   `/artikel/gaya.css`, bukan `/gaya.css`
//! - jumlah lembar, ukuran per lembar, dan total byte dibatasi; lembar yang
//!   melewati batas dilewati **dengan alasan yang tercatat**, bukan dipotong,
//!   karena CSS yang terpotong di tengah aturan menghasilkan kaskade yang
//!   salah tanpa satu pun galat

use engine::native::Document;
use engine::native::css::{FontFace, LembarLuar, tautan_eksternal};
use futures_util::future::join_all;
use url::Url;

use super::jaringan::{
    self, BATAS_BYTE_CSS, BATAS_BYTE_FONT, BATAS_TOTAL_CSS, BATAS_TOTAL_FONT, BATAS_WAKTU_CSS,
    BATAS_WAKTU_FONT, Batas, MAKS_FONT, MAKS_LEMBAR,
};

/// Hasil pengambilan lembar gaya untuk satu dokumen.
#[derive(Debug, Default)]
pub struct Hasil {
    /// Lembar yang berhasil diambil, urut kemunculan di dokumen.
    pub lembar: Vec<LembarLuar>,
    /// Tautan yang dilewati atau gagal, beserta alasannya. Halaman yang tampak
    /// tanpa gaya bisa dijelaskan dari sini tanpa menebak.
    pub catatan: Vec<String>,
}

/// Mengambil semua stylesheet eksternal milik `html`.
///
/// Tidak pernah mengembalikan galat: satu stylesheet yang gagal diambil tidak
/// boleh menggagalkan navigasi. Yang gagal masuk `catatan`.
pub async fn ambil(url_dokumen: &str, html: &str) -> Hasil {
    let dokumen = match Document::parse_html(html) {
        Ok(dok) => dok,
        Err(err) => {
            return Hasil {
                lembar: Vec::new(),
                catatan: vec![format!("pohon dokumen gagal diurai: {err}")],
            };
        }
    };
    ambil_dari(url_dokumen, &dokumen).await
}

/// Sama dengan [`ambil`], tapi pohon dokumennya sudah ada.
pub async fn ambil_dari(url_dokumen: &str, dokumen: &Document) -> Hasil {
    let (tautan, catatan_tautan) = tautan_eksternal(dokumen);
    if tautan.is_empty() {
        return Hasil {
            lembar: Vec::new(),
            catatan: catatan_tautan,
        };
    }
    let mut ktx = Konteks {
        catatan: catatan_tautan,
        dilihat: std::collections::HashSet::new(),
        total: 0,
    };

    let dasar = dasar_url(url_dokumen, dokumen);
    let diambil = tautan.iter().take(MAKS_LEMBAR);
    let jumlah_dilewati = tautan.len().saturating_sub(MAKS_LEMBAR);
    if jumlah_dilewati > 0 {
        ktx.catatan.push(format!(
            "{jumlah_dilewati} tautan stylesheet tidak diambil: batas {MAKS_LEMBAR} lembar per dokumen"
        ));
    }

    // Dasar yang dipakai untuk tiap tautan, sejajar dengan `diambil`.
    let mut sasaran = Vec::new();
    for t in diambil {
        match selesaikan(&dasar, &t.href) {
            Some(url) => sasaran.push((t.href.clone(), url)),
            None => ktx.catatan.push(format!(
                "`{}` dilewati: bukan URL http/https setelah diselesaikan terhadap `{dasar}`",
                t.href
            )),
        }
    }

    let batas = Batas {
        byte: BATAS_BYTE_CSS,
        waktu: BATAS_WAKTU_CSS,
        ..Batas::default()
    };
    // Diambil bersamaan: enam belas lembar yang diambil berurutan bisa
    // menghabiskan 80 detik pada batas waktu per lembar, dan navigasi tidak
    // boleh selama itu. `join_all` menjaga urutan hasil, jadi kaskade tetap
    // mengikuti urutan dokumen.
    let hasil = join_all(
        sasaran
            .iter()
            .map(|(_, url)| jaringan::muat_css(url, batas)),
    )
    .await;

    let mut lembar = Vec::new();
    for ((href, url), hasil) in sasaran.iter().zip(hasil) {
        match hasil {
            Ok(gaya) => {
                if ktx.total + gaya.byte > BATAS_TOTAL_CSS {
                    ktx.catatan.push(format!(
                        "`{href}` dilewati: total CSS melewati batas {BATAS_TOTAL_CSS} byte"
                    ));
                    continue;
                }
                ktx.total += gaya.byte;
                ktx.dilihat.insert(url.clone());
                // `@import` diikuti di sini, dan isinya ditaruh di DEPAN lembar
                // induk: aturan impor harus mendahului aturan lain di lembar
                // yang mengimpornya, jadi lembar yang diimpor kalah oleh
                // aturan induknya, bukan menang.
                let teks = kembangkan(url, gaya.teks, 0, &mut ktx, batas).await;
                lembar.push(LembarLuar::baru(href.clone(), teks));
            }
            Err(galat) => ktx
                .catatan
                .push(format!("`{url}` gagal diambil: {}", galat.pesan)),
        }
    }

    Hasil {
        lembar,
        catatan: ktx.catatan,
    }
}

/// Hasil pengunduhan webfont untuk satu dokumen.
#[derive(Debug, Default)]
pub struct HasilFont {
    /// Pasangan `(nama keluarga, byte berkas)`, urut kemunculan `@font-face`.
    pub face: Vec<(String, Vec<u8>)>,
    /// Face yang dilewati atau gagal diunduh, beserta alasannya.
    pub catatan: Vec<String>,
}

/// Mengunduh byte setiap `@font-face` yang dideklarasikan `faces`.
///
/// Hanya `url()` **pertama** yang dipakai per face: daftar `src` adalah
/// preferensi format, dan byte tetap diperiksa `ttf_parser` saat dipasang.
/// Kalau `url()` pertama gagal, face itu dilewati, bukan dicoba ke `url()`
/// berikutnya: fallback format adalah pilihan penulis halaman, bukan izin
/// menebak berkas mana pun yang bisa diambil.
///
/// Tidak pernah mengembalikan galat. Satu webfont yang gagal tidak boleh
/// menggagalkan navigasi; yang gagal masuk `catatan`.
pub async fn ambil_font(dasar: &str, faces: &[FontFace]) -> HasilFont {
    let mut hasil = HasilFont::default();
    let mut sasaran = Vec::new();
    for face in faces {
        if sasaran.len() >= MAKS_FONT {
            hasil.catatan.push(format!(
                "`@font-face` `{}` tidak diunduh: batas {MAKS_FONT} face per dokumen",
                face.keluarga
            ));
            continue;
        }
        let Some(sumber) = face.sumber.first() else {
            continue;
        };
        match selesaikan(dasar, sumber) {
            Some(url) => sasaran.push((face, url)),
            None => hasil.catatan.push(format!(
                "`@font-face` `{}` dilewati: `{sumber}` bukan URL http/https setelah \
                 diselesaikan terhadap `{dasar}`",
                face.keluarga
            )),
        }
    }
    if sasaran.is_empty() {
        return hasil;
    }

    let batas = Batas {
        byte: BATAS_BYTE_FONT,
        waktu: BATAS_WAKTU_FONT,
        ..Batas::default()
    };
    // Diambil bersamaan dengan alasan yang sama seperti stylesheet: delapan
    // font berurutan pada batas waktu masing-masing bisa menahan navigasi
    // lebih dari satu menit.
    let jawaban = join_all(
        sasaran
            .iter()
            .map(|(_, url)| jaringan::muat_font(url, batas)),
    )
    .await;

    let mut total = 0usize;
    for ((face, url), jawaban) in sasaran.iter().zip(jawaban) {
        match jawaban {
            Ok(font) => {
                if total + font.byte.len() > BATAS_TOTAL_FONT {
                    hasil.catatan.push(format!(
                        "`@font-face` `{}` dilewati: total font melewati batas \
                         {BATAS_TOTAL_FONT} byte",
                        face.keluarga
                    ));
                    continue;
                }
                total += font.byte.len();
                hasil.face.push((face.keluarga.clone(), font.byte));
            }
            Err(galat) => hasil.catatan.push(format!(
                "`@font-face` `{}` gagal diambil dari `{url}`: {}",
                face.keluarga, galat.pesan
            )),
        }
    }
    hasil
}

/// URL dasar untuk menyelesaikan `href` relatif./// URL dasar untuk menyelesaikan `href` relatif.
///
/// `<base href>` menang atas URL dokumen, sama seperti browser. Kalau
/// `<base href>` sendiri relatif, dia diselesaikan terhadap URL dokumen dulu.
#[must_use]
pub fn dasar_url(url_dokumen: &str, dokumen: &Document) -> String {
    let Some(base) = base_href(dokumen) else {
        return url_dokumen.to_owned();
    };
    selesaikan(url_dokumen, &base).unwrap_or_else(|| url_dokumen.to_owned())
}

/// `href` dari `<base>` pertama di dokumen, apa adanya.
#[must_use]
pub fn base_href(dokumen: &Document) -> Option<String> {
    dokumen.nodes().iter().find_map(|node| {
        (node.tag() == Some("base"))
            .then(|| node.attr("href"))
            .flatten()
            .map(|href| href.trim().to_owned())
            .filter(|href| !href.is_empty())
    })
}

/// Menyelesaikan `href` terhadap `dasar`, dan hanya menerima hasil
/// `http`/`https`.
///
/// Skema lain (`javascript:`, `data:`, `file:`) mengembalikan `None` dengan
/// sengaja: mesin ini tidak punya jalan mengambilnya, dan mengembalikan URL
/// yang tidak bisa diambil hanya memindahkan kegagalan ke tempat yang lebih
/// jauh dari sebabnya.
#[must_use]
pub fn selesaikan(dasar: &str, href: &str) -> Option<String> {
    let dasar = Url::parse(dasar).ok()?;
    let hasil = dasar.join(href.trim()).ok()?;
    matches!(hasil.scheme(), "http" | "https").then(|| hasil.to_string())
}

/// Satu `@import` yang ditemukan di dalam stylesheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Impor {
    /// `href` apa adanya dari `@import`, sudah dilepas tanda kutipnya.
    pub href: String,
    /// Teks kondisi media sesudah `href`, kalau ada.
    pub media: Option<String>,
}

/// Kedalaman `@import` terbanyak yang diikuti. Lebih dari ini dicatat, bukan
/// diambil: stylesheet yang mengimpor dirinya sendiri lewat rantai panjang akan
/// menghabiskan seluruh anggaran byte tanpa menambah satu aturan pun yang
/// berlaku.
pub const KEDALAMAN_IMPOR: usize = 3;

/// Keadaan lintas lembar selama satu dokumen dikerjakan.
struct Konteks {
    catatan: Vec<String>,
    /// URL absolut yang sudah dilihat. Inilah pemutus siklus A -> B -> A.
    dilihat: std::collections::HashSet<String>,
    total: usize,
}

/// Memecah `@import` dari satu lembar gaya.
///
/// Mengembalikan daftar impor **dan** teks lembar tanpa pernyataan
/// `@import`-nya. Pernyataan itu dibuang, bukan dibiarkan: pengurai di `engine`
/// mencatat setiap at-rule sebagai "tidak dijalankan", dan setelah isinya
/// benar-benar disisipkan, catatan itu jadi bohong.
#[must_use]
pub fn impor_dari(teks: &str) -> (Vec<Impor>, String) {
    let bytes = teks.as_bytes();
    let mut impor = Vec::new();
    let mut sisa = String::with_capacity(teks.len());
    let mut i = 0;
    let mut disalin = 0;
    while i + 7 <= bytes.len() {
        let kandidat = teks.get(i..i + 7).unwrap_or("");
        let batas_kiri = i == 0
            || matches!(
                bytes[i - 1],
                b' ' | b'\t' | b'\n' | b'\r' | b';' | b'}' | b'{'
            );
        if !kandidat.eq_ignore_ascii_case("@import") || !batas_kiri {
            i += 1;
            continue;
        }
        // Akhir pernyataan: `;` di luar tanda kutip dan di luar `url(...)`.
        let Some(akhir) = akhir_pernyataan(bytes, i) else {
            i += 7;
            continue;
        };
        let isi = &teks[i + 7..akhir];
        // `@import` yang bentuknya tidak dikenali tetap dibuang dari sisa:
        // membiarkannya berarti mesin mencoba menjalankannya sebagai aturan
        // biasa dan mencatat alasan yang salah.
        if let Some(satu) = urai_impor(isi) {
            sisa.push_str(&teks[disalin..i]);
            impor.push(satu);
            disalin = akhir + 1;
        }
        i = akhir + 1;
    }
    sisa.push_str(&teks[disalin..]);
    (impor, sisa)
}

/// Indeks `;` yang menutup pernyataan `@import` mulai dari `mulai`.
fn akhir_pernyataan(bytes: &[u8], mulai: usize) -> Option<usize> {
    let mut kutip: Option<u8> = None;
    let mut dalam_kurung = 0usize;
    let mut i = mulai;
    while i < bytes.len() {
        let b = bytes[i];
        match kutip {
            Some(q) => {
                if b == q {
                    kutip = None;
                }
            }
            None => match b {
                b'"' | b'\'' => kutip = Some(b),
                b'(' => dalam_kurung += 1,
                b')' => dalam_kurung = dalam_kurung.saturating_sub(1),
                b';' if dalam_kurung == 0 => return Some(i),
                b'{' => return None,
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// `url("x") screen` / `"x"` / `url(x)` -> href + media.
fn urai_impor(isi: &str) -> Option<Impor> {
    let isi = isi.trim();
    let (href, sisa) = if let Some(rest) = isi.strip_prefix("url(") {
        let tutup = rest.find(')')?;
        let dalam = rest[..tutup].trim().trim_matches(['"', '\'']);
        (dalam.to_owned(), &rest[tutup + 1..])
    } else {
        let q = isi.chars().next()?;
        if q != '"' && q != '\'' {
            return None;
        }
        let rest = &isi[1..];
        let tutup = rest.find(q)?;
        (rest[..tutup].to_owned(), &rest[tutup + 1..])
    };
    if href.is_empty() {
        return None;
    }
    let media = {
        let m = sisa.trim().trim_end_matches(';').trim();
        (!m.is_empty()).then(|| m.to_owned())
    };
    Some(Impor { href, media })
}

/// Apakah kondisi media sebuah `@import` bisa dijalankan mesin ini.
///
/// Aturannya sama dengan `<link media=...>`: kosong, `all`, atau `screen`.
/// Kondisi lain dicatat dan dilewati, bukan dijalankan dengan tebakan.
#[must_use]
pub fn media_dijalankan(media: Option<&str>) -> bool {
    media.is_none_or(|m| {
        m.split(',')
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .all(|m| m.eq_ignore_ascii_case("all") || m.eq_ignore_ascii_case("screen"))
    })
}

/// Mengikuti `@import` satu lembar secara rekursif dan mengembalikan teks
/// lembar itu dengan isi impor-nya **di depan**.
///
/// Urutan itu yang membuat kaskade benar: aturan `@import` harus mendahului
/// aturan lain di lembar yang mengimpornya, jadi lembar yang diimpor kalah oleh
/// aturan induknya, bukan menang.
fn kembangkan<'a>(
    url: &'a str,
    teks: String,
    kedalaman: usize,
    ktx: &'a mut Konteks,
    batas: jaringan::Batas,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + 'a>> {
    Box::pin(async move {
        let (impor, sisa) = impor_dari(&teks);
        if impor.is_empty() {
            return sisa;
        }
        let mut depan = String::new();
        for satu in impor {
            if !media_dijalankan(satu.media.as_deref()) {
                ktx.catatan.push(format!(
                    "`@import \"{}\" {}` di `{url}` dilewati: kondisi media tidak dievaluasi mesin ini",
                    satu.href,
                    satu.media.unwrap_or_default()
                ));
                continue;
            }
            let Some(absolut) = selesaikan(url, &satu.href) else {
                ktx.catatan.push(format!(
                    "`@import \"{}\"` di `{url}` dilewati: bukan URL http/https",
                    satu.href
                ));
                continue;
            };
            if !ktx.dilihat.insert(absolut.clone()) {
                ktx.catatan.push(format!(
                    "`@import \"{}\"` di `{url}` dilewati: siklus, `{absolut}` sudah diambil",
                    satu.href
                ));
                continue;
            }
            if kedalaman + 1 > KEDALAMAN_IMPOR {
                ktx.catatan.push(format!(
                    "`@import \"{}\"` di `{url}` dilewati: kedalaman melebihi {KEDALAMAN_IMPOR}",
                    satu.href
                ));
                continue;
            }
            let gaya = match jaringan::muat_css(&absolut, batas).await {
                Ok(g) => g,
                Err(galat) => {
                    ktx.catatan.push(format!(
                        "`@import \"{}\"` di `{url}` gagal diambil: {}",
                        satu.href, galat.pesan
                    ));
                    continue;
                }
            };
            if ktx.total + gaya.byte > BATAS_TOTAL_CSS {
                ktx.catatan.push(format!(
                    "`@import \"{}\"` di `{url}` dilewati: total CSS melewati batas {BATAS_TOTAL_CSS} byte",
                    satu.href
                ));
                continue;
            }
            ktx.total += gaya.byte;
            let dalam = kembangkan(&absolut, gaya.teks, kedalaman + 1, ktx, batas).await;
            depan.push_str(&dalam);
            if !dalam.ends_with('\n') {
                depan.push('\n');
            }
        }
        depan.push_str(&sisa);
        depan
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn href_relatif_diselesaikan_terhadap_url_dokumen() {
        assert_eq!(
            selesaikan("https://a.test/artikel/satu", "gaya.css").as_deref(),
            Some("https://a.test/artikel/gaya.css"),
            "relatif mengikuti direktori dokumen, bukan akar host"
        );
        assert_eq!(
            selesaikan("https://a.test/artikel/satu", "/gaya.css").as_deref(),
            Some("https://a.test/gaya.css")
        );
        assert_eq!(
            selesaikan("https://a.test/artikel/satu", "//cdn.test/gaya.css").as_deref(),
            Some("https://cdn.test/gaya.css")
        );
    }

    #[test]
    fn href_absolut_dibiarkan() {
        assert_eq!(
            selesaikan("https://a.test/", "https://cdn.test/x.css").as_deref(),
            Some("https://cdn.test/x.css")
        );
    }

    #[test]
    fn skema_yang_tidak_bisa_diambil_ditolak() {
        assert_eq!(selesaikan("https://a.test/", "javascript:alert(1)"), None);
        assert_eq!(selesaikan("https://a.test/", "data:text/css,body{}"), None);
        assert_eq!(selesaikan("https://a.test/", "file:///etc/passwd"), None);
    }

    #[test]
    fn base_href_menggeser_dasar() {
        let dok = Document::parse_html(
            r#"<!doctype html><html><head><base href="https://cdn.test/aset/"></head><body></body></html>"#,
        )
        .expect("urai");
        assert_eq!(
            dasar_url("https://a.test/artikel/", &dok),
            "https://cdn.test/aset/"
        );
        assert_eq!(
            selesaikan(&dasar_url("https://a.test/artikel/", &dok), "gaya.css").as_deref(),
            Some("https://cdn.test/aset/gaya.css")
        );
    }

    #[test]
    fn tanpa_base_dasar_adalah_url_dokumen() {
        let dok = Document::parse_html("<!doctype html><html><body></body></html>").expect("urai");
        assert_eq!(dasar_url("https://a.test/x/", &dok), "https://a.test/x/");
        assert_eq!(base_href(&dok), None);
    }

    #[test]
    fn base_kosong_diabaikan() {
        let dok = Document::parse_html(
            r#"<!doctype html><html><head><base href="  "></head><body></body></html>"#,
        )
        .expect("urai");
        assert_eq!(base_href(&dok), None);
        assert_eq!(dasar_url("https://a.test/", &dok), "https://a.test/");
    }
}
