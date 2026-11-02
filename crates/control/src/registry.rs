//! Daftar handoff yang menunggu manusia.
//!
//! Registry ini **tidak** menerapkan jawaban. Dia hanya menyimpan state,
//! memvalidasi transisi, dan menyerahkan jawaban ke antrean yang diproses
//! daemon. Pemisahan itu disengaja: registry bisa diuji tanpa browser, dan
//! jalur yang menyentuh CDP hanya ada di satu tempat.
//!
//! Waktu **disuntik** sebagai parameter di setiap method. Handoff punya batas
//! 15 dan 30 menit, dan test yang harus menunggu 15 menit tidak akan pernah
//! dijalankan.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Handoff ditahan 15 menit, lalu dilepas dari pin pada menit ke-30.
pub const HOLD_SECS: u64 = 900;
pub const RELEASE_SECS: u64 = 1_800;

/// Jenis handoff, sesuai kolom `kind` di tabel handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Captcha,
    Otp,
    Login,
}

impl Kind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Captcha => "captcha",
            Self::Otp => "otp",
            Self::Login => "login",
        }
    }
}

/// Jawaban owner, dalam bentuk yang dikirim agent lewat HTTP.
///
/// Bentuknya dari dokumen 03 §7.1, dan `type` adalah pembeda, bukan field
/// tambahan: agent yang salah menulis bentuk harus mendapat galat, bukan
/// jawaban yang diam-diam diabaikan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    Click {
        x: f64,
        y: f64,
    },
    Text {
        value: String,
        selector: Option<String>,
    },
    Keys {
        value: String,
    },
    Abort,
}

/// Yang dikirim ke daemon saat ada jawaban yang perlu diterapkan.
#[derive(Debug, Clone, PartialEq)]
pub struct ApplyRequest {
    pub id: String,
    pub target_id: String,
    pub answer: Answer,
}

/// Hasil penerapan, dilaporkan balik oleh daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    Solved,
    Failed { detail: String },
}

/// Keadaan satu handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Pending,
    Applying,
    Solved,
    Aborted,
    Expired,
    Failed,
}

impl State {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applying => "applying",
            Self::Solved => "solved",
            Self::Aborted => "aborted",
            Self::Expired => "expired",
            Self::Failed => "failed",
        }
    }

    const fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::Applying)
    }
}

/// Data yang dibutuhkan untuk membuat handoff. Semua field wajib diisi
/// pemanggil: tidak ada nilai bawaan yang menyembunyikan apa yang sebenarnya
/// dikirim ke owner.
#[derive(Debug, Clone)]
pub struct NewHandoff {
    pub session_id: String,
    pub target_id: String,
    pub kind: Kind,
    pub url: String,
    pub screenshot: Option<PathBuf>,
    pub expected_selector: Option<String>,
    pub detail: String,
}

/// Satu handoff, apa adanya.
#[derive(Debug, Clone)]
pub struct Handoff {
    pub id: String,
    pub session_id: String,
    pub target_id: String,
    pub kind: Kind,
    pub url: String,
    pub screenshot: Option<PathBuf>,
    pub expected_selector: Option<String>,
    pub detail: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub release_at: u64,
    pub state: State,
    pub answered_at: Option<u64>,
    pub fail_detail: Option<String>,
}

/// Bentuk yang dikirim ke agent. Path screenshot dikirim apa adanya supaya
/// agent bisa melampirkannya; isinya tidak pernah dikirim lewat JSON.
#[derive(Debug, Clone, Serialize)]
pub struct View {
    pub id: String,
    pub session_id: String,
    pub kind: Kind,
    pub url: String,
    pub screenshot: Option<String>,
    pub expected_selector: Option<String>,
    pub detail: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub state: State,
    pub answered_at: Option<u64>,
}

impl From<&Handoff> for View {
    fn from(handoff: &Handoff) -> Self {
        Self {
            id: handoff.id.clone(),
            session_id: handoff.session_id.clone(),
            kind: handoff.kind,
            url: handoff.url.clone(),
            screenshot: handoff
                .screenshot
                .as_ref()
                .map(|path| path.display().to_string()),
            expected_selector: handoff.expected_selector.clone(),
            detail: handoff.detail.clone(),
            created_at: handoff.created_at,
            expires_at: handoff.expires_at,
            state: handoff.state,
            answered_at: handoff.answered_at,
        }
    }
}

