//! Klasifikasi kegagalan buka-web: enam kategori, selalu dengan bukti.
//!
//! Yang dijaga modul ini satu hal: **tidak ada kategori tanpa bukti.** Kalau
//! bukti tidak cukup, jawabannya bukan "mungkin anti-bot", tapi "halaman ini
//! terbuka". Menebak kategori lebih mahal daripada tidak mengklasifikasi,
//! karena keputusan berikutnya (ulang, ganti mesin, handoff) diambil dari
//! kategori itu.
//!
//! Klasifikasi ini **bukan** penilaian mutu isi halaman. Status 404 dari server
//! yang sehat tetap dihitung "terbuka": halamannya memang tampil, mesin dan
//! jaringannya tidak ada masalah. Yang diklasifikasi hanya kegagalan membuka.
//!
//! Urutan pemeriksaan dipilih dari yang paling tidak bisa disalahartikan:
//! mesin mati dulu (tidak ada halaman yang bisa muncul), lalu pagar tab, lalu
//! jaringan, lalu kebutuhan manusia, lalu tantangan anti-bot, terakhir lambat.
//! Tantangan diperiksa sesudah OTP karena halaman 2FA sering memuat widget
//! captcha di dalamnya, dan "butuh manusia" lebih berguna bagi operator
//! daripada "kena blok".
//!
//! Bukti nyata yang membentuk aturan ini (4 Nov 2026, daemon 9224):
//! `google.com/search` menjawab 200 lalu **429** di `/sorry/index` dengan body
//! 0 karakter, `nowsecure.nl` tidak mengirim event dokumen sama sekali karena
//! navigasinya dimulai sebelum `Network.enable`, dan host yang tidak ada
//! berakhir di `chrome-error://chromewebdata/` dengan body 0 karakter juga.
//! Deteksi yang hanya membaca teks halaman akan menyebut ketiganya "normal".

use policy::challenge::{self, DomFinding, Evidence};
use serde::Serialize;

/// Enam kategori tetap, tidak boleh bertambah tanpa keputusan owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kategori {
    /// DNS gagal, koneksi ditolak, TLS putus, atau mesin berhenti di halaman
    /// galatnya sendiri.
    Jaringan,
    /// Halaman tantangan anti-bot benar-benar terlihat (status, pola URL, atau
    /// sinyal DOM yang lolos ambang).
    BlokAntiBot,
    /// Mesin hidup dan jawab, tapi dokumen tidak selesai memuat sampai batas
    /// waktu.
    RenderLambat,
    /// Pagar jumlah tab tercapai: permintaan tidak pernah sampai ke jaringan.
    MesinPenuh,
    /// Proses mesin tidak hidup lagi, atau keluar dengan kode bukan nol.
    MesinMati,
    /// Halaman menunggu manusia: OTP, 2FA, atau captcha yang tidak bisa
    /// dilewati mesin.
    ButuhManusia,
}

impl Kategori {
    /// Semua nilai, urut sesuai urutan pemeriksaan di [`nilai`].
    pub const SEMUA: [Self; 6] = [
        Self::MesinMati,
        Self::MesinPenuh,
        Self::Jaringan,
        Self::ButuhManusia,
        Self::BlokAntiBot,
        Self::RenderLambat,
    ];

    /// Label yang dipakai di log, ringkasan artefak, dan label metrik.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Jaringan => "jaringan",
            Self::BlokAntiBot => "blok_anti_bot",
            Self::RenderLambat => "render_lambat",
            Self::MesinPenuh => "mesin_penuh",
            Self::MesinMati => "mesin_mati",
            Self::ButuhManusia => "butuh_manusia",
        }
    }

    /// Kebalikan [`Kategori::as_str`]. Dipakai saat membaca ulang artefak.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::SEMUA
            .into_iter()
            .find(|kategori| kategori.as_str() == raw.trim())
    }
}

