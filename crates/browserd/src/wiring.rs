//! Pekerja latar daemon: penerap jawaban, probe tantangan, penyapu handoff.
//!
//! Ketiganya berjalan sebagai task terpisah dengan alasan yang sama: jalur
//! proxy CDP tidak boleh menunggu jaringan, dan registry tidak boleh memegang
//! lock selama panggilan CDP. Yang menghubungkan mereka hanya antrean.
//!
//! Semua jam diambil di sini, bukan di dalam crate `policy` atau `control`,
//! supaya aturan 15 menit dan cooldown 10 menit tetap bisa diuji tanpa tidur.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use control::{ApplyOutcome, ApplyRequest, Registry, RegistryLimits};
use policy::challenge::{LadderAvailability, Verdict};
use policy::counters::{CaptchaCounters, Kind, Outcome};
use policy::rate::{HandoffLimits, RateState};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::cdp_client::Client;
use crate::challenge::{Action, ChallengeObserver, Probe, dom_of, handoff_baru, probe_params};

/// Berapa kali probe DOM diulang sebelum halaman dianggap bersih.
///
/// Angka ini perdagangan langsung: terlalu kecil dan tantangan yang lambat
/// dirender lolos sebagai halaman normal, terlalu besar dan setiap 403 biasa
/// menahan pekerja selama berdetik-detik. 8 x 200 ms = 1,6 detik, cukup untuk
/// dokumen lokal selesai diparse tapi masih di bawah ambang sabar manusia.
const PROBE_ATTEMPTS: usize = 8;
const PROBE_DELAY: Duration = Duration::from_millis(200);

/// Jeda antar sapuan handoff kedaluwarsa. Cukup rapat untuk melepas tab
/// sebelum owner berikutnya menunggu, cukup jarang untuk tidak membangunkan
/// runtime 20 kali per detik.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(15);

/// Semua bagian yang perlu dipegang daemon setelah pekerja dinyalakan.
pub struct Wired {
    pub registry: Arc<Registry>,
    pub counters: Arc<CaptchaCounters>,
    pub observer: Arc<ChallengeObserver>,
    /// Alamat WebSocket mesin, disimpan supaya jalur kontrol bisa meminta
    /// probe tanpa membangun koneksi baru dari nol.
    pub engine_ws: String,
}

fn sekarang() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |durasi| durasi.as_secs())
}

/// Host dari URL, untuk pembatasan laju per host.
///
/// URL yang tidak bisa diparse memakai seluruh string sebagai kunci. Itu
/// pilihan yang aman: kunci yang salah membuat batasnya lebih ketat, bukan
/// lebih longgar.
#[must_use]
pub fn host_of(url: &str) -> String {
    let tanpa_skema = url.split_once("://").map_or(url, |(_, rest)| rest);
    tanpa_skema
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(tanpa_skema)
        .to_owned()
}

/// Mesin apa saja yang benar-benar terpasang.
///
/// v0.1 hanya punya `shell`, jadi L1 dan L2 tidak tersedia dan eskalasi
/// langsung menuju handoff. Mengarang ketersediaan di sini akan membuat
/// daemon mencoba biner yang tidak ada.
const AVAILABLE: LadderAvailability = LadderAvailability {
    full: false,
    cloak: false,
};

/// Menyalakan pekerja latar dan mengembalikan state yang dipakai daemon.
pub fn start(
    engine_ws: String,
    handoff_limits: HandoffLimits,
    registry_limits: RegistryLimits,
) -> Wired {
    let (apply_tx, apply_rx) = unbounded_channel();
    let registry = Arc::new(Registry::new(
        registry_limits,
        Box::new(move |request| {
            // Antrean mati berarti daemon sedang berhenti. Jawaban owner yang
            // tidak sempat diterapkan tetap tercatat di registry sebagai
            // `Applying`, dan itu terlihat di `GET /handoff/pending`.
            let _ = apply_tx.send(request);
        }),
    ));
    let counters = Arc::new(CaptchaCounters::new());
    let (probe_tx, probe_rx) = unbounded_channel();
    let observer = Arc::new(ChallengeObserver::new(probe_tx, Arc::clone(&counters)));
    let engine_ws_awal = engine_ws.clone();

    tokio::spawn(apply_loop(
        apply_rx,
        Arc::clone(&registry),
        Arc::clone(&counters),
        engine_ws.clone(),
    ));
    tokio::spawn(probe_loop(
        probe_rx,
        Arc::clone(&observer),
        Arc::clone(&registry),
        Arc::clone(&counters),
        Arc::new(Mutex::new(RateState::new(handoff_limits))),
        engine_ws,
    ));
    tokio::spawn(sweep_loop(Arc::clone(&registry), Arc::clone(&counters)));

    Wired {
        registry,
        counters,
        observer,
        engine_ws: engine_ws_awal,
    }
}

