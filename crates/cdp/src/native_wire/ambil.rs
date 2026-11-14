//! Gerbang jaringan untuk `fetch()` yang dipanggil skrip halaman.
//!
//! Skrip halaman tidak boleh membuka soket sendiri. Kalau ia bisa, satu baris
//! JavaScript di halaman mana pun menjadi jalan keluar dari proses daemon ke
//! alamat apa saja yang bisa dijangkau host, termasuk alamat internal. Karena
//! itu jalurnya lewat sini: satu kebijakan yang dibaca dari config daemon,
//! satu keputusan yang dicatat, satu pengambilan yang dibatasi ukuran dan
//! waktunya.
//!
//! Aturan yang berlaku, dan diuji satu per satu di `mod tests`:
//!
//! - **Daftar izin kosong berarti menolak semua.** Kebalikannya (kosong =
//!   izinkan semua) membuat daemon yang lupa dikonfigurasi menjadi proxy
//!   terbuka, dan itu kesalahan yang tidak kelihatan sampai ada yang menyalah-
//!   gunakannya.
//! - Hanya `http`/`https`. Skema lain ditolak dengan nama skemanya.
//! - Daftar tolak menang atas daftar izin.
//! - Metode dibatasi `GET`/`HEAD`/`POST`. `PUT`/`DELETE`/`PATCH` tidak dipakai
//!   `fetch` dari halaman dan setiap metode tambahan adalah permukaan uji baru.
//! - Alamat IP literal yang menunjuk ke loopback atau jaringan privat ditolak
//!   **kecuali operator menuliskan alamat itu sendiri** di daftar izin. Alamat
//!   link-local (`169.254.0.0/16`) dan alamat tak tentu ditolak selalu: tidak
//!   ada halaman web sah yang tinggal di sana, dan di banyak host itu justru
//!   layanan metadata penyedia awan.
//! - Pengalihan diperiksa ulang: host tujuan akhir wajib lolos kebijakan yang
//!   sama, jadi `izin = ["a.test"]` tidak bisa dipakai melompat ke host lain
//!   lewat `302`.
//!
//! Yang **belum** dijaga di sini, dan tidak disembunyikan: pada jalur
//! `fetch()` daftar izin host sudah menutup nama yang menyelesaikan ke alamat
//! internal (hanya host yang ditulis operator yang boleh), sedangkan pada jalur
//! dokumen nama itu **diselesaikan** dan ditolak kalau mendarat di alamat
//! internal (lihat `putuskan_dokumen`). Yang tetap tersisa di kedua jalur
//! adalah DNS rebinding yang mengubah jawaban di antara resolusi di sini dan
//! resolusi klien HTTP; pertahanan untuk itu ada di tingkat jaringan host
//! (egress filter), bukan di proses ini.
//! Badan jawaban didekode sebagai UTF-8 lossy; `content-type` charset tidak
//! dipakai, karena `fetch` dari halaman menerima teks, bukan byte.

use std::io::Read;
use std::net::ToSocketAddrs;

use engine::native::ambil::{JawabanAmbil, Pengambil, PermintaanAmbil};
use policy::select::host_matches;
use url::Url;

use super::jaringan::Batas;

/// Metode yang boleh dipakai `fetch` dari halaman.
pub const METODE_IZIN: &[&str] = &["GET", "HEAD", "POST"];

/// Satu keputusan gerbang, beserta alasannya kalau ditolak.
///
/// Dipisah dari hasil pengambilan supaya keputusannya bisa diuji tanpa
/// menyentuh jaringan sama sekali.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keputusan {
    Izinkan,
    Tolak(String),
}

impl Keputusan {
    /// Alasan penolakan, kalau ditolak.
    #[must_use]
    pub fn alasan(&self) -> Option<&str> {
        match self {
            Self::Izinkan => None,
            Self::Tolak(alasan) => Some(alasan),
        }
    }
}

/// Kebijakan jaringan untuk `fetch()` di dalam isolat.
#[derive(Debug, Clone)]
pub struct KebijakanJaringan {
    izin: Vec<String>,
    tolak: Vec<String>,
    metode: Vec<String>,
    batas: Batas,
}

