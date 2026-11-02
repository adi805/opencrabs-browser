//! Akuntansi memori dan ratchet anggaran.
//!
//! Satu-satunya angka yang boleh dipakai untuk keputusan adalah **PSS**, bukan
//! RSS. Alasannya terukur, bukan selera: 3 tab memberi 858.8 MB RSS pada mode
//! multi-proses tapi 522.8 MB pada single-process, sementara PSS-nya 356.0 vs
//! 355.8 MB. Memakai RSS berarti menghukum isolasi proses yang justru benar.
//!
//! Dua lapisan sengaja dipisah:
//!
//! 1. **Fungsi murni** ([`parse_smaps_rollup`], [`Budget::observe`]) yang bisa
//!    diuji tanpa `/proc` dan tanpa Chromium.
//! 2. **Pembaca** ([`read_process_group`]) yang menyentuh `/proc` dan boleh
//!    gagal I/O.
//!
//! Ratchet bekerja begini: satu tingkat hanya ditembak sekali sampai memori
//! benar-benar turun di bawah ambang re-arm. Tanpa ini, PSS yang berayun di
//! sekitar 600 MB akan memicu eviction setiap poll, dan "menghemat memori"
//! berubah jadi "menutup tab orang terus-menerus".

use std::error::Error;
use std::fmt;
use std::io;
use std::path::Path;

/// Tingkat tekanan anggaran. Urutannya penting: hanya boleh naik dalam satu
/// generasi engine, turunnya lewat re-arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Soft,
    High,
    Hard,
}

impl Level {
    /// Aksi yang diperintahkan tingkat ini, sesuai `04-OPERASIONAL-ROADMAP` §3.3.
    #[must_use]
    pub const fn action(self) -> Option<Action> {
        match self {
            Self::Ok => None,
            Self::Soft => Some(Action::EvictIdleExpired),
            Self::High => Some(Action::CloseOldestNonSticky),
            Self::Hard => Some(Action::DrainAndReap),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Soft => "soft",
            Self::High => "high",
            Self::Hard => "hard",
        }
    }

    const fn index(self) -> Option<usize> {
        match self {
            Self::Ok => None,
            Self::Soft => Some(0),
            Self::High => Some(1),
            Self::Hard => Some(2),
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Tindakan yang harus dijalankan pemanggil. Crate ini tidak menutup tab
/// sendiri: dia cuma bilang apa yang harus terjadi dan pada tingkat apa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Tutup sesi idle yang umurnya melewati `tab_ttl_secs`, paling lama dulu.
    EvictIdleExpired,
    /// Tutup tab tertua yang tidak sticky, tanpa menunggu TTL.
    CloseOldestNonSticky,
    /// Drain semua sesi, reap engine, backoff.
    DrainAndReap,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nama = match self {
            Self::EvictIdleExpired => "evict_idle_expired",
            Self::CloseOldestNonSticky => "close_oldest_non_sticky",
            Self::DrainAndReap => "drain_and_reap",
        };
        f.write_str(nama)
    }
}

/// Kesalahan konfigurasi atau pembacaan `/proc`.
#[derive(Debug)]
pub enum BudgetError {
    /// Urutan ambang terbalik atau bernilai nol.
    Invalid {
        soft_mb: u64,
        high_mb: u64,
        hard_mb: u64,
    },

    /// `/proc` tidak bisa dibaca.
    Io { path: String, source: io::Error },
}

impl fmt::Display for BudgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid {
                soft_mb,
                high_mb,
                hard_mb,
            } => write!(
                f,
                "anggaran tidak masuk akal: soft {soft_mb} MB, high {high_mb} MB, \
                 hard {hard_mb} MB (harus 0 < soft < high < hard)"
            ),
            Self::Io { path, source } => write!(f, "membaca {path}: {source}"),
        }
    }
}

impl Error for BudgetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Invalid { .. } => None,
        }
    }
}

/// Ambang anggaran dalam byte, plus titik re-arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetConfig {
    pub soft_bytes: u64,
    pub high_bytes: u64,
    pub hard_bytes: u64,
    /// Re-arm terjadi saat PSS turun ke `soft_bytes * rearm_permille / 1000`.
    /// Bawaannya 900, jadi butuh turun 10% di bawah soft sebelum eviction
    /// berikutnya boleh dipicu.
    pub rearm_permille: u16,
    pub poll_ms: u64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            soft_bytes: 600 * MB,
            high_bytes: 800 * MB,
            hard_bytes: 950 * MB,
            rearm_permille: 900,
            poll_ms: 5000,
        }
    }
}

