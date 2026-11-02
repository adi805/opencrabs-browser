//! Kapan engine dimatikan, dan dalam urutan apa.
//!
//! Dua hal yang dipisah tegas di sini:
//!
//! 1. **Keputusan** (fungsi murni): kapan idle sudah cukup lama, tab mana yang
//!    boleh ditutup, dan tab mana yang **tidak boleh disentuh**.
//! 2. **Eksekusi**: kirim `SIGTERM`, tunggu, lalu `SIGKILL`. Sinyal dikirim
//!    lewat [`ProcessGroup`] supaya urutannya bisa diuji tanpa benar-benar
//!    membunuh proses, dan supaya crate ini tetap tanpa dependensi.
//!
//! Angka 60 detik dan 5 detik bukan selera: `04-OPERASIONAL` §3.3 mengukur
//! bahwa Chromium mempertahankan 249.5 MB PSS setelah semua tab ditutup, jadi
//! satu-satunya cara memori kembali adalah mematikan process group-nya.
//! `SIGTERM` lebih dulu supaya profil dan cookie sempat ditutup rapi; `SIGKILL`
//! hanya kalau grace habis.

use std::error::Error;
use std::fmt;
use std::io;
use std::time::{Duration, Instant};

/// Kenapa engine dimatikan. Nilai ini masuk ke log dan metrik, jadi tidak boleh
/// berubah diam-diam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapReason {
    /// Tidak ada sesi aktif selama `idle_reap`.
    Idle,
    /// Anggaran memori menembus hard cap.
    BudgetHard,
    /// Proses lahir tapi tidak pernah siap.
    BootFailure,
    /// Daemon dimatikan.
    Shutdown,
    /// Perintah operator.
    Manual,
}

impl ReapReason {
    /// Semua alasan, urutannya sama dengan indeks penghitung di
    /// [`crate::metrics`]. Metrik yang kehilangan satu label akan terbaca
    /// sebagai "tidak pernah terjadi", jadi daftarnya harus lengkap.
    pub const ALL: [Self; 5] = [
        Self::Idle,
        Self::BudgetHard,
        Self::BootFailure,
        Self::Shutdown,
        Self::Manual,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::BudgetHard => "budget_hard",
            Self::BootFailure => "boot_failure",
            Self::Shutdown => "shutdown",
            Self::Manual => "manual",
        }
    }
}

impl fmt::Display for ReapReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Ambang waktu untuk reaper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaperConfig {
    /// Berapa lama tanpa sesi aktif sebelum engine dimatikan.
    pub idle_reap: Duration,
    /// Berapa lama menunggu setelah `SIGTERM` sebelum `SIGKILL`.
    pub term_grace: Duration,
    /// Berapa lama menunggu setelah `SIGKILL` sebelum menyatakan proses bandel.
    pub kill_grace: Duration,
    /// Jarak antar pemeriksaan selama grace.
    pub poll: Duration,
    /// Batas keras jumlah poll, supaya loop tidak pernah abadi walau jam
    /// sistem melompat.
    pub max_polls: u32,
}

impl Default for ReaperConfig {
    fn default() -> Self {
        Self {
            idle_reap: Duration::from_secs(60),
            term_grace: Duration::from_secs(5),
            kill_grace: Duration::from_secs(5),
            poll: Duration::from_millis(250),
            max_polls: 2_000,
        }
    }
}

/// Satu tab yang hidup di engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tab {
    pub id: u64,
    /// Sudah berapa lama tab ini tidak dipakai.
    pub idle_for: Duration,
    /// Tab yang ditahan untuk handoff manusia (captcha/OTP) tidak pernah
    /// ditutup otomatis: orangnya mungkin sedang mengetik jawabannya.
    pub sticky: bool,
}

