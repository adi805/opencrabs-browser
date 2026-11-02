//! Penghitung yang bisa ditarik Prometheus, dan yang tidak bisa berbohong.
//!
//! Aturan modul ini: **hanya mencatat apa yang sudah terjadi.** Tidak ada
//! penghitung yang dinaikkan karena sesuatu "seharusnya" terjadi. Karena itu
//! semua nilai di sini berasal dari [`ReapReport`], [`Level`], dan [`State`]
//! yang sudah dihitung di modul lain, bukan dari niat.
//!
//! Gauge yang tidak bisa dijawab (mis. PSS saat proses engine sudah mati) tidak
//! ditulis sebagai `0`. Nol itu klaim "tidak memakai memori", dan itu klaim yang
//! berbeda dari "tidak tahu". Kalau tidak tahu, yang ditulis adalah komentar.
//!
//! Nama metrik mengikuti `04-OPERASIONAL-ROADMAP` §4.1.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::budget::Level;
use crate::reaper::{ReapOutcome, ReapReason, ReapReport};
use crate::state::State;

/// Jumlah sinyal yang mungkin dicatat, diindeks oleh [`Signal`]-nya sendiri.
///
/// [`Signal`]: crate::reaper::Signal
const SIGNALS: usize = 2;

/// Jumlah alasan reap yang dicatat, diindeks oleh [`ReapReason::as_str`].
const REASONS: usize = 5;

/// Tingkat anggaran yang menggerakkan aksi, tanpa `ok` karena `ok` tidak
/// menggerakkan apa pun. Ini yang jadi label `level` di
/// `browser_budget_action_total`, sesuai roadmap.
const LEVELS: usize = 3;

/// Penghitung proses-luas. Aman dipakai lintas thread tanpa kunci.
#[derive(Debug)]
pub struct Counters {
    reaps: AtomicU64,
    signal_sent: [AtomicU64; SIGNALS],
    survivors: AtomicU64,
    reap_reasons: [AtomicU64; REASONS],
    budget_actions: [AtomicU64; LEVELS],
}

impl Default for Counters {
    fn default() -> Self {
        Self::new()
    }
}