impl std::fmt::Display for Kategori {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Apa yang benar-benar diamati pada satu percobaan buka-web.
///
/// Semua field `Option` dengan alasan yang sama: "tidak tahu" berbeda dari
/// "nol". Tab yang belum pernah dihitung bukan berarti nol tab, dan detik yang
/// tidak terukur bukan berarti cepat.
#[derive(Debug, Clone, Default)]
pub struct Kenyataan {
    /// Status respons dokumen utama. Hilang kalau navigasi sudah jalan sebelum
    /// `Network.enable`, dan itu kejadian biasa, bukan galat.
    pub status_dokumen: Option<u16>,
    /// URL akhir dari `Page.frameNavigated` frame utama.
    pub url_akhir: Option<String>,
    /// Judul dokumen saat diperiksa.
    pub judul: Option<String>,
    /// `Network.loadingFailed.errorText`, mis. `net::ERR_NAME_NOT_RESOLVED`.
    pub galat_jaringan: Option<String>,
    /// Hasil probe DOM dari `policy::challenge::JS_DETECT`.
    pub sinyal_dom: Option<DomFinding>,
    /// Ada field OTP/2FA di halaman.
    pub otp_terlihat: bool,
    /// `document.readyState == "complete"` sebelum batas waktu.
    pub dokumen_selesai: Option<bool>,
    /// Waktu tunggu yang benar-benar terpakai, detik.
    pub detik: Option<f64>,
    /// Batas tunggu yang ditetapkan untuk percobaan ini, detik.
    pub batas_detik: Option<f64>,
    /// Tab yang sudah terpakai di mesin ini.
    pub tab_terpakai: Option<u32>,
    /// Pagar tab mesin ini. `0` berarti tanpa pagar.
    pub batas_tab: Option<u32>,
    /// Apakah proses mesin masih hidup.
    pub mesin_hidup: Option<bool>,
    /// Kode keluar proses mesin, kalau sudah keluar.
    pub exit_code: Option<i32>,
}

/// Satu kategori beserta alasan dan angka yang mendasarinya.
///
/// Tidak bisa dibentuk tanpa `sebab` non-kosong: itu yang membuat AC-002
/// mustahil dilanggar lewat jalan biasa, bukan hanya lewat disiplin penulis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Bukti {
    pub kategori: Kategori,
    /// Label sebab, mis. `status=429`, `url=/sorry/index`, `load=net::ERR_NAME_NOT_RESOLVED`.
    pub sebab: Vec<String>,
    pub status: Option<u16>,
    pub url_akhir: Option<String>,
    pub judul: Option<String>,
    pub galat: Option<String>,
    pub tab: Option<String>,
    /// Hidup/matinya proses mesin saat diperiksa (`hidup=false`).
    pub mesin: Option<String>,
    /// Terlihatnya field OTP/2FA saat diperiksa.
    pub otp: Option<bool>,
    pub exit_code: Option<i32>,
    pub detik: Option<String>,
}

impl Bukti {
    /// `None` kalau tidak ada satu pun sebab. Pemanggil **tidak** boleh
    /// menggantinya dengan kategori default: ketiadaan bukti berarti halaman
    /// itu dianggap terbuka.
    #[must_use]
    pub fn baru(kategori: Kategori, sebab: Vec<String>, kenyataan: &Kenyataan) -> Option<Self> {
        if sebab.is_empty() {
            return None;
        }
        Some(Self {
            kategori,
            sebab,
            status: kenyataan.status_dokumen,
            url_akhir: kenyataan.url_akhir.clone(),
            judul: kenyataan.judul.clone(),
            galat: kenyataan.galat_jaringan.clone(),
            tab: match (kenyataan.tab_terpakai, kenyataan.batas_tab) {
                (Some(terpakai), Some(batas)) => Some(format!("{terpakai}/{batas}")),
                (Some(terpakai), None) => Some(terpakai.to_string()),
                _ => None,
            },
            mesin: kenyataan.mesin_hidup.map(|hidup| format!("hidup={hidup}")),
            otp: kenyataan.otp_terlihat.then_some(true),
            exit_code: kenyataan.exit_code,
            detik: match (kenyataan.detik, kenyataan.batas_detik) {
                (Some(detik), Some(batas)) => Some(format!("{detik:.1}/{batas:.1}")),
                (Some(detik), None) => Some(format!("{detik:.1}")),
                _ => None,
            },
        })
    }

