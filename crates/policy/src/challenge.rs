//! Menjawab satu pertanyaan: halaman yang sedang terbuka ini tantangan, bukan?
//!
//! Sinyalnya sudah ditetapkan di dokumen `03-CAPTCHA-OTP` §2. Yang dijaga di
//! sini adalah dua hal yang gampang salah:
//!
//! 1. **Tidak ada keputusan dari satu sinyal lemah.** Status 403 sendirian itu
//!    bisa berarti apa saja, jadi bobot 2 tidak cukup.
//! 2. **Yang dikembalikan bukan isi halaman.** [`DomFinding`] hanya berisi
//!    nama sinyal. Isi halaman bisa memuat token sesi, dan log yang memuatnya
//!    adalah kebocoran permanen.

use serde::{Deserialize, Serialize};

/// Sinyal yang mungkin terlihat, beserta bobotnya dari dokumen 03 §2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Signal {
    /// S1: dokumen utama 403, 429, atau 503.
    DocumentStatus,
    /// S2: URL akhir berpola tantangan.
    ChallengeUrl,
    /// S3: DOM punya penanda tantangan Cloudflare.
    CloudflareDom,
    /// S4: DOM punya widget captcha.
    CaptchaWidget,
    /// S5: teks halaman memuat kalimat tantangan.
    ChallengeText,
    /// S6: global `turnstile`/`grecaptcha` ada.
    CaptchaGlobal,
    /// S7: selector yang diharapkan untuk host ini tidak muncul.
    ExpectedMissing,
    /// S8: `cf_clearance` hilang padahal profil menyimpannya.
    ClearanceMissing,
}

impl Signal {
    /// Bobot menurut dokumen 03 §2. Angka ini yang dipakai, bukan angka lain
    /// yang terlihat lebih masuk akal.
    #[must_use]
    pub const fn weight(self) -> u8 {
        match self {
            Self::DocumentStatus | Self::ChallengeText | Self::ExpectedMissing => 2,
            Self::ChallengeUrl | Self::CloudflareDom | Self::CaptchaWidget => 3,
            Self::CaptchaGlobal | Self::ClearanceMissing => 1,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DocumentStatus => "s1_document_status",
            Self::ChallengeUrl => "s2_challenge_url",
            Self::CloudflareDom => "s3_cloudflare_dom",
            Self::CaptchaWidget => "s4_captcha_widget",
            Self::ChallengeText => "s5_challenge_text",
            Self::CaptchaGlobal => "s6_captcha_global",
            Self::ExpectedMissing => "s7_expected_missing",
            Self::ClearanceMissing => "s8_clearance_missing",
        }
    }

    /// Nama pendek yang dipakai [`JS_DETECT`] dan parser hasilnya.
    #[must_use]
    pub fn from_dom_label(label: &str) -> Option<Self> {
        match label.trim() {
            "cf_dom" => Some(Self::CloudflareDom),
            "captcha_dom" => Some(Self::CaptchaWidget),
            "challenge_text" => Some(Self::ChallengeText),
            "captcha_global" => Some(Self::CaptchaGlobal),
            _ => None,
        }
    }
}

/// Hasil satu kali `Runtime.evaluate` terhadap [`JS_DETECT`].
///
/// Semua field berbentuk boolean. Tidak ada teks halaman, tidak ada URL, tidak
/// ada token: yang dikembalikan skrip hanya nama sinyal yang terlihat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomFinding {
    #[serde(default)]
    pub challenge_form: bool,
    #[serde(default)]
    pub captcha_widget: bool,
    #[serde(default)]
    pub challenge_text: bool,
    #[serde(default)]
    pub captcha_global: bool,
}

impl DomFinding {
    /// Membaca daftar label sinyal yang dikembalikan [`JS_DETECT`].
    #[must_use]
    pub fn from_labels(labels: &[String]) -> Self {
        let mut out = Self::default();
        for label in labels {
            match Signal::from_dom_label(label) {
                Some(Signal::CloudflareDom) => out.challenge_form = true,
                Some(Signal::CaptchaWidget) => out.captcha_widget = true,
                Some(Signal::ChallengeText) => out.challenge_text = true,
                Some(Signal::CaptchaGlobal) => out.captcha_global = true,
                // Label asing diabaikan, bukan dianggap tantangan: menaikkan
                // bobot dari label yang tidak dikenal adalah cara termudah
                // membuat deteksi salah terus.
                _ => {}
            }
        }
        out
    }

