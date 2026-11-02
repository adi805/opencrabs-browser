//! Pengamat frame CDP: membaca tanda tantangan, memutuskan, lalu menyiapkan
//! bahan handoff.
//!
//! Tiga hal yang sengaja dipisah di sini, karena ketiganya diuji dengan cara
//! berbeda:
//!
//! - [`parse_message`] hanya membaca JSON jadi [`Observed`]. Tidak menyimpan
//!   apa pun, jadi bisa diuji tanpa soket.
//! - [`Tracking`] menyimpan jejak per target dan memutuskan kapan menjalankan
//!   probe dan kapan menyerahkan ke owner. Waktu tidak diambil di dalamnya.
//! - [`ChallengeObserver`] menempelkan keduanya ke jalur panas proxy lewat
//!   [`cdp::FrameObserver`]. Dia hanya mengantre; pekerjaan yang menunggu
//!   jaringan (`Runtime.evaluate`) dilakukan pekerja terpisah supaya frame
//!   tidak pernah tertahan oleh halaman yang sedang dicek.
//!
//! Yang **tidak** dilakukan pengamat: menyimpan isi halaman, menaruh URL
//! lengkap di metrik, atau mengirim jawaban sendiri. Bukti yang dikumpulkan
//! hanya status, URL akhir, dan nama sinyal DOM.

use std::collections::HashMap;
use std::sync::Mutex;

use cdp::FrameObserver;
use control::registry::{Kind as HandoffKind, NewHandoff};
use policy::challenge::{
    self, Confidence, DomFinding, Escalation, Evidence, JS_DETECT, LadderAvailability, Verdict,
};
use policy::counters::{CaptchaCounters, Kind as CounterKind, Outcome};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

/// Satu kejadian yang dibaca dari pesan mesin.
///
/// Hanya tiga bentuk yang dipakai. Sisanya bukan urusan pengamat, dan
/// mengubah seluruh pesan CDP menjadi struktur akan menggandakan biaya jalur
/// panas untuk informasi yang tidak pernah dibaca.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    /// Sesi baru menempel ke sebuah target.
    Attached {
        session_id: String,
        target_id: String,
    },
    /// Respons dokumen utama.
    Document {
        session_id: String,
        status: u16,
        url: String,
    },
    /// Navigasi frame utama selesai.
    Navigated { session_id: String, url: String },
}

/// Membaca satu pesan teks CDP menjadi [`Observed`].
///
/// `None` berarti pesan itu bukan kejadian yang dipantau: balasan perintah,
/// `Target.targetCreated`, atau event frame anak. Frame anak sengaja dibuang
/// karena tantangan selalu mendarat di dokumen utama; mempercayai frame iklan
/// akan membuat halaman normal terlihat seperti tantangan.
#[must_use]
pub fn parse_message(text: &str) -> Option<Observed> {
    let pesan: Value = serde_json::from_str(text).ok()?;
    let method = pesan.get("method")?.as_str()?;
    let params = pesan.get("params")?;

    match method {
        "Target.attachedToTarget" => Some(Observed::Attached {
            session_id: params.get("sessionId")?.as_str()?.to_owned(),
            target_id: params
                .get("targetInfo")?
                .get("targetId")?
                .as_str()?
                .to_owned(),
        }),
        "Network.responseReceived" => {
            if params.get("type")?.as_str()? != "Document" {
                return None;
            }
            let response = params.get("response")?;
            Some(Observed::Document {
                session_id: params.get("sessionId")?.as_str()?.to_owned(),
                status: u16::try_from(response.get("status")?.as_u64()?).ok()?,
                url: response.get("url")?.as_str()?.to_owned(),
            })
        }
        "Page.frameNavigated" => {
            let frame = params.get("frame")?;
            // Frame anak punya `parentId`. Navigasi di dalam iframe bukan
            // navigasi halaman, dan URL-nya sering berisi kata "challenge"
            // milik skrip iklan.
            if frame.get("parentId").is_some() {
                return None;
            }
            Some(Observed::Navigated {
                session_id: params.get("sessionId")?.as_str()?.to_owned(),
                url: frame.get("url")?.as_str()?.to_owned(),
            })
        }
        _ => None,
    }
}