impl Default for KebijakanJaringan {
    /// Menolak semua: daftar izin kosong.
    fn default() -> Self {
        Self {
            izin: Vec::new(),
            tolak: Vec::new(),
            metode: METODE_IZIN.iter().map(|m| (*m).to_owned()).collect(),
            batas: Batas::default(),
        }
    }
}

impl KebijakanJaringan {
    /// Kebijakan dari config. Pola host memakai aturan yang sama dengan
    /// `[policy.rules]`: sama persis, atau subdomain darinya.
    #[must_use]
    pub fn baru(izin: Vec<String>, tolak: Vec<String>, metode: Vec<String>) -> Self {
        let bawaan = Self::default();
        Self {
            izin,
            tolak,
            metode: if metode.is_empty() {
                bawaan.metode
            } else {
                metode.into_iter().map(|m| m.to_ascii_uppercase()).collect()
            },
            batas: bawaan.batas,
        }
    }

    /// Batas ukuran dan waktu, bisa diperkecil tes.
    #[must_use]
    pub fn dengan_batas(mut self, batas: Batas) -> Self {
        self.batas = batas;
        self
    }

    #[must_use]
    pub fn batas(&self) -> Batas {
        self.batas
    }

    #[must_use]
    pub fn izin(&self) -> &[String] {
        &self.izin
    }

    /// Apakah kebijakan ini mengizinkan apa pun. Dipakai daemon untuk
    /// memperingatkan saat `[fetch]` kosong.
    #[must_use]
    pub fn mengizinkan_sesuatu(&self) -> bool {
        !self.izin.is_empty()
    }

    /// Memutuskan satu permintaan `fetch()`, tanpa menyentuh jaringan.
    #[must_use]
    pub fn putuskan(&self, permintaan: &PermintaanAmbil) -> Keputusan {
        let metode = permintaan.metode.to_ascii_uppercase();
        if !self.metode.iter().any(|m| m == &metode) {
            return Keputusan::Tolak(format!(
                "metode '{metode}' tidak diizinkan gerbang jaringan (yang diizinkan: {})",
                self.metode.join(", ")
            ));
        }

        let (host, disebut) = match self.urai_host(&permintaan.url) {
            Ok(hasil) => hasil,
            Err(alasan) => return Keputusan::Tolak(alasan),
        };
        if let Some(alasan) = alamat_internal(&host, disebut) {
            return Keputusan::Tolak(alasan);
        }

        if self.izin.is_empty() {
            return Keputusan::Tolak(format!(
                "daftar izin gerbang jaringan kosong, jadi '{host}' ditolak; isi [fetch] izin_host di config daemon"
            ));
        }
        if !disebut {
            return Keputusan::Tolak(format!(
                "host '{host}' tidak ada di daftar izin gerbang jaringan"
            ));
        }
        Keputusan::Izinkan
    }

    /// Memutuskan satu URL **dokumen**: `Page.navigate`, `Target.createTarget`,
    /// dan sub-sumber yang diambil mesin untuk dokumen itu (stylesheet, font,
    /// `iframe`).
    ///
    /// Beda dengan [`Self::putuskan`], dan bedanya disengaja. `fetch()` dari
    /// skrip halaman hanya boleh menghubungi host di `izin_host`, karena tidak
    /// ada alasan sah bagi halaman untuk keluar sendiri. Navigasi tidak bisa
    /// memakai aturan itu tanpa memaksa operator menuliskan seluruh web di
    /// `izin_host`, jadi yang diberlakukan di sini adalah bagian **SSRF**-nya
    /// saja: skema `http`/`https`, daftar tolak, dan alamat internal host ini.
    /// Host publik lain diizinkan.
    ///
    /// Gerbang ini wajib ada karena tanpanya `Page.navigate` menjadi jalur
    /// keliling yang tidak diperiksa sama sekali: `fetch()` ke `127.0.0.1`
    /// ditolak, sedangkan navigasi ke alamat yang sama berhasil dan isinya
    /// masuk ke pohon DOM yang dibaca agen.
    #[must_use]
    pub fn putuskan_dokumen(&self, url: &str) -> Keputusan {
        let (host, disebut) = match self.urai_host(url) {
            Ok(hasil) => hasil,
            Err(alasan) => return Keputusan::Tolak(alasan),
        };
        if let Some(alasan) =
            alamat_internal(&host, disebut).or_else(|| alamat_setelah_resolusi(&host, disebut))
        {
            return Keputusan::Tolak(alasan);
        }
        Keputusan::Izinkan
    }

