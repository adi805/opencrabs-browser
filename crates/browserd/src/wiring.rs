//! Pekerja latar daemon: penerap jawaban, probe tantangan, penyapu handoff.
//!
//! Ketiganya berjalan sebagai task terpisah dengan alasan yang sama: jalur
//! proxy CDP tidak boleh menunggu jaringan, dan registry tidak boleh memegang
//! lock selama panggilan CDP. Yang menghubungkan mereka hanya antrean.
//!
//! Semua jam diambil di sini, bukan di dalam crate `policy` atau `control`,
//! supaya aturan 15 menit dan cooldown 10 menit tetap bisa diuji tanpa tidur.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use control::{ApplyOutcome, ApplyRequest, Registry, RegistryLimits};
use engine::Slot;
use lifecycle::Gudang;
use policy::challenge::{LadderAvailability, Verdict};
use policy::counters::{CaptchaCounters, Kind, Outcome};
use policy::rate::{HandoffLimits, RateState};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::cdp_client::Client;
use crate::challenge::{
    Action, ChallengeObserver, Konteks, Probe, dom_of, handoff_baru, probe_params,
};

/// Berapa kali probe DOM diulang sebelum halaman dianggap bersih.
///
/// Angka ini perdagangan langsung: terlalu kecil dan tantangan yang lambat
/// dirender lolos sebagai halaman normal, terlalu besar dan setiap 403 biasa
/// menahan pekerja selama berdetik-detik. 8 x 200 ms = 1,6 detik, cukup untuk
/// dokumen lokal selesai diparse tapi masih di bawah ambang sabar manusia.
const PROBE_ATTEMPTS: usize = 8;
const PROBE_DELAY: Duration = Duration::from_millis(200);

/// Batas ukuran berkas layar. PNG 1280x720 biasanya jauh di bawah ini; angka
/// ini hanya menahan jawaban aneh dari mesin, bukan mengatur mutu gambar.
const LAYAR_MAKS_BYTES: usize = 8 * 1024 * 1024;

/// Ekspresi pengambil judul. Dibatasi 200 karakter: cukup untuk membuat owner
/// mengenali halamannya, dan tetap bukan isi halaman.
const JS_JUDUL: &str = "(document.title || '').slice(0, 200)";

/// Jeda antar sapuan handoff kedaluwarsa. Cukup rapat untuk melepas tab
/// sebelum owner berikutnya menunggu, cukup jarang untuk tidak membangunkan
/// runtime 20 kali per detik.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(15);

/// Semua bagian yang perlu dipegang daemon setelah pekerja dinyalakan.
pub struct Wired {
    pub registry: Arc<Registry>,
    pub counters: Arc<CaptchaCounters>,
    pub observer: Arc<ChallengeObserver>,
    /// Slot mesin bawaan. Yang disimpan **slot**, bukan string alamat: mesin
    /// yang di-reap lalu lahir lagi memilih port baru, jadi alamat yang dicatat
    /// sekali saat boot akan menunjuk port mati. Lihat [`alamat_mesin`].
    pub slot: Arc<Slot>,
    /// Batas tunggu boot, dipakai [`alamat_mesin`] saat membangunkan mesin yang
    /// sedang tidur.
    pub ready_timeout_ms: u64,
    /// Penghitung kegagalan buka-web. Dipakai jalur yang **benar-benar**
    /// mengamati kegagalan (handoff tantangan), bukan diisi di dashboard.
    pub kegagalan: Arc<crate::metrics::Kegagalan>,
    /// Lease tab per sesi. Ditaruh di sini, bukan di dalam loop reap, karena
    /// jalur yang memegang tab (proxy CDP, kontrol) dan jalur yang memutuskan
    /// reap harus melihat gudang yang **sama**. Dua gudang berarti lease yang
    /// dipegang satu jalur tidak terlihat oleh pemutus reap.
    pub leases: Arc<Mutex<Gudang>>,
}

