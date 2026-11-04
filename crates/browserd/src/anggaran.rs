//! Supervisor anggaran memori per mesin.
//!
//! Satu mesin yang menembus anggaran **tidak boleh** mematikan daemon dan tidak
//! boleh menolak permintaan ke mesin lain (AC-017). Karena itu pemulihan
//! dilakukan per mesin: hentikan proses group mesin itu, nyalakan lagi dari
//! resep yang sama, dan biarkan mesin lain terus melayani selama proses itu
//! berjalan.
//!
//! Dua hal yang sengaja dipisah dari I/O:
//!
//! * **Pengukuran** lewat trait [`Pengukur`]. Membaca PSS dari `/proc` tidak
//!   bisa diuji tanpa proses nyata, dan mesin palsu yang melaporkan PSS besar
//!   adalah satu-satunya cara menguji jalur "menembus anggaran" tanpa
//!   menunggu Chromium benar-benar memakan gigabyte.
//! * **Jam** disuntik sebagai [`Instant`]. Uji batas 30 detik tidak boleh
//!   berarti uji yang tidur 30 detik.
//!
//! Kegagalan pengukuran **tidak** memicu pemulihan (gagal-terbuka, NFR-004):
//! `/proc` yang tidak terbaca sesaat adalah alasan yang buruk untuk mematikan
//! mesin yang sedang dipakai orang, dan mematikan mesin justru menjatuhkan
//! pekerjaan yang sedang berjalan.

#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use lifecycle::budget::{Action, Budget, BudgetConfig};

/// Batas waktu pemulihan yang dijanjikan ke operator (AC-016).
///
/// Angka ini dipakai untuk MEMUTUSKAN, bukan untuk dirapikan di laporan: kalau
/// pemulihan melewatinya, itu dilaporkan sebagai pelanggaran janji, bukan
/// sebagai pemulihan yang berhasil.
pub const PEMULIHAN_MAKS: Duration = Duration::from_secs(30);

/// Sumber angka PSS. Implementasi nyata membaca `/proc`; uji menyuntik angka.
pub trait Pengukur: Send + Sync {
    /// PSS satu process group, dalam byte.
    fn pss(&self, pgid: u32) -> Result<u64, String>;
}

/// Pengukur nyata: PSS process group dari `/proc`.
///
/// `pgid` sama dengan pid karena mesin dinyalakan sebagai pemimpin process
/// group-nya sendiri (`setsid`), jadi process group mesin adalah tempat semua
/// proses turunannya berkumpul. Membaca RSS proses utama saja akan melaporkan
/// angka jauh di bawah pemakaian sebenarnya: Chromium menaruh renderer dan GPU
/// di proses terpisah.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcPengukur;

impl Pengukur for ProcPengukur {
    fn pss(&self, pgid: u32) -> Result<u64, String> {
        lifecycle::budget::read_process_group(pgid)
            .map(|group| group.pss_bytes)
            .map_err(|err| err.to_string())
    }
}

/// Pemulihan satu mesin yang sedang berjalan.
#[derive(Debug, Clone, Copy, Default)]
pub struct Pemulihan {
    mulai: Option<Instant>,
}

impl Pemulihan {
    #[must_use]
    pub const fn baru() -> Self {
        Self { mulai: None }
    }

    /// Mesin mulai dihentikan. Dipanggil sekali, bukan tiap putaran: mengulang
    /// di sini akan membuat jam pemulihan di-reset dan janji 30 detik tidak
    /// pernah bisa dilanggar secara terukur.
    pub fn mulai(&mut self, sekarang: Instant) {
        if self.mulai.is_none() {
            self.mulai = Some(sekarang);
        }
    }

    #[must_use]
    pub const fn berjalan(&self) -> bool {
        self.mulai.is_some()
    }

    /// Mesin sudah hidup lagi: berapa lama pemulihannya.
    pub fn selesai(&mut self, sekarang: Instant) -> Option<Duration> {
        self.mulai
            .take()
            .map(|mulai| sekarang.saturating_duration_since(mulai))
    }
}

/// Putusan satu putaran pemeriksaan untuk satu mesin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Putusan {
    /// Masih di bawah anggaran. `pss` dilaporkan apa adanya.
    Diam { pss: u64 },
    /// Menembus anggaran: hentikan dan nyalakan ulang mesin ini.
    Pulihkan { pss: u64, sebab: Action },
    /// Angka tidak terbaca. Dipilih gagal-terbuka: jangan pulihkan.
    TakTerukur { sebab: String },
}

/// Akuntan anggaran satu mesin, plus jam pemulihannya.
#[derive(Debug)]
pub struct Penjaga {
    budget: Budget,
    pulih: Pemulihan,
    pulih_selesai: u64,
    terlambat: u64,
}

impl Penjaga {
    #[must_use]
    pub fn baru(cfg: BudgetConfig) -> Self {
        Self {
            budget: Budget::new(cfg),
            pulih: Pemulihan::baru(),
            pulih_selesai: 0,
            terlambat: 0,
        }
    }

