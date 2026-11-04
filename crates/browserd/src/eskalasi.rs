//! Eskalasi berjenjang: mencoba sampai berhasil, berhenti di langkah pertama
//! yang membuahkan hasil, dan selalu menyisakan jejak yang bisa dibaca ulang.
//!
//! Rantai dari PRD FR-002: **tunggu jaringan tenang → ulang dengan backoff
//! (maksimal 2) → ganti mesin → handoff manusia.** Urutannya sengaja dari yang
//! paling murah: menunggu tidak memakai mesin kedua, dan menyalakan mesin
//! penuh untuk halaman yang cuma lambat itu pemborosan yang terukur.
//!
//! Dua batas yang dibedakan di sini, karena keduanya sering dicampur:
//!
//! - **maksimal 2 pengulangan** per kategori, jadi percobaan keempat tidak
//!   pernah dijalankan. Percobaan = pembukaan sungguhan, dan mengulang tanpa
//!   mengubah apa pun hanya memperbanyak permintaan ke server yang sudah
//!   menolak kita;
//! - **langkah yang tidak butuh percobaan** (menunggu, handoff) tidak dihitung
//!   sebagai pengulangan, karena tidak ada permintaan baru yang dikirim.
//!
//! Mesin ini tidak melakukan I/O dan tidak membaca jam: pemanggil yang
//! menjalankan langkahnya, tidur selama backoff, dan menandai hasilnya. Itu
//! yang membuat "maksimal 2 ulang" bisa diuji tanpa menunggu detik sungguhan.

use serde::Serialize;

use crate::klasifikasi::Kategori;

/// Jumlah pengulangan maksimal. Percobaan pertama tidak dihitung ulang.
pub const MAKS_ULANG: u32 = 2;

/// Mesin mana yang menggantikan mesin bawaan saat langkah "ganti mesin".
///
/// `None` berarti langkah itu dilewati: mengganti ke mesin yang tidak
/// terpasang adalah langkah yang tidak akan berhasil, dan mencatatnya di jejak
/// sebagai "sudah dicoba" itu kebohongan.
#[derive(Debug, Clone, Copy, Default)]
pub struct MesinLain {
    pub ada: bool,
    pub nama: &'static str,
}

/// Satu langkah eskalasi.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "langkah", rename_all = "snake_case")]
pub enum Langkah {
    /// Diam sampai jaringan tenang. Tidak mengirim permintaan baru.
    TungguJaringanTenang,
    /// Ulang percobaan setelah backoff.
    Ulang {
        /// Pengulangan ke berapa (1 atau 2).
        ke: u32,
        /// Backoff yang dijalani sebelum percobaan ini, detik.
        backoff_detik: u64,
    },
    /// Nyalakan/ganti mesin, lalu ulang di mesin itu.
    GantiMesin {
        /// Nama mesin tujuan.
        mesin: String,
    },
    /// Berhenti dan serahkan ke manusia.
    Handoff,
}

impl Langkah {
    /// Apakah langkah ini mengirim satu permintaan buka-web baru.
    #[must_use]
    pub const fn percobaan(&self) -> bool {
        matches!(self, Self::Ulang { .. } | Self::GantiMesin { .. })
    }

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TungguJaringanTenang => "tunggu_jaringan_tenang",
            Self::Ulang { .. } => "ulang",
            Self::GantiMesin { .. } => "ganti_mesin",
            Self::Handoff => "handoff",
        }
    }

    /// Kalimat alasan untuk log terstruktur. Yang dibaca orang saat kejadian
    /// adalah alasan, bukan nama langkah.
    #[must_use]
    pub fn alasan(&self, kategori: Kategori) -> String {
        match self {
            Self::TungguJaringanTenang => {
                format!("{kategori}: tunggu jaringan tenang sebelum mengulang")
            }
            Self::Ulang { ke, backoff_detik } => format!(
                "{kategori}: ulang ke-{ke} dari {MAKS_ULANG} setelah backoff {backoff_detik}s"
            ),
            Self::GantiMesin { mesin } => {
                format!("{kategori}: pindah ke mesin {mesin} lalu ulang")
            }
            Self::Handoff => format!("{kategori}: langkah mesin habis, serahkan ke manusia"),
        }
    }
}