    /// Membaca JSON array string hasil `JSON.stringify(signals)`.
    #[must_use]
    pub fn parse_json(raw: &str) -> Option<Self> {
        let labels: Vec<String> = serde_json::from_str(raw).ok()?;
        Some(Self::from_labels(&labels))
    }
}

/// Semua yang diamati untuk satu navigasi. Tidak ada field tebakan.
#[derive(Debug, Clone, Default)]
pub struct Evidence {
    /// Status dokumen utama dari `Network.responseReceived`.
    pub document_status: Option<u16>,
    /// URL akhir dari `Page.frameNavigated` (frame utama saja).
    pub final_url: Option<String>,
    /// Hasil probe DOM. `None` berarti probe tidak jalan, dan itu bukan bukti
    /// tidak ada tantangan.
    pub dom: Option<DomFinding>,
    /// Selector yang diharapkan untuk host ini tidak muncul.
    pub expected_missing: bool,
    /// Profil menyimpan `cf_clearance` tapi mesin tidak lagi punya.
    pub clearance_missing: bool,
}

impl Evidence {
    fn signals(&self) -> Vec<Signal> {
        let mut out = Vec::new();
        if self.document_status.is_some_and(is_challenge_status) {
            out.push(Signal::DocumentStatus);
        }
        if self
            .final_url
            .as_deref()
            .is_some_and(looks_like_challenge_url)
        {
            out.push(Signal::ChallengeUrl);
        }
        if let Some(dom) = self.dom {
            if dom.challenge_form {
                out.push(Signal::CloudflareDom);
            }
            if dom.captcha_widget {
                out.push(Signal::CaptchaWidget);
            }
            if dom.challenge_text {
                out.push(Signal::ChallengeText);
            }
            if dom.captcha_global {
                out.push(Signal::CaptchaGlobal);
            }
        }
        if self.expected_missing {
            out.push(Signal::ExpectedMissing);
        }
        if self.clearance_missing {
            out.push(Signal::ClearanceMissing);
        }
        out
    }
}

/// Seberapa yakin keputusannya.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    None,
    Low,
    Medium,
    High,
}

impl Confidence {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Hasil penilaian satu navigasi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub is_challenge: bool,
    pub weight: u8,
    pub signals: Vec<Signal>,
    pub confidence: Confidence,
}

impl Verdict {
    /// Label sinyal untuk log dan `captcha_event.detail`. Aman dicatat: isinya
    /// nama sinyal, bukan isi halaman.
    #[must_use]
    pub fn signal_labels(&self) -> Vec<&'static str> {
        self.signals.iter().map(|signal| signal.as_str()).collect()
    }
}

/// Aturan keputusan dari dokumen 03 §2, apa adanya.
///
/// - Tantangan kalau bobot total >= 3 **dan** minimal dua sinyal berbeda.
/// - Satu sinyal berbobot 3 sendirian tetap tantangan, dengan `confidence=medium`.
/// - Selain itu bukan tantangan, dan sinyal yang ada tetap dilaporkan supaya
///   ambang batasnya bisa ditinjau dari data nyata.
#[must_use]
pub fn verdict(evidence: &Evidence) -> Verdict {
    let signals = evidence.signals();
    let weight: u8 = signals.iter().map(|signal| signal.weight()).sum();

    let (is_challenge, confidence) = match signals.len() {
        0 => (false, Confidence::None),
        1 => {
            if weight >= 3 {
                (true, Confidence::Medium)
            } else {
                (false, Confidence::Low)
            }
        }
        _ => {
            if weight >= 3 {
                let confidence = if weight >= 6 {
                    Confidence::High
                } else {
                    Confidence::Medium
                };
                (true, confidence)
            } else {
                (false, Confidence::Low)
            }
        }
    };

    Verdict {
        is_challenge,
        weight,
        signals,
        confidence,
    }
}