async fn apply_loop(
    mut rx: UnboundedReceiver<ApplyRequest>,
    registry: Arc<Registry>,
    counters: Arc<CaptchaCounters>,
    engine_ws: String,
) {
    while let Some(request) = rx.recv().await {
        let now = sekarang();
        let dibuat = registry.get(&request.id, now).map(|view| view.created_at);

        let hasil = match Client::connect(&engine_ws).await {
            Ok(mut client) => crate::handoff::apply(&mut client, &request).await,
            Err(err) => Err(err),
        };
        let hasil = match hasil {
            Ok(outcome) => outcome,
            Err(err) => ApplyOutcome::Failed {
                detail: format!("{err:#}"),
            },
        };

        match hasil {
            ApplyOutcome::Solved => {
                counters.record(Kind::Challenge, Outcome::SolvedHandoff);
                if let Some(dibuat) = dibuat {
                    counters.observe_answer_seconds(now.saturating_sub(dibuat));
                }
                tracing::info!(id = %request.id, "jawaban owner diterapkan");
            }
            ApplyOutcome::Failed { ref detail } => {
                // Gagal menerapkan bukan "selesai": tingkatnya naik, dan itu
                // yang dicatat supaya pola kegagalan berulang terlihat.
                counters.record(Kind::Challenge, Outcome::Escalated);
                tracing::warn!(id = %request.id, detail, "jawaban owner gagal diterapkan");
            }
        }

        if let Err(err) = registry.finish(&request.id, hasil, now) {
            tracing::warn!(id = %request.id, %err, "hasil penerapan ditolak registry");
        }
    }
}

async fn probe_loop(
    mut rx: UnboundedReceiver<Probe>,
    observer: Arc<ChallengeObserver>,
    registry: Arc<Registry>,
    counters: Arc<CaptchaCounters>,
    rate: Arc<Mutex<RateState>>,
    engine_ws: String,
) {
    while let Some(probe) = rx.recv().await {
        let dom = match run_probe(&engine_ws, &probe).await {
            Ok(dom) => dom,
            Err(err) => {
                // Probe yang gagal TIDAK membatalkan kecurigaan: tanpa bukti
                // DOM, status 403 tetap alasan yang sah untuk serah ke owner
                // kalau tingkatnya sudah sampai sana.
                tracing::warn!(target_id = %probe.target_id, %err, "probe DOM gagal");
                continue;
            }
        };

        let Some(hasil) = observer.probed(&probe.target_id, dom) else {
            continue;
        };
        if hasil.action != Action::Handoff {
            tracing::debug!(target_id = %probe.target_id, "probe bersih, tidak ada handoff");
            continue;
        }

        let host = host_of(&probe.url);
        let now = sekarang();
        {
            let mut rate = rate.lock().unwrap_or_else(|err| err.into_inner());
            let izin = rate.check(&host, now);
            if !izin.is_yes() {
                counters.record(Kind::Challenge, Outcome::Skipped);
                tracing::warn!(host = %host, alasan = izin.reason(), "handoff ditahan batas laju");
                continue;
            }
            rate.record(&host, now);
        }

        let putusan = policy::challenge::verdict(&hasil.evidence);
        let handoff = handoff_baru(
            &hasil.session_id,
            &probe.target_id,
            &hasil.evidence,
            &putusan,
        );
        match registry.create(now, handoff) {
            Ok(view) => {
                observer.escalate(&probe.target_id, AVAILABLE);
                counters.record(Kind::Challenge, Outcome::Detected);
                tracing::info!(
                    id = %view.id,
                    host = %host,
                    bobot = putusan.weight,
                    "handoff dibuat untuk owner"
                );
            }
            Err(err) => {
                counters.record(Kind::Challenge, Outcome::Skipped);
                tracing::warn!(host = %host, %err, "handoff tidak bisa dibuat");
            }
        }
    }
}