    /// Satu baris untuk log dan ringkasan artefak.
    #[must_use]
    pub fn ringkas(&self) -> String {
        format!("{}: {}", self.kategori, self.sebab.join(", "))
    }
}

/// Hasil satu percobaan buka-web.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hasil {
    /// Halaman terbuka. Status HTTP tidak diperiksa di sini: 404 dari server
    /// yang sehat tetap halaman yang terbuka.
    Terbuka {
        status: Option<u16>,
        detik: Option<String>,
    },
    Gagal(Bukti),
}

impl Hasil {
    #[must_use]
    pub fn kategori(&self) -> Option<Kategori> {
        match self {
            Self::Terbuka { .. } => None,
            Self::Gagal(bukti) => Some(bukti.kategori),
        }
    }

    #[must_use]
    pub const fn terbuka(&self) -> bool {
        matches!(self, Self::Terbuka { .. })
    }
}

/// Klasifikasi satu percobaan. Murni: tidak menyentuh jaringan, waktu, atau
/// berkas, jadi bisa diuji persis dan dipakai ulang oleh pemanggil mana pun.
#[must_use]
pub fn nilai(kenyataan: &Kenyataan) -> Hasil {
    if let Some(bukti) = mesin_mati(kenyataan)
        .or_else(|| mesin_penuh(kenyataan))
        .or_else(|| jaringan(kenyataan))
        .or_else(|| butuh_manusia(kenyataan))
        .or_else(|| blok_anti_bot(kenyataan))
        .or_else(|| render_lambat(kenyataan))
    {
        return Hasil::Gagal(bukti);
    }
    Hasil::Terbuka {
        status: kenyataan.status_dokumen,
        detik: kenyataan.detik.map(|detik| format!("{detik:.1}")),
    }
}

fn mesin_mati(k: &Kenyataan) -> Option<Bukti> {
    let mut sebab = Vec::new();
    if k.mesin_hidup == Some(false) {
        sebab.push("proses mesin tidak hidup".to_owned());
    }
    if let Some(kode) = k.exit_code.filter(|kode| *kode != 0) {
        sebab.push(format!("exit_code={kode}"));
    }
    Bukti::baru(Kategori::MesinMati, sebab, k)
}

fn mesin_penuh(k: &Kenyataan) -> Option<Bukti> {
    let (terpakai, batas) = (k.tab_terpakai?, k.batas_tab?);
    if batas == 0 || terpakai < batas {
        return None;
    }
    Bukti::baru(
        Kategori::MesinPenuh,
        vec![format!("tab={terpakai}/{batas}")],
        k,
    )
}

/// Pola URL yang menandai halaman antara milik sistem anti-bot, di luar pola
/// yang sudah dikenal `policy::challenge`. `/sorry/index` adalah halaman
/// blokir Google: dia tidak memuat kata "challenge" dan statusnya 429.
#[must_use]
pub fn url_blokir(url: &str) -> bool {
    let lowered = url.to_ascii_lowercase();
    lowered.contains("/sorry/")
        || lowered.contains("/unusual_traffic")
        || lowered.contains("__cf_chl")
        || lowered.contains("/cdn-cgi/challenge-platform")
}