#[must_use]
pub const fn is_challenge_status(status: u16) -> bool {
    matches!(status, 403 | 429 | 503)
}

/// Pola URL tantangan dari dokumen 03 §2 (S2).
#[must_use]
pub fn looks_like_challenge_url(url: &str) -> bool {
    let lowered = url.to_ascii_lowercase();
    lowered.contains("__cf_chl")
        || lowered.contains("/cdn-cgi/challenge-platform/")
        || lowered.contains("challenge")
}

/// Satu ekspresi yang dijalankan lewat `Runtime.evaluate`.
///
/// Syarat bentuknya, dan ketiganya punya alasan:
/// - **satu ekspresi**, karena setiap round-trip tambahan memperlambat deteksi
///   tepat saat halaman sedang menahan kita;
/// - **mengembalikan nama sinyal saja**, karena isi halaman tidak boleh masuk
///   log, metrik, maupun jawaban HTTP;
/// - **tidak melempar galat**, karena halaman tantangan sering memblokir
///   `document.body` atau memasang getter yang melempar.
pub const JS_DETECT: &str = r#"(() => {
  const s = [];
  try {
    if (document.querySelector('#challenge-form, #cf-challenge-running')) s.push('cf_dom');
    if (document.querySelector('iframe[src*="challenges.cloudflare.com"]')) s.push('cf_dom');
  } catch (e) {}
  try {
    if (document.querySelector('.g-recaptcha, [data-sitekey], iframe[src*="hcaptcha.com"]')) s.push('captcha_dom');
  } catch (e) {}
  try {
    const t = (document.body && document.body.innerText ? document.body.innerText : '').slice(0, 4000).toLowerCase();
    if (['just a moment', 'verify you are human', 'checking your browser', 'unusual traffic', 'verifikasi anda'].some((k) => t.includes(k))) s.push('challenge_text');
  } catch (e) {}
  try {
    if (window.turnstile || window.grecaptcha) s.push('captcha_global');
  } catch (e) {}
  return JSON.stringify(s);
})()"#;

/// Tingkat eskalasi dari dokumen 03 §3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Escalation {
    /// Pakai ulang cookie dan UA profil: tanpa aksi tambahan.
    Reuse,
    /// Ulang sekali dengan `chrome-full`.
    RetryFull,
    /// Jalur artefak `cloakbrowser`, ambil cookie hasilnya.
    CloakArtifact,
    /// Handoff ke owner.
    Handoff,
    /// Solver berbayar: **tidak diimplementasikan di v0**.
    Solver,
}

impl Escalation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reuse => "l0_reuse",
            Self::RetryFull => "l1_retry_full",
            Self::CloakArtifact => "l2_cloak",
            Self::Handoff => "l3_handoff",
            Self::Solver => "l4_solver",
        }
    }
}

/// Mesin mana yang benar-benar tersedia hari ini.
#[derive(Debug, Clone, Copy, Default)]
pub struct LadderAvailability {
    pub full: bool,
    pub cloak: bool,
}

/// Naik satu tingkat setelah kegagalan.
///
/// Aturan dari dokumen: naik satu tingkat per kegagalan, tidak pernah melompat,
/// dan setelah L2 gagal berhenti di handoff. Satu pengecualian yang disengaja:
/// tingkat yang mesinnya **tidak terpasang** dilewati, dan yang dilewati
/// dicatat oleh pemanggil. Melompati mesin yang tidak ada bukan eskalasi cepat,
/// itu satu-satunya jalan supaya permintaan tidak menggantung menunggu biner
/// yang tidak pernah dipasang.
#[must_use]
pub fn next_after_failure(current: Escalation, available: LadderAvailability) -> Escalation {
    match current {
        Escalation::Reuse => first_available(
            &[
                (Escalation::RetryFull, available.full),
                (Escalation::CloakArtifact, available.cloak),
                (Escalation::Handoff, true),
            ],
            Escalation::Handoff,
        ),
        Escalation::RetryFull => first_available(
            &[
                (Escalation::CloakArtifact, available.cloak),
                (Escalation::Handoff, true),
            ],
            Escalation::Handoff,
        ),
        // Dokumen 03 §3: setelah L2 gagal, berhenti dan handoff. Mengulang L2
        // berkali-kali adalah pola yang membuat IP diblokir.
        Escalation::CloakArtifact | Escalation::Handoff | Escalation::Solver => Escalation::Handoff,
    }
}

