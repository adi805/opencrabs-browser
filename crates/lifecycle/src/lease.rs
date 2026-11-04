//! Lease tab per sesi: janji bahwa tab yang sedang dipakai tidak dicabut.
//!
//! Sampai modul ini ada, satu-satunya yang menahan reap adalah handoff yang
//! menunggu jawaban owner. Akibatnya tab yang masih dipakai sesi lain **tetap
//! di-reap** begitu mesin menganggur `idle_reap_secs`, dan pekerjaan sesi itu
//! mati di tengah jalan tanpa satu pun galat yang menyebut kenapa. Lease
//! membalik keputusan itu: selama ada lease hidup, mesin tidak boleh dianggap
//! menganggur.
//!
//! Tiga hal yang disengaja di sini:
//!
//! - **Tanpa I/O dan tanpa jam dinding.** Waktu masuk sebagai parameter, jadi
//!   aturan kedaluwarsa bisa diuji tanpa `sleep`. Test yang tidur adalah test
//!   yang lambat dan kadang merah di mesin yang sibuk.
//! - **Kunci peta adalah target, bukan sesi.** Satu sesi boleh memegang lebih
//!   dari satu tab (FR-006), jadi sesi tidak bisa menjadi kunci. Target dipilih
//!   karena CDP sendiri mengidentifikasi tab dengan target id, dan dua pekerjaan
//!   tidak boleh memegang tab yang sama secara bersamaan.
//! - **Batas dihitung per sesi.** Batas per mesin sudah ada di
//!   [`crate::tabs`], tapi batas itu tidak mencegah satu sesi menghabiskan
//!   seluruh jatah mesin dan menutup tab sesi lain. Yang dihitung di sini hanya
//!   milik sesi yang meminta, dan penolakannya menyebut nama sesi itu.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

/// Satu tab yang dipegang sebuah sesi sampai waktu tertentu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    sesi: String,
    target: String,
    kedaluwarsa: u64,
}

impl Lease {
    #[must_use]
    pub fn sesi(&self) -> &str {
        &self.sesi
    }

    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    #[must_use]
    pub const fn kedaluwarsa(&self) -> u64 {
        self.kedaluwarsa
    }

    /// Hidup pada detik `sekarang`. Batasnya inklusif: lease yang kedaluwarsa
    /// tepat pada detik ini masih dihitung hidup, karena mematikannya satu detik
    /// terlalu cepat mematikan pekerjaan yang baru saja diperpanjang.
    #[must_use]
    pub const fn hidup(&self, sekarang: u64) -> bool {
        sekarang <= self.kedaluwarsa
    }
}

/// Kenapa sebuah permintaan lease ditolak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GalatLease {
    /// TTL nol tidak punya arti: lease yang langsung kedaluwarsa sama dengan
    /// tidak memegang apa pun, dan menerimanya diam-diam membuat pemanggil
    /// mengira tabnya dilindungi padahal tidak.
    TtlNol,
    /// Target sudah dipegang sesi lain yang lease-nya masih hidup.
    TargetDipakai { target: String, pemegang: String },
    /// Batas tab milik sesi ini sudah tercapai. Sesi lain tidak disebut karena
    /// tab mereka memang tidak tersentuh.
    BatasSesi {
        sesi: String,
        dipakai: usize,
        batas: usize,
    },
    /// Pelepasan diminta oleh sesi yang bukan pemegang lease.
    BukanPemegang {
        target: String,
        sesi: String,
        pemegang: String,
    },
}

impl fmt::Display for GalatLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TtlNol => write!(
                f,
                "lease dengan ttl 0 tidak memegang apa pun; pakai angka detik lebih dari 0"
            ),
            Self::TargetDipakai { target, pemegang } => write!(
                f,
                "tab '{target}' sedang dipegang sesi '{pemegang}'; satu tab tidak boleh dipakai dua \
                 pekerjaan yang berjalan bersamaan"
            ),
            Self::BatasSesi {
                sesi,
                dipakai,
                batas,
            } => write!(
                f,
                "sesi '{sesi}' sudah memegang {dipakai} dari batas {batas} tab; tutup satu tab sesi \
                 itu lebih dulu, tab sesi lain tidak ikut ditutup"
            ),
            Self::BukanPemegang {
                target,
                sesi,
                pemegang,
            } => write!(
                f,
                "sesi '{sesi}' tidak boleh melepas tab '{target}': lease-nya dipegang '{pemegang}'"
            ),
        }
    }
}