/// Jawaban atas `POST /handoff/{id}/answer`.
#[derive(Debug, Clone, Serialize)]
pub struct Ack {
    pub id: String,
    pub state: State,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    NotFound { id: String },
    NotOpen { id: String, state: State },
    Busy { pending: usize, limit: usize },
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { id } => write!(f, "handoff {id} tidak ada"),
            Self::NotOpen { id, state } => {
                write!(f, "handoff {id} sudah {}", state.as_str())
            }
            Self::Busy { pending, limit } => {
                write!(f, "{pending} handoff menunggu, batas {limit}")
            }
        }
    }
}

impl std::error::Error for RegistryError {}

/// Batas yang bisa diatur pemanggil.
#[derive(Debug, Clone, Copy)]
pub struct RegistryLimits {
    pub hold_secs: u64,
    pub release_secs: u64,
    /// Handoff yang menunggu sekaligus. Melebihi ini berarti owner sedang
    /// dibanjiri, dan menambah antrean tidak membuatnya lebih cepat menjawab.
    pub max_pending: usize,
}

impl Default for RegistryLimits {
    fn default() -> Self {
        Self {
            hold_secs: HOLD_SECS,
            release_secs: RELEASE_SECS,
            max_pending: 20,
        }
    }
}

type Notify = Box<dyn Fn(ApplyRequest) + Send + Sync>;

/// Semua handoff proses ini.
pub struct Registry {
    limits: RegistryLimits,
    handoffs: Mutex<BTreeMap<String, Handoff>>,
    notify: Notify,
    counter: AtomicU64,
}

impl Registry {
    #[must_use]
    pub fn new(limits: RegistryLimits, notify: Notify) -> Self {
        Self {
            limits,
            handoffs: Mutex::new(BTreeMap::new()),
            notify,
            counter: AtomicU64::new(1),
        }
    }

    #[must_use]
    pub fn limits(&self) -> RegistryLimits {
        self.limits
    }

    /// Membuat handoff baru. Handoff yang sudah kedaluwarsa disapu lebih dulu
    /// supaya batas antrean menghitung yang benar-benar masih menunggu.
    pub fn create(&self, now: u64, new: NewHandoff) -> Result<View, RegistryError> {
        let mut handoffs = self.handoffs.lock().unwrap_or_else(|err| err.into_inner());
        expire_locked(&mut handoffs, now);

        let pending = handoffs.values().filter(|h| h.state.is_open()).count();
        if pending >= self.limits.max_pending {
            return Err(RegistryError::Busy {
                pending,
                limit: self.limits.max_pending,
            });
        }

        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        let id = buat_id(now, seq);
        let handoff = Handoff {
            id: id.clone(),
            session_id: new.session_id,
            target_id: new.target_id,
            kind: new.kind,
            url: new.url,
            screenshot: new.screenshot,
            expected_selector: new.expected_selector,
            detail: new.detail,
            created_at: now,
            expires_at: now + self.limits.hold_secs,
            release_at: now + self.limits.release_secs,
            state: State::Pending,
            answered_at: None,
            fail_detail: None,
        };
        let view = View::from(&handoff);
        handoffs.insert(id, handoff);
        Ok(view)
    }

    /// Handoff yang masih menunggu atau sedang diterapkan.
    pub fn pending(&self, now: u64) -> Vec<View> {
        let mut handoffs = self.handoffs.lock().unwrap_or_else(|err| err.into_inner());
        expire_locked(&mut handoffs, now);
        handoffs
            .values()
            .filter(|handoff| handoff.state.is_open())
            .map(View::from)
            .collect()
    }

    #[must_use]
    pub fn get(&self, id: &str, now: u64) -> Option<View> {
        let mut handoffs = self.handoffs.lock().unwrap_or_else(|err| err.into_inner());
        expire_locked(&mut handoffs, now);
        handoffs.get(id).map(View::from)
    }

    /// Menerima jawaban owner dan menyerahkannya ke antrean penerapan.
    ///
    /// `Abort` tidak masuk antrean: tidak ada yang perlu diterapkan ke browser,
    /// hanya state yang berubah.
    pub fn answer(&self, id: &str, answer: Answer, now: u64) -> Result<Ack, RegistryError> {
        let mut handoffs = self.handoffs.lock().unwrap_or_else(|err| err.into_inner());
        expire_locked(&mut handoffs, now);

        let Some(handoff) = handoffs.get_mut(id) else {
            return Err(RegistryError::NotFound { id: id.to_owned() });
        };
        // Hanya `Pending` yang boleh dijawab. `Applying` juga "terbuka" untuk
        // sapuan kedaluwarsa, tapi jawaban kedua atas handoff yang sedang
        // diterapkan berarti dua klik dikirim untuk satu tantangan.
        if handoff.state != State::Pending {
            return Err(RegistryError::NotOpen {
                id: id.to_owned(),
                state: handoff.state,
            });
        }

        handoff.answered_at = Some(now);

        if answer == Answer::Abort {
            handoff.state = State::Aborted;
            return Ok(Ack {
                id: id.to_owned(),
                state: State::Aborted,
            });
        }

        handoff.state = State::Applying;
        let request = ApplyRequest {
            id: id.to_owned(),
            target_id: handoff.target_id.clone(),
            answer,
        };
        let ack = Ack {
            id: id.to_owned(),
            state: State::Applying,
        };
        // Dipanggil setelah semua field berubah, dan tanpa memegang lock saat
        // callback jalan: callback bisa memanggil registry lagi lewat jalur
        // lain, dan lock yang dipegang di situ akan jadi deadlock.
        drop(handoffs);
        (self.notify)(request);
        Ok(ack)
    }