/// Permintaan menjalankan probe DOM pada satu target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub session_id: String,
    pub target_id: String,
    pub url: String,
    pub evidence: Evidence,
}

/// Apa yang harus dilakukan setelah satu navigasi dinilai.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Bukan tantangan, atau bukti belum cukup untuk menyebutnya begitu.
    Ignore,
    /// Status/URL mencurigakan: jalankan probe DOM sebelum memutuskan.
    Probe,
    /// Bukti sudah cukup kuat: serahkan ke owner.
    Handoff,
}

/// Apakah bukti mengandung petunjuk yang layak diperiksa lebih dalam.
///
/// Sengaja lebih longgar daripada [`challenge::verdict`]. Status 403 berbobot
/// 2 (S1) dan URL tantangan berbobot 3 (S2), jadi satu status aneh **tidak**
/// cukup untuk menyebutnya tantangan, tapi cukup untuk menjalankan probe.
/// Menunggu sampai bobotnya melewati ambang berarti probe tidak pernah jalan
/// untuk kasus yang justru paling sering terjadi.
#[must_use]
pub fn suspicious(evidence: &Evidence) -> bool {
    evidence
        .document_status
        .is_some_and(challenge::is_challenge_status)
        || evidence
            .final_url
            .as_deref()
            .is_some_and(challenge::looks_like_challenge_url)
}

/// Keputusan dari bukti yang ada sekarang.
///
/// Aturannya: selama probe DOM belum jalan, sebuah tantangan yang sudah
/// melewati ambang **belum** diserahkan ke owner. Menyerahkan handoff dari
/// status 403 saja akan memanggil owner untuk halaman yang sebenarnya cuma
/// butuh cookie baru. Sebaliknya, begitu probe selesai dan hasilnya tetap
/// tantangan, menunggu lebih lama hanya memperpanjang tab tertahan.
#[must_use]
pub fn action(evidence: &Evidence) -> Action {
    if evidence.dom.is_none() {
        return if suspicious(evidence) {
            Action::Probe
        } else {
            Action::Ignore
        };
    }
    let putusan = challenge::verdict(evidence);
    if !putusan.is_challenge {
        return Action::Ignore;
    }
    match putusan.confidence {
        Confidence::High | Confidence::Medium => Action::Handoff,
        Confidence::None | Confidence::Low => Action::Ignore,
    }
}

/// Menyusun permintaan handoff dari bukti yang sudah dinilai.
///
/// `detail` hanya memuat nama sinyal dan status: itu yang dibaca owner untuk
/// memutuskan, dan tidak ada satu pun isi halaman di dalamnya.
#[must_use]
pub fn handoff_baru(
    session_id: &str,
    target_id: &str,
    evidence: &Evidence,
    putusan: &Verdict,
) -> NewHandoff {
    let sinyal = putusan.signal_labels().join(",");
    let status = evidence
        .document_status
        .map_or_else(|| "tidak-ada".to_owned(), |status| status.to_string());
    NewHandoff {
        session_id: session_id.to_owned(),
        target_id: target_id.to_owned(),
        kind: HandoffKind::Captcha,
        url: evidence.final_url.clone().unwrap_or_default(),
        screenshot: None,
        expected_selector: None,
        detail: format!(
            "sinyal={sinyal} status={status} bobot={} keyakinan={}",
            putusan.weight,
            putusan.confidence.as_str()
        ),
    }
}

/// Satu target yang sedang diamati.
#[derive(Debug, Clone)]
struct Track {
    session_id: String,
    evidence: Evidence,
    /// URL yang sudah diputuskan. Dipakai agar navigasi berulang ke halaman
    /// yang sama tidak membuat handoff kedua untuk masalah yang sama.
    decided_for: Option<String>,
    level: Escalation,
}