impl Counters {
    #[must_use]
    pub fn new() -> Self {
        Self {
            reaps: AtomicU64::new(0),
            signal_sent: std::array::from_fn(|_| AtomicU64::new(0)),
            survivors: AtomicU64::new(0),
            reap_reasons: std::array::from_fn(|_| AtomicU64::new(0)),
            budget_actions: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Catat satu laporan reap apa adanya: sinyal yang benar-benar dikirim, dan
    /// apakah ada proses yang selamat.
    pub fn record_reap(&self, report: &ReapReport) {
        self.reaps.fetch_add(1, Ordering::Relaxed);
        if report.sent_term {
            self.signal_sent[0].fetch_add(1, Ordering::Relaxed);
        }
        if report.sent_kill {
            self.signal_sent[1].fetch_add(1, Ordering::Relaxed);
        }
        if report.outcome == ReapOutcome::Survived {
            self.survivors.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(idx) = reason_index(report.reason) {
            self.reap_reasons[idx].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Catat satu aksi anggaran. `Ok` tidak dicatat: dia tidak menggerakkan
    /// apa pun, jadi menghitungnya akan membuat metrik ini selalu naik dan
    /// tidak berarti.
    pub fn record_budget_action(&self, level: Level) {
        if let Some(idx) = level_index(level) {
            self.budget_actions[idx].fetch_add(1, Ordering::Relaxed);
        }
    }

    #[must_use]
    pub fn reaps(&self) -> u64 {
        self.reaps.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn terms_sent(&self) -> u64 {
        self.signal_sent[0].load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn kills_sent(&self) -> u64 {
        self.signal_sent[1].load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn survivors(&self) -> u64 {
        self.survivors.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn budget_actions(&self, level: Level) -> u64 {
        level_index(level).map_or(0, |idx| self.budget_actions[idx].load(Ordering::Relaxed))
    }

    #[must_use]
    pub fn reaps_for(&self, reason: ReapReason) -> u64 {
        reason_index(reason).map_or(0, |idx| self.reap_reasons[idx].load(Ordering::Relaxed))
    }

    /// Seluruh metrik dalam format teks Prometheus: bagian per mesin lalu
    /// bagian milik daemon.
    ///
    /// `pss_bytes` adalah pembacaan hidup; `None` berarti proses engine tidak
    /// ada atau statusnya tidak terbaca, dan itu ditulis sebagai komentar.
    #[must_use]
    pub fn render(&self, engine: &str, state: State, pss_bytes: Option<u64>) -> String {
        let mut out = self.render_engine(engine, state, pss_bytes);
        out.push_str(&self.render_global());
        out
    }

    /// Metrik yang **milik satu mesin**: memori dan state.
    ///
    /// Dipisah dari [`Self::render_global`] karena daemon sekarang bisa
    /// menjalankan lebih dari satu mesin. Kalau seluruh blok dicetak per
    /// mesin, penghitung yang hanya punya satu nilai untuk seluruh daemon
    /// (reap, anggaran) muncul dua kali dengan angka yang sama, dan scraper
    /// akan menjumlahkannya menjadi dua kali lipat kejadian yang sebenarnya.
    #[must_use]
    pub fn render_engine(&self, engine: &str, state: State, pss_bytes: Option<u64>) -> String {
        let mut out = String::with_capacity(512);

        match pss_bytes {
            Some(bytes) => {
                out.push_str("# TYPE browser_pss_bytes gauge\n");
                let _ = writeln!(out, "browser_pss_bytes{{engine=\"{engine}\"}} {bytes}");
            }
            None => out.push_str(
                "# browser_pss_bytes tidak ditulis: proses engine tidak hidup atau /proc tidak terbaca\n",
            ),
        }

        out.push_str("# TYPE browser_engine_state gauge\n");
        for candidate in State::ALL {
            let value = u8::from(candidate == state);
            let _ = writeln!(
                out,
                "browser_engine_state{{engine=\"{engine}\",state=\"{}\"}} {value}",
                candidate.as_str()
            );
        }

        out
    }

    /// Metrik yang **milik daemon**, tanpa label mesin: satu nilai untuk
    /// seluruh proses, jadi hanya boleh dicetak sekali.
    #[must_use]
    pub fn render_global(&self) -> String {
        let mut out = String::with_capacity(768);

        out.push_str("# TYPE browser_engine_restarts_total counter\n");
        for reason in ReapReason::ALL {
            if let Some(idx) = reason_index(reason) {
                let _ = writeln!(
                    out,
                    "browser_engine_restarts_total{{reason=\"{}\"}} {}",
                    reason.as_str(),
                    self.reap_reasons[idx].load(Ordering::Relaxed)
                );
            }
        }

        out.push_str("# TYPE browser_reap_total counter\n");
        let _ = writeln!(out, "browser_reap_total {}", self.reaps());
        let _ = writeln!(
            out,
            "browser_reap_signal_total{{signal=\"term\"}} {}",
            self.terms_sent()
        );
        let _ = writeln!(
            out,
            "browser_reap_signal_total{{signal=\"kill\"}} {}",
            self.kills_sent()
        );
        let _ = writeln!(out, "browser_reap_survivors_total {}", self.survivors());

        out.push_str("# TYPE browser_budget_action_total counter\n");
        for level in [Level::Soft, Level::High, Level::Hard] {
            let _ = writeln!(
                out,
                "browser_budget_action_total{{level=\"{}\"}} {}",
                level.as_str(),
                self.budget_actions(level)
            );
        }

        out
    }
}

const fn reason_index(reason: ReapReason) -> Option<usize> {
    match reason {
        ReapReason::Idle => Some(0),
        ReapReason::BudgetHard => Some(1),
        ReapReason::BootFailure => Some(2),
        ReapReason::Shutdown => Some(3),
        ReapReason::Manual => Some(4),
    }
}

const fn level_index(level: Level) -> Option<usize> {
    match level {
        Level::Ok => None,
        Level::Soft => Some(0),
        Level::High => Some(1),
        Level::Hard => Some(2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reaper::ReapOutcome;
    use std::time::Duration;

    fn laporan(outcome: ReapOutcome, sent_term: bool, sent_kill: bool) -> ReapReport {
        ReapReport {
            reason: ReapReason::Idle,
            outcome,
            sent_term,
            sent_kill,
            waited: Duration::from_millis(1),
            survivors: usize::from(outcome == ReapOutcome::Survived),
            polls: 1,
        }
    }

    #[test]
    fn awal_semua_nol_dan_tidak_ada_angka_karangan() {
        let counters = Counters::new();
        assert_eq!(counters.reaps(), 0);
        assert_eq!(counters.terms_sent(), 0);
        assert_eq!(counters.kills_sent(), 0);
        assert_eq!(counters.survivors(), 0);
        assert_eq!(counters.budget_actions(Level::Soft), 0);
        assert_eq!(counters.budget_actions(Level::Ok), 0);
    }

    #[test]
    fn laporan_terminated_mencatat_term_saja() {
        let counters = Counters::new();
        counters.record_reap(&laporan(ReapOutcome::Terminated, true, false));

        assert_eq!(counters.reaps(), 1);
        assert_eq!(counters.terms_sent(), 1);
        assert_eq!(
            counters.kills_sent(),
            0,
            "KILL tidak dikirim, tidak dicatat"
        );
        assert_eq!(counters.reaps_for(ReapReason::Idle), 1);
    }

    #[test]
    fn laporan_killed_mencatat_term_dan_kill() {
        let counters = Counters::new();
        counters.record_reap(&laporan(ReapOutcome::Killed, true, true));

        assert_eq!(counters.terms_sent(), 1);
        assert_eq!(counters.kills_sent(), 1);
    }

    #[test]
    fn sudah_mati_tidak_dihitung_sebagai_sinyal() {
        let counters = Counters::new();
        counters.record_reap(&laporan(ReapOutcome::AlreadyGone, false, false));

        assert_eq!(counters.reaps(), 1, "reap tetap dicatat");
        assert_eq!(counters.terms_sent(), 0, "tidak ada sinyal yang dikirim");
        assert_eq!(counters.kills_sent(), 0);
    }

    #[test]
    fn survivors_dihitung_terpisah() {
        let counters = Counters::new();
        counters.record_reap(&laporan(ReapOutcome::Survived, true, true));

        assert_eq!(counters.survivors(), 1);
        assert_eq!(counters.reaps(), 1);
    }

    #[test]
    fn aksi_anggaran_dihitung_per_tingkat_dan_ok_diabaikan() {
        let counters = Counters::new();
        counters.record_budget_action(Level::Ok);
        counters.record_budget_action(Level::Soft);
        counters.record_budget_action(Level::Hard);
        counters.record_budget_action(Level::Hard);

        assert_eq!(counters.budget_actions(Level::Ok), 0);
        assert_eq!(counters.budget_actions(Level::Soft), 1);
        assert_eq!(counters.budget_actions(Level::High), 0);
        assert_eq!(counters.budget_actions(Level::Hard), 2);
    }

    #[test]
    fn state_ditulis_satu_hot_saja() {
        let text = Counters::new().render("shell", State::Ready, None);

        assert!(
            text.contains("browser_engine_state{engine=\"shell\",state=\"ready\"} 1"),
            "state hidup harus 1:\n{text}"
        );
        assert!(
            text.contains("browser_engine_state{engine=\"shell\",state=\"cold\"} 0"),
            "state lain harus 0:\n{text}"
        );
        assert_eq!(
            text.matches(" 1\n").count(),
            1,
            "tepat satu state bernilai 1:\n{text}"
        );
    }

    #[test]
    fn pss_tidak_ditulis_kalau_tidak_terbaca() {
        let text = Counters::new().render("shell", State::Stopped, None);

        assert!(
            !text.contains("browser_pss_bytes{"),
            "tidak boleh ada angka PSS palsu:\n{text}"
        );
        assert!(
            text.contains("tidak ditulis"),
            "harus ada alasan yang terbaca:\n{text}"
        );
    }

    #[test]
    fn pss_ditulis_apa_adanya_kalau_ada() {
        let text = Counters::new().render("shell", State::Ready, Some(356_000_000));

        assert!(
            text.contains("browser_pss_bytes{engine=\"shell\"} 356000000"),
            "dapat:\n{text}"
        );
    }

    #[test]
    fn label_level_hanya_tiga_tanpa_ok() {
        let text = Counters::new().render("shell", State::Cold, None);

        assert!(text.contains("browser_budget_action_total{level=\"soft\"} 0"));
        assert!(text.contains("browser_budget_action_total{level=\"high\"} 0"));
        assert!(text.contains("browser_budget_action_total{level=\"hard\"} 0"));
        assert!(
            !text.contains("level=\"ok\""),
            "ok bukan aksi, tidak boleh jadi label:\n{text}"
        );
    }
}