    /// Melaporkan hasil penerapan. Hanya transisi dari `Applying` yang diterima:
    /// hasil yang datang setelah owner membatalkan tidak boleh menimpa
    /// pembatalan itu.
    pub fn finish(&self, id: &str, outcome: ApplyOutcome, now: u64) -> Result<(), RegistryError> {
        let mut handoffs = self.handoffs.lock().unwrap_or_else(|err| err.into_inner());
        let Some(handoff) = handoffs.get_mut(id) else {
            return Err(RegistryError::NotFound { id: id.to_owned() });
        };
        if handoff.state != State::Applying {
            return Err(RegistryError::NotOpen {
                id: id.to_owned(),
                state: handoff.state,
            });
        }
        match outcome {
            ApplyOutcome::Solved => {
                handoff.state = State::Solved;
                handoff.answered_at.get_or_insert(now);
            }
            ApplyOutcome::Failed { detail } => {
                handoff.state = State::Failed;
                handoff.fail_detail = Some(detail);
            }
        }
        Ok(())
    }

    /// Menyapu handoff yang lewat batas tahan. Mengembalikan **hanya** id yang
    /// baru berubah di sapuan ini, supaya pemanggil mencatat satu event per
    /// handoff, bukan satu event setiap kali sapuan jalan.
    pub fn expire(&self, now: u64) -> Vec<String> {
        let mut handoffs = self.handoffs.lock().unwrap_or_else(|err| err.into_inner());
        expire_locked(&mut handoffs, now)
    }

    /// Target yang wajib ditahan: tidak boleh di-reap dan tidak masuk eviction
    /// LRU. Handoff yang lewat 15 menit tetap menahan tab sampai menit ke-30,
    /// karena owner yang menjawab terlambat tidak boleh menemukan tabnya sudah
    /// hilang. Yang dilepas hanya yang sudah selesai, dibatalkan, atau gagal.
    #[must_use]
    pub fn pinned_targets(&self, now: u64) -> Vec<String> {
        let handoffs = self.handoffs.lock().unwrap_or_else(|err| err.into_inner());
        handoffs
            .values()
            .filter(|handoff| {
                matches!(
                    handoff.state,
                    State::Pending | State::Applying | State::Expired
                ) && handoff.release_at > now
            })
            .map(|handoff| handoff.target_id.clone())
            .collect()
    }