/// Kapan engine boleh dimatikan karena menganggur.
///
/// `pending_handoffs` ikut dihitung karena handoff adalah alasan paling sah
/// untuk mempertahankan engine hidup tanpa sesi otomatis.
#[must_use]
pub fn idle_reap_due(
    active_sessions: usize,
    pending_handoffs: usize,
    idle_for: Duration,
    cfg: &ReaperConfig,
) -> Option<ReapReason> {
    if active_sessions > 0 || pending_handoffs > 0 {
        return None;
    }
    (idle_for >= cfg.idle_reap).then_some(ReapReason::Idle)
}

/// Tab yang boleh ditutup pada tingkat `soft`: idle melewati TTL, terlama dulu.
///
/// Tab `sticky` tidak pernah muncul di hasil, seberapa lama pun dia menganggur.
#[must_use]
pub fn idle_expired(tabs: &[Tab], ttl: Duration, maksimal: usize) -> Vec<u64> {
    let mut kandidat: Vec<&Tab> = tabs
        .iter()
        .filter(|tab| !tab.sticky && tab.idle_for >= ttl)
        .collect();

    kandidat.sort_by(|a, b| b.idle_for.cmp(&a.idle_for).then(a.id.cmp(&b.id)));

    kandidat
        .into_iter()
        .take(maksimal)
        .map(|tab| tab.id)
        .collect()
}

/// Tab yang boleh ditutup pada tingkat `high`: tertua yang tidak sticky,
/// tanpa menunggu TTL.
#[must_use]
pub fn oldest_non_sticky(tabs: &[Tab], maksimal: usize) -> Vec<u64> {
    let mut kandidat: Vec<&Tab> = tabs.iter().filter(|tab| !tab.sticky).collect();

    kandidat.sort_by(|a, b| b.idle_for.cmp(&a.idle_for).then(a.id.cmp(&b.id)));

    kandidat
        .into_iter()
        .take(maksimal)
        .map(|tab| tab.id)
        .collect()
}

/// Sinyal yang bisa dikirim ke process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
}

impl Signal {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Term => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Proses yang punya group sendiri.
///
/// Implementasi nyata ada di lapisan yang punya akses OS; di sini cukup
/// kontraknya, sehingga urutan `SIGTERM -> tunggu -> SIGKILL` bisa diuji
/// tanpa membunuh apa pun.
pub trait ProcessGroup {
    /// Kirim sinyal ke **seluruh** group, bukan hanya satu proses.
    fn signal(&self, signal: Signal) -> Result<(), SignalError>;

    /// Jumlah proses hidup yang masih tersisa di group. `0` berarti bersih.
    fn alive(&self) -> Result<usize, SignalError>;
}

/// Kegagalan saat menyentuh proses.
#[derive(Debug)]
pub enum SignalError {
    /// Sinyal ditolak kernel (mis. group sudah tidak ada).
    Send { signal: Signal, source: io::Error },
    /// Status group tidak bisa dibaca.
    Alive { source: io::Error },
}

impl fmt::Display for SignalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Send { signal, source } => write!(f, "gagal mengirim {signal}: {source}"),
            Self::Alive { source } => write!(f, "gagal membaca status group: {source}"),
        }
    }
}

impl Error for SignalError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Send { source, .. } | Self::Alive { source } => Some(source),
        }
    }
}

/// Hasil akhir satu proses reap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapOutcome {
    /// Group sudah kosong sebelum sinyal apa pun dikirim.
    AlreadyGone,
    /// Mati setelah `SIGTERM`.
    Terminated,
    /// Butuh `SIGKILL`.
    Killed,
    /// Masih hidup setelah `SIGKILL`. Ini insiden.
    Survived,
}

impl ReapOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyGone => "already_gone",
            Self::Terminated => "terminated",
            Self::Killed => "killed",
            Self::Survived => "survived",
        }
    }

    /// Apakah memori benar-benar sudah kembali.
    #[must_use]
    pub const fn released(self) -> bool {
        !matches!(self, Self::Survived)
    }
}