fn jaringan(k: &Kenyataan) -> Option<Bukti> {
    let mut sebab = Vec::new();
    if let Some(galat) = k.galat_jaringan.as_deref() {
        sebab.push(format!("load={galat}"));
    }
    if k.url_akhir
        .as_deref()
        .is_some_and(|url| url.starts_with("chrome-error://"))
    {
        sebab.push("url=chrome-error".to_owned());
    }
    Bukti::baru(Kategori::Jaringan, sebab, k)
}

fn butuh_manusia(k: &Kenyataan) -> Option<Bukti> {
    if !k.otp_terlihat {
        return None;
    }
    Bukti::baru(
        Kategori::ButuhManusia,
        vec!["form OTP/2FA terlihat".to_owned()],
        k,
    )
}

fn blok_anti_bot(k: &Kenyataan) -> Option<Bukti> {
    let mut sebab = Vec::new();
    let evidence = Evidence {
        document_status: k.status_dokumen,
        final_url: k.url_akhir.clone(),
        dom: k.sinyal_dom,
        expected_missing: false,
        clearance_missing: false,
    };
    let putusan = challenge::verdict(&evidence);
    if putusan.is_challenge {
        sebab.push(format!(
            "sinyal={} bobot={}",
            putusan.signal_labels().join("+"),
            putusan.weight
        ));
    } else if let Some(status) = k.status_dokumen.filter(|status| *status == 429) {
        // 429 sendirian berbobot 2 dan sengaja tidak melewati ambang, tapi
        // digabung pola URL blokir dia jadi bukti yang cukup.
        if k.url_akhir.as_deref().is_some_and(url_blokir) {
            sebab.push(format!("status={status}"));
        }
    }
    if k.url_akhir.as_deref().is_some_and(url_blokir) && !sebab.is_empty() {
        sebab.push("url=pola blokir".to_owned());
    }
    Bukti::baru(Kategori::BlokAntiBot, sebab, k)
}

