//! Pengambilan dokumen dari jaringan untuk mesin native.
//!
//! Mesin ini tidak boleh menyerahkan pengambilan dokumen ke `curl` atau
//! `wget`: satu biner luar lagi berarti satu proses lagi yang bisa bocor dan
//! satu lagi yang harus ada di host. Kliennya `ureq` dengan TLS rustls
//! (`deny.toml` melarang openssl), dan panggilan blocking-nya dijalankan lewat
//! `spawn_blocking` supaya task tokio yang melayani soket CDP tidak berhenti
//! menunggu jaringan.
//!
//! Yang dijaga di sini, dan diuji satu per satu di `tests/jaringan.rs`:
//!
//! - hanya `http`/`https`; skema lain ditolak dengan nama skemanya
//! - batas ukuran badan, batas waktu, dan batas jumlah pengalihan
//! - `content-type` yang bukan HTML ditolak, bukan di-parse jadi pohon kosong
//!   yang membuat klien mengira navigasinya berhasil
//! - status 4xx/5xx tetap diambil badannya, seperti browser: halaman 404 punya
//!   HTML yang perlu dibaca agen, dan statusnya dilaporkan apa adanya

use std::io::Read;
use std::time::Duration;

use super::state::Galat;

/// Batas badan yang diterima. Delapan mebibyte cukup untuk halaman artikel
/// terbesar yang masuk akal, dan cukup kecil untuk tidak menghabiskan RAM
/// daemon yang anggarannya 100 MB.
pub const BATAS_BYTE: usize = 8 * 1024 * 1024;

/// Batas waktu satu pengambilan, termasuk koneksi dan seluruh pengalihan.
pub const BATAS_WAKTU: Duration = Duration::from_secs(20);

/// Batas waktu membuka koneksi TCP/TLS. Dipisah supaya host yang tidak
/// menjawab gagal cepat, bukan menunggu batas penuh.
pub const BATAS_CONNECT: Duration = Duration::from_secs(5);

/// Batas jumlah pengalihan. Sama dengan bawaan Chrome.
pub const BATAS_REDIRECT: u32 = 5;

/// Batas badan satu lembar gaya.
///
/// Satu mebibyte cukup untuk CSS produksi terbesar yang masuk akal (Bootstrap 5
/// minified ~230 KB). Lembar yang lebih besar **dilewati dengan alasan**, bukan
/// dipotong: CSS yang terpotong di tengah aturan menghasilkan kaskade yang
/// salah tanpa satu pun galat, dan itu jauh lebih sulit dilacak daripada
/// halaman yang jelas tanpa gaya.
pub const BATAS_BYTE_CSS: usize = 1024 * 1024;

/// Batas waktu satu lembar gaya, lebih pendek dari dokumen: halaman yang CSS-nya
/// lambat tetap harus bisa dibaca, dan lembar yang belum datang dilewati.
pub const BATAS_WAKTU_CSS: Duration = Duration::from_secs(5);

/// Jumlah lembar gaya terbanyak yang diambil untuk satu dokumen.
pub const MAKS_LEMBAR: usize = 16;

/// Total byte CSS terbanyak untuk satu dokumen, dijumlahkan lintas lembar.
pub const BATAS_TOTAL_CSS: usize = 2 * 1024 * 1024;

/// Batas byte satu berkas font.
///
/// Delapan mebibyte memuat Noto Sans CJK penuh (~16 MB tidak, tapi subset yang
/// dipakai halaman web biasanya di bawah 2 MB). Berkas yang lebih besar
/// **dilewati dengan alasan**: font yang dipotong di tengah `glyf` bukan font,
/// dan `ttf_parser` akan menolaknya tanpa menjelaskan kenapa.
pub const BATAS_BYTE_FONT: usize = 8 * 1024 * 1024;

/// Batas waktu satu berkas font. Lebih longgar dari CSS karena font memang
/// besar, tapi tetap terbatas: halaman tanpa webfont masih bisa dibaca.
pub const BATAS_WAKTU_FONT: Duration = Duration::from_secs(8);