    /// Jumlah handoff yang sedang disimpan, termasuk yang sudah selesai dan
    /// menunggu dibersihkan. Dipakai uji dan laporan status.
    #[must_use]
    pub fn len(&self) -> usize {
        self.handoffs
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn expire_locked(handoffs: &mut BTreeMap<String, Handoff>, now: u64) -> Vec<String> {
    let mut berubah = Vec::new();
    for handoff in handoffs.values_mut() {
        if handoff.state.is_open() && handoff.expires_at <= now {
            handoff.state = State::Expired;
            berubah.push(handoff.id.clone());
        }
    }
    berubah
}

fn buat_id(now: u64, seq: u64) -> String {
    // Bukan rahasia dan tidak perlu sulit ditebak: id hanya dipakai untuk
    // merujuk handoff yang sudah diotorisasi token. Yang penting unik.
    let pid = u64::from(std::process::id());
    format!("{now:x}{pid:x}{seq:04x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    fn baru(kind: Kind) -> NewHandoff {
        NewHandoff {
            session_id: "s-1".to_owned(),
            target_id: "T-1".to_owned(),
            kind,
            url: "https://situs.test/beranda".to_owned(),
            screenshot: Some(PathBuf::from("/tmp/captcha.png")),
            expected_selector: None,
            detail: "s2_challenge_url".to_owned(),
        }
    }

    fn registry() -> (
        Arc<Registry>,
        Arc<AtomicUsize>,
        Arc<Mutex<Vec<ApplyRequest>>>,
    ) {
        let counter = Arc::new(AtomicUsize::new(0));
        let antrean = Arc::new(Mutex::new(Vec::new()));
        let c = Arc::clone(&counter);
        let q = Arc::clone(&antrean);
        let registry = Arc::new(Registry::new(
            RegistryLimits::default(),
            Box::new(move |request| {
                c.fetch_add(1, Ordering::SeqCst);
                q.lock().unwrap().push(request);
            }),
        ));
        (registry, counter, antrean)
    }

    #[test]
    fn create_mengisi_batas_waktu() {
        let (registry, _, _) = registry();
        let view = registry
            .create(1_000, baru(Kind::Captcha))
            .expect("harus dibuat");
        assert_eq!(view.state, State::Pending);
        assert_eq!(view.expires_at, 1_000 + HOLD_SECS);
        assert!(view.screenshot.is_some());
    }

    #[test]
    fn pending_hanya_yang_masih_terbuka() {
        let (registry, _, _) = registry();
        registry.create(1_000, baru(Kind::Captcha)).unwrap();
        assert_eq!(registry.pending(1_001).len(), 1);
        assert_eq!(
            registry.pending(1_000 + HOLD_SECS).len(),
            0,
            "lewat 15 menit"
        );
    }

    #[test]
    fn jawaban_click_masuk_antrean_dan_state_applying() {
        let (registry, jumlah, antrean) = registry();
        let view = registry.create(1_000, baru(Kind::Captcha)).unwrap();

        let ack = registry
            .answer(&view.id, Answer::Click { x: 10.0, y: 20.0 }, 1_010)
            .expect("harus diterima");
        assert_eq!(ack.state, State::Applying);
        assert_eq!(jumlah.load(Ordering::SeqCst), 1);

        let antrean = antrean.lock().unwrap();
        assert_eq!(antrean[0].target_id, "T-1");
        assert_eq!(antrean[0].answer, Answer::Click { x: 10.0, y: 20.0 });
        assert_eq!(
            registry.pending(1_011).len(),
            1,
            "handoff yang sedang diterapkan tetap menunggu"
        );
    }

    #[test]
    fn abort_tidak_masuk_antrean() {
        let (registry, jumlah, _) = registry();
        let view = registry.create(1_000, baru(Kind::Otp)).unwrap();
        let ack = registry.answer(&view.id, Answer::Abort, 1_010).unwrap();
        assert_eq!(ack.state, State::Aborted);
        assert_eq!(jumlah.load(Ordering::SeqCst), 0);
        assert!(registry.pending(1_011).is_empty());
    }

    #[test]
    fn jawaban_kedua_ditolak() {
        let (registry, _, _) = registry();
        let view = registry.create(1_000, baru(Kind::Captcha)).unwrap();
        registry
            .answer(
                &view.id,
                Answer::Keys {
                    value: "Enter".to_owned(),
                },
                1_010,
            )
            .unwrap();

        let err = registry
            .answer(&view.id, Answer::Abort, 1_011)
            .expect_err("yang kedua harus ditolak");
        assert!(
            matches!(
                err,
                RegistryError::NotOpen {
                    state: State::Applying,
                    ..
                }
            ),
            "dapat: {err}"
        );
    }

    #[test]
    fn handoff_tidak_ada_ditolak_dengan_id() {
        let (registry, _, _) = registry();
        let err = registry
            .answer("tidak-ada", Answer::Abort, 1)
            .expect_err("harus ditolak");
        assert!(matches!(err, RegistryError::NotFound { .. }));
    }

    #[test]
    fn handoff_kedaluwarsa_tidak_bisa_dijawab() {
        let (registry, _, _) = registry();
        let view = registry.create(1_000, baru(Kind::Captcha)).unwrap();
        let err = registry
            .answer(&view.id, Answer::Abort, 1_000 + HOLD_SECS + 1)
            .expect_err("harus kedaluwarsa");
        assert!(matches!(
            err,
            RegistryError::NotOpen {
                state: State::Expired,
                ..
            }
        ));
    }

    #[test]
    fn finish_menandai_selesai_dan_gagal() {
        let (registry, _, _) = registry();
        let view = registry.create(1_000, baru(Kind::Captcha)).unwrap();
        registry
            .answer(
                &view.id,
                Answer::Text {
                    value: "482913".to_owned(),
                    selector: None,
                },
                1_010,
            )
            .unwrap();

        registry
            .finish(&view.id, ApplyOutcome::Solved, 1_020)
            .unwrap();
        let selesai = registry.get(&view.id, 1_021).unwrap();
        assert_eq!(selesai.state, State::Solved);
        assert_eq!(selesai.answered_at, Some(1_010));
        assert!(registry.pending(1_021).is_empty());
    }

    #[test]
    fn hasil_setelah_abort_tidak_menimpa() {
        let (registry, _, _) = registry();
        let view = registry.create(1_000, baru(Kind::Captcha)).unwrap();
        registry.answer(&view.id, Answer::Abort, 1_010).unwrap();
        let err = registry
            .finish(&view.id, ApplyOutcome::Solved, 1_020)
            .expect_err("abort tidak bisa ditimpa hasil");
        assert!(matches!(
            err,
            RegistryError::NotOpen {
                state: State::Aborted,
                ..
            }
        ));
    }

    #[test]
    fn batas_antrean_ditegakkan() {
        let registry = Registry::new(
            RegistryLimits {
                max_pending: 1,
                ..RegistryLimits::default()
            },
            Box::new(|_| {}),
        );
        registry.create(1_000, baru(Kind::Captcha)).unwrap();
        let err = registry
            .create(1_001, baru(Kind::Captcha))
            .expect_err("antrean penuh");
        assert_eq!(
            err,
            RegistryError::Busy {
                pending: 1,
                limit: 1
            }
        );
    }

    #[test]
    fn target_ditahan_sampai_batas_lepas() {
        let (registry, _, _) = registry();
        let view = registry.create(1_000, baru(Kind::Captcha)).unwrap();
        assert_eq!(registry.pinned_targets(1_001), vec!["T-1".to_owned()]);

        // Lewat 15 menit handoff kedaluwarsa, tapi tab-nya ditahan sampai menit
        // ke-30 supaya owner yang menjawab terlambat tidak kehilangan tab.
        assert!(registry.pending(1_000 + HOLD_SECS + 1).is_empty());
        assert_eq!(
            registry.pinned_targets(1_000 + HOLD_SECS + 1),
            vec!["T-1".to_owned()],
            "masih ditahan antara menit 15 dan 30"
        );
        assert!(
            registry.pinned_targets(1_000 + RELEASE_SECS).is_empty(),
            "lepas pin tepat di menit ke-30"
        );

        let _ = view;
    }

    #[test]
    fn abort_melepas_pin_tanpa_menunggu_tiga_puluh_menit() {
        let (registry, _, _) = registry();
        let view = registry.create(1_000, baru(Kind::Captcha)).unwrap();
        assert_eq!(registry.pinned_targets(1_001), vec!["T-1".to_owned()]);

        registry
            .answer(&view.id, Answer::Abort, 1_002)
            .expect("abort diterima");
        assert!(
            registry.pinned_targets(1_003).is_empty(),
            "abort melepas pin"
        );
    }

    #[test]
    fn expire_mengembalikan_id_yang_berubah() {
        let (registry, _, _) = registry();
        let a = registry.create(1_000, baru(Kind::Captcha)).unwrap();
        registry.create(1_001, baru(Kind::Otp)).unwrap();

        assert!(registry.expire(1_100).is_empty(), "belum ada yang lewat");
        // Tepat di batas handoff pertama; yang kedua masih punya satu detik.
        let lewat = registry.expire(1_000 + HOLD_SECS);
        assert_eq!(lewat, vec![a.id], "hanya satu yang lewat batas");
    }

    #[test]
    fn id_unik_dan_berbeda() {
        let (registry, _, _) = registry();
        let a = registry.create(1_000, baru(Kind::Captcha)).unwrap();
        let b = registry.create(1_000, baru(Kind::Captcha)).unwrap();
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn jawaban_json_sesuai_dokumen() {
        let click: Answer =
            serde_json::from_str(r#"{"type":"click","x":842,"y":517}"#).expect("click");
        assert_eq!(click, Answer::Click { x: 842.0, y: 517.0 });

        let text: Answer =
            serde_json::from_str(r#"{"type":"text","value":"482913"}"#).expect("text");
        assert_eq!(
            text,
            Answer::Text {
                value: "482913".to_owned(),
                selector: None
            }
        );

        let keys: Answer =
            serde_json::from_str(r#"{"type":"keys","value":"Enter"}"#).expect("keys");
        assert_eq!(
            keys,
            Answer::Keys {
                value: "Enter".to_owned()
            }
        );

        let abort: Answer = serde_json::from_str(r#"{"type":"abort"}"#).expect("abort");
        assert_eq!(abort, Answer::Abort);

        assert!(serde_json::from_str::<Answer>(r#"{"type":"drag"}"#).is_err());
    }
}