impl Track {
    fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_owned(),
            evidence: Evidence::default(),
            decided_for: None,
            level: Escalation::Reuse,
        }
    }
}

/// Jejak seluruh target, tanpa I/O.
#[derive(Debug, Default)]
pub struct Tracking {
    sessions: HashMap<String, String>,
    tracks: HashMap<String, Track>,
}

impl Tracking {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mencatat pemetaan sesi ke target. Tanpa ini, event `Network.*` tidak
    /// bisa dihubungkan ke target yang bisa dikirimi perintah.
    pub fn attach(&mut self, session_id: &str, target_id: &str) {
        self.sessions
            .insert(session_id.to_owned(), target_id.to_owned());
        self.tracks
            .entry(target_id.to_owned())
            .or_insert_with(|| Track::new(session_id));
    }

    /// Target id untuk sebuah sesi, kalau sudah pernah menempel.
    #[must_use]
    pub fn target_of(&self, session_id: &str) -> Option<&str> {
        self.sessions.get(session_id).map(String::as_str)
    }

    /// Tingkat eskalasi terakhir untuk satu target.
    #[must_use]
    pub fn level(&self, target_id: &str) -> Option<Escalation> {
        self.tracks.get(target_id).map(|track| track.level)
    }

    /// Menaikkan tingkat eskalasi dan mengembalikan tingkat barunya.
    ///
    /// Mesin yang tidak terpasang dilewati oleh
    /// [`challenge::next_after_failure`]; yang dilewati tetap dilaporkan
    /// pemanggil lewat [`LadderAvailability`], bukan disembunyikan di sini.
    pub fn escalate(
        &mut self,
        target_id: &str,
        available: LadderAvailability,
    ) -> Option<Escalation> {
        let track = self.tracks.get_mut(target_id)?;
        track.level = challenge::next_after_failure(track.level, available);
        Some(track.level)
    }

    /// Mencatat hasil probe DOM dan memutuskan langkah berikutnya.
    pub fn probed(&mut self, target_id: &str, dom: DomFinding) -> Action {
        let Some(track) = self.tracks.get_mut(target_id) else {
            return Action::Ignore;
        };
        track.evidence.dom = Some(dom);
        action(&track.evidence)
    }

    /// Memasukkan satu kejadian. Mengembalikan probe bila perlu dijalankan.
    pub fn observe(&mut self, observed: &Observed) -> Option<Probe> {
        match observed {
            Observed::Attached {
                session_id,
                target_id,
            } => {
                self.attach(session_id, target_id);
                None
            }
            Observed::Document {
                session_id,
                status,
                url,
            } => {
                let target_id = self.target_of(session_id)?.to_owned();
                let track = self.tracks.get_mut(&target_id)?;
                track.evidence.document_status = Some(*status);
                if track.evidence.final_url.is_none() {
                    track.evidence.final_url = Some(url.clone());
                }
                self.maybe_probe(&target_id, url)
            }
            Observed::Navigated { session_id, url } => {
                let target_id = self.target_of(session_id)?.to_owned();
                let track = self.tracks.get_mut(&target_id)?;
                track.evidence.final_url = Some(url.clone());
                self.maybe_probe(&target_id, url)
            }
        }
    }

    /// Memutuskan apakah navigasi ini perlu probe, dan menyiapkan buktinya.
    fn maybe_probe(&mut self, target_id: &str, url: &str) -> Option<Probe> {
        let track = self.tracks.get_mut(target_id)?;
        if track.decided_for.as_deref() == Some(url) {
            return None;
        }
        if action(&track.evidence) == Action::Ignore {
            return None;
        }
        track.decided_for = Some(url.to_owned());
        Some(Probe {
            session_id: track.session_id.clone(),
            target_id: target_id.to_owned(),
            url: url.to_owned(),
            evidence: track.evidence.clone(),
        })
    }
}