/// Kenapa rantai berhenti.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Alasan {
    /// Ada langkah yang berhasil.
    Berhasil,
    /// Semua langkah habis dan tetap gagal.
    HabisLangkah,
    /// Kategori ini memang cuma bisa diselesaikan manusia.
    ButuhManusia,
}

/// Jejak satu rantai eskalasi, bentuk yang dicatat ke log dan artefak.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Jejak {
    pub kategori: Kategori,
    /// Percobaan buka-web sungguhan yang sudah dijalankan.
    pub percobaan: u32,
    pub langkah: Vec<Langkah>,
    pub berhenti: Alasan,
}

impl Jejak {
    /// Satu baris per langkah, untuk log terstruktur (AC-003).
    #[must_use]
    pub fn ringkas(&self) -> Vec<String> {
        let mut out = self
            .langkah
            .iter()
            .map(|langkah| langkah.alasan(self.kategori))
            .collect::<Vec<_>>();
        out.push(format!(
            "berhenti: {:?} setelah {} percobaan",
            self.berhenti, self.percobaan
        ));
        out
    }
}

/// Rantai eskalasi untuk satu kegagalan.
#[derive(Debug, Clone)]
pub struct Eskalasi {
    kategori: Kategori,
    sisa: Vec<Langkah>,
    dikerjakan: Vec<Langkah>,
    berhenti: Option<Alasan>,
    percobaan: u32,
}

impl Eskalasi {
    /// Rantai untuk kategori ini. Handoff selalu jadi langkah terakhir, tanpa
    /// terkecuali: rantai yang berakhir tanpa handoff akan menggantung.
    #[must_use]
    pub fn mulai(kategori: Kategori, mesin_lain: MesinLain) -> Self {
        let mut rantai = Vec::new();
        match kategori {
            // Pagar tab tidak sembuh dengan menunggu atau mengulang: tab-nya
            // harus ditutup dulu, dan itu keputusan klien. Halaman yang
            // menunggu OTP/2FA juga tidak perlu dikunjungi ulang sampai owner
            // selesai. Dua-duanya langsung ke handoff, yang ditambahkan di
            // ekor rantai.
            Kategori::MesinPenuh | Kategori::ButuhManusia => {}
            Kategori::MesinMati => {
                if mesin_lain.ada {
                    rantai.push(Langkah::GantiMesin {
                        mesin: mesin_lain.nama.to_owned(),
                    });
                }
                rantai.extend(ulangan());
            }
            Kategori::Jaringan | Kategori::BlokAntiBot | Kategori::RenderLambat => {
                rantai.push(Langkah::TungguJaringanTenang);
                rantai.extend(ulangan());
                if mesin_lain.ada {
                    rantai.push(Langkah::GantiMesin {
                        mesin: mesin_lain.nama.to_owned(),
                    });
                }
            }
        }
        rantai.push(Langkah::Handoff);
        Self {
            kategori,
            sisa: rantai,
            dikerjakan: Vec::new(),
            berhenti: None,
            percobaan: 0,
        }
    }

    /// Langkah berikutnya, atau `None` kalau rantai habis.
    ///
    /// Pemanggil **tidak boleh** menambah percobaan setelah `None`: itu yang
    /// membedakan "maksimal 2 ulang" dari "ulang sampai berhasil".
    pub fn langkah(&mut self) -> Option<Langkah> {
        let langkah = self.sisa.first().cloned()?;
        self.sisa.remove(0);
        self.dikerjakan.push(langkah.clone());
        if langkah.percobaan() {
            self.percobaan += 1;
        }
        if langkah == Langkah::Handoff {
            self.berhenti = Some(Alasan::HabisLangkah);
        }
        Some(langkah)
    }

    /// Langkah ini berhasil: berhenti di sini, langkah sisa tidak dijalankan.
    pub fn berhasil(&mut self) {
        self.berhenti = Some(Alasan::Berhasil);
        self.sisa.clear();
    }

