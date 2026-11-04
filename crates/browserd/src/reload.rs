//! Batas yang bisa berubah tanpa restart daemon.
//!
//! `max_tabs` dan `idle_reap_secs` dulu dibaca sekali saat boot lalu dibekukan
//! di dalam `ReaperConfig` dan peta batas pool. Akibatnya menaikkan batas untuk
//! pekerjaan yang sedang berjalan menuntut restart daemon, dan restart berarti
//! semua tab mati. Itu pertukaran yang tidak perlu: yang berubah hanya dua
//! angka, bukan topologi mesin.
//!
//! Aturan yang dijaga di sini (AC-011): **config rusak tidak boleh menurunkan
//! batas**. Parse gagal atau berkasnya tidak terbaca berarti nilai terakhir
//! dipertahankan dan galatnya dicatat. Jatuh ke "tanpa batas" akan membuat satu
//! salah tulis config menghapus pagar memori tanpa satu pun orang menyadarinya,
//! dan jatuh ke nol akan mematikan semua tab.
//!
//! Modul ini sengaja tidak menyentuh sinyal, jaringan, atau `EnginePool`:
//! pemanggil yang memutuskan kapan membaca berkas, dan yang menerapkan hasilnya
//! ke pool. Yang diuji di sini adalah keputusan "pakai yang baru atau
//! pertahankan yang lama", tanpa perlu mengirim SIGHUP ke proses mana pun.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

/// Angka yang boleh berubah saat daemon hidup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batas {
    /// Batas tab per mesin. Mesin tanpa entri berarti tanpa batas.
    pub max_tabs: BTreeMap<String, u32>,
    /// `None` berarti fitur reap dimatikan (`idle_reap_secs = 0`).
    pub idle_reap: Option<Duration>,
}

impl Batas {
    #[must_use]
    pub fn baru(max_tabs: BTreeMap<String, u32>, idle_reap: Option<Duration>) -> Self {
        Self {
            max_tabs,
            idle_reap,
        }
    }
}

/// Hasil satu kali penerapan: yang menang, dan angkanya.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hasil {
    /// Nilai baru dipakai.
    Diterapkan {
        mesin: usize,
        idle_reap: Option<Duration>,
    },
    /// Nilai lama dipertahankan, dengan alasan yang bisa dicatat.
    Dipertahankan { sebab: String },
}

/// Batas hidup yang dibaca loop reap dan diterapkan jalur pembuatan tab.
#[derive(Debug)]
pub struct Hidup {
    isi: Mutex<Batas>,
}

impl Hidup {
    #[must_use]
    pub fn baru(batas: Batas) -> Self {
        Self {
            isi: Mutex::new(batas),
        }
    }

    /// Ambang reap yang berlaku sekarang. `None` berarti reap dimatikan.
    #[must_use]
    pub fn idle_reap(&self) -> Option<Duration> {
        self.kunci().idle_reap
    }

    /// Peta batas tab yang berlaku sekarang, untuk diteruskan ke pool.
    #[must_use]
    pub fn max_tabs(&self) -> BTreeMap<String, u32> {
        self.kunci().max_tabs.clone()
    }

    /// Menerapkan hasil pembacaan config.
    ///
    /// `Err` tidak mengubah apa pun: itulah bentuk konkret "config rusak tidak
    /// menurunkan batas". Pemanggil tetap mendapat alasannya untuk dicatat,
    /// karena mempertahankan nilai lama tanpa sepatah kata membuat operator
    /// mengira berkasnya sudah terbaca.
    pub fn terapkan(&self, hasil: Result<Batas, String>) -> Hasil {
        match hasil {
            Ok(baru) => {
                let mesin = baru.max_tabs.len();
                let idle_reap = baru.idle_reap;
                *self.kunci() = baru;
                Hasil::Diterapkan { mesin, idle_reap }
            }
            Err(sebab) => Hasil::Dipertahankan { sebab },
        }
    }

    /// Membaca tanpa memegang lock lebih lama dari satu panggilan.
    fn kunci(&self) -> std::sync::MutexGuard<'_, Batas> {
        self.isi.lock().unwrap_or_else(|err| err.into_inner())
    }
}

/// Peta batas tab yang ditegakkan, dari pasangan `(id mesin, max_tabs)`.
///
/// Satu implementasi untuk dua pemanggil (boot dari `Resolved`, reload dari
/// `FileConfig`) supaya aturan arti nol tidak bisa berbeda di dua jalur: dua
/// salinan akan menyimpang dan salah satunya akan menegakkan nol.
///
/// Mesin tanpa `max_tabs` dan mesin dengan `max_tabs = 0` tidak masuk peta:
/// keduanya berarti tanpa batas, dan mesin tanpa entri tidak membayar satu
/// pemeriksaan pun di jalur pembuatan tab.
#[must_use]
pub fn batas_tab<'a>(
    mesin: impl IntoIterator<Item = (&'a str, Option<u32>)>,
) -> BTreeMap<String, u32> {
    mesin
        .into_iter()
        .filter_map(|(id, batas)| {
            let batas = batas?;
            (batas > 0).then(|| (id.to_owned(), batas))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peta(entries: &[(&str, u32)]) -> BTreeMap<String, u32> {
        entries
            .iter()
            .map(|(id, batas)| ((*id).to_owned(), *batas))
            .collect()
    }

    fn hidup() -> Hidup {
        Hidup::baru(Batas::baru(
            peta(&[("kiri", 6)]),
            Some(Duration::from_secs(60)),
        ))
    }

    // AC-010: angka baru berlaku seketika, tanpa restart.
    #[test]
    fn angka_baru_langsung_berlaku() {
        let hidup = hidup();
        let hasil = hidup.terapkan(Ok(Batas::baru(
            peta(&[("kiri", 3), ("kanan", 4)]),
            Some(Duration::from_secs(120)),
        )));

        assert_eq!(
            hasil,
            Hasil::Diterapkan {
                mesin: 2,
                idle_reap: Some(Duration::from_secs(120))
            }
        );
        assert_eq!(hidup.max_tabs(), peta(&[("kiri", 3), ("kanan", 4)]));
        assert_eq!(hidup.idle_reap(), Some(Duration::from_secs(120)));
    }

    // AC-011: config rusak mempertahankan nilai terakhir, bukan jatuh ke tanpa
    // batas dan bukan jatuh ke nol.
    #[test]
    fn config_rusak_mempertahankan_batas_lama() {
        let hidup = hidup();
        let hasil = hidup.terapkan(Err("mem-parse config: expected `=`".to_owned()));

        assert_eq!(
            hasil,
            Hasil::Dipertahankan {
                sebab: "mem-parse config: expected `=`".to_owned()
            }
        );
        assert_eq!(hidup.max_tabs(), peta(&[("kiri", 6)]));
        assert_eq!(hidup.idle_reap(), Some(Duration::from_secs(60)));
    }

    #[test]
    fn reap_bisa_dimatikan_dan_dinyalakan_lagi_tanpa_restart() {
        let hidup = hidup();
        hidup.terapkan(Ok(Batas::baru(peta(&[("kiri", 6)]), None)));
        assert_eq!(hidup.idle_reap(), None, "0 harus mematikan reap");

        hidup.terapkan(Ok(Batas::baru(
            peta(&[("kiri", 6)]),
            Some(Duration::from_secs(30)),
        )));
        assert_eq!(hidup.idle_reap(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn batas_nol_dan_kosong_tidak_masuk_peta() {
        let peta = batas_tab([("kiri", Some(0)), ("kanan", None), ("tengah", Some(2))]);
        assert_eq!(peta, self::peta(&[("tengah", 2)]));
    }
}
