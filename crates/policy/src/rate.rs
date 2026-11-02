//! Batas laju handoff, dari dokumen `03-CAPTCHA-OTP` §3.1.
//!
//! Batas ini punya alasan yang bukan estetika: membanjiri owner dengan
//! screenshot membuat satu-satunya manusia di jalur ini berhenti menjawab, dan
//! membanjiri situs dengan percobaan membuat IP-nya diblokir. Karena itu
//! batasnya ditegakkan sebelum notifikasi dikirim, bukan sesudah.
//!
//! Jam **disuntik** sebagai parameter, bukan dibaca dari `SystemTime` di dalam.
//! Tanpa itu, aturan "cooldown 10 menit" hanya bisa diuji dengan menunggu 10
//! menit, dan test yang menunggu tidak pernah dijalankan orang.

use std::collections::{HashMap, VecDeque};

use crate::select::CaptchaSection;

const HOUR: u64 = 3_600;
const DAY: u64 = 86_400;

/// Cooldown dipasang setelah tiga kegagalan beruntun pada host yang sama.
const FAIL_STREAK: u32 = 3;

/// Batas retry otomatis L1. Lebih dari sekali adalah pola yang memancing blokir.
const L1_MAX_RETRY: u32 = 1;

/// Batas yang berlaku, diambil dari `[captcha]` di config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandoffLimits {
    pub per_host_hour: u32,
    pub total_day: u32,
    pub cooldown_secs: u64,
}

impl Default for HandoffLimits {
    fn default() -> Self {
        Self {
            per_host_hour: 5,
            total_day: 20,
            cooldown_secs: 600,
        }
    }
}

impl From<&CaptchaSection> for HandoffLimits {
    fn from(section: &CaptchaSection) -> Self {
        Self {
            per_host_hour: section.handoff_per_host_hour,
            total_day: section.handoff_total_day,
            cooldown_secs: section.cooldown_secs,
        }
    }
}

/// Jawaban atas pertanyaan "boleh kirim handoff sekarang?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allow {
    Yes,
    HostLimit { used: u32, limit: u32 },
    DayLimit { used: u32, limit: u32 },
    Cooldown { seconds_left: u64 },
}

impl Allow {
    #[must_use]
    pub const fn is_yes(self) -> bool {
        matches!(self, Self::Yes)
    }

    /// Alasan yang bisa dicatat di `captcha_event.detail`.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Yes => "ok",
            Self::HostLimit { .. } => "host_limit",
            Self::DayLimit { .. } => "day_limit",
            Self::Cooldown { .. } => "cooldown",
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Streak {
    failures: u32,
    cooldown_until: u64,
}

/// State batas laju. Satu instance per proses daemon.
#[derive(Debug)]
pub struct RateState {
    limits: HandoffLimits,
    per_host: HashMap<String, VecDeque<u64>>,
    total: VecDeque<u64>,
    streaks: HashMap<String, Streak>,
}

impl RateState {
    #[must_use]
    pub fn new(limits: HandoffLimits) -> Self {
        Self {
            limits,
            per_host: HashMap::new(),
            total: VecDeque::new(),
            streaks: HashMap::new(),
        }
    }

    #[must_use]
    pub fn limits(&self) -> HandoffLimits {
        self.limits
    }

    /// Memeriksa tanpa mengubah apa pun. Membuang catatan yang sudah basi
    /// dilakukan di sini supaya jawaban `used` selalu berarti "dalam jendela
    /// waktu", bukan "sejak daemon menyala".
    pub fn check(&mut self, host: &str, now: u64) -> Allow {
        self.prune(now);

        if let Some(streak) = self.streaks.get(host)
            && streak.cooldown_until > now
        {
            return Allow::Cooldown {
                seconds_left: streak.cooldown_until - now,
            };
        }

        let used_host = self.per_host.get(host).map_or(0, VecDeque::len) as u32;
        if used_host >= self.limits.per_host_hour {
            return Allow::HostLimit {
                used: used_host,
                limit: self.limits.per_host_hour,
            };
        }

        let used_day = self.total.len() as u32;
        if used_day >= self.limits.total_day {
            return Allow::DayLimit {
                used: used_day,
                limit: self.limits.total_day,
            };
        }

        Allow::Yes
    }

    /// Mencatat handoff yang benar-benar dikirim. Dipanggil hanya setelah
    /// [`Allow::Yes`], supaya catatan ini tidak pernah lebih besar dari
    /// kenyataan.
    pub fn record(&mut self, host: &str, now: u64) {
        self.prune(now);
        self.per_host
            .entry(host.to_owned())
            .or_default()
            .push_back(now);
        self.total.push_back(now);
    }

    /// Kegagalan beruntun pada host yang sama memasang cooldown.
    pub fn record_failure(&mut self, host: &str, now: u64) {
        let streak = self.streaks.entry(host.to_owned()).or_default();
        streak.failures += 1;
        if streak.failures >= FAIL_STREAK && self.limits.cooldown_secs > 0 {
            streak.cooldown_until = now + self.limits.cooldown_secs;
            // Streak di-reset setelah cooldown dipasang: cooldown-nya sendiri
            // yang menahan percobaan berikutnya, bukan hitungan lama.
            streak.failures = 0;
        }
    }