/// Satu megabyte desimal, sama dengan yang dipakai konfigurasi.
pub const MB: u64 = 1_000_000;

impl BudgetConfig {
    /// Bangun dari angka megabyte di konfigurasi. Urutan yang salah ditolak,
    /// bukan dibetulkan diam-diam: ambang terbalik akan membuat `hard` tidak
    /// pernah tercapai dan engine tidak pernah direap.
    pub fn from_mb(
        soft_mb: u64,
        high_mb: u64,
        hard_mb: u64,
        poll_ms: u64,
    ) -> Result<Self, BudgetError> {
        if soft_mb == 0 || soft_mb >= high_mb || high_mb >= hard_mb {
            return Err(BudgetError::Invalid {
                soft_mb,
                high_mb,
                hard_mb,
            });
        }
        Ok(Self {
            soft_bytes: soft_mb * MB,
            high_bytes: high_mb * MB,
            hard_bytes: hard_mb * MB,
            rearm_permille: 900,
            poll_ms,
        })
    }

    #[must_use]
    pub const fn level_for(&self, pss_bytes: u64) -> Level {
        if pss_bytes >= self.hard_bytes {
            Level::Hard
        } else if pss_bytes >= self.high_bytes {
            Level::High
        } else if pss_bytes >= self.soft_bytes {
            Level::Soft
        } else {
            Level::Ok
        }
    }

    /// Ambang turun yang membuka kunci tingkat yang sudah ditembak.
    #[must_use]
    pub const fn rearm_bytes(&self) -> u64 {
        self.soft_bytes * self.rearm_permille as u64 / 1000
    }
}

/// Akuntan anggaran untuk satu engine.
///
/// Tidak menyimpan pid: pid berubah tiap boot, sedangkan ratchet harus bertahan
/// sepanjang satu generasi engine.
#[derive(Debug, Clone)]
pub struct Budget {
    cfg: BudgetConfig,
    last_bytes: u64,
    peak_bytes: u64,
    armed: Level,
    fired: [u64; 3],
}

impl Budget {
    #[must_use]
    pub const fn new(cfg: BudgetConfig) -> Self {
        Self {
            cfg,
            last_bytes: 0,
            peak_bytes: 0,
            armed: Level::Ok,
            fired: [0, 0, 0],
        }
    }

    #[must_use]
    pub const fn config(&self) -> BudgetConfig {
        self.cfg
    }

    /// Pembacaan terakhir, apa adanya.
    #[must_use]
    pub const fn last_bytes(&self) -> u64 {
        self.last_bytes
    }

    /// Puncak sejak generasi engine ini dimulai. Hanya naik (ratchet).
    #[must_use]
    pub const fn peak_bytes(&self) -> u64 {
        self.peak_bytes
    }

    /// Tingkat tertinggi yang sudah ditembak pada generasi ini.
    #[must_use]
    pub const fn armed(&self) -> Level {
        self.armed
    }

    /// Berapa kali tiap tingkat ditembak pada generasi ini.
    #[must_use]
    pub const fn fired(&self, level: Level) -> u64 {
        match level.index() {
            Some(i) => self.fired[i],
            None => 0,
        }
    }

    /// Catat satu pembacaan dan putuskan perlu tindakan atau tidak.
    ///
    /// `None` berarti tidak ada yang perlu dilakukan: entah masih di bawah
    /// soft, atau tingkat itu sudah ditembak dan memorinya belum turun cukup
    /// jauh untuk membuka kunci.
    pub fn observe(&mut self, pss_bytes: u64) -> Option<Action> {
        self.last_bytes = pss_bytes;
        self.peak_bytes = self.peak_bytes.max(pss_bytes);

        let level = self.cfg.level_for(pss_bytes);
        if level == Level::Ok {
            if pss_bytes <= self.cfg.rearm_bytes() {
                self.armed = Level::Ok;
            }
            return None;
        }

        if level <= self.armed {
            return None;
        }

        self.armed = level;
        if let Some(i) = level.index() {
            self.fired[i] = self.fired[i].saturating_add(1);
        }
        level.action()
    }