impl Error for GalatLease {}

/// Kumpulan lease hidup, di-key oleh target id.
#[derive(Debug, Clone, Default)]
pub struct Gudang {
    leases: BTreeMap<String, Lease>,
    batas_per_sesi: usize,
}

impl Gudang {
    /// `batas_per_sesi = 0` berarti tanpa batas per sesi, mengikuti arti nol
    /// yang sudah dipakai config ini (`idle_reap_secs = 0`, `max_tabs = 0`).
    #[must_use]
    pub fn baru(batas_per_sesi: usize) -> Self {
        Self {
            leases: BTreeMap::new(),
            batas_per_sesi,
        }
    }

    #[must_use]
    pub const fn batas_per_sesi(&self) -> usize {
        self.batas_per_sesi
    }

    /// Batas baru berlaku untuk permintaan berikutnya. Lease yang sudah dipegang
    /// **tidak** dicabut: menurunkan batas di tengah kerja tidak boleh mematikan
    /// tab yang sedang dipakai, dan itu sama dengan aturan `max_tabs` yang hanya
    /// menolak tab baru.
    pub fn ganti_batas(&mut self, batas_per_sesi: usize) {
        self.batas_per_sesi = batas_per_sesi;
    }

    /// Meminta satu tab untuk sesi ini.
    ///
    /// Lease kedaluwarsa disapu lebih dulu, karena tab yang sudah mati tidak
    /// boleh menahan permintaan baru atau membuat batas terlihat penuh.
    pub fn pegang(
        &mut self,
        sesi: &str,
        target: &str,
        ttl: u64,
        sekarang: u64,
    ) -> Result<Lease, GalatLease> {
        if ttl == 0 {
            return Err(GalatLease::TtlNol);
        }
        self.sapu(sekarang);

        if let Some(lama) = self.leases.get(target) {
            if lama.sesi == sesi {
                // Sesi yang sama memegang kembali tabnya: itu perpanjangan,
                // bukan pesaingan. Menolaknya akan memaksa klien melepas dulu
                // setiap kali memakai tab yang sama.
                let baru = Lease {
                    sesi: sesi.to_owned(),
                    target: target.to_owned(),
                    kedaluwarsa: sekarang.saturating_add(ttl),
                };
                self.leases.insert(target.to_owned(), baru.clone());
                return Ok(baru);
            }
            return Err(GalatLease::TargetDipakai {
                target: target.to_owned(),
                pemegang: lama.sesi.clone(),
            });
        }

        let dipakai = self.jumlah_sesi(sesi, sekarang);
        if self.batas_per_sesi > 0 && dipakai >= self.batas_per_sesi {
            return Err(GalatLease::BatasSesi {
                sesi: sesi.to_owned(),
                dipakai,
                batas: self.batas_per_sesi,
            });
        }

        let lease = Lease {
            sesi: sesi.to_owned(),
            target: target.to_owned(),
            kedaluwarsa: sekarang.saturating_add(ttl),
        };
        self.leases.insert(target.to_owned(), lease.clone());
        Ok(lease)
    }

    /// Melepas lease milik sesi pemanggil. Lease milik sesi lain tidak bisa
    /// dilepas dari sini: itulah bentuk konkret "menutup satu sesi tidak menutup
    /// tab sesi lain" (AC-012).
    pub fn lepas(&mut self, sesi: &str, target: &str) -> Result<Lease, GalatLease> {
        let Some(lease) = self.leases.get(target) else {
            return Err(GalatLease::BukanPemegang {
                target: target.to_owned(),
                sesi: sesi.to_owned(),
                pemegang: "tidak ada".to_owned(),
            });
        };
        if lease.sesi != sesi {
            return Err(GalatLease::BukanPemegang {
                target: target.to_owned(),
                sesi: sesi.to_owned(),
                pemegang: lease.sesi.clone(),
            });
        }
        self.leases
            .remove(target)
            .ok_or_else(|| GalatLease::BukanPemegang {
                target: target.to_owned(),
                sesi: sesi.to_owned(),
                pemegang: "tidak ada".to_owned(),
            })
    }