    /// Urai URL lalu kembalikan host yang sudah dinormalkan, beserta apakah ia
    /// disebut di daftar izin. Galatnya adalah alasan penolakan siap kirim.
    ///
    /// Dipisah supaya jalur `fetch` dan jalur dokumen memakai aturan skema,
    /// host, dan daftar tolak yang **sama persis**: dua salinan aturan yang
    /// sama pasti menyimpang, dan yang tertinggal selalu yang lebih jarang
    /// dibaca.
    fn urai_host(&self, url: &str) -> Result<(String, bool), String> {
        let teks = url.trim();
        let url = Url::parse(teks)
            .map_err(|err| format!("'{teks}' bukan URL yang bisa diurai: {err}"))?;
        let skema = url.scheme().to_ascii_lowercase();
        if skema != "http" && skema != "https" {
            return Err(format!(
                "skema '{skema}' tidak diizinkan gerbang jaringan; hanya http dan https"
            ));
        }
        let Some(host) = url.host_str() else {
            return Err(format!("'{teks}' tidak punya host"));
        };
        let host = host.to_ascii_lowercase();
        // `Url::host_str` mengembalikan IPv6 dalam bentuk ber-kurung siku
        // (`[::1]`), sedangkan operator menulis alamatnya tanpa kurung dan
        // `IpAddr::from_str` menolak bentuk ber-kurung. Tanpa penyamaan ini dua
        // hal bocor bersamaan: `[::1]` tidak pernah dikenali sebagai alamat
        // loopback (alasannya jadi "tidak ada di daftar izin", bukan "jaringan
        // internal"), dan entri `::1` di `izin_host` tidak pernah cocok
        // sehingga operator tidak punya cara membukanya secara sadar.
        let host = host
            .strip_prefix('[')
            .and_then(|tanpa_buka| tanpa_buka.strip_suffix(']'))
            .unwrap_or(&host)
            .to_owned();

        if self.tolak.iter().any(|pola| host_matches(pola, &host)) {
            return Err(format!(
                "host '{host}' ada di daftar tolak gerbang jaringan"
            ));
        }
        let disebut = self.izin.iter().any(|pola| host_matches(pola, &host));
        Ok((host, disebut))
    }
}

/// Menolak alamat IP literal yang menunjuk ke dalam host sendiri.
///
/// Dua tingkat, dan bedanya disengaja:
///
/// - **Selalu ditolak:** link-local dan alamat tak tentu. Tidak ada halaman web
///   sah di `169.254.0.0/16`, sedangkan di banyak host justru di sanalah
///   layanan metadata penyedia awan menjawab, jadi tidak ada alasan operator
///   perlu membukanya.
/// - **Ditolak kecuali disebut sendiri:** loopback dan jaringan privat. Itu
///   alamat yang bisa dipakai halaman sungguhan di jaringan operator, jadi
///   larangan mutlak akan menjadi batas yang tidak bisa ia cabut. Menuliskannya
///   di `izin_host` adalah tindakan sadar, dan itulah yang membedakannya dari
///   pola lebar yang tidak sengaja.
///
/// `disebut` berarti alamat itu sudah cocok dengan daftar izin.
fn alamat_internal(host: &str, disebut: bool) -> Option<String> {
    let ip: std::net::IpAddr = host.parse().ok()?;
    let (selalu, kecuali_disebut) = match ip {
        std::net::IpAddr::V4(v4) => (
            v4.is_link_local() || v4.is_unspecified(),
            v4.is_loopback() || v4.is_private(),
        ),
        std::net::IpAddr::V6(v6) => {
            // `::ffff:a.b.c.d` adalah alamat IPv4 yang ditulis sebagai IPv6.
            let v4 = v6.to_ipv4_mapped();
            (
                v6.is_unspecified()
                    || v4.is_some_and(|v4| v4.is_link_local() || v4.is_unspecified()),
                v6.is_loopback() || v4.is_some_and(|v4| v4.is_loopback() || v4.is_private()),
            )
        }
    };
    if selalu {
        return Some(format!(
            "alamat '{host}' adalah alamat link-local atau tak tentu, dan gerbang jaringan \
             menolaknya tanpa pengecualian"
        ));
    }
    (kecuali_disebut && !disebut).then(|| {
        format!(
            "alamat '{host}' menunjuk ke jaringan internal host ini; tulis alamat itu sendiri di \
             [fetch] izin_host kalau memang itu yang dimaksud"
        )
    })
}