    /// Keberhasilan mengosongkan streak, karena cooldown untuk host yang sudah
    /// lolos hanya menghukum percobaan yang sebenarnya berhasil.
    pub fn record_success(&mut self, host: &str) {
        self.streaks.remove(host);
    }

    fn prune(&mut self, now: u64) {
        let batas_jam = now.saturating_sub(HOUR);
        let batas_hari = now.saturating_sub(DAY);

        self.per_host.retain(|_, catatan| {
            while catatan.front().is_some_and(|waktu| *waktu <= batas_jam) {
                catatan.pop_front();
            }
            !catatan.is_empty()
        });

        while self.total.front().is_some_and(|waktu| *waktu <= batas_hari) {
            self.total.pop_front();
        }
    }
}

/// Retry otomatis L1 hanya sekali per navigasi.
#[must_use]
pub const fn l1_retry_allowed(attempts: u32) -> bool {
    attempts < L1_MAX_RETRY
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> RateState {
        RateState::new(HandoffLimits::default())
    }

    #[test]
    fn batas_per_host_lima_per_jam() {
        let mut state = state();
        let now = 1_000_000;
        for i in 0..5 {
            assert!(
                state.check("situs.test", now + i).is_yes(),
                "ke-{i} harus boleh"
            );
            state.record("situs.test", now + i);
        }
        assert_eq!(
            state.check("situs.test", now + 10),
            Allow::HostLimit { used: 5, limit: 5 }
        );
        // Host lain tetap boleh.
        assert!(state.check("lain.test", now + 10).is_yes());
    }

    #[test]
    fn catatan_basi_dibuang_setelah_sejam() {
        let mut state = state();
        let now = 1_000_000;
        for i in 0..5 {
            state.record("situs.test", now + i);
        }
        assert!(!state.check("situs.test", now + 10).is_yes());
        assert!(
            state.check("situs.test", now + HOUR + 10).is_yes(),
            "catatan lebih dari sejam tidak boleh lagi dihitung"
        );
    }

    #[test]
    fn batas_total_dua_puluh_per_hari() {
        let mut state = state();
        let now = 5_000_000;
        for i in 0..20 {
            // Host berbeda supaya batas per host tidak lebih dulu kena.
            let host = format!("situs-{i}.test");
            assert!(state.check(&host, now + i).is_yes());
            state.record(&host, now + i);
        }
        assert_eq!(
            state.check("situs-baru.test", now + 30),
            Allow::DayLimit {
                used: 20,
                limit: 20
            }
        );
    }

    #[test]
    fn cooldown_setelah_tiga_gagal_beruntun() {
        let mut state = state();
        let now = 2_000_000;
        state.record_failure("situs.test", now);
        state.record_failure("situs.test", now + 1);
        assert!(
            state.check("situs.test", now + 2).is_yes(),
            "dua kegagalan belum memasang cooldown"
        );
        state.record_failure("situs.test", now + 2);

        assert_eq!(
            state.check("situs.test", now + 3),
            Allow::Cooldown { seconds_left: 599 }
        );
        assert!(
            state.check("situs.test", now + 2 + 600).is_yes(),
            "cooldown sepuluh menit sudah lewat"
        );
        // Host lain tidak terkena.
        assert!(state.check("lain.test", now + 3).is_yes());
    }

    #[test]
    fn keberhasilan_mengosongkan_streak() {
        let mut state = state();
        let now = 3_000_000;
        state.record_failure("situs.test", now);
        state.record_failure("situs.test", now + 1);
        state.record_success("situs.test");
        state.record_failure("situs.test", now + 2);
        assert!(
            state.check("situs.test", now + 3).is_yes(),
            "satu kegagalan setelah sukses bukan tiga beruntun"
        );
    }

    #[test]
    fn retry_l1_hanya_sekali() {
        assert!(l1_retry_allowed(0));
        assert!(!l1_retry_allowed(1));
        assert!(!l1_retry_allowed(2));
    }

    #[test]
    fn limit_dari_config_dipakai_apa_adanya() {
        let section = CaptchaSection {
            handoff_per_host_hour: 1,
            handoff_total_day: 2,
            cooldown_secs: 60,
        };
        let mut state = RateState::new(HandoffLimits::from(&section));
        let now = 10;
        assert!(state.check("a.test", now).is_yes());
        state.record("a.test", now);
        assert_eq!(
            state.check("a.test", now + 1),
            Allow::HostLimit { used: 1, limit: 1 }
        );
        assert_eq!(state.limits().cooldown_secs, 60);
    }

    #[test]
    fn alasan_masuk_ke_detail_yang_bisa_dibaca() {
        assert_eq!(Allow::Yes.reason(), "ok");
        assert_eq!(Allow::Cooldown { seconds_left: 5 }.reason(), "cooldown");
    }
}