    /// Satu putaran pemeriksaan.
    ///
    /// Selama pemulihan sebelumnya belum selesai, putaran ini **tidak**
    /// memutuskan apa pun: mesin yang sedang dinyalakan ulang akan terlihat
    /// menembus anggaran lagi (proses lama belum hilang, proses baru sudah
    /// lahir) dan keputusan tambahan hanya akan membuat dua pemulihan
    /// bertumpuk pada satu mesin.
    pub fn putaran(&mut self, pengukur: &dyn Pengukur, pgid: u32) -> Putusan {
        if self.pulih.berjalan() {
            return Putusan::Diam { pss: 0 };
        }
        match pengukur.pss(pgid) {
            Ok(pss) => match self.budget.observe(pss) {
                Some(sebab) => Putusan::Pulihkan { pss, sebab },
                None => Putusan::Diam { pss },
            },
            Err(sebab) => Putusan::TakTerukur { sebab },
        }
    }

    /// Mesin mulai dihentikan untuk dipulihkan.
    pub fn mulai_pulih(&mut self, sekarang: Instant) {
        self.pulih.mulai(sekarang);
    }

    /// Mesin sudah hidup lagi.
    ///
    /// Mengembalikan lama pemulihan, dan mencatat pelanggaran kalau melewati
    /// [`PEMULIHAN_MAKS`]. Ratchet anggaran dibersihkan karena ini generasi
    /// mesin yang baru: `fired` sengaja tidak, angka itu justru yang dilaporkan.
    pub fn selesai_pulih(&mut self, sekarang: Instant) -> Option<Duration> {
        let lama = self.pulih.selesai(sekarang)?;
        if lama > PEMULIHAN_MAKS {
            self.terlambat = self.terlambat.saturating_add(1);
        } else {
            self.pulih_selesai = self.pulih_selesai.saturating_add(1);
        }
        self.budget.on_engine_stopped();
        Some(lama)
    }

    /// Berapa pemulihan yang selesai dalam batas waktu janji.
    #[must_use]
    pub const fn pulih_dalam_batas(&self) -> u64 {
        self.pulih_selesai
    }

    /// Berapa pemulihan yang melewati janji 30 detik.
    #[must_use]
    pub const fn pulih_terlambat(&self) -> u64 {
        self.terlambat
    }