/// Jumlah `@font-face` terbanyak yang diunduh untuk satu dokumen.
pub const MAKS_FONT: usize = 8;

/// Total byte font terbanyak untuk satu dokumen.
pub const BATAS_TOTAL_FONT: usize = 24 * 1024 * 1024;

/// Batas yang dipakai satu pengambilan. Bisa diperkecil di tes.
#[derive(Debug, Clone, Copy)]
pub struct Batas {
    pub byte: usize,
    pub waktu: Duration,
    pub connect: Duration,
    pub redirect: u32,
}

impl Default for Batas {
    fn default() -> Self {
        Self {
            byte: BATAS_BYTE,
            waktu: BATAS_WAKTU,
            connect: BATAS_CONNECT,
            redirect: BATAS_REDIRECT,
        }
    }
}

/// Dokumen yang berhasil diambil, beserta apa yang dikatakan server.
///
/// `url` adalah URL **setelah** pengalihan, bukan yang diminta: itulah yang
/// harus dipakai `Page.getFrameTree` dan tanda terima, karena alamat akhir
/// adalah yang sedang dibaca pengguna.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ambilan {
    pub url: String,
    pub html: String,
    pub status: u16,
    pub byte: usize,
    pub charset: String,
    pub content_type: String,
}

/// Apa yang dikatakan server, tanpa badannya.
///
/// Dipisah dari [`Ambilan`] supaya dokumen yang sudah di-parse bisa menyimpan
/// metadata ini tanpa menyalin badan HTML-nya lagi: satu halaman 8 MB akan
/// diklon dua kali di setiap navigasi kalau metadatanya menempel pada badan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sumber {
    /// URL setelah pengalihan.
    pub url: String,
    pub status: u16,
    pub byte: usize,
    pub charset: String,
    pub content_type: String,
}

impl Ambilan {
    /// Metadata pengambilan, tanpa badan.
    #[must_use]
    pub fn sumber(&self) -> Sumber {
        Sumber {
            url: self.url.clone(),
            status: self.status,
            byte: self.byte,
            charset: self.charset.clone(),
            content_type: self.content_type.clone(),
        }
    }
}

/// Satu lembar gaya yang berhasil diambil, beserta apa yang dikatakan server.
///
/// `url` adalah URL **setelah** pengalihan. `teks` sudah didekode menurut
/// charset yang diklaim server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gaya {
    pub url: String,
    pub teks: String,
    pub status: u16,
    pub byte: usize,
    pub charset: String,
    pub content_type: String,
}

/// Satu berkas font yang berhasil diambil, apa adanya sebagai byte.
///
/// Tidak didekode di sini: yang memutuskan byte itu font yang bisa dipakai
/// adalah `ttf_parser` di lapisan teks, dan mendekode dua kali hanya menambah
/// tempat salahnya.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Font {
    pub url: String,
    pub byte: Vec<u8>,
    pub status: u16,
    pub content_type: String,
}

/// Apakah URL ini butuh jaringan. Pemanggil async memakai ini untuk
/// memutuskan mengambil dokumen **sebelum** kunci state dipegang.
#[must_use]
pub fn butuh_jaringan(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.starts_with("http://") || u.starts_with("https://")
}

/// Mengambil dokumen di `spawn_blocking`.
///
/// Galat dari thread pengambilan diteruskan sebagai galat protokol, bukan
/// `panic`: satu halaman yang tidak bisa diambil tidak boleh menjatuhkan
/// daemon yang sedang melayani tab lain.
pub async fn muat(url: &str, batas: Batas) -> Result<Ambilan, Galat> {
    let url = url.to_owned();
    let untuk_galat = url.clone();
    tokio::task::spawn_blocking(move || muat_blokir(&url, batas))
        .await
        .map_err(|err| {
            Galat::baru(
                -32000,
                format!("pengambilan '{untuk_galat}' gagal di thread: {err}"),
            )
        })?
}