/// Menjalankan [`probe_script`] di dalam sesi target.
async fn run_probe(engine_ws: &str, probe: &Probe) -> Result<policy::challenge::DomFinding> {
    let mut client = Client::connect(engine_ws).await?;
    let session = client
        .call(
            "Target.attachToTarget",
            serde_json::json!({ "targetId": probe.target_id, "flatten": true }),
        )
        .await?
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("Target.attachToTarget tidak mengembalikan sessionId")?;

    // Satu percobaan tidak cukup. `Network.responseReceived` datang sebelum
    // DOM selesai diparse, jadi probe yang jalan sekali bisa membaca halaman
    // kosong lalu menyimpulkan "bersih" untuk halaman tantangan, dan
    // `decided_for` membuat URL itu tidak pernah diperiksa lagi. Diukur di
    // uji `m3_proof`: hasilnya hilang-timbul tanpa percobaan ulang.
    let mut terakhir = policy::challenge::DomFinding::default();
    for percobaan in 0..PROBE_ATTEMPTS {
        let nilai = client
            .call_in(&session, "Runtime.evaluate", probe_params())
            .await
            .context("menjalankan probe DOM")?;
        terakhir = dom_of(&nilai).context("hasil probe tidak bisa dibaca sebagai daftar sinyal")?;
        if terakhir != policy::challenge::DomFinding::default() {
            break;
        }
        if percobaan + 1 < PROBE_ATTEMPTS {
            tokio::time::sleep(PROBE_DELAY).await;
        }
    }

    let _ = client
        .call(
            "Target.detachFromTarget",
            serde_json::json!({ "sessionId": session }),
        )
        .await;

    Ok(terakhir)
}

/// Probe satu target tanpa syarat bukti apa pun.
///
/// Dipakai jalur kontrol `POST /api/v1/challenge/probe`: operator ingin tahu
/// apakah tab yang sedang terbuka sekarang menampilkan tanda tantangan. Bukti
/// status/URL tidak ikut dinilai di sini, karena yang ditanya hanya DOM.
pub async fn probe_target(
    engine_ws: &str,
    target_id: &str,
) -> Result<policy::challenge::DomFinding> {
    let probe = Probe {
        session_id: String::new(),
        target_id: target_id.to_owned(),
        url: String::new(),
        evidence: policy::challenge::Evidence::default(),
    };
    run_probe(engine_ws, &probe).await
}

async fn sweep_loop(registry: Arc<Registry>, counters: Arc<CaptchaCounters>) {
    let mut tick = tokio::time::interval(SWEEP_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let laporan = crate::handoff::sweep(&registry, sekarang());
        for id in &laporan.expired {
            counters.record(Kind::Challenge, Outcome::Expired);
            tracing::info!(id = %id, "handoff kedaluwarsa");
        }
        if !laporan.masih_ditahan.is_empty() {
            tracing::debug!(ditahan = ?laporan.masih_ditahan, "target masih ditahan handoff");
        }
    }
}

/// Ringkasan untuk log saat daemon start.
#[must_use]
pub fn ringkas(registry: &Registry) -> String {
    format!("handoff tersimpan={}", registry.len())
}

/// Verdict untuk satu set bukti, dipisah supaya jalur log dan jalur keputusan
/// memakai fungsi yang sama.
#[must_use]
pub fn nilai(evidence: &policy::challenge::Evidence) -> Verdict {
    policy::challenge::verdict(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use policy::challenge::{DomFinding, Evidence};

    #[test]
    fn host_diambil_dari_url() {
        assert_eq!(host_of("https://shop.example/a/b?c=1"), "shop.example");
        assert_eq!(host_of("http://127.0.0.1:8080/x"), "127.0.0.1:8080");
        assert_eq!(host_of("tanpa-skema.example/x"), "tanpa-skema.example");
        assert_eq!(host_of(""), "");
    }

    #[test]
    fn bukti_tantangan_menghasilkan_handoff() {
        let evidence = Evidence {
            document_status: Some(403),
            final_url: Some("https://shop.example/x".to_owned()),
            dom: Some(DomFinding {
                challenge_form: true,
                ..DomFinding::default()
            }),
            ..Evidence::default()
        };
        let putusan = nilai(&evidence);
        assert!(putusan.is_challenge, "harus dianggap tantangan");
    }
}
