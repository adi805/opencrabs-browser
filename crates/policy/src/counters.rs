//! Penghitung tantangan, dalam bentuk yang dibaca Prometheus.
//!
//! Nama dan labelnya mengikuti dokumen `03-CAPTCHA-OTP` §9. Yang penting di
//! sini bukan angkanya, tapi **apa yang tidak dicatat**: kode OTP, isi halaman,
//! URL lengkap. Semua itu tidak punya tempat di metrik, karena metrik adalah
//! hal pertama yang dikirim ke tempat lain saat ada masalah.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

/// Jenis kejadian, sesuai `handoff.kind` di store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Challenge,
    Otp,
    Login,
}

impl Kind {
    pub const ALL: [Self; 3] = [Self::Challenge, Self::Otp, Self::Login];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Challenge => "challenge",
            Self::Otp => "otp",
            Self::Login => "login",
        }
    }
}

/// Hasil akhir satu kejadian. Tidak ada nilai "mungkin": keputusan yang belum
/// selesai tidak dicatat sebagai hasil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Tantangan terdeteksi, belum ada hasil.
    Detected,
    /// Lolos tanpa interaksi (cookie dan profil dipakai ulang).
    SolvedReuse,
    /// Owner menjawab handoff dan verifikasi lolos.
    SolvedHandoff,
    /// Naik tingkat eskalasi (L1/L2), belum selesai.
    Escalated,
    /// Handoff kedaluwarsa tanpa jawaban.
    Expired,
    /// Owner membatalkan.
    Aborted,
    /// Tingkat dilewati karena mesinnya tidak terpasang.
    Skipped,
}

impl Outcome {
    pub const ALL: [Self; 7] = [
        Self::Detected,
        Self::SolvedReuse,
        Self::SolvedHandoff,
        Self::Escalated,
        Self::Expired,
        Self::Aborted,
        Self::Skipped,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Detected => "detected",
            Self::SolvedReuse => "solved_reuse",
            Self::SolvedHandoff => "solved_handoff",
            Self::Escalated => "escalated",
            Self::Expired => "expired",
            Self::Aborted => "aborted",
            Self::Skipped => "skipped",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Detected => 0,
            Self::SolvedReuse => 1,
            Self::SolvedHandoff => 2,
            Self::Escalated => 3,
            Self::Expired => 4,
            Self::Aborted => 5,
            Self::Skipped => 6,
        }
    }
}

const KINDS: usize = 3;
const OUTCOMES: usize = 7;

/// Batas atas ember waktu jawab handoff, dalam detik.
///
/// Ember terakhir adalah `+Inf` yang selalu ikut ditulis, karena histogram
/// tanpa `+Inf` tidak bisa dipakai menghitung total.
const BUCKETS: [u64; 5] = [30, 60, 120, 300, 900];

/// Penghitung proses-luas.
#[derive(Debug)]
pub struct CaptchaCounters {
    events: [[AtomicU64; OUTCOMES]; KINDS],
    buckets: [AtomicU64; 5],
    answer_sum: AtomicU64,
    answer_count: AtomicU64,
}