impl fmt::Display for ReapOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Catatan satu kali reap, cukup lengkap untuk satu baris log JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReapReport {
    pub reason: ReapReason,
    pub outcome: ReapOutcome,
    pub sent_term: bool,
    pub sent_kill: bool,
    /// Waktu dari sinyal pertama sampai group kosong (atau menyerah).
    pub waited: Duration,
    /// Proses yang masih hidup saat menyerah. `0` kalau bersih.
    pub survivors: usize,
    pub polls: u32,
}

impl fmt::Display for ReapReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "reap {}: {} (term={}, kill={}, {} ms, sisa {})",
            self.reason,
            self.outcome,
            self.sent_term,
            self.sent_kill,
            self.waited.as_millis(),
            self.survivors
        )
    }
}

/// Jalankan protokol reap terhadap satu process group.
///
/// Urutannya tetap: periksa, `SIGTERM`, tunggu `term_grace`, `SIGKILL`, tunggu
/// `kill_grace`. Tidak ada jalan pintas ke `SIGKILL`: melewatkan `SIGTERM`
/// berarti profil browser tidak pernah ditutup dan cookie hilang.
///
/// Fungsi ini **memblokir** (ada `sleep` di dalamnya). Pemanggil async wajib
/// menjalankannya di `spawn_blocking`.
pub fn reap(
    group: &dyn ProcessGroup,
    cfg: &ReaperConfig,
    reason: ReapReason,
) -> Result<ReapReport, SignalError> {
    let mulai = Instant::now();
    let mut polls = 0u32;

    let awal = group.alive()?;
    if awal == 0 {
        return Ok(ReapReport {
            reason,
            outcome: ReapOutcome::AlreadyGone,
            sent_term: false,
            sent_kill: false,
            waited: mulai.elapsed(),
            survivors: 0,
            polls,
        });
    }

    group.signal(Signal::Term)?;

    if tunggu_habis(group, cfg.term_grace, cfg, &mut polls)?.is_some() {
        return Ok(ReapReport {
            reason,
            outcome: ReapOutcome::Terminated,
            sent_term: true,
            sent_kill: false,
            waited: mulai.elapsed(),
            survivors: 0,
            polls,
        });
    }

    group.signal(Signal::Kill)?;

    if tunggu_habis(group, cfg.kill_grace, cfg, &mut polls)?.is_some() {
        return Ok(ReapReport {
            reason,
            outcome: ReapOutcome::Killed,
            sent_term: true,
            sent_kill: true,
            waited: mulai.elapsed(),
            survivors: 0,
            polls,
        });
    }

    Ok(ReapReport {
        reason,
        outcome: ReapOutcome::Survived,
        sent_term: true,
        sent_kill: true,
        waited: mulai.elapsed(),
        survivors: group.alive()?,
        polls,
    })
}