/// Versi blocking, dipakai tes dan `spawn_blocking`.
pub fn muat_blokir(url: &str, batas: Batas) -> Result<Ambilan, Galat> {
    let jawaban = ambil_badan(url, batas)?;
    let mime = mime_dari(&jawaban.content_type);
    if !mime.is_empty() && mime != "text/html" && mime != "application/xhtml+xml" {
        return Err(Galat::baru(
            -32000,
            format!(
                "'{}' menjawab content-type '{mime}'; mesin ini hanya mengurai \
                 dokumen HTML, dan mengurai biner jadi pohon kosong akan menyesatkan",
                jawaban.url
            ),
        ));
    }
    let charset = charset_dari(&jawaban.content_type).unwrap_or_else(|| "utf-8".to_owned());
    let byte = jawaban.badan.len();
    let html = dekode(&jawaban.badan, &charset);
    Ok(Ambilan {
        url: jawaban.url,
        html,
        status: jawaban.status,
        byte,
        charset,
        content_type: jawaban.content_type,
    })
}

/// Mengambil satu berkas font di `spawn_blocking`.
pub async fn muat_font(url: &str, batas: Batas) -> Result<Font, Galat> {
    let url = url.to_owned();
    let untuk_galat = url.clone();
    tokio::task::spawn_blocking(move || muat_font_blokir(&url, batas))
        .await
        .map_err(|err| {
            Galat::baru(
                -32000,
                format!("pengambilan font '{untuk_galat}' gagal di thread: {err}"),
            )
        })?
}

/// Versi blocking [`muat_font`].
///
/// `content-type` yang diterima: kosong, `font/*`, `application/font-*`,
/// `application/x-font-*`, `application/vnd.ms-fontobject`, dan
/// `application/octet-stream`. Yang **ditolak** adalah `text/html`: server yang
/// menyajikan halaman galat di URL `.woff2` akan menghasilkan byte yang bukan
/// font, dan memasangnya sebagai face berarti melaporkan "font rusak" untuk
/// masalah yang sebenarnya 404.
pub fn muat_font_blokir(url: &str, batas: Batas) -> Result<Font, Galat> {
    let jawaban = ambil_badan(url, batas)?;
    let mime = mime_dari(&jawaban.content_type);
    let diterima = mime.is_empty()
        || mime.starts_with("font/")
        || mime.starts_with("application/font")
        || mime.starts_with("application/x-font")
        || mime == "application/vnd.ms-fontobject"
        || mime == "application/octet-stream";
    if !diterima {
        return Err(Galat::baru(
            -32000,
            format!(
                "'{}' menjawab content-type '{mime}', bukan berkas font; webfont \
                 dilewati alih-alih dipasang sebagai face",
                jawaban.url
            ),
        ));
    }
    Ok(Font {
        url: jawaban.url,
        byte: jawaban.badan,
        status: jawaban.status,
        content_type: jawaban.content_type,
    })
}

/// Mengambil satu lembar gaya di `spawn_blocking`.
pub async fn muat_css(url: &str, batas: Batas) -> Result<Gaya, Galat> {
    let url = url.to_owned();
    let untuk_galat = url.clone();
    tokio::task::spawn_blocking(move || muat_css_blokir(&url, batas))
        .await
        .map_err(|err| {
            Galat::baru(
                -32000,
                format!("pengambilan CSS '{untuk_galat}' gagal di thread: {err}"),
            )
        })?
}

/// Versi blocking [`muat_css`].
///
/// `content-type` yang diterima hanya `text/css` atau kosong. Server yang
/// mengirim stylesheet sebagai `text/plain` atau `application/octet-stream`
/// akan dilewati dengan nama mime-nya, bukan ditebak: menebak berarti
/// menjalankan apa pun yang kebetulan berakhiran `.css` sebagai CSS, dan
/// `text/html` yang disajikan di URL `.css` adalah halaman galat, bukan gaya.
pub fn muat_css_blokir(url: &str, batas: Batas) -> Result<Gaya, Galat> {
    let jawaban = ambil_badan(url, batas)?;
    let mime = mime_dari(&jawaban.content_type);
    if !mime.is_empty() && mime != "text/css" {
        return Err(Galat::baru(
            -32000,
            format!(
                "'{}' menjawab content-type '{mime}', bukan 'text/css'; stylesheet \
                 dilewati alih-alih diurai sebagai CSS",
                jawaban.url
            ),
        ));
    }
    let charset = charset_dari(&jawaban.content_type).unwrap_or_else(|| "utf-8".to_owned());
    let byte = jawaban.badan.len();
    let teks = dekode(&jawaban.badan, &charset);
    Ok(Gaya {
        url: jawaban.url,
        teks,
        status: jawaban.status,
        byte,
        charset,
        content_type: jawaban.content_type,
    })
}