/// Alamat WebSocket mesin, **dibaca saat dipakai**.
///
/// Mesin yang tidur dibangunkan di sini. Semua jalur yang benar-benar akan
/// bicara ke mesin (terapkan jawaban owner, probe DOM) lewat fungsi ini, karena
/// jalur-jalur itu tidak punya gunanya tanpa mesin hidup.
///
/// Boot berjalan di `spawn_blocking`: menunggu Chromium siap mem-poll port dan
/// berkas `DevToolsActivePort` selama belasan detik, dan itu tidak boleh
/// menahan reactor yang juga melayani proxy CDP.
pub async fn alamat_mesin(slot: Arc<Slot>, timeout_ms: u64) -> Result<String> {
    let id = slot.id().to_owned();
    let engine = tokio::task::spawn_blocking(move || {
        slot.pastikan(timeout_ms, engine::slot::detik_sekarang())
    })
    .await
    .with_context(|| format!("tugas boot mesin '{id}' panik"))?
    .with_context(|| format!("mesin '{id}' tidak bisa dinyalakan"))?;
    Ok(engine.web_socket_url().to_owned())
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
    slot: Arc<Slot>,
    ready_timeout_ms: u64,
    handoff_limits: HandoffLimits,
    registry_limits: RegistryLimits,
    layar_dir: Option<PathBuf>,
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
    let kegagalan = Arc::new(crate::metrics::Kegagalan::baru());
    // Batas per sesi belum punya kunci config sendiri; `0` berarti tanpa batas
    // per sesi, dan batas per mesin (`max_tabs`) tetap berlaku lewat jalur cdp.
    // Angkanya bisa diganti saat config dimuat ulang tanpa mengganti gudangnya.
    let leases = Arc::new(Mutex::new(Gudang::baru(0)));
    let (probe_tx, probe_rx) = unbounded_channel();
    let observer = Arc::new(ChallengeObserver::new(probe_tx, Arc::clone(&counters)));
    tokio::spawn(apply_loop(
        apply_rx,
        Arc::clone(&registry),
        Arc::clone(&counters),
        Arc::clone(&slot),
        ready_timeout_ms,
    ));
    tokio::spawn(probe_loop(
        probe_rx,
        ProbeDeps {
            observer: Arc::clone(&observer),
            registry: Arc::clone(&registry),
            counters: Arc::clone(&counters),
            rate: Arc::new(Mutex::new(RateState::new(handoff_limits))),
            slot: Arc::clone(&slot),
            ready_timeout_ms,
            kegagalan: Arc::clone(&kegagalan),
            layar_dir,
        },
    ));
    tokio::spawn(sweep_loop(Arc::clone(&registry), Arc::clone(&counters)));

    Wired {
        registry,
        counters,
        observer,
        slot,
        ready_timeout_ms,
        kegagalan,
        leases,
    }
}