impl Default for CaptchaCounters {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptchaCounters {
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            answer_sum: AtomicU64::new(0),
            answer_count: AtomicU64::new(0),
        }
    }

    pub fn record(&self, kind: Kind, outcome: Outcome) {
        self.events[kind_index(kind)][outcome.index()].fetch_add(1, Ordering::Relaxed);
    }

    /// Mencatat waktu dari handoff dibuat sampai jawaban diterapkan.
    ///
    /// Nilainya detik, dan hanya dicatat setelah jawaban benar-benar
    /// diterapkan. Handoff yang kedaluwarsa masuk [`Outcome::Expired`], bukan
    /// di sini: mencampur keduanya membuat rata-rata waktu jawab terlihat bagus
    /// padahal owner tidak pernah menjawab.
    pub fn observe_answer_seconds(&self, seconds: u64) {
        self.answer_sum.fetch_add(seconds, Ordering::Relaxed);
        self.answer_count.fetch_add(1, Ordering::Relaxed);
        for (index, limit) in BUCKETS.iter().enumerate() {
            if seconds <= *limit {
                self.buckets[index].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[must_use]
    pub fn events(&self, kind: Kind, outcome: Outcome) -> u64 {
        self.events[kind_index(kind)][outcome.index()].load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn answer_count(&self) -> u64 {
        self.answer_count.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn answer_sum(&self) -> u64 {
        self.answer_sum.load(Ordering::Relaxed)
    }

    /// Seluruh metrik dalam format teks Prometheus.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(2048);

        out.push_str("# TYPE captcha_events_total counter\n");
        for kind in Kind::ALL {
            for outcome in Outcome::ALL {
                let _ = writeln!(
                    out,
                    "captcha_events_total{{kind=\"{}\",outcome=\"{}\"}} {}",
                    kind.as_str(),
                    outcome.as_str(),
                    self.events(kind, outcome)
                );
            }
        }

        out.push_str("# TYPE handoff_time_to_answer_seconds histogram\n");
        for (index, limit) in BUCKETS.iter().enumerate() {
            let _ = writeln!(
                out,
                "handoff_time_to_answer_seconds_bucket{{le=\"{limit}\"}} {}",
                self.buckets[index].load(Ordering::Relaxed)
            );
        }
        let _ = writeln!(
            out,
            "handoff_time_to_answer_seconds_bucket{{le=\"+Inf\"}} {}",
            self.answer_count()
        );
        let _ = writeln!(
            out,
            "handoff_time_to_answer_seconds_sum {}",
            self.answer_sum()
        );
        let _ = writeln!(
            out,
            "handoff_time_to_answer_seconds_count {}",
            self.answer_count()
        );

        out
    }
}

const fn kind_index(kind: Kind) -> usize {
    match kind {
        Kind::Challenge => 0,
        Kind::Otp => 1,
        Kind::Login => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awal_semua_nol() {
        let counters = CaptchaCounters::new();
        for kind in Kind::ALL {
            for outcome in Outcome::ALL {
                assert_eq!(counters.events(kind, outcome), 0);
            }
        }
        assert_eq!(counters.answer_count(), 0);
    }

    #[test]
    fn kejadian_dihitung_per_kind_dan_outcome() {
        let counters = CaptchaCounters::new();
        counters.record(Kind::Challenge, Outcome::Detected);
        counters.record(Kind::Challenge, Outcome::Detected);
        counters.record(Kind::Challenge, Outcome::SolvedHandoff);

        assert_eq!(counters.events(Kind::Challenge, Outcome::Detected), 2);
        assert_eq!(counters.events(Kind::Challenge, Outcome::SolvedHandoff), 1);
        assert_eq!(counters.events(Kind::Otp, Outcome::Detected), 0);
    }

    #[test]
    fn render_memuat_label_yang_dipakai_dokumen() {
        let counters = CaptchaCounters::new();
        counters.record(Kind::Challenge, Outcome::Detected);
        let text = counters.render();

        assert!(text.contains("captcha_events_total{kind=\"challenge\",outcome=\"detected\"} 1"));
        assert!(
            text.contains("captcha_events_total{kind=\"challenge\",outcome=\"solved_reuse\"} 0")
        );
    }

    #[test]
    fn histogram_menghitung_ember_kumulatif() {
        let counters = CaptchaCounters::new();
        counters.observe_answer_seconds(10);
        counters.observe_answer_seconds(45);
        counters.observe_answer_seconds(2000);

        let text = counters.render();
        assert!(
            text.contains("handoff_time_to_answer_seconds_bucket{le=\"30\"} 1"),
            "{text}"
        );
        assert!(text.contains("handoff_time_to_answer_seconds_bucket{le=\"60\"} 2"));
        assert!(text.contains("handoff_time_to_answer_seconds_bucket{le=\"120\"} 2"));
        assert!(text.contains("handoff_time_to_answer_seconds_bucket{le=\"900\"} 2"));
        assert!(text.contains("handoff_time_to_answer_seconds_bucket{le=\"+Inf\"} 3"));
        assert!(text.contains("handoff_time_to_answer_seconds_sum 2055"));
        assert!(text.contains("handoff_time_to_answer_seconds_count 3"));
    }

    #[test]
    fn nama_label_stabil() {
        assert_eq!(Kind::Challenge.as_str(), "challenge");
        assert_eq!(Outcome::SolvedReuse.as_str(), "solved_reuse");
        assert_eq!(Outcome::SolvedHandoff.as_str(), "solved_handoff");
        assert_eq!(Outcome::Expired.as_str(), "expired");
    }
}