    /// Melepas semua lease milik satu sesi, mis. saat sesi ditutup.
    pub fn lepas_sesi(&mut self, sesi: &str) -> Vec<Lease> {
        let target: Vec<String> = self
            .leases
            .iter()
            .filter(|(_, lease)| lease.sesi == sesi)
            .map(|(target, _)| target.clone())
            .collect();
        target
            .into_iter()
            .filter_map(|target| self.leases.remove(&target))
            .collect()
    }

    /// Membuang lease yang sudah lewat waktunya dan mengembalikannya, supaya
    /// pemanggil tahu tab mana yang **benar-benar** perlu ditutup.
    pub fn sapu(&mut self, sekarang: u64) -> Vec<Lease> {
        let mati: Vec<String> = self
            .leases
            .iter()
            .filter(|(_, lease)| !lease.hidup(sekarang))
            .map(|(target, _)| target.clone())
            .collect();
        mati.into_iter()
            .filter_map(|target| self.leases.remove(&target))
            .collect()
    }

    /// Lease yang masih hidup, urut menurut target.
    #[must_use]
    pub fn ditahan(&self, sekarang: u64) -> Vec<&Lease> {
        self.leases
            .values()
            .filter(|lease| lease.hidup(sekarang))
            .collect()
    }

    /// Target yang masih dipegang. Dipakai pemutus reap: daftar kosong berarti
    /// tidak ada pekerjaan yang menahan mesin.
    #[must_use]
    pub fn target_ditahan(&self, sekarang: u64) -> Vec<String> {
        self.ditahan(sekarang)
            .into_iter()
            .map(|lease| lease.target.clone())
            .collect()
    }

    /// Berapa tab yang dipegang satu sesi pada detik ini.
    #[must_use]
    pub fn jumlah_sesi(&self, sesi: &str, sekarang: u64) -> usize {
        self.ditahan(sekarang)
            .into_iter()
            .filter(|lease| lease.sesi == sesi)
            .count()
    }

    /// Berapa lease hidup seluruhnya, dipakai metrik dan log.
    #[must_use]
    pub fn total(&self, sekarang: u64) -> usize {
        self.ditahan(sekarang).len()
    }