/// Pengamat yang dipasang ke proxy CDP.
///
/// [`FrameObserver::observe`] berjalan di jalur panas dan karena itu tidak
/// boleh menunggu: dia hanya mengunci peta kecil, membaca JSON, lalu mengirim
/// ke antrean. Antrean mati (pekerja probe sudah berhenti) dicatat sebagai
/// [`Outcome::Skipped`] supaya hilangnya deteksi terlihat di metrik, bukan
/// hilang diam-diam.
pub struct ChallengeObserver {
    tracking: Mutex<Tracking>,
    probe_tx: UnboundedSender<Probe>,
    counters: std::sync::Arc<CaptchaCounters>,
}

impl ChallengeObserver {
    #[must_use]
    pub fn new(
        probe_tx: UnboundedSender<Probe>,
        counters: std::sync::Arc<CaptchaCounters>,
    ) -> Self {
        Self {
            tracking: Mutex::new(Tracking::new()),
            probe_tx,
            counters,
        }
    }

    /// Memasukkan satu kejadian secara sinkron. Jalur `observe` memakai ini
    /// juga, jadi perilakunya satu dan tidak bisa berbeda antara uji dan
    /// produksi.
    pub fn ingest(&self, observed: &Observed) {
        let probe = {
            let mut tracking = self
                .tracking
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            tracking.observe(observed)
        };
        if let Some(probe) = probe {
            self.counters
                .record(CounterKind::Challenge, Outcome::Detected);
            if self.probe_tx.send(probe).is_err() {
                self.counters
                    .record(CounterKind::Challenge, Outcome::Skipped);
            }
        }
    }

    /// Membaca pesan tanpa memasukkannya. Dipakai pengujian parser.
    #[must_use]
    pub fn sidik(text: &str) -> Option<Observed> {
        parse_message(text)
    }
}

impl FrameObserver for ChallengeObserver {
    fn observe(&self, payload: &str) {
        if let Some(observed) = parse_message(payload) {
            self.ingest(&observed);
        }
    }
}

/// Skrip probe DOM, diambil dari crate policy supaya hanya ada satu versi.
#[must_use]
pub const fn probe_script() -> &'static str {
    JS_DETECT
}