/// Badan satu jawaban HTTP, sudah dibaca penuh dan dibatasi ukurannya.
struct Jawaban {
    url: String,
    status: u16,
    content_type: String,
    badan: Vec<u8>,
}

/// Satu permintaan GET yang badan dan metadatanya dipakai bersama oleh
/// pengambilan dokumen dan pengambilan stylesheet.
///
/// Status 4xx/5xx tetap diambil badannya, seperti browser: halaman 404 punya
/// HTML yang perlu dibaca agen, dan stylesheet 404 tetap dilaporkan statusnya
/// alih-alih diperlakukan sebagai "tidak ada gaya".
fn ambil_badan(url: &str, batas: Batas) -> Result<Jawaban, Galat> {
    if !butuh_jaringan(url) {
        return Err(Galat::baru(
            -32000,
            format!("'{url}' bukan URL http/https, jadi tidak ada yang bisa diambil dari jaringan"),
        ));
    }
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(batas.connect)
        .timeout(batas.waktu)
        .redirects(batas.redirect)
        .user_agent(super::USER_AGENT)
        .build();
    let resp = match agent.get(url).call() {
        Ok(resp) => resp,
        Err(ureq::Error::Status(_, resp)) => resp,
        Err(ureq::Error::Transport(t)) => {
            return Err(Galat::baru(
                -32000,
                format!("'{url}' tidak bisa diambil: {t}"),
            ));
        }
    };
    let status = resp.status();
    let url_akhir = resp.get_url().to_owned();
    let content_type = resp.header("content-type").unwrap_or("").to_owned();
    let mut buf = Vec::new();
    let batas_baca = u64::try_from(batas.byte)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    resp.into_reader()
        .take(batas_baca)
        .read_to_end(&mut buf)
        .map_err(|err| Galat::baru(-32000, format!("badan '{url_akhir}' gagal dibaca: {err}")))?;
    if buf.len() > batas.byte {
        return Err(Galat::baru(
            -32000,
            format!(
                "badan '{url_akhir}' melebihi batas {} byte; permintaan dihentikan alih-alih \
                 memakan RAM daemon",
                batas.byte
            ),
        ));
    }
    Ok(Jawaban {
        url: url_akhir,
        status,
        content_type,
        badan: buf,
    })
}

/// Mime telanjang dari header `content-type`, tanpa parameter.
fn mime_dari(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// `charset=` dari header `content-type`, kalau ada.
#[must_use]
pub fn charset_dari(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|bagian| {
        let (nama, nilai) = bagian.split_once('=')?;
        if nama.trim().eq_ignore_ascii_case("charset") {
            let nilai = nilai.trim().trim_matches('"');
            (!nilai.is_empty()).then(|| nilai.to_ascii_lowercase())
        } else {
            None
        }
    })
}

/// Mendekode badan menurut charset yang diklaim server.
///
/// Charset yang tidak dikenal jatuh ke UTF-8 lossy, dan itu disebut di
/// `Ambilan.charset` supaya klien tahu teksnya mungkin tidak persis.
fn dekode(bytes: &[u8], charset: &str) -> String {
    match encoding_rs::Encoding::for_label(charset.as_bytes()) {
        Some(enc) if enc != encoding_rs::UTF_8 => {
            let (teks, _, _) = enc.decode(bytes);
            teks.into_owned()
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}