    /// Lease milik satu target, kalau ada dan masih hidup.
    #[must_use]
    pub fn lihat(&self, target: &str, sekarang: u64) -> Option<&Lease> {
        self.leases
            .get(target)
            .filter(|lease| lease.hidup(sekarang))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-009: lease hidup menahan pemiliknya, dan yang lewat waktu berhenti
    // menahan. Keduanya diuji tanpa tidur satu detik pun.
    #[test]
    fn lease_hidup_sampai_detik_kedaluwarsa() {
        let mut gudang = Gudang::baru(2);
        let lease = gudang.pegang("sesi-a", "T1", 60, 1_000).unwrap();
        assert_eq!(lease.kedaluwarsa(), 1_060);
        assert_eq!(gudang.target_ditahan(1_000), vec!["T1".to_owned()]);
        assert_eq!(gudang.target_ditahan(1_060), vec!["T1".to_owned()]);
        assert!(gudang.target_ditahan(1_061).is_empty());
    }

    // AC-012: dua sesi memegang tab masing-masing, dan menutup satu sesi tidak
    // menyentuh tab sesi lain.
    #[test]
    fn menutup_satu_sesi_tidak_menutup_tab_sesi_lain() {
        let mut gudang = Gudang::baru(2);
        gudang.pegang("sesi-a", "TA", 60, 100).unwrap();
        gudang.pegang("sesi-b", "TB", 60, 100).unwrap();

        let dilepas = gudang.lepas_sesi("sesi-a");
        assert_eq!(dilepas.len(), 1);
        assert_eq!(dilepas[0].target(), "TA");
        assert_eq!(gudang.target_ditahan(100), vec!["TB".to_owned()]);
    }

    // AC-013: batas per sesi menolak dengan nama sesi, dan jumlah tab sesi lain
    // tidak berubah satu pun.
    #[test]
    fn batas_per_sesi_menolak_dengan_nama_sesi_tanpa_menyentuh_sesi_lain() {
        let mut gudang = Gudang::baru(1);
        gudang.pegang("sesi-a", "TA", 60, 100).unwrap();
        gudang.pegang("sesi-b", "TB", 60, 100).unwrap();

        let galat = gudang.pegang("sesi-a", "TA2", 60, 100).unwrap_err();
        assert_eq!(
            galat,
            GalatLease::BatasSesi {
                sesi: "sesi-a".to_owned(),
                dipakai: 1,
                batas: 1
            }
        );
        assert!(galat.to_string().contains("sesi-a"));
        assert_eq!(gudang.jumlah_sesi("sesi-a", 100), 1);
        assert_eq!(gudang.jumlah_sesi("sesi-b", 100), 1, "sesi lain utuh");
    }

    #[test]
    fn target_yang_sedang_dipegang_sesi_lain_ditolak() {
        let mut gudang = Gudang::baru(0);
        gudang.pegang("sesi-a", "T1", 60, 100).unwrap();

        let galat = gudang.pegang("sesi-b", "T1", 60, 100).unwrap_err();
        assert_eq!(
            galat,
            GalatLease::TargetDipakai {
                target: "T1".to_owned(),
                pemegang: "sesi-a".to_owned()
            }
        );
    }

    #[test]
    fn sesi_yang_sama_memperpanjang_bukan_menggandakan() {
        let mut gudang = Gudang::baru(1);
        gudang.pegang("sesi-a", "T1", 60, 100).unwrap();
        let lanjut = gudang.pegang("sesi-a", "T1", 60, 150).unwrap();

        assert_eq!(lanjut.kedaluwarsa(), 210);
        assert_eq!(gudang.total(150), 1);
        assert_eq!(gudang.jumlah_sesi("sesi-a", 150), 1);
    }

    #[test]
    fn lepas_oleh_sesi_bukan_pemegang_ditolak_dan_lease_tetap_ada() {
        let mut gudang = Gudang::baru(0);
        gudang.pegang("sesi-a", "T1", 60, 100).unwrap();

        let galat = gudang.lepas("sesi-b", "T1").unwrap_err();
        assert!(matches!(galat, GalatLease::BukanPemegang { .. }));
        assert_eq!(gudang.target_ditahan(100), vec!["T1".to_owned()]);
    }

    #[test]
    fn sapu_mengembalikan_hanya_yang_sudah_lewat() {
        let mut gudang = Gudang::baru(0);
        gudang.pegang("sesi-a", "LAMA", 10, 100).unwrap();
        gudang.pegang("sesi-b", "BARU", 100, 100).unwrap();

        let dilepas = gudang.sapu(111);
        assert_eq!(dilepas.len(), 1);
        assert_eq!(dilepas[0].target(), "LAMA");
        assert_eq!(gudang.target_ditahan(111), vec!["BARU".to_owned()]);
    }

    #[test]
    fn lease_kedaluwarsa_tidak_menghabiskan_batas_sesi() {
        let mut gudang = Gudang::baru(1);
        gudang.pegang("sesi-a", "T1", 10, 100).unwrap();
        // Permintaan berikutnya datang sesudah lease lama mati: sesi yang sama
        // boleh memakai tab baru, karena tab lama sudah tidak dipegang siapa pun.
        let baru = gudang.pegang("sesi-a", "T2", 60, 200).unwrap();
        assert_eq!(baru.target(), "T2");
        assert_eq!(gudang.jumlah_sesi("sesi-a", 200), 1);
    }

    #[test]
    fn ttl_nol_ditolak_dengan_alasan() {
        let mut gudang = Gudang::baru(2);
        let galat = gudang.pegang("sesi-a", "T1", 0, 100).unwrap_err();
        assert_eq!(galat, GalatLease::TtlNol);
        assert!(galat.to_string().contains("ttl 0"));
    }

    #[test]
    fn menurunkan_batas_tidak_mencabut_lease_yang_sudah_dipegang() {
        let mut gudang = Gudang::baru(0);
        gudang.pegang("sesi-a", "T1", 60, 100).unwrap();
        gudang.pegang("sesi-a", "T2", 60, 100).unwrap();

        gudang.ganti_batas(1);
        assert_eq!(gudang.total(100), 2, "lease lama tidak dicabut");
        assert!(gudang.pegang("sesi-a", "T3", 60, 100).is_err());
    }

    #[test]
    fn nol_berarti_tanpa_batas_per_sesi() {
        let mut gudang = Gudang::baru(0);
        for i in 0..5 {
            gudang.pegang("sesi-a", &format!("T{i}"), 60, 100).unwrap();
        }
        assert_eq!(gudang.jumlah_sesi("sesi-a", 100), 5);
    }
}