/// Menolak **nama host** yang menyelesaikan ke alamat internal.
///
/// Pemeriksaan IP literal saja tidak cukup: `localhost`, `nas.local`, atau nama
/// apa pun yang diarahkan ke `127.0.0.1` lolos sebagai "bukan alamat internal"
/// lalu disambungkan oleh klien HTTP-nya sendiri, dan dari sisi gerbang
/// akibatnya sama dengan navigasi langsung ke alamat itu.
///
/// Resolusinya diulang oleh klien HTTP, jadi ini bukan penjagaan sempurna
/// terhadap DNS rebinding yang mengubah jawaban di antara dua kueri. Yang
/// ditutup di sini adalah kasus yang jelas dan murah ditutup; sisanya tetap
/// tugas egress filter di host.
fn alamat_setelah_resolusi(host: &str, disebut: bool) -> Option<String> {
    // `localhost` dan `*.localhost` adalah loopback menurut RFC 6761, jadi
    // tidak perlu ditanyakan ke resolver.
    if host == "localhost" || host.ends_with(".localhost") {
        return (!disebut).then(|| {
            format!(
                "host '{host}' adalah loopback; tulis namanya sendiri di [fetch] izin_host \
                 kalau memang itu yang dimaksud"
            )
        });
    }
    // Alamat IP literal sudah diperiksa `alamat_internal`, dan nama yang tidak
    // bisa diselesaikan tidak punya alamat untuk diperiksa: klien HTTP-nya
    // sendiri yang akan gagal, dengan galat yang lebih jelas.
    if host.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    for alamat in (host, 0_u16).to_socket_addrs().ok()? {
        if let Some(alasan) = alamat_internal(&alamat.ip().to_string(), disebut) {
            return Some(format!(
                "host '{host}' menyelesaikan ke {}: {alasan}",
                alamat.ip()
            ));
        }
    }
    None
}

/// Pengambil yang menegakkan [`KebijakanJaringan`].
///
/// Ini satu-satunya implementasi [`Pengambil`] yang menyentuh jaringan. Ia
/// blocking, sama seperti pengambilan dokumen di [`super::jaringan`]: pemanggil
/// yang tidak boleh menahan task tokio wajib membungkusnya dengan
/// `spawn_blocking`.
#[derive(Debug, Clone)]
pub struct PengambilJaringan {
    kebijakan: KebijakanJaringan,
}

impl PengambilJaringan {
    #[must_use]
    pub fn baru(kebijakan: KebijakanJaringan) -> Self {
        Self { kebijakan }
    }

    #[must_use]
    pub fn kebijakan(&self) -> &KebijakanJaringan {
        &self.kebijakan
    }
}

impl Pengambil for PengambilJaringan {
    fn ambil(&self, permintaan: &PermintaanAmbil) -> Result<JawabanAmbil, String> {
        let keputusan = self.kebijakan.putuskan(permintaan);
        if let Some(alasan) = keputusan.alasan() {
            // Dicatat, bukan ditelan: penolakan yang tidak meninggalkan jejak
            // membuat halaman yang "tiba-tiba tidak bisa fetch" mustahil
            // dilacak, dan operator tidak punya cara tahu daftar izinnya kurang.
            tracing::warn!(
                url = %permintaan.url,
                metode = %permintaan.metode,
                alasan = %alasan,
                "fetch dari halaman ditolak gerbang jaringan"
            );
            return Err(alasan.to_owned());
        }
        tracing::debug!(
            url = %permintaan.url,
            metode = %permintaan.metode,
            "fetch dari halaman diizinkan gerbang jaringan"
        );
        ambil_blokir(&self.kebijakan, permintaan)
    }
}