/// Parameter `Runtime.evaluate` untuk probe.
#[must_use]
pub fn probe_params() -> Value {
    serde_json::json!({
        "expression": probe_script(),
        "returnByValue": true,
        "awaitPromise": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attached() -> Observed {
        Observed::Attached {
            session_id: "S1".to_owned(),
            target_id: "T1".to_owned(),
        }
    }

    fn dokumen(status: u16) -> Observed {
        Observed::Document {
            session_id: "S1".to_owned(),
            status,
            url: "https://x.test/a".to_owned(),
        }
    }

    #[test]
    fn hanya_tiga_kejadian_yang_dibaca() {
        let doc = r#"{"method":"Network.responseReceived","params":{"sessionId":"S1","type":"Document","response":{"status":403,"url":"https://x.test/a"}}}"#;
        assert_eq!(parse_message(doc), Some(dokumen(403)));

        assert_eq!(parse_message(r#"{"id":7,"result":{}}"#), None);

        let gambar = r#"{"method":"Network.responseReceived","params":{"sessionId":"S1","type":"Image","response":{"status":403,"url":"https://x.test/g"}}}"#;
        assert_eq!(parse_message(gambar), None);
    }

    #[test]
    fn frame_anak_diabaikan() {
        let anak = r#"{"method":"Page.frameNavigated","params":{"sessionId":"S1","frame":{"url":"https://ads.test/challenge","parentId":"F0"}}}"#;
        assert_eq!(parse_message(anak), None);

        let utama = r#"{"method":"Page.frameNavigated","params":{"sessionId":"S1","frame":{"url":"https://x.test/a"}}}"#;
        assert_eq!(
            parse_message(utama),
            Some(Observed::Navigated {
                session_id: "S1".to_owned(),
                url: "https://x.test/a".to_owned(),
            })
        );
    }

    #[test]
    fn status_tantangan_memicu_satu_probe_saja() {
        let mut tracking = Tracking::new();
        assert!(tracking.observe(&attached()).is_none());

        let probe = tracking
            .observe(&dokumen(403))
            .expect("probe pertama harus jalan");
        assert_eq!(probe.target_id, "T1");
        assert_eq!(probe.session_id, "S1");

        assert!(
            tracking.observe(&dokumen(403)).is_none(),
            "URL yang sama tidak boleh memicu probe kedua"
        );
    }

    #[test]
    fn navigasi_normal_tidak_memicu_probe() {
        let mut tracking = Tracking::new();
        tracking.observe(&attached());
        assert!(tracking.observe(&dokumen(200)).is_none());
    }

    #[test]
    fn probe_bersih_membatalkan_handoff() {
        let mut tracking = Tracking::new();
        tracking.observe(&attached());
        assert!(tracking.observe(&dokumen(403)).is_some());
        assert_eq!(tracking.probed("T1", DomFinding::default()), Action::Ignore);
    }

    #[test]
    fn probe_kotor_menyerahkan_ke_owner() {
        let mut tracking = Tracking::new();
        tracking.observe(&attached());
        assert!(tracking.observe(&dokumen(403)).is_some());
        let dom = DomFinding {
            challenge_form: true,
            ..DomFinding::default()
        };
        assert_eq!(tracking.probed("T1", dom), Action::Handoff);
    }

    #[test]
    fn eskalasi_melewati_mesin_yang_tidak_terpasang() {
        let mut tracking = Tracking::new();
        tracking.observe(&attached());
        assert_eq!(tracking.level("T1"), Some(Escalation::Reuse));
        let tingkat = tracking.escalate(
            "T1",
            LadderAvailability {
                full: false,
                cloak: false,
            },
        );
        assert_eq!(tingkat, Some(Escalation::Handoff));
    }

    #[test]
    fn target_asing_tidak_panik() {
        let mut tracking = Tracking::new();
        assert_eq!(tracking.probed("T9", DomFinding::default()), Action::Ignore);
        assert_eq!(tracking.escalate("T9", LadderAvailability::default()), None);
    }

    #[test]
    fn handoff_tidak_memuat_isi_halaman() {
        let evidence = Evidence {
            document_status: Some(403),
            final_url: Some("https://x.test/a".to_owned()),
            dom: Some(DomFinding {
                challenge_form: true,
                ..DomFinding::default()
            }),
            ..Evidence::default()
        };
        let putusan = challenge::verdict(&evidence);
        let new = handoff_baru("S1", "T1", &evidence, &putusan);
        assert_eq!(new.kind, HandoffKind::Captcha);
        assert_eq!(new.url, "https://x.test/a");
        assert!(
            new.detail.contains("s3_cloudflare_dom"),
            "detail: {}",
            new.detail
        );
        assert!(!new.detail.contains('<'));
    }

    #[test]
    fn antrean_mati_dicatat_sebagai_skipped() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);
        let counters = std::sync::Arc::new(CaptchaCounters::new());
        let observer = ChallengeObserver::new(tx, counters.clone());

        observer.ingest(&attached());
        observer.ingest(&dokumen(403));

        assert_eq!(
            counters.events(CounterKind::Challenge, Outcome::Detected),
            1
        );
        assert_eq!(counters.events(CounterKind::Challenge, Outcome::Skipped), 1);
    }

    #[test]
    fn observer_menelan_pesan_yang_tidak_relevan() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let observer = ChallengeObserver::new(tx, std::sync::Arc::new(CaptchaCounters::new()));
        observer.observe("bukan json");
        observer.observe(r#"{"method":"Page.loadEventFired","params":{}}"#);
        assert!(rx.try_recv().is_err());
    }
}