    /// Engine sudah mati dan diganti: generasi baru, ratchet bersih.
    ///
    /// Penghitung `fired` sengaja **tidak** direset: angka itu yang dilaporkan
    /// ke `browser_budget_action_total` dan harus bertahan lebih lama daripada
    /// satu engine.
    pub fn on_engine_stopped(&mut self) {
        self.last_bytes = 0;
        self.peak_bytes = 0;
        self.armed = Level::Ok;
    }

    /// Pagar terakhir: dipanggil **setelah** tindakan terakhir dijalankan,
    /// dengan pembacaan group yang terbaru.
    ///
    /// Kalau masih ada PSS di atas hard cap, tidak ada tab lagi yang bisa
    /// ditutup untuk memperbaikinya. Mengembalikan `Ok` di sini berarti daemon
    /// berbohong soal anggaran yang dijanjikan ke pemilik mesin.
    pub fn enforce(&self, reading: GroupReading) -> Result<(), BudgetViolation> {
        if reading.pss_bytes >= self.cfg.hard_bytes {
            return Err(BudgetViolation {
                pss_bytes: reading.pss_bytes,
                hard_bytes: self.cfg.hard_bytes,
                processes: reading.processes,
            });
        }
        Ok(())
    }
}

/// Hasil satu pembacaan `smaps_rollup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SmapsRollup {
    pub pss_bytes: u64,
    pub rss_bytes: u64,
}

/// Baca `Pss:` dan `Rss:` dari isi `smaps_rollup`.
///
/// Nilai di berkas itu dalam kB, jadi dikali 1024. Baris yang tidak dikenal
/// dilewati tanpa mengarang nilai: kalau `Pss:` tidak ada, hasilnya `None`.
#[must_use]
pub fn parse_smaps_rollup(text: &str) -> Option<SmapsRollup> {
    let mut pss = None;
    let mut rss = None;

    for line in text.lines() {
        let Some((kunci, sisa)) = line.split_once(':') else {
            continue;
        };
        let kunci = kunci.trim();
        if kunci != "Pss" && kunci != "Rss" {
            continue;
        }
        let Some(nilai) = sisa.split_whitespace().next() else {
            continue;
        };
        let Ok(kb) = nilai.parse::<u64>() else {
            continue;
        };

        match kunci {
            "Pss" => pss = kb.checked_mul(1024),
            _ => rss = kb.checked_mul(1024),
        }
    }

    Some(SmapsRollup {
        pss_bytes: pss?,
        rss_bytes: rss.unwrap_or(0),
    })
}

/// Ambil `pgrp` (field 5) dari satu baris `/proc/<pid>/stat`.
///
/// `comm` boleh berisi spasi dan kurung, jadi pemisahan harus dari kurung tutup
/// **terakhir**, bukan dari spasi pertama. Salah di sini berarti proses dari
/// group lain ikut dihitung, atau proses kita tidak pernah dihitung.
#[must_use]
pub fn pgrp_from_stat(text: &str) -> Option<u32> {
    let tutup = text.rfind(')')?;
    let sisa = text.get(tutup + 1..)?;
    // sisa dimulai dari field 3 (state), jadi pgrp ada di indeks 2.
    sisa.split_whitespace().nth(2)?.parse().ok()
}

/// Pelanggaran anggaran keras yang sudah tidak bisa diperbaiki dengan menutup
/// tab lagi. Ini insiden, bukan keadaan normal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetViolation {
    pub pss_bytes: u64,
    pub hard_bytes: u64,
    pub processes: usize,
}

impl BudgetViolation {
    #[must_use]
    pub const fn pss_mb(&self) -> u64 {
        self.pss_bytes / MB
    }

    #[must_use]
    pub const fn hard_mb(&self) -> u64 {
        self.hard_bytes / MB
    }
}

impl fmt::Display for BudgetViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "anggaran keras dilanggar: PSS {} MB (hard {} MB) masih tersisa pada {} proses \
             setelah reap",
            self.pss_mb(),
            self.hard_mb(),
            self.processes
        )
    }
}

impl Error for BudgetViolation {}

/// Satu pembacaan seluruh process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GroupReading {
    pub pss_bytes: u64,
    pub rss_bytes: u64,
    pub processes: usize,
    /// Proses yang hilang di antara `readdir` dan pembacaan. Bukan kesalahan:
    /// tab yang ditutup bersamaan memang begitu.
    pub skipped: usize,
}

fn io_err(path: impl AsRef<Path>, source: io::Error) -> BudgetError {
    BudgetError::Io {
        path: path.as_ref().display().to_string(),
        source,
    }
}

