//! Batas jumlah tab per mesin.
//!
//! `max_tabs` di config adalah janji ke operator: "mesin ini tidak akan pernah
//! memakai lebih dari N tab". Sampai modul ini ada, janji itu cuma dibaca lalu
//! dilaporkan sebagai "belum ditegakkan", dan config yang menyebut angka tanpa
//! efek lebih berbahaya daripada config yang tidak menyebut apa pun: orang
//! mengira memori sudah dibatasi padahal tidak.
//!
//! Yang ada di sini hanya **keputusan**, dan sengaja tanpa I/O serta tanpa
//! parser JSON: jumlah tab datang dari pemanggil (`cdp` yang membaca
//! `/json/list` milik mesin). Pemisahan itu yang membuat aturannya bisa diuji
//! tanpa Chromium, dan membuat satu-satunya tempat angka batas dibandingkan
//! adalah fungsi di bawah ini.

#![forbid(unsafe_code)]

/// Apakah mesin masih boleh membuka tab baru.
///
/// `Penuh` membawa angka yang bisa dilaporkan apa adanya ke klien. Pesan galat
/// yang tidak menyebut berapa tab terbuka dan berapa batasnya memaksa orang
/// menebak, dan tebakan pertama biasanya "daemon-nya rusak".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keputusan {
    /// Tab baru boleh dibuka.
    Boleh,
    /// Batas sudah tercapai; tab baru harus ditolak.
    Penuh { terbuka: usize, batas: u32 },
}

impl Keputusan {
    #[must_use]
    pub const fn ditolak(self) -> bool {
        matches!(self, Self::Penuh { .. })
    }

    /// Kalimat penolakan untuk klien, atau `None` kalau tabnya boleh dibuka.
    ///
    /// Menyebut nama mesin karena daemon bisa punya lebih dari satu mesin dan
    /// batasnya bisa berbeda: "batas tab penuh" tanpa nama mesin membuat orang
    /// memeriksa mesin yang salah.
    #[must_use]
    pub fn pesan(self, mesin: &str) -> Option<String> {
        match self {
            Self::Boleh => None,
            Self::Penuh { terbuka, batas } => Some(format!(
                "mesin '{mesin}' sudah memakai {terbuka} dari batas {batas} tab (max_tabs = {batas}); \
                 tutup satu tab lebih dulu, atau naikkan max_tabs di config kalau batas ini memang \
                 yang salah"
            )),
        }
    }
}

/// Membandingkan jumlah tab terbuka dengan batas yang dikonfigurasi.
///
/// Dua hal yang disengaja:
///
/// - `None` dan `Some(0)` sama-sama berarti **tanpa batas**. Nol mengikuti arti
///   yang sudah dipakai di config ini (`idle_reap_secs = 0`, `port = 0`): angka
///   nol mematikan fiturnya, bukan membuat fiturnya mustahil dipakai.
/// - Batas dihitung `>=`, bukan `>`. Batas 6 berarti **paling banyak** 6 tab;
///   tab ketujuh yang ditolak, dan itulah yang dijanjikan angka itu.
#[must_use]
pub fn periksa(terbuka: usize, batas: Option<u32>) -> Keputusan {
    match batas {
        None | Some(0) => Keputusan::Boleh,
        Some(batas) => {
            let batas = batas as usize;
            if terbuka >= batas {
                Keputusan::Penuh {
                    terbuka,
                    batas: batas as u32,
                }
            } else {
                Keputusan::Boleh
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tanpa_batas_selalu_boleh() {
        assert_eq!(periksa(0, None), Keputusan::Boleh);
        assert_eq!(periksa(100, None), Keputusan::Boleh);
    }

    #[test]
    fn batas_nol_berarti_tanpa_batas() {
        // Nol disamakan dengan "tidak diatur", bukan "tidak boleh ada tab".
        // Kalau nol berarti larangan total, satu salah tulis di config akan
        // membuat daemon menolak setiap tab dan terlihat rusak.
        assert_eq!(periksa(3, Some(0)), Keputusan::Boleh);
    }

    #[test]
    fn batas_dihitung_setelah_tab_terakhir_yang_diizinkan() {
        assert_eq!(periksa(0, Some(6)), Keputusan::Boleh);
        assert_eq!(periksa(5, Some(6)), Keputusan::Boleh);
        assert_eq!(
            periksa(6, Some(6)),
            Keputusan::Penuh {
                terbuka: 6,
                batas: 6
            }
        );
        assert_eq!(
            periksa(9, Some(6)),
            Keputusan::Penuh {
                terbuka: 9,
                batas: 6
            }
        );
    }

    #[test]
    fn penolakan_menyebut_mesin_jumlah_dan_batas() {
        let pesan = periksa(2, Some(2))
            .pesan("chrome_full")
            .expect("penuh harus punya pesan");
        assert!(pesan.contains("chrome_full"), "pesan: {pesan}");
        assert!(pesan.contains("2 dari batas 2"), "pesan: {pesan}");
        assert!(pesan.contains("max_tabs"), "pesan: {pesan}");
        // Pesan harus menyebut jalan keluarnya, bukan cuma melarang.
        assert!(pesan.contains("tutup satu tab"), "pesan: {pesan}");
    }

    #[test]
    fn yang_boleh_tidak_punya_pesan() {
        assert!(periksa(1, Some(6)).pesan("chrome_shell").is_none());
        assert!(!periksa(1, Some(6)).ditolak());
        assert!(periksa(6, Some(6)).ditolak());
    }
}