    /// Kategori yang memang hanya bisa diselesaikan manusia. Dipertahankan
    /// terpisah dari [`Eskalasi::berhasil`] supaya laporan tidak menyebut
    /// "berhasil" untuk halaman yang masih menunggu OTP.
    pub fn butuh_manusia(&mut self) {
        self.berhenti = Some(Alasan::ButuhManusia);
        self.sisa.clear();
    }

    #[must_use]
    pub fn kategori(&self) -> Kategori {
        self.kategori
    }

    /// Jejak lengkap. Rantai yang belum selesai dilaporkan sebagai
    /// [`Alasan::HabisLangkah`] hanya kalau memang sudah habis; selama masih
    /// ada sisa, itu tetap laporan "belum berhenti" dan jujurnya begitu.
    #[must_use]
    pub fn jejak(&self) -> Jejak {
        Jejak {
            kategori: self.kategori,
            percobaan: self.percobaan,
            langkah: self.dikerjakan.clone(),
            berhenti: self.berhenti.unwrap_or(Alasan::HabisLangkah),
        }
    }
}

fn ulangan() -> Vec<Langkah> {
    (1..=MAKS_ULANG)
        .map(|ke| Langkah::Ulang {
            ke,
            // Backoff naik dua kali lipat. Angkanya kecil karena halaman yang
            // perlu diulang biasanya butuh satu putaran render tambahan, bukan
            // satu menit.
            backoff_detik: 2_u64.pow(ke),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesin() -> MesinLain {
        MesinLain {
            ada: true,
            nama: "chrome_full",
        }
    }

    #[test]
    fn urutan_rantai_jaringan_lengkap() {
        let rantai = Eskalasi::mulai(Kategori::Jaringan, mesin());
        let nama: Vec<&str> = rantai.sisa.iter().map(Langkah::as_str).collect();
        assert_eq!(
            nama,
            vec![
                "tunggu_jaringan_tenang",
                "ulang",
                "ulang",
                "ganti_mesin",
                "handoff"
            ]
        );
    }

    #[test]
    fn maksimal_dua_ulang_dan_handoff_selalu_terakhir() {
        // AC-004: percobaan keempat tidak pernah dijalankan.
        for kategori in Kategori::SEMUA {
            let r = Eskalasi::mulai(kategori, mesin());
            let ulang = r
                .sisa
                .iter()
                .filter(|l| matches!(l, Langkah::Ulang { .. }))
                .count();
            assert!(ulang <= MAKS_ULANG as usize, "{kategori}: {ulang} ulangan");
            assert_eq!(
                r.sisa.last(),
                Some(&Langkah::Handoff),
                "{kategori} tanpa handoff di ekor"
            );
            let percobaan = r.sisa.iter().filter(|l| l.percobaan()).count();
            assert!(
                percobaan <= 1 + MAKS_ULANG as usize,
                "{kategori}: {percobaan} percobaan"
            );
        }
    }

    #[test]
    fn mesin_penuh_dan_butuh_manusia_langsung_handoff() {
        for kategori in [Kategori::MesinPenuh, Kategori::ButuhManusia] {
            let r = Eskalasi::mulai(kategori, mesin());
            assert_eq!(r.sisa, vec![Langkah::Handoff], "{kategori}");
        }
    }

    #[test]
    fn mesin_mati_pertama_ganti_mesin_bukan_menunggu() {
        // Menunggu tidak menghidupkan proses yang sudah mati.
        let r = Eskalasi::mulai(Kategori::MesinMati, mesin());
        assert_eq!(r.sisa.first().map(Langkah::as_str), Some("ganti_mesin"));
    }

    #[test]
    fn langkah_yang_tidak_butuh_percobaan_tidak_dihitung() {
        let mut e = Eskalasi::mulai(Kategori::Jaringan, MesinLain::default());
        assert_eq!(e.langkah(), Some(Langkah::TungguJaringanTenang));
        assert_eq!(
            e.langkah(),
            Some(Langkah::Ulang {
                ke: 1,
                backoff_detik: 2
            })
        );
        assert_eq!(e.jejak().percobaan, 1, "menunggu bukan percobaan");
        assert_eq!(
            e.langkah(),
            Some(Langkah::Ulang {
                ke: 2,
                backoff_detik: 4
            })
        );
        assert_eq!(e.jejak().percobaan, 2);
        e.berhasil();
        assert_eq!(
            e.jejak().berhenti,
            Alasan::Berhasil,
            "berhenti di langkah yang berhasil"
        );
        assert_eq!(e.langkah(), None, "langkah sisa dibuang setelah berhasil");
    }

    #[test]
    fn percobaan_keempat_tidak_pernah_jalan() {
        // Server yang selalu gagal: hitung percobaan sungguhan sampai rantai
        // habis, termasuk langkah ganti mesin.
        let mut e = Eskalasi::mulai(Kategori::BlokAntiBot, mesin());
        let mut percobaan = 0;
        while let Some(langkah) = e.langkah() {
            if langkah.percobaan() {
                percobaan += 1;
            }
        }
        assert_eq!(
            percobaan,
            1 + MAKS_ULANG as usize,
            "harus 1 percobaan awal + {MAKS_ULANG} ulang"
        );
        assert_eq!(e.jejak().berhenti, Alasan::HabisLangkah);
    }

    #[test]
    fn jejak_menyebut_langkah_dan_alasan_berhenti() {
        // AC-003: log terstruktur memuat daftar langkah dan alasan berhenti.
        let mut e = Eskalasi::mulai(Kategori::BlokAntiBot, mesin());
        assert_eq!(e.langkah(), Some(Langkah::TungguJaringanTenang));
        assert_eq!(
            e.langkah(),
            Some(Langkah::Ulang {
                ke: 1,
                backoff_detik: 2
            })
        );
        e.berhasil();

        let jejak = e.jejak();
        let baris = jejak.ringkas();
        assert_eq!(baris.len(), 3);
        assert!(baris[0].contains("tunggu jaringan tenang"));
        assert!(baris[1].contains("backoff 2s"));
        assert!(baris[2].contains("Berhasil"));
        assert!(baris[2].contains("1 percobaan"), "{}", baris[2]);

        let json = serde_json::to_string(&jejak).expect("jejak harus bisa disimpan");
        assert!(json.contains("\"blok_anti_bot\""));
        assert!(json.contains("\"berhasil\""));
    }

    #[test]
    fn langkah_ganti_mesin_menyebut_nama_mesin() {
        let mut e = Eskalasi::mulai(Kategori::BlokAntiBot, mesin());
        let mut terakhir = None;
        while let Some(langkah) = e.langkah() {
            terakhir = Some(langkah);
        }
        assert_eq!(
            terakhir,
            Some(Langkah::Handoff),
            "handoff selalu langkah terakhir"
        );
        let jejak = e.jejak();
        assert!(
            jejak
                .langkah
                .iter()
                .any(|l| matches!(l, Langkah::GantiMesin { mesin } if mesin == "chrome_full")),
            "{:?}",
            jejak.langkah
        );
    }

    #[test]
    fn tanpa_mesin_lain_langkahnya_dilewati_bukan_dicatat() {
        let mut e = Eskalasi::mulai(Kategori::MesinMati, MesinLain::default());
        assert_eq!(
            e.langkah(),
            Some(Langkah::Ulang {
                ke: 1,
                backoff_detik: 2
            })
        );
        assert!(
            !e.jejak()
                .langkah
                .iter()
                .any(|l| matches!(l, Langkah::GantiMesin { .. })),
            "langkah yang tidak akan berhasil tidak boleh tercatat"
        );
    }

    #[test]
    fn butuh_manusia_bukan_berhasil() {
        let mut e = Eskalasi::mulai(Kategori::ButuhManusia, mesin());
        assert_eq!(e.langkah(), Some(Langkah::Handoff));
        e.butuh_manusia();
        assert_eq!(e.jejak().berhenti, Alasan::ButuhManusia);
    }
}