/// PSS satu proses. Proses yang sudah mati menghasilkan `NotFound`.
pub fn pss_of_process(pid: u32) -> Result<SmapsRollup, BudgetError> {
    let path = format!("/proc/{pid}/smaps_rollup");
    let text = std::fs::read_to_string(&path).map_err(|err| io_err(&path, err))?;
    parse_smaps_rollup(&text).ok_or_else(|| BudgetError::Io {
        path,
        source: io::Error::new(io::ErrorKind::InvalidData, "tidak ada baris Pss:"),
    })
}

/// Semua pid yang `pgrp`-nya sama, urut naik supaya hasilnya bisa dibandingkan.
pub fn processes_in_group(pgid: u32) -> Result<Vec<u32>, BudgetError> {
    let dir = std::fs::read_dir("/proc").map_err(|err| io_err("/proc", err))?;
    let mut pids = Vec::new();

    for entry in dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let path = format!("/proc/{pid}/stat");
        let Ok(stat) = std::fs::read_to_string(&path) else {
            continue;
        };
        if pgrp_from_stat(&stat) == Some(pgid) {
            pids.push(pid);
        }
    }

    pids.sort_unstable();
    Ok(pids)
}

/// Jumlahkan PSS seluruh proses dalam satu process group.
///
/// Anggota yang tidak bisa dibaca (sudah mati, atau `smaps_rollup` milik user
/// lain) dihitung `skipped`, bukan menggagalkan seluruh pembacaan. Kegagalan
/// total berarti daemon kehilangan kemampuan memantau dirinya sendiri justru
/// di host yang paling butuh pemantauan: runner CI dan mesin bersama, di mana
/// proses satu group bisa dimiliki user berbeda.
pub fn read_process_group(pgid: u32) -> Result<GroupReading, BudgetError> {
    let pids = processes_in_group(pgid)?;
    let mut reading = GroupReading::default();

    for pid in pids {
        match pss_of_process(pid) {
            Ok(smaps) => {
                reading.pss_bytes = reading.pss_bytes.saturating_add(smaps.pss_bytes);
                reading.rss_bytes = reading.rss_bytes.saturating_add(smaps.rss_bytes);
                reading.processes += 1;
            }
            Err(BudgetError::Io { source, .. }) if anggota_boleh_dilewati(source.kind()) => {
                reading.skipped += 1;
            }
            Err(err) => return Err(err),
        }
    }

    Ok(reading)
}