async fn apply_loop(
    mut rx: UnboundedReceiver<ApplyRequest>,
    registry: Arc<Registry>,
    counters: Arc<CaptchaCounters>,
    slot: Arc<Slot>,
    ready_timeout_ms: u64,
) {
    while let Some(request) = rx.recv().await {
        let now = sekarang();
        let dibuat = registry.get(&request.id, now).map(|view| view.created_at);

        let hasil = match alamat_mesin(Arc::clone(&slot), ready_timeout_ms).await {
            Ok(alamat) => match Client::connect(&alamat).await {
                Ok(mut client) => crate::handoff::apply(&mut client, &request).await,
                Err(err) => Err(err),
            },
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

/// Yang dibutuhkan [`probe_loop`] di tiap putaran.
///
/// Dikelompokkan supaya tanda tangan fungsi tetap terbaca: delapan argumen
/// sejenis membuat pemanggil mudah menukar dua di antaranya tanpa sadar.
struct ProbeDeps {
    observer: Arc<ChallengeObserver>,
    registry: Arc<Registry>,
    counters: Arc<CaptchaCounters>,
    rate: Arc<Mutex<RateState>>,
    slot: Arc<Slot>,
    ready_timeout_ms: u64,
    kegagalan: Arc<crate::metrics::Kegagalan>,
    /// Direktori penyimpan layar handoff. `None` berarti handoff dibuat tanpa
    /// gambar: jalur yang tidak boleh diam-diam dianggap punya gambar.
    layar_dir: Option<PathBuf>,
}

async fn probe_loop(mut rx: UnboundedReceiver<Probe>, deps: ProbeDeps) {
    let ProbeDeps {
        observer,
        registry,
        counters,
        rate,
        slot,
        ready_timeout_ms,
        kegagalan,
        layar_dir,
    } = deps;
    while let Some(probe) = rx.recv().await {
        let alamat = match alamat_mesin(Arc::clone(&slot), ready_timeout_ms).await {
            Ok(alamat) => alamat,
            Err(err) => {
                tracing::warn!(target_id = %probe.target_id, %err, "mesin tidak bisa dipakai untuk probe");
                continue;
            }
        };
        let hasil_probe = match run_probe(&alamat, &probe).await {
            Ok(hasil) => hasil,
            Err(err) => {
                // Probe yang gagal TIDAK membatalkan kecurigaan: tanpa bukti
                // DOM, status 403 tetap alasan yang sah untuk serah ke owner
                // kalau tingkatnya sudah sampai sana.
                tracing::warn!(target_id = %probe.target_id, %err, "probe DOM gagal");
                continue;
            }
        };

        let Some(hasil) = observer.probed(&probe.target_id, hasil_probe.dom) else {
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
        let konteks = konteks_halaman(
            &alamat,
            &probe.target_id,
            hasil_probe.judul,
            now,
            layar_dir.as_deref(),
        )
        .await;
        let handoff = handoff_baru(
            &hasil.session_id,
            &probe.target_id,
            &hasil.evidence,
            &putusan,
            &konteks,
        );
        match registry.create(now, handoff) {
            Ok(view) => {
                observer.escalate(&probe.target_id, AVAILABLE);
                counters.record(Kind::Challenge, Outcome::Detected);
                // Halaman ini diamati tidak bisa dilewati mesin: itu kegagalan
                // buka-web, dan satu-satunya jalur yang punya bukti cukup untuk
                // mengatakannya. Angkanya diambil dari pengamatan, bukan dari
                // definisi kategori.
                kegagalan.catat(crate::klasifikasi::Kategori::BlokAntiBot);
                tracing::info!(
                    id = %view.id,
                    host = %host,
                    bobot = putusan.weight,
                    "handoff dibuat untuk owner"
                );
            }
            Err(err) => {
                // Handoff tidak jadi, jadi gambarnya juga tidak: berkas yang
                // tidak dirujuk siapa pun cuma menumpuk di direktori data.
                if let Some(path) = konteks.layar.as_deref()
                    && let Err(gagal) = std::fs::remove_file(path)
                {
                    tracing::warn!(
                        path = %path.display(),
                        %gagal,
                        "layar handoff yatim tidak bisa dihapus"
                    );
                }
                counters.record(Kind::Challenge, Outcome::Skipped);
                tracing::warn!(host = %host, %err, "handoff tidak bisa dibuat");
            }
        }
    }
}

/// Menjalankan [`probe_script`] di dalam sesi target.
async fn run_probe(engine_ws: &str, probe: &Probe) -> Result<ProbeHasil> {
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

    // Judul dibaca SESUDAH probe DOM dan kegagalannya tidak membatalkan
    // handoff: judul hanya alat bantu pengenalan, dan handoff tanpa judul jauh
    // lebih berguna daripada handoff yang tidak pernah dibuat.
    let judul = match client
        .call_in(
            &session,
            "Runtime.evaluate",
            serde_json::json!({
                "expression": JS_JUDUL,
                "returnByValue": true,
                "awaitPromise": false,
            }),
        )
        .await
    {
        Ok(nilai) => judul_dari(&nilai),
        Err(err) => {
            tracing::warn!(target_id = %probe.target_id, %err, "judul halaman tidak bisa dibaca");
            None
        }
    };

    let _ = client
        .call(
            "Target.detachFromTarget",
            serde_json::json!({ "sessionId": session }),
        )
        .await;

    Ok(ProbeHasil {
        dom: terakhir,
        judul,
    })
}

/// Hasil satu probe: sinyal DOM, plus judul halaman kalau terbaca.
struct ProbeHasil {
    dom: policy::challenge::DomFinding,
    judul: Option<String>,
}

/// Mengambil judul dari hasil `Runtime.evaluate`. String kosong dianggap tidak
/// ada: halaman tanpa `<title>` bukan alasan menampilkan judul kosong ke owner.
fn judul_dari(nilai: &Value) -> Option<String> {
    let teks = nilai.get("result")?.get("value")?.as_str()?.trim();
    if teks.is_empty() {
        return None;
    }
    Some(teks.to_owned())
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
    Ok(run_probe(engine_ws, &probe).await?.dom)
}

/// Direktori penyimpan layar handoff: di samping DB store, bukan di `/tmp`.
///
/// Isinya gambar halaman yang bisa memuat data sesi owner, jadi tempatnya ikut
/// siklus hidup data daemon: dibersihkan bersama profil, bukan tertinggal di
/// direktori sementara yang dibagi semua proses di host.
#[must_use]
pub fn dir_layar(store_db: &Path) -> PathBuf {
    store_db
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
        .join("handoff")
}

/// Menyusun konteks halaman untuk handoff. Semua kegagalan di sini berakhir
/// sebagai `None` plus `warn`: handoff yang hilang karena screenshot gagal
/// adalah kerugian yang lebih besar daripada handoff tanpa screenshot.
async fn konteks_halaman(
    engine_ws: &str,
    target_id: &str,
    judul: Option<String>,
    now: u64,
    dir: Option<&Path>,
) -> Konteks {
    let mut konteks = Konteks { judul, layar: None };
    let Some(dir) = dir else {
        return konteks;
    };
    match tangkap_layar(engine_ws, target_id).await {
        Ok(bytes) => match simpan_layar(dir, now, target_id, &bytes) {
            Ok(path) => konteks.layar = Some(path),
            Err(err) => tracing::warn!(target_id, %err, "layar handoff tidak bisa disimpan"),
        },
        Err(err) => tracing::warn!(target_id, %err, "layar handoff tidak bisa diambil"),
    }
    konteks
}

/// Mengambil satu layar PNG dari target yang sedang menampilkan tantangan.
async fn tangkap_layar(engine_ws: &str, target_id: &str) -> Result<Vec<u8>> {
    let mut client = Client::connect(engine_ws).await?;
    let session = client
        .call(
            "Target.attachToTarget",
            serde_json::json!({ "targetId": target_id, "flatten": true }),
        )
        .await?
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("Target.attachToTarget tidak mengembalikan sessionId")?;

    let hasil = client
        .call_in(
            &session,
            "Page.captureScreenshot",
            serde_json::json!({ "format": "png" }),
        )
        .await;
    let _ = client
        .call(
            "Target.detachFromTarget",
            serde_json::json!({ "sessionId": session }),
        )
        .await;

    let nilai = hasil.context("Page.captureScreenshot gagal")?;
    let data = nilai
        .get("data")
        .and_then(Value::as_str)
        .context("Page.captureScreenshot tidak mengembalikan data")?;
    let bytes = crate::base64::dekode(data)
        .map_err(|err| anyhow::anyhow!("layar bukan base64 yang sah: {err}"))?;
    if bytes.len() > LAYAR_MAKS_BYTES {
        anyhow::bail!("layar terlalu besar: {} byte", bytes.len());
    }
    Ok(bytes)
}

/// Menulis layar ke direktori handoff dengan nama yang menyebut waktunya.
///
/// Nama memuat target supaya berkas bisa dicocokkan ke handoff-nya tanpa
/// membaca DB, dan penambahan akhiran saat nama sudah terpakai mencegah dua
/// handoff dalam detik yang sama saling menimpa gambarnya.
fn simpan_layar(dir: &Path, now: u64, target_id: &str, bytes: &[u8]) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("membuat direktori layar {}", dir.display()))?;
    let dasar = format!("handoff-{now}-{}", nama_aman(target_id));
    let mut path = dir.join(format!("{dasar}.png"));
    let mut urutan = 0_u32;
    while path.exists() {
        urutan += 1;
        path = dir.join(format!("{dasar}-{urutan}.png"));
    }
    std::fs::write(&path, bytes).with_context(|| format!("menulis layar {}", path.display()))?;
    Ok(path)
}

/// Nama berkas dari id target: hanya huruf, angka, `-`, dan `_` yang dibiarkan.
/// Id target datang dari mesin, dan id yang memuat `/` akan menulis di luar
/// direktori handoff kalau dipakai apa adanya.
fn nama_aman(target_id: &str) -> String {
    target_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
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
    fn judul_dibaca_dari_hasil_runtime_evaluate() {
        let nilai = serde_json::json!({"result": {"type": "string", "value": "  Just a moment  "}});
        assert_eq!(judul_dari(&nilai).as_deref(), Some("Just a moment"));
        let kosong = serde_json::json!({"result": {"type": "string", "value": "   "}});
        assert_eq!(
            judul_dari(&kosong),
            None,
            "judul kosong bukan judul: owner tidak perlu baris kosong"
        );
        let bukan_string = serde_json::json!({"result": {"type": "object"}});
        assert_eq!(judul_dari(&bukan_string), None);
    }

    #[test]
    fn nama_berkas_layar_tidak_bisa_keluar_dari_direktori() {
        // Id target datang dari mesin. `../` di dalamnya akan menulis PNG di
        // luar direktori handoff kalau dipakai apa adanya.
        assert_eq!(nama_aman("A1B2"), "A1B2");
        assert_eq!(nama_aman("../../etc/passwd"), "______etc_passwd");
        assert_eq!(nama_aman("a b"), "a_b");
    }

    #[test]
    fn layar_disimpan_di_samping_store_dan_tidak_saling_menimpa() {
        let dir = std::env::temp_dir().join(format!("ocb-layar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("browserd.db");
        let target = dir_layar(&db);
        assert_eq!(target, dir.join("handoff"));

        let satu = simpan_layar(&target, 1_000, "T1", b"png-1").expect("tulis pertama");
        let dua = simpan_layar(&target, 1_000, "T1", b"png-2").expect("tulis kedua");
        assert_ne!(
            satu, dua,
            "dua handoff pada detik yang sama tidak boleh berbagi berkas"
        );
        assert_eq!(std::fs::read(&satu).expect("baca pertama"), b"png-1");
        assert_eq!(std::fs::read(&dua).expect("baca kedua"), b"png-2");
        let _ = std::fs::remove_dir_all(&dir);
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