/// Tunggu sampai group kosong atau batas waktu habis.
///
/// `Some(0)` berarti sudah bersih; `None` berarti waktu habis dan pemanggil
/// harus naik ke langkah berikutnya.
fn tunggu_habis(
    group: &dyn ProcessGroup,
    batas: Duration,
    cfg: &ReaperConfig,
    polls: &mut u32,
) -> Result<Option<usize>, SignalError> {
    let mulai = Instant::now();

    loop {
        let sisa = group.alive()?;
        if sisa == 0 {
            return Ok(Some(0));
        }
        if mulai.elapsed() >= batas || *polls >= cfg.max_polls {
            return Ok(None);
        }
        *polls += 1;
        if !cfg.poll.is_zero() {
            std::thread::sleep(cfg.poll);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn cepat() -> ReaperConfig {
        ReaperConfig {
            idle_reap: Duration::from_secs(60),
            term_grace: Duration::from_millis(20),
            kill_grace: Duration::from_millis(20),
            poll: Duration::from_millis(1),
            max_polls: 100,
        }
    }

    struct FakeGroup {
        /// Jumlah proses sebelum sinyal apa pun. Dipisah dari jumlah sesudah
        /// TERM supaya "group yang sudah kosong" bisa diuji: group kosong tidak
        /// boleh menerima sinyal apa pun.
        hidup_awal: usize,
        hidup_setelah_term: usize,
        hidup_setelah_kill: usize,
        sinyal: RefCell<Vec<Signal>>,
    }

    impl FakeGroup {
        fn baru(hidup_setelah_term: usize, hidup_setelah_kill: usize) -> Self {
            Self::dengan_awal(2, hidup_setelah_term, hidup_setelah_kill)
        }

        fn dengan_awal(
            hidup_awal: usize,
            hidup_setelah_term: usize,
            hidup_setelah_kill: usize,
        ) -> Self {
            Self {
                hidup_awal,
                hidup_setelah_term,
                hidup_setelah_kill,
                sinyal: RefCell::new(Vec::new()),
            }
        }

        fn jumlah(&self, signal: Signal) -> usize {
            self.sinyal
                .borrow()
                .iter()
                .filter(|s| **s == signal)
                .count()
        }

        fn urutan(&self) -> Vec<Signal> {
            self.sinyal.borrow().clone()
        }
    }

    impl ProcessGroup for FakeGroup {
        fn signal(&self, signal: Signal) -> Result<(), SignalError> {
            self.sinyal.borrow_mut().push(signal);
            Ok(())
        }

        fn alive(&self) -> Result<usize, SignalError> {
            let terkirim = self.sinyal.borrow().clone();
            let terakhir = terkirim.last();
            Ok(match terakhir {
                None => self.hidup_awal,
                Some(Signal::Term) => self.hidup_setelah_term,
                Some(Signal::Kill) => self.hidup_setelah_kill,
            })
        }
    }

    #[test]
    fn idle_reap_hanya_kalau_tidak_ada_sesi_dan_handoff() {
        let cfg = cepat();

        assert_eq!(
            idle_reap_due(0, 0, Duration::from_secs(59), &cfg),
            None,
            "belum 60 detik"
        );
        assert_eq!(
            idle_reap_due(0, 0, Duration::from_secs(60), &cfg),
            Some(ReapReason::Idle)
        );
        assert_eq!(
            idle_reap_due(1, 0, Duration::from_secs(600), &cfg),
            None,
            "ada sesi aktif: umur idle tidak relevan"
        );
        assert_eq!(
            idle_reap_due(0, 1, Duration::from_secs(600), &cfg),
            None,
            "handoff menunggu manusia tidak boleh dimatikan"
        );
    }

    #[test]
    fn reaper_tidak_pernah_membunuh_tab_lebih_muda_dari_ttl() {
        let ttl = Duration::from_secs(120);
        let tabs = [
            Tab {
                id: 1,
                idle_for: Duration::from_secs(30),
                sticky: false,
            },
            Tab {
                id: 2,
                idle_for: Duration::from_secs(119),
                sticky: false,
            },
            Tab {
                id: 3,
                idle_for: Duration::from_secs(120),
                sticky: false,
            },
        ];

        assert_eq!(
            idle_expired(&tabs, ttl, 10),
            vec![3],
            "hanya yang lewat TTL"
        );

        // Tanpa TTL pun (tingkat high), tab termuda tetap bukan kandidat
        // pertama: yang paling lama menganggur yang ditutup lebih dulu.
        assert_eq!(oldest_non_sticky(&tabs, 1), vec![3]);
        assert_eq!(oldest_non_sticky(&tabs, 10), vec![3, 2, 1]);
    }

    #[test]
    fn tab_sticky_selalu_dilewati() {
        let tabs = [
            Tab {
                id: 10,
                idle_for: Duration::from_secs(9_000),
                sticky: true,
            },
            Tab {
                id: 11,
                idle_for: Duration::from_secs(130),
                sticky: false,
            },
        ];

        assert_eq!(idle_expired(&tabs, Duration::from_secs(120), 10), vec![11]);
        assert_eq!(oldest_non_sticky(&tabs, 10), vec![11]);
    }

    #[test]
    fn urutan_eviction_deterministik_saat_idle_sama() {
        let tabs = [
            Tab {
                id: 5,
                idle_for: Duration::from_secs(300),
                sticky: false,
            },
            Tab {
                id: 4,
                idle_for: Duration::from_secs(300),
                sticky: false,
            },
        ];
        assert_eq!(idle_expired(&tabs, Duration::from_secs(60), 10), vec![4, 5]);
    }

    #[test]
    fn group_yang_sudah_mati_tidak_diberi_sinyal() {
        let group = FakeGroup::dengan_awal(0, 0, 0);
        let report = reap(&group, &cepat(), ReapReason::Shutdown).expect("reap harus jalan");

        assert_eq!(report.outcome, ReapOutcome::AlreadyGone);
        assert!(!report.sent_term && !report.sent_kill);
        assert!(group.urutan().is_empty(), "group kosong tidak disinyal");
    }

    #[test]
    fn sigterm_cukup_tidak_ada_sigkill() {
        let group = FakeGroup::baru(0, 0);
        let report = reap(&group, &cepat(), ReapReason::Idle).expect("reap harus jalan");

        assert_eq!(report.outcome, ReapOutcome::Terminated);
        assert!(report.sent_term);
        assert!(
            !report.sent_kill,
            "SIGKILL tidak boleh dipakai kalau TERM cukup"
        );
        assert_eq!(group.urutan(), vec![Signal::Term]);
        assert_eq!(group.jumlah(Signal::Term), 1, "SIGTERM dikirim sekali");
        assert_eq!(report.survivors, 0);
        assert!(report.outcome.released());
    }

    #[test]
    fn proses_bandel_dieskalasi_ke_sigkill() {
        let group = FakeGroup::baru(2, 0);
        let report = reap(&group, &cepat(), ReapReason::BudgetHard).expect("reap harus jalan");

        assert_eq!(report.outcome, ReapOutcome::Killed);
        assert_eq!(
            group.urutan(),
            vec![Signal::Term, Signal::Kill],
            "urutan wajib TERM lalu KILL"
        );
        assert!(report.waited >= Duration::ZERO);
        assert!(report.outcome.released());
    }

    #[test]
    fn proses_yang_tetap_hidup_dilaporkan_jujur() {
        let group = FakeGroup::baru(3, 3);
        let report = reap(&group, &cepat(), ReapReason::BootFailure).expect("reap harus jalan");

        assert_eq!(report.outcome, ReapOutcome::Survived);
        assert!(!report.outcome.released());
        assert_eq!(report.survivors, 3);
        assert!(report.sent_term && report.sent_kill);
        assert!(report.polls <= cepat().max_polls);
        assert!(
            report.to_string().contains("survived"),
            "laporan harus menyebut hasil apa adanya: {report}"
        );
    }

    #[test]
    fn batas_poll_mencegah_loop_abadi() {
        let mut cfg = cepat();
        cfg.max_polls = 3;
        cfg.term_grace = Duration::from_secs(3600);
        cfg.kill_grace = Duration::from_secs(3600);
        cfg.poll = Duration::ZERO;

        let group = FakeGroup::baru(1, 1);
        let report = reap(&group, &cfg, ReapReason::Manual).expect("reap harus jalan");

        assert_eq!(report.outcome, ReapOutcome::Survived);
        assert!(
            report.polls <= 3,
            "poll harus berhenti di batas: {}",
            report.polls
        );
    }

    #[test]
    fn alasan_reap_punya_label_unik() {
        let daftar = [
            ReapReason::Idle,
            ReapReason::BudgetHard,
            ReapReason::BootFailure,
            ReapReason::Shutdown,
            ReapReason::Manual,
        ];
        let mut terlihat: Vec<&str> = Vec::new();
        for alasan in daftar {
            let nama = alasan.as_str();
            assert_eq!(nama, nama.to_ascii_lowercase());
            assert!(!terlihat.contains(&nama), "label {nama} dobel");
            terlihat.push(nama);
        }
    }
}