/// Apakah kegagalan satu anggota grup boleh dilewati tanpa membatalkan
/// pembacaan. Dua jenis yang sah: proses sudah hilang, dan proses yang tidak
/// boleh dibaca user ini (`/proc/<pid>/smaps_rollup` milik user lain).
fn anggota_boleh_dilewati(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOFT: u64 = 600 * MB;
    const HIGH: u64 = 800 * MB;
    const HARD: u64 = 950 * MB;

    fn budget() -> Budget {
        Budget::new(
            BudgetConfig::from_mb(600, 800, 950, 5000).expect("konfigurasi desain harus sah"),
        )
    }

    #[test]
    fn bawaan_cocok_dengan_dokumen_desain() {
        let cfg = BudgetConfig::default();
        assert_eq!(cfg.soft_bytes, 600 * MB);
        assert_eq!(cfg.high_bytes, 800 * MB);
        assert_eq!(cfg.hard_bytes, 950 * MB);
        assert_eq!(cfg.poll_ms, 5000, "poll_ms di 04-OPERASIONAL §2");
        assert_eq!(cfg.rearm_bytes(), 540 * MB, "re-arm 90% dari soft");
    }

    #[test]
    fn ambang_dibaca_tepat_di_batas() {
        let cfg = BudgetConfig::default();
        assert_eq!(cfg.level_for(SOFT - 1), Level::Ok);
        assert_eq!(cfg.level_for(SOFT), Level::Soft);
        assert_eq!(cfg.level_for(HIGH - 1), Level::Soft);
        assert_eq!(cfg.level_for(HIGH), Level::High);
        assert_eq!(cfg.level_for(HARD - 1), Level::High);
        assert_eq!(cfg.level_for(HARD), Level::Hard);
        assert_eq!(cfg.level_for(HARD * 4), Level::Hard);
    }

    #[test]
    fn konfigurasi_terbalik_ditolak() {
        for (soft, high, hard) in [(0, 800, 950), (800, 600, 950), (600, 950, 950)] {
            assert!(
                BudgetConfig::from_mb(soft, high, hard, 5000).is_err(),
                "soft {soft} high {high} hard {hard} seharusnya ditolak"
            );
        }
        assert!(BudgetConfig::from_mb(600, 800, 950, 5000).is_ok());
    }

    #[test]
    fn ratchet_menembak_sekali_per_tingkat() {
        let mut b = budget();

        assert_eq!(b.observe(SOFT - 1), None);
        assert_eq!(
            b.observe(SOFT),
            Some(Action::EvictIdleExpired),
            "melewati soft harus memicu eviction"
        );
        assert_eq!(
            b.observe(SOFT + 10 * MB),
            None,
            "tingkat yang sama tidak diulang"
        );
        assert_eq!(b.observe(HIGH - 1), None);
        assert_eq!(
            b.observe(HIGH),
            Some(Action::CloseOldestNonSticky),
            "naik ke high harus menembak"
        );
        assert_eq!(b.observe(HARD), Some(Action::DrainAndReap));
        assert_eq!(
            b.observe(HARD + 50 * MB),
            None,
            "hard tidak ditembak dua kali"
        );

        assert_eq!(b.fired(Level::Soft), 1);
        assert_eq!(b.fired(Level::High), 1);
        assert_eq!(b.fired(Level::Hard), 1);
        assert_eq!(b.fired(Level::Ok), 0);
        assert_eq!(b.armed(), Level::Hard);
    }

    #[test]
    fn rearm_butuh_turun_di_bawah_soft_minus_histeresis() {
        let mut b = budget();
        assert_eq!(b.observe(SOFT + 5 * MB), Some(Action::EvictIdleExpired));

        // Masih di atas titik re-arm (540 MB): soft belum boleh ditembak lagi.
        assert_eq!(b.observe(SOFT - 1), None);
        assert_eq!(b.armed(), Level::Soft);

        // Turun di bawah 540 MB: kunci dibuka.
        assert_eq!(b.observe(500 * MB), None);
        assert_eq!(b.armed(), Level::Ok);

        assert_eq!(
            b.observe(SOFT + 5 * MB),
            Some(Action::EvictIdleExpired),
            "setelah re-arm, soft boleh menembak lagi"
        );
        assert_eq!(b.fired(Level::Soft), 2);
    }

    #[test]
    fn puncak_hanya_naik_dan_reset_per_generasi() {
        let mut b = budget();
        b.observe(300 * MB);
        b.observe(500 * MB);
        assert_eq!(b.peak_bytes(), 500 * MB);

        b.observe(120 * MB);
        assert_eq!(b.peak_bytes(), 500 * MB, "puncak tidak boleh turun");
        assert_eq!(b.last_bytes(), 120 * MB, "pembacaan terakhir apa adanya");

        b.on_engine_stopped();
        assert_eq!(b.peak_bytes(), 0, "generasi baru mulai dari nol");
        assert_eq!(b.last_bytes(), 0);
        assert_eq!(b.armed(), Level::Ok, "ratchet ikut bersih");
        assert_eq!(
            b.fired(Level::Soft),
            0,
            "generasi baru menghitung ulang aksinya"
        );
    }

    #[test]
    fn smaps_rollup_dibaca_dalam_byte() {
        let isi = "\
Rss:             858800 kB
Pss:             356000 kB
Pss_Anon:        300000 kB
Pss_File:         50000 kB
Pss_Shmem:         6000 kB
";
        let smaps = parse_smaps_rollup(isi).expect("berkas contoh harus terbaca");
        assert_eq!(smaps.pss_bytes, 356_000 * 1024);
        assert_eq!(smaps.rss_bytes, 858_800 * 1024);
    }

    #[test]
    fn smaps_rollup_tanpa_pss_tidak_mengarang_nilai() {
        assert!(parse_smaps_rollup("Rss: 100 kB\n").is_none());
        assert!(parse_smaps_rollup("").is_none());
        assert!(parse_smaps_rollup("Pss: bukan-angka kB\n").is_none());
    }

    #[test]
    fn pgrp_dibaca_dari_kurung_tutup_terakhir() {
        assert_eq!(
            pgrp_from_stat("1234 (chrome) S 1 4321 4321 0 -1"),
            Some(4321)
        );
        assert_eq!(
            pgrp_from_stat("77 (Web Content (x)) R 12 900 900 0"),
            Some(900),
            "comm berisi spasi dan kurung tidak boleh menggeser field"
        );
        assert_eq!(pgrp_from_stat("rusak"), None);
        assert_eq!(
            pgrp_from_stat("12 (x) S 1 bukan-angka 3"),
            None,
            "pgrp yang bukan angka tidak boleh diarang"
        );
        assert_eq!(
            pgrp_from_stat("12 (x) S 1"),
            None,
            "baris terpotong sebelum pgrp tidak boleh menebak"
        );
    }

    #[test]
    fn proses_yang_sudah_hilang_tidak_menggagalkan_pembacaan_group() {
        // pgid yang tidak pernah dipakai: hasilnya kosong, bukan error.
        let reading = read_process_group(0x7FFF_FFF0).expect("/proc harus bisa dibaca");
        assert_eq!(reading.processes, 0);
        assert_eq!(reading.pss_bytes, 0);
    }

    #[test]
    fn ratchet_lulus_saat_anggaran_dipatuhi() {
        let mut b = budget();

        // Siklus normal: naik, ditembak, turun, re-arm.
        assert_eq!(b.observe(SOFT), Some(Action::EvictIdleExpired));
        assert_eq!(b.observe(500 * MB), None);
        assert_eq!(
            b.enforce(GroupReading {
                pss_bytes: 0,
                rss_bytes: 0,
                processes: 0,
                skipped: 0,
            }),
            Ok(()),
            "setelah reap bersih, anggaran keras tidak dilanggar"
        );
        assert_eq!(
            b.enforce(GroupReading {
                pss_bytes: HARD - 1,
                rss_bytes: HARD - 1,
                processes: 4,
                skipped: 0,
            }),
            Ok(()),
            "tepat di bawah hard masih boleh"
        );
    }

    #[test]
    fn ratchet_melempar_galat_saat_anggaran_dilanggar() {
        let mut b = budget();
        b.observe(HARD); // memicu DrainAndReap

        // Semua tindakan sudah dijalankan, tapi group masih memakan 980 MB.
        let violation = b
            .enforce(GroupReading {
                pss_bytes: 980 * MB,
                rss_bytes: 1_400 * MB,
                processes: 3,
                skipped: 1,
            })
            .expect_err("PSS 980 MB di atas hard 950 MB harus jadi galat");

        assert_eq!(violation.pss_bytes, 980 * MB);
        assert_eq!(violation.hard_bytes, HARD);
        assert_eq!(violation.processes, 3);
        assert_eq!(violation.pss_mb(), 980);
        assert_eq!(violation.hard_mb(), 950);
        assert!(
            violation.to_string().contains("980"),
            "pesan galat harus memuat angka yang bisa diaudit: {violation}"
        );
    }

    #[test]
    fn group_sendiri_terbaca_dan_memuat_proses_ini() {
        let pgid = pgrp_from_stat(
            &std::fs::read_to_string("/proc/self/stat").expect("stat proses sendiri"),
        )
        .expect("pgrp proses sendiri");
        assert!(pgid > 1);

        let reading = read_process_group(pgid).expect("group sendiri harus terbaca");
        assert!(
            reading.processes >= 1,
            "proses test ini sendiri harus terhitung"
        );
        assert!(reading.pss_bytes > 0, "PSS proses hidup tidak mungkin nol");
        assert!(
            reading.pss_bytes <= reading.rss_bytes + 4 * 1024 * 1024 * 1024,
            "PSS tidak masuk akal dibanding RSS"
        );
    }

    /// Kegagalan yang sah untuk satu anggota grup. Kalau salah satu jenis ini
    /// ikut membatalkan pembacaan, CI (yang menolak `smaps_rollup` proses lain
    /// di group yang sama) mematikan gate, dan di produksi daemon buta terhadap
    /// pemakaiannya sendiri. Deterministik: tidak menyentuh /proc.
    #[test]
    fn anggota_yang_hilang_atau_terlarang_dilewati_sisanya_tetap_galat() {
        assert!(anggota_boleh_dilewati(io::ErrorKind::NotFound));
        assert!(anggota_boleh_dilewati(io::ErrorKind::PermissionDenied));
        for lain in [
            io::ErrorKind::InvalidData,
            io::ErrorKind::OutOfMemory,
            io::ErrorKind::Other,
        ] {
            assert!(
                !anggota_boleh_dilewati(lain),
                "{lain:?} harus tetap menggagalkan pembacaan, bukan dilewati diam-diam"
            );
        }
    }
}