fn first_available(candidates: &[(Escalation, bool)], otherwise: Escalation) -> Escalation {
    candidates
        .iter()
        .find_map(|(step, ok)| ok.then_some(*step))
        .unwrap_or(otherwise)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dom() -> DomFinding {
        DomFinding::default()
    }

    #[test]
    fn status_403_sendirian_bukan_tantangan() {
        let verdict = verdict(&Evidence {
            document_status: Some(403),
            ..Evidence::default()
        });
        assert!(!verdict.is_challenge);
        assert_eq!(verdict.weight, 2);
        assert_eq!(verdict.confidence, Confidence::Low);
    }

    #[test]
    fn url_tantangan_sendirian_bobot_tiga_confidence_medium() {
        let verdict = verdict(&Evidence {
            final_url: Some("https://situs.test/cdn-cgi/challenge-platform/h/b".to_owned()),
            ..Evidence::default()
        });
        assert!(verdict.is_challenge);
        assert_eq!(verdict.weight, 3);
        assert_eq!(verdict.confidence, Confidence::Medium);
        assert_eq!(verdict.signals, vec![Signal::ChallengeUrl]);
    }

    #[test]
    fn status_dan_url_dua_sinyal_bobot_lima() {
        let verdict = verdict(&Evidence {
            document_status: Some(503),
            final_url: Some("https://situs.test/__cf_chl_tk/abc".to_owned()),
            ..Evidence::default()
        });
        assert!(verdict.is_challenge);
        assert_eq!(verdict.weight, 5);
        assert_eq!(verdict.confidence, Confidence::Medium);
        assert_eq!(verdict.signals.len(), 2);
    }

    #[test]
    fn dom_tantangan_dan_teks_dua_sinyal_bobot_lima() {
        let finding = DomFinding {
            challenge_form: true,
            challenge_text: true,
            ..dom()
        };
        let verdict = verdict(&Evidence {
            dom: Some(finding),
            ..Evidence::default()
        });
        assert!(verdict.is_challenge);
        assert_eq!(verdict.weight, 5);
        assert_eq!(
            verdict.signals,
            vec![Signal::CloudflareDom, Signal::ChallengeText]
        );
    }

    #[test]
    fn sinyal_lemah_bertiga_belum_cukup() {
        let finding = DomFinding {
            captcha_global: true,
            ..dom()
        };
        let verdict = verdict(&Evidence {
            dom: Some(finding),
            clearance_missing: true,
            ..Evidence::default()
        });
        assert!(!verdict.is_challenge, "bobot 2 masih di bawah ambang");
        assert_eq!(verdict.weight, 2);
    }

    #[test]
    fn bobot_tinggi_dari_banyak_sinyal_confidence_high() {
        let finding = DomFinding {
            challenge_form: true,
            captcha_widget: true,
            challenge_text: true,
            captcha_global: true,
        };
        let verdict = verdict(&Evidence {
            document_status: Some(403),
            final_url: Some("https://situs.test/challenge".to_owned()),
            dom: Some(finding),
            ..Evidence::default()
        });
        assert!(verdict.is_challenge);
        assert_eq!(verdict.weight, 3 + 3 + 3 + 2 + 1 + 2);
        assert_eq!(verdict.confidence, Confidence::High);
        assert_eq!(verdict.signals.len(), 6);
    }

    #[test]
    fn halaman_normal_tanpa_sinyal_bukan_tantangan() {
        let verdict = verdict(&Evidence {
            document_status: Some(200),
            final_url: Some("https://situs.test/beranda".to_owned()),
            dom: Some(dom()),
            ..Evidence::default()
        });
        assert!(!verdict.is_challenge);
        assert!(verdict.signals.is_empty());
        assert_eq!(verdict.confidence, Confidence::None);
    }

    #[test]
    fn selector_yang_hilang_sendirian_belum_cukup() {
        let verdict = verdict(&Evidence {
            expected_missing: true,
            ..Evidence::default()
        });
        assert!(!verdict.is_challenge);
        assert_eq!(verdict.weight, 2);
    }

    #[test]
    fn selector_hilang_plus_global_cukup_tiga() {
        let verdict = verdict(&Evidence {
            expected_missing: true,
            clearance_missing: true,
            ..Evidence::default()
        });
        assert!(verdict.is_challenge);
        assert_eq!(verdict.weight, 3);
        assert_eq!(verdict.confidence, Confidence::Medium);
    }

    #[test]
    fn label_dom_asing_diabaikan() {
        let finding = DomFinding::from_labels(&["sesuatu_baru".to_owned(), "cf_dom".to_owned()]);
        assert!(finding.challenge_form);
        assert!(!finding.challenge_text);
    }

    #[test]
    fn json_hasil_js_detect_terbaca() {
        let finding = DomFinding::parse_json(r#"["cf_dom","challenge_text"]"#)
            .expect("array label harus terbaca");
        assert!(finding.challenge_form);
        assert!(finding.challenge_text);
        assert!(!finding.captcha_widget);
    }

    #[test]
    fn json_rusak_tidak_menjadi_tantangan() {
        assert!(DomFinding::parse_json("bukan json").is_none());
        assert!(DomFinding::parse_json(r#"{"a":1}"#).is_none());
    }

    #[test]
    fn pola_url_sesuai_dokumen() {
        assert!(looks_like_challenge_url("https://a.test/__cf_chl_rt_tk=x"));
        assert!(looks_like_challenge_url(
            "https://a.test/cdn-cgi/challenge-platform/h/g/orchestrate"
        ));
        assert!(looks_like_challenge_url("https://a.test/Challenge/verify"));
        assert!(!looks_like_challenge_url("https://a.test/produk/123"));
    }

    #[test]
    fn status_tantangan_hanya_tiga_nilai() {
        for status in [403_u16, 429, 503] {
            assert!(is_challenge_status(status));
        }
        for status in [200_u16, 301, 401, 500] {
            assert!(!is_challenge_status(status));
        }
    }

    #[test]
    fn tangga_naik_satu_tingkat_dan_berhenti_di_handoff() {
        let semua = LadderAvailability {
            full: true,
            cloak: true,
        };
        assert_eq!(
            next_after_failure(Escalation::Reuse, semua),
            Escalation::RetryFull
        );
        assert_eq!(
            next_after_failure(Escalation::RetryFull, semua),
            Escalation::CloakArtifact
        );
        assert_eq!(
            next_after_failure(Escalation::CloakArtifact, semua),
            Escalation::Handoff
        );
        assert_eq!(
            next_after_failure(Escalation::Handoff, semua),
            Escalation::Handoff,
            "tidak ada solver otomatis di v0"
        );
    }

    #[test]
    fn tingkat_tanpa_mesin_dilewati() {
        let cuma_handoff = LadderAvailability::default();
        assert_eq!(
            next_after_failure(Escalation::Reuse, cuma_handoff),
            Escalation::Handoff
        );
    }

    #[test]
    fn label_sinyal_aman_untuk_log() {
        let verdict = verdict(&Evidence {
            final_url: Some("https://a.test/challenge".to_owned()),
            ..Evidence::default()
        });
        assert_eq!(verdict.signal_labels(), vec!["s2_challenge_url"]);
    }

    #[test]
    fn skrip_deteksi_tidak_mengembalikan_isi_halaman() {
        // Penjaga sederhana: skrip hanya boleh mengembalikan label sinyal.
        assert!(JS_DETECT.contains("JSON.stringify(s)"));
        assert!(!JS_DETECT.contains("innerText.slice") || JS_DETECT.contains("slice(0, 4000)"));
        assert!(!JS_DETECT.contains("document.cookie"));
    }
}