fn render_lambat(k: &Kenyataan) -> Option<Bukti> {
    let mut sebab = Vec::new();
    if k.dokumen_selesai == Some(false) {
        sebab.push("dokumen tidak selesai memuat".to_owned());
    }
    if let (Some(detik), Some(batas)) = (k.detik, k.batas_detik)
        && detik > batas
    {
        sebab.push(format!("detik={detik:.1} > batas={batas:.1}"));
    }
    Bukti::baru(Kategori::RenderLambat, sebab, k)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kenyataan(status: u16) -> Kenyataan {
        Kenyataan {
            status_dokumen: Some(status),
            url_akhir: Some("http://127.0.0.1:8793/normal.html".to_owned()),
            judul: Some("Halaman normal".to_owned()),
            dokumen_selesai: Some(true),
            detik: Some(0.4),
            batas_detik: Some(10.0),
            ..Kenyataan::default()
        }
    }

    #[test]
    fn kategori_putar_balik_nama() {
        for kategori in Kategori::SEMUA {
            assert_eq!(Kategori::parse(kategori.as_str()), Some(kategori));
        }
        assert_eq!(Kategori::parse("entah"), None);
        assert_eq!(Kategori::SEMUA.len(), 6);
    }

    #[test]
    fn tiga_halaman_tiga_kategori_stabil() {
        // AC-001: satu halaman normal, satu 403 bertanda tantangan, satu
        // butuh 2FA. Diulang tiga kali: fungsi murni, jadi hasilnya wajib sama.
        let normal = kenyataan(200);

        let tantangan = Kenyataan {
            status_dokumen: Some(403),
            url_akhir: Some("http://127.0.0.1:8793/tantangan.html".to_owned()),
            judul: Some("Just a moment...".to_owned()),
            sinyal_dom: Some(DomFinding {
                challenge_form: true,
                challenge_text: true,
                ..DomFinding::default()
            }),
            dokumen_selesai: Some(true),
            detik: Some(1.2),
            batas_detik: Some(10.0),
            ..Kenyataan::default()
        };

        let dua_faktor = Kenyataan {
            status_dokumen: Some(200),
            url_akhir: Some("http://127.0.0.1:8793/otp.html".to_owned()),
            judul: Some("Verifikasi dua langkah".to_owned()),
            otp_terlihat: true,
            dokumen_selesai: Some(true),
            detik: Some(0.8),
            batas_detik: Some(10.0),
            ..Kenyataan::default()
        };

        for _ in 0..3 {
            assert!(nilai(&normal).terbuka(), "halaman normal tidak boleh gagal");
            assert_eq!(
                nilai(&tantangan).kategori(),
                Some(Kategori::BlokAntiBot),
                "403 + DOM tantangan"
            );
            assert_eq!(
                nilai(&dua_faktor).kategori(),
                Some(Kategori::ButuhManusia),
                "form OTP lebih berguna daripada label anti-bot"
            );
        }
    }

    #[test]
    fn kategori_selalu_membawa_bukti() {
        // AC-002: setiap kategori yang mungkin keluar wajib punya sebab, dan
        // sebab itu wajib menyebut angka atau teks, bukan hanya nama kategori.
        let kasus = [
            (
                Kategori::MesinMati,
                Kenyataan {
                    mesin_hidup: Some(false),
                    ..Kenyataan::default()
                },
            ),
            (
                Kategori::MesinPenuh,
                Kenyataan {
                    tab_terpakai: Some(6),
                    batas_tab: Some(6),
                    ..Kenyataan::default()
                },
            ),
            (
                Kategori::Jaringan,
                Kenyataan {
                    galat_jaringan: Some("net::ERR_NAME_NOT_RESOLVED".to_owned()),
                    url_akhir: Some("chrome-error://chromewebdata/".to_owned()),
                    ..Kenyataan::default()
                },
            ),
            (
                Kategori::ButuhManusia,
                Kenyataan {
                    otp_terlihat: true,
                    ..Kenyataan::default()
                },
            ),
            (
                Kategori::BlokAntiBot,
                Kenyataan {
                    status_dokumen: Some(429),
                    url_akhir: Some("https://www.google.com/sorry/index?continue=x".to_owned()),
                    ..Kenyataan::default()
                },
            ),
            (
                Kategori::RenderLambat,
                Kenyataan {
                    dokumen_selesai: Some(false),
                    detik: Some(11.0),
                    batas_detik: Some(10.0),
                    ..Kenyataan::default()
                },
            ),
        ];
        for (harapan, k) in kasus {
            let hasil = nilai(&k);
            let bukti = match hasil {
                Hasil::Gagal(bukti) => bukti,
                Hasil::Terbuka { .. } => panic!("{harapan} tidak terdeteksi dari {k:?}"),
            };
            assert_eq!(bukti.kategori, harapan);
            assert!(!bukti.sebab.is_empty(), "{harapan} tanpa sebab");
            assert!(
                bukti.status.is_some()
                    || bukti.url_akhir.is_some()
                    || bukti.judul.is_some()
                    || bukti.galat.is_some()
                    || bukti.tab.is_some()
                    || bukti.mesin.is_some()
                    || bukti.otp.is_some()
                    || bukti.detik.is_some(),
                "{harapan} kategori tanpa satu pun angka/teks pendukung: {:?}",
                bukti
            );
            assert!(bukti.ringkas().starts_with(harapan.as_str()));
        }
    }

    #[test]
    fn tanpa_kenyataan_tidak_menuduh() {
        let hasil = nilai(&Kenyataan::default());
        assert!(hasil.terbuka(), "tanpa bukti tidak boleh ada kategori");
        assert_eq!(hasil.kategori(), None);
    }

    #[test]
    fn status_403_sendirian_bukan_tantangan() {
        // Bobot 2 tidak melewati ambang, dan 403 dari server biasa bukan
        // tantangan. Halamannya tetap terbuka.
        let k = Kenyataan {
            status_dokumen: Some(403),
            url_akhir: Some("http://127.0.0.1:8793/terlarang.html".to_owned()),
            ..Kenyataan::default()
        };
        assert!(nilai(&k).terbuka());
    }

    #[test]
    fn google_429_sorry_terklasifikasi() {
        // Kasus nyata 4 Nov 2026: 200 di /search, lalu 429 di /sorry/index
        // dengan body 0 karakter.
        let k = Kenyataan {
            status_dokumen: Some(429),
            url_akhir: Some("https://www.google.com/sorry/index?continue=x".to_owned()),
            judul: Some(String::new()),
            dokumen_selesai: Some(true),
            detik: Some(25.1),
            batas_detik: Some(25.0),
            ..Kenyataan::default()
        };
        let hasil = nilai(&k);
        assert_eq!(
            hasil.kategori(),
            Some(Kategori::BlokAntiBot),
            "429 di halaman blokir lebih khusus daripada sekadar lambat"
        );
    }

    #[test]
    fn chrome_error_url_jadi_jaringan() {
        let k = Kenyataan {
            url_akhir: Some("chrome-error://chromewebdata/".to_owned()),
            judul: Some(String::new()),
            dokumen_selesai: Some(true),
            detik: Some(0.3),
            batas_detik: Some(10.0),
            ..Kenyataan::default()
        };
        assert_eq!(nilai(&k).kategori(), Some(Kategori::Jaringan));
    }

    #[test]
    fn tab_penuh_mengalahkan_lambat() {
        let k = Kenyataan {
            tab_terpakai: Some(9),
            batas_tab: Some(6),
            dokumen_selesai: Some(false),
            detik: Some(30.0),
            batas_detik: Some(10.0),
            ..Kenyataan::default()
        };
        assert_eq!(nilai(&k).kategori(), Some(Kategori::MesinPenuh));
    }

    #[test]
    fn mesin_mati_mengalahkan_semua() {
        let k = Kenyataan {
            mesin_hidup: Some(false),
            exit_code: Some(139),
            status_dokumen: Some(403),
            url_akhir: Some("chrome-error://chromewebdata/".to_owned()),
            dokumen_selesai: Some(false),
            ..Kenyataan::default()
        };
        let hasil = nilai(&k);
        assert_eq!(hasil.kategori(), Some(Kategori::MesinMati));
        match hasil {
            Hasil::Gagal(bukti) => {
                assert_eq!(bukti.exit_code, Some(139));
                assert!(bukti.sebab.iter().any(|s| s.contains("139")));
            }
            Hasil::Terbuka { .. } => panic!("harus gagal"),
        }
    }

    #[test]
    fn pagar_tab_nol_berarti_tanpa_pagar() {
        let k = Kenyataan {
            tab_terpakai: Some(99),
            batas_tab: Some(0),
            ..Kenyataan::default()
        };
        assert!(nilai(&k).terbuka());
    }

    #[test]
    fn halaman_404_tetap_terbuka() {
        // Keputusan sadar: klasifikasi ini soal kegagalan MEMBUKA, bukan mutu
        // isi. 404 dari server sehat berarti halaman tampil.
        let k = kenyataan(404);
        let hasil = nilai(&k);
        assert!(hasil.terbuka());
        match hasil {
            Hasil::Terbuka { status, .. } => assert_eq!(status, Some(404)),
            Hasil::Gagal(bukti) => panic!("404 bukan kegagalan buka: {}", bukti.ringkas()),
        }
    }

    #[test]
    fn pola_url_blokir_hanya_yang_dikenal() {
        assert!(url_blokir("https://www.google.com/sorry/index?continue=x"));
        assert!(url_blokir("https://a.test/cdn-cgi/challenge-platform/h/b"));
        assert!(!url_blokir("https://a.test/produk/123"));
        assert!(!url_blokir("https://a.test/sorry-not-sorry"));
    }
}
