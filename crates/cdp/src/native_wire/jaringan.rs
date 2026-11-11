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
    let jawaban = agent.get(url).call();
    let resp = match jawaban {
        Ok(resp) => resp,
        // 4xx/5xx tetap punya badan, dan browser merendernya. Yang tidak boleh
        // adalah melaporkan status 200 untuk halaman yang server bilang 404.
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
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !mime.is_empty() && mime != "text/html" && mime != "application/xhtml+xml" {
        return Err(Galat::baru(
            -32000,
            format!(
                "'{url_akhir}' menjawab content-type '{mime}'; mesin ini hanya mengurai \
                 dokumen HTML, dan mengurai biner jadi pohon kosong akan menyesatkan"
            ),
        ));
    }
    let charset = charset_dari(&content_type).unwrap_or_else(|| "utf-8".to_owned());
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
    let byte = buf.len();
    let html = dekode(&buf, &charset);
    Ok(Ambilan {
        url: url_akhir,
        html,
        status,
        byte,
        charset,
        content_type,
    })
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