/// Satu pengambilan yang sudah lolos kebijakan.
fn ambil_blokir(
    kebijakan: &KebijakanJaringan,
    permintaan: &PermintaanAmbil,
) -> Result<JawabanAmbil, String> {
    let batas = kebijakan.batas;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(batas.connect)
        .timeout(batas.waktu)
        .redirects(batas.redirect)
        .user_agent(super::USER_AGENT)
        .build();

    let hasil = match permintaan.metode.as_str() {
        "GET" => agent.get(&permintaan.url).call(),
        "HEAD" => agent.head(&permintaan.url).call(),
        "POST" => agent
            .post(&permintaan.url)
            .send_string(permintaan.badan.as_deref().unwrap_or("")),
        lain => {
            return Err(format!(
                "metode '{lain}' tidak punya jalur pengambilan di gerbang jaringan"
            ));
        }
    };
    let resp = match hasil {
        Ok(resp) => resp,
        // 4xx/5xx tetap diambil badannya, sama seperti pengambilan dokumen:
        // halaman galat punya isi yang perlu dibaca skrip.
        Err(ureq::Error::Status(_, resp)) => resp,
        Err(ureq::Error::Transport(t)) => {
            return Err(format!("'{}' tidak bisa diambil: {t}", permintaan.url));
        }
    };

    let url_akhir = resp.get_url().to_owned();
    // Pengalihan diperiksa ulang. Tanpa ini `izin = ["a.test"]` cukup dengan
    // satu `302` untuk mendarat di host mana pun.
    let akhir = PermintaanAmbil {
        url: url_akhir.clone(),
        metode: permintaan.metode.clone(),
        badan: None,
    };
    if let Some(alasan) = kebijakan.putuskan(&akhir).alasan() {
        tracing::warn!(
            dari = %permintaan.url,
            ke = %url_akhir,
            alasan = %alasan,
            "pengalihan fetch ditolak gerbang jaringan"
        );
        return Err(format!("pengalihan ke '{url_akhir}' ditolak: {alasan}"));
    }

    let status = resp.status();
    let content_type = resp.header("content-type").map(str::to_owned);
    let mut buf = Vec::new();
    let batas_baca = u64::try_from(batas.byte)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    resp.into_reader()
        .take(batas_baca)
        .read_to_end(&mut buf)
        .map_err(|err| format!("badan '{url_akhir}' gagal dibaca: {err}"))?;
    if buf.len() > batas.byte {
        return Err(format!(
            "badan '{url_akhir}' melebihi batas {} byte; permintaan dihentikan alih-alih \
             memakan RAM daemon",
            batas.byte
        ));
    }

    Ok(JawabanAmbil {
        status,
        url: url_akhir,
        content_type,
        badan: String::from_utf8_lossy(&buf).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minta(url: &str, metode: &str) -> PermintaanAmbil {
        PermintaanAmbil {
            url: url.to_owned(),
            metode: metode.to_owned(),
            badan: None,
        }
    }

    fn tolak(kebijakan: &KebijakanJaringan, url: &str) -> String {
        let keputusan = kebijakan.putuskan(&minta(url, "GET"));
        keputusan
            .alasan()
            .unwrap_or_else(|| panic!("'{url}' seharusnya ditolak"))
            .to_owned()
    }

    #[test]
    fn daftar_izin_kosong_menolak_semua() {
        let kebijakan = KebijakanJaringan::default();
        let alasan = tolak(&kebijakan, "https://contoh.test/");
        assert!(
            alasan.contains("daftar izin"),
            "alasan harus menyebut daftar izin kosong: {alasan}"
        );
    }

    #[test]
    fn host_yang_diizinkan_lolos() {
        let kebijakan =
            KebijakanJaringan::baru(vec!["contoh.test".to_owned()], Vec::new(), Vec::new());
        assert_eq!(
            kebijakan.putuskan(&minta("https://contoh.test/a/b?c=1", "GET")),
            Keputusan::Izinkan
        );
        // Subdomain ikut, sama seperti aturan `[policy.rules]`.
        assert_eq!(
            kebijakan.putuskan(&minta("https://api.contoh.test/", "GET")),
            Keputusan::Izinkan
        );
    }

    #[test]
    fn host_di_luar_daftar_izin_ditolak_dengan_namanya() {
        let kebijakan =
            KebijakanJaringan::baru(vec!["contoh.test".to_owned()], Vec::new(), Vec::new());
        let alasan = tolak(&kebijakan, "https://lain.test/");
        assert!(
            alasan.contains("lain.test"),
            "alasan harus menyebut host: {alasan}"
        );
    }

    #[test]
    fn daftar_tolak_menang_atas_daftar_izin() {
        let kebijakan = KebijakanJaringan::baru(
            vec!["contoh.test".to_owned()],
            vec!["rahasia.contoh.test".to_owned()],
            Vec::new(),
        );
        assert_eq!(
            kebijakan.putuskan(&minta("https://contoh.test/", "GET")),
            Keputusan::Izinkan
        );
        let alasan = tolak(&kebijakan, "https://rahasia.contoh.test/");
        assert!(alasan.contains("daftar tolak"), "{alasan}");
    }

    #[test]
    fn skema_selain_http_ditolak() {
        let kebijakan =
            KebijakanJaringan::baru(vec!["contoh.test".to_owned()], Vec::new(), Vec::new());
        let alasan = tolak(&kebijakan, "file:///etc/passwd");
        assert!(alasan.contains("skema 'file'"), "{alasan}");
    }

    #[test]
    fn metode_di_luar_daftar_ditolak() {
        let kebijakan =
            KebijakanJaringan::baru(vec!["contoh.test".to_owned()], Vec::new(), Vec::new());
        let keputusan = kebijakan.putuskan(&minta("https://contoh.test/", "DELETE"));
        let alasan = keputusan.alasan().expect("DELETE harus ditolak");
        assert!(alasan.contains("DELETE"), "{alasan}");
        assert_eq!(
            kebijakan.putuskan(&minta("https://contoh.test/", "POST")),
            Keputusan::Izinkan
        );
    }

    #[test]
    fn alamat_ip_internal_ditolak_kalau_tidak_disebut() {
        let kebijakan =
            KebijakanJaringan::baru(vec!["contoh.test".to_owned()], Vec::new(), Vec::new());
        for url in [
            "http://127.0.0.1:8080/",
            "http://10.0.0.5/",
            "http://[::1]/",
        ] {
            let alasan = tolak(&kebijakan, url);
            assert!(alasan.contains("jaringan internal"), "'{url}': {alasan}");
        }
    }

    #[test]
    fn alamat_link_local_ditolak_sekalipun_disebut() {
        // 169.254.169.254 adalah alamat metadata penyedia awan: tidak ada
        // halaman sah di sana, jadi larangannya tidak punya pengecualian.
        let kebijakan =
            KebijakanJaringan::baru(vec!["169.254.169.254".to_owned()], Vec::new(), Vec::new());
        let alasan = tolak(&kebijakan, "http://169.254.169.254/latest/meta-data/");
        assert!(alasan.contains("link-local"), "{alasan}");
    }

    #[test]
    fn alamat_loopback_boleh_kalau_ditulis_operator() {
        // Layanan lokal milik operator adalah kasus sah, dan larangan mutlak
        // akan jadi batas yang tidak bisa ia cabut.
        let kebijakan =
            KebijakanJaringan::baru(vec!["127.0.0.1".to_owned()], Vec::new(), Vec::new());
        assert_eq!(
            kebijakan.putuskan(&minta("http://127.0.0.1:8080/", "GET")),
            Keputusan::Izinkan
        );
    }

    #[test]
    fn alamat_ipv6_loopback_dikenali_dan_bisa_dibuka_operator() {
        // Dua sisi satu bug: `Url::host_str` memberi `[::1]` ber-kurung, jadi
        // tanpa penyamaan bentuk, alamat ini (a) tidak dikenali sebagai
        // loopback dan (b) tidak pernah cocok dengan entri `::1` yang ditulis
        // operator. Tes ini mengunci keduanya sekaligus.
        let kosong =
            KebijakanJaringan::baru(vec!["contoh.test".to_owned()], Vec::new(), Vec::new());
        let alasan = tolak(&kosong, "http://[::1]:8080/");
        assert!(alasan.contains("jaringan internal"), "{alasan}");

        let dibuka = KebijakanJaringan::baru(vec!["::1".to_owned()], Vec::new(), Vec::new());
        assert_eq!(
            dibuka.putuskan(&minta("http://[::1]:8080/", "GET")),
            Keputusan::Izinkan
        );
    }

    #[test]
    fn nama_localhost_boleh_kalau_ditulis_operator() {
        let kebijakan =
            KebijakanJaringan::baru(vec!["localhost".to_owned()], Vec::new(), Vec::new());
        assert_eq!(
            kebijakan.putuskan(&minta("http://localhost:8080/", "GET")),
            Keputusan::Izinkan
        );
    }

    #[test]
    fn url_ngawur_ditolak() {
        let kebijakan =
            KebijakanJaringan::baru(vec!["contoh.test".to_owned()], Vec::new(), Vec::new());
        let alasan = tolak(&kebijakan, "bukan url");
        assert!(alasan.contains("bukan URL"), "{alasan}");
    }

    /// Alasan penolakan satu URL dokumen, dengan pesan yang gagal keras kalau
    /// URL itu ternyata diizinkan.
    fn tolak_dokumen(kebijakan: &KebijakanJaringan, url: &str) -> String {
        kebijakan
            .putuskan_dokumen(url)
            .alasan()
            .unwrap_or_else(|| panic!("'{url}' seharusnya ditolak"))
            .to_owned()
    }

    #[test]
    fn dokumen_loopback_ditolak_dengan_kebijakan_bawaan() {
        // Inti lubang SSRF: `Page.navigate` dulu tidak membaca kebijakan sama
        // sekali, jadi alamat ini masuk walaupun `izin_host` tidak
        // menyebutkannya. Kebijakan bawaan (izin kosong) harus menolaknya
        // sebagai alamat internal, bukan sebagai "tidak ada di daftar izin":
        // kalimatnya yang membedakan sebabnya bagi operator.
        let bawaan = KebijakanJaringan::default();
        let alasan = tolak_dokumen(&bawaan, "http://127.0.0.1:8099/tes.html");
        assert!(alasan.contains("jaringan internal"), "{alasan}");
    }

    #[test]
    fn dokumen_loopback_boleh_kalau_ditulis_operator() {
        // Tanpa pengecualian ini, halaman uji di host sendiri mustahil dibuka
        // dan operator tidak punya cara memperbaikinya selain mematikan
        // gerbang.
        let kebijakan =
            KebijakanJaringan::baru(vec!["127.0.0.1".to_owned()], Vec::new(), Vec::new());
        assert_eq!(
            kebijakan.putuskan_dokumen("http://127.0.0.1:8099/tes.html"),
            Keputusan::Izinkan
        );
    }

    #[test]
    fn dokumen_link_local_ditolak_sekalipun_disebut() {
        // Sama seperti jalur `fetch`: tidak ada halaman sah di
        // `169.254.0.0/16`, dan di banyak host justru layanan metadata awan.
        let kebijakan =
            KebijakanJaringan::baru(vec!["169.254.169.254".to_owned()], Vec::new(), Vec::new());
        let alasan = tolak_dokumen(&kebijakan, "http://169.254.169.254/latest/meta-data/");
        assert!(alasan.contains("link-local"), "{alasan}");
    }

    #[test]
    fn dokumen_host_publik_diizinkan_tanpa_daftar_izin() {
        // Aturan "daftar izin kosong = tolak semua" milik `fetch`, bukan milik
        // navigasi: kalau diberlakukan di sini, browser hanya bisa membuka host
        // yang ditulis operator dan itu bukan browser.
        let bawaan = KebijakanJaringan::default();
        assert_eq!(
            bawaan.putuskan_dokumen("https://contoh.invalid/artikel"),
            Keputusan::Izinkan
        );
    }

    #[test]
    fn dokumen_nama_localhost_ditolak_tanpa_izin() {
        // Pemeriksaan IP literal saja akan meloloskan nama ini, lalu klien
        // HTTP-nya sendiri yang menyambung ke loopback.
        let kebijakan =
            KebijakanJaringan::baru(vec!["contoh.invalid".to_owned()], Vec::new(), Vec::new());
        let alasan = tolak_dokumen(&kebijakan, "http://localhost:8099/tes.html");
        assert!(alasan.contains("loopback"), "{alasan}");

        let dibuka = KebijakanJaringan::baru(vec!["localhost".to_owned()], Vec::new(), Vec::new());
        assert_eq!(
            dibuka.putuskan_dokumen("http://localhost:8099/tes.html"),
            Keputusan::Izinkan
        );
    }

    #[test]
    fn dokumen_menolak_skema_di_luar_http() {
        let kebijakan =
            KebijakanJaringan::baru(vec!["contoh.invalid".to_owned()], Vec::new(), Vec::new());
        let alasan = tolak_dokumen(&kebijakan, "file:///etc/passwd");
        assert!(alasan.contains("skema 'file'"), "{alasan}");
    }
}