    #[must_use]
    pub const fn budget(&self) -> &Budget {
        &self.budget
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use lifecycle::budget::MB;

    /// Pengukur palsu: angka tetap, atau galat, tanpa `/proc` dan tanpa proses.
    struct Tetap {
        hasil: Mutex<Result<u64, String>>,
    }

    impl Tetap {
        fn pss(byte: u64) -> Self {
            Self {
                hasil: Mutex::new(Ok(byte)),
            }
        }

        fn galat(sebab: &str) -> Self {
            Self {
                hasil: Mutex::new(Err(sebab.to_owned())),
            }
        }

        fn ubah(&self, hasil: Result<u64, String>) {
            *self.hasil.lock().expect("kunci") = hasil;
        }
    }

    impl Pengukur for Tetap {
        fn pss(&self, _pgid: u32) -> Result<u64, String> {
            self.hasil.lock().expect("kunci").clone()
        }
    }

    fn cfg() -> BudgetConfig {
        BudgetConfig::from_mb(200, 400, 700, 1000).expect("anggaran sah")
    }

    fn penjaga() -> Penjaga {
        Penjaga::baru(cfg())
    }

    /// Pengukur per pid: tiap mesin bisa diberi angka sendiri.
    struct PerPid(std::collections::BTreeMap<u32, u64>);

    impl Pengukur for PerPid {
        fn pss(&self, pgid: u32) -> Result<u64, String> {
            self.0
                .get(&pgid)
                .copied()
                .ok_or_else(|| format!("pid {pgid} tidak dikenal"))
        }
    }

    // AC-017: dua mesin, satu menembus anggaran. Mesin yang lain tidak ikut
    // diputuskan, dan pemulihan mesin pertama tidak menyentuh angka mesin kedua.
    #[test]
    fn mesin_lain_tidak_ikut_terdampak() {
        let pengukur = PerPid([(1001, 900 * MB), (1002, 100 * MB)].into_iter().collect());
        let mut besar = penjaga();
        let mut kecil = penjaga();

        assert!(
            matches!(besar.putaran(&pengukur, 1001), Putusan::Pulihkan { .. }),
            "mesin 1001 menembus hard cap"
        );
        assert_eq!(
            kecil.putaran(&pengukur, 1002),
            Putusan::Diam { pss: 100 * MB },
            "mesin 1002 tetap di bawah soft"
        );

        // Pemulihan mesin 1001 berjalan penuh, dan mesin 1002 tidak melihatnya.
        let mulai = Instant::now();
        besar.mulai_pulih(mulai);
        besar.selesai_pulih(mulai + Duration::from_secs(3));

        assert_eq!(besar.pulih_dalam_batas(), 1);
        assert_eq!(kecil.pulih_dalam_batas(), 0);
        assert_eq!(kecil.pulih_terlambat(), 0);
        assert_eq!(
            kecil.budget().last_bytes(),
            100 * MB,
            "angka mesin lain tidak boleh tertimpa"
        );
    }

    // AC-016: mesin yang menembus hard cap diputuskan untuk dipulihkan, dan
    // keputusan itu berasal dari ANGKA PSS, bukan dari tebakan.
    #[test]
    fn pss_di_atas_hard_memutuskan_pulihkan() {
        let penjaga_palsu = Tetap::pss(800 * MB);
        let mut penjaga = penjaga();
        match penjaga.putaran(&penjaga_palsu, 4242) {
            Putusan::Pulihkan { pss, sebab } => {
                assert_eq!(pss, 800 * MB);
                assert_eq!(sebab, Action::DrainAndReap);
            }
            lain => panic!("harus memutuskan pulihkan, dapat {lain:?}"),
        }
    }

    #[test]
    fn pss_di_bawah_soft_tidak_menyentuh_mesin() {
        let penjaga_palsu = Tetap::pss(120 * MB);
        let mut penjaga = penjaga();
        assert_eq!(
            penjaga.putaran(&penjaga_palsu, 1),
            Putusan::Diam { pss: 120 * MB }
        );
    }

    // NFR-004 / AC-020: pengukuran gagal = gagal-terbuka. Mesin yang sedang
    // dipakai tidak boleh dimatikan hanya karena `/proc` tidak terbaca.
    #[test]
    fn pengukuran_gagal_tidak_memicu_pemulihan() {
        let penjaga_palsu = Tetap::galat("/proc/4242/smaps_rollup tidak ada");
        let mut penjaga = penjaga();
        match penjaga.putaran(&penjaga_palsu, 4242) {
            Putusan::TakTerukur { sebab } => {
                assert!(sebab.contains("smaps_rollup"), "sebab harus asli: {sebab}");
            }
            lain => panic!("harus tak terukur, dapat {lain:?}"),
        }
    }

    // AC-016: pemulihan yang selesai 12 detik dicatat sebagai patuh janji.
    #[test]
    fn pemulihan_dalam_batas_dihitung_patuh() {
        let mut penjaga = penjaga();
        let mulai = Instant::now();
        penjaga.mulai_pulih(mulai);
        let selesai = mulai.checked_add(Duration::from_secs(12)).expect("waktu");

        assert_eq!(
            penjaga.selesai_pulih(selesai),
            Some(Duration::from_secs(12))
        );
        assert_eq!(penjaga.pulih_dalam_batas(), 1);
        assert_eq!(penjaga.pulih_terlambat(), 0);
    }

    // AC-016 batas: 31 detik BUKAN "berhasil". Angka itu dilaporkan sebagai
    // pelanggaran, bukan dibulatkan jadi pemulihan yang memenuhi janji.
    #[test]
    fn pemulihan_lewat_30_detik_dihitung_terlambat() {
        let mut penjaga = penjaga();
        let mulai = Instant::now();
        penjaga.mulai_pulih(mulai);
        let selesai = mulai.checked_add(Duration::from_secs(31)).expect("waktu");

        assert_eq!(
            penjaga.selesai_pulih(selesai),
            Some(Duration::from_secs(31))
        );
        assert_eq!(penjaga.pulih_dalam_batas(), 0);
        assert_eq!(penjaga.pulih_terlambat(), 1);
    }

    // Selama pemulihan berjalan, putaran berikutnya tidak boleh memutuskan
    // pulihkan lagi: dua pemulihan bertumpuk pada satu mesin akan saling
    // mematikan proses yang baru lahir.
    #[test]
    fn putaran_selama_pemulihan_tidak_menambah_pemulihan() {
        let pengukur = Tetap::pss(900 * MB);
        let mut penjaga = penjaga();
        penjaga.mulai_pulih(Instant::now());

        assert_eq!(
            penjaga.putaran(&pengukur, 1),
            Putusan::Diam { pss: 0 },
            "selama pemulihan, putaran harus diam walau PSS masih di atas hard"
        );
    }

    // Sesudah pemulihan selesai, ratchet dibersihkan: mesin generasi baru
    // berhak dinilai dari angkanya sendiri, bukan dari puncak generasi lama.
    #[test]
    fn ratchet_dibersihkan_setelah_pemulihan_selesai() {
        let pengukur = Tetap::pss(800 * MB);
        let mut penjaga = penjaga();
        assert!(matches!(
            penjaga.putaran(&pengukur, 1),
            Putusan::Pulihkan { .. }
        ));
        assert_eq!(penjaga.budget().peak_bytes(), 800 * MB);

        let mulai = Instant::now();
        penjaga.mulai_pulih(mulai);
        penjaga.selesai_pulih(mulai + Duration::from_secs(5));

        assert_eq!(penjaga.budget().peak_bytes(), 0, "generasi baru");
        assert_eq!(
            penjaga.budget().armed(),
            lifecycle::budget::Level::Ok,
            "kunci tingkat dibuka lagi untuk generasi baru"
        );

        pengukur.ubah(Ok(100 * MB));
        assert_eq!(
            penjaga.putaran(&pengukur, 1),
            Putusan::Diam { pss: 100 * MB }
        );
    }
}
