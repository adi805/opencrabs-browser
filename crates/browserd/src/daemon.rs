//! Merangkai semuanya: nyalakan mesin, layani klien, matikan dengan rapi.
//!
//! Urutannya penting dan tidak boleh ditukar:
//!
//! 1. Mesin dinyalakan dan **ditunggu sampai benar-benar siap**. Bind port
//!    sebelum mesin siap berarti klien bisa menyambung ke daemon yang belum
//!    punya apa-apa untuk diteruskan.
//! 2. Baru daemon bind dan melayani.
//! 3. Saat berhenti, mesin dimatikan sebagai process group, bukan hanya pid.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use axum::Router;
use cdp::native_wire::{KIND as NATIVE_KIND, PenggerakNative};
use cdp::{EnginePool, ProxyState};
use control::Registry;
use engine::{
    Engine, Keadaan, Penggerak, Slot, discover_full, discover_oxi, discover_shell,
    slot::penggerak_untuk,
};
use lifecycle::reaper::{ReapReason, ReapReport, ReaperConfig, idle_reap_due};
use lifecycle::{Counters, Event, Gudang, Machine};
use tokio::net::TcpListener;

use crate::anggaran::{Penjaga, ProcPengukur, Putusan};
use crate::cli::ServeArgs;
use crate::config::{self, FileConfig};
use crate::metrics;
use crate::reload::{self, Batas, Hasil, Hidup};
use crate::tabstore;
use crate::{wiring, wiring::Wired};

const DETACHED_ENV: &str = "OCB_BROWSERD_DETACHED";

pub async fn run(args: ServeArgs, file: FileConfig, config_path: PathBuf) -> Result<()> {
    let resolved = Resolved::resolve(&args, &file)?;

    // Daftar jenis yang punya adapter. Disimpan di sini supaya pesan errornya
    // menyebut pilihan yang benar-benar ada, bukan "v0.1" yang sudah basi.
    if penggerak_kind(&args.engine).is_none() {
        bail!(
            "engine '{}' belum punya adapter; yang tersedia: {}",
            args.engine,
            jenis_tersedia()
        );
    }

    // Store dibuka SEBELUM mesin dinyalakan: kunci yang hilang atau rusak harus
    // terlihat sebagai daemon yang tidak mau start, bukan sebagai daemon sehat
    // yang diam-diam kehilangan semua cookie saat restart berikutnya.
    // Disimpan hidup selama daemon berjalan: perintah cookie (export/import)
    // dan sesi berikutnya memakai koneksi yang sama.
    let _store = buka_store(&file)?;

    // Penyimpanan dingin untuk mesin native: tab yang disingkirkan pool
    // mendarat di SQLite, jadi restart daemon tidak membuang tab. Adapter ini
    // membuka koneksinya sendiri per pool karena `Store` tidak `Sync`.
    //
    // Pabriknya dibuat per mesin di `nyalakan`, bukan sekali di sini: nomor tab
    // mulai dari 1 di tiap pool, jadi sesi bersama akan membuat dua mesin
    // native saling menimpa baris `tab_snapshot`.
    let dingin = tabstore::SumberDingin::new(file.store_db(), file.store_key());

    // Gerbang jaringan untuk `fetch()` di dalam isolat mesin native. Dibangun
    // di sini, sekali, lalu dipegang setiap tab: skrip halaman tidak punya
    // jalur jaringan sendiri, dan satu-satunya jalan keluarnya adalah kebijakan
    // ini. Daftar izin kosong berarti menolak semua, dan itu dilaporkan supaya
    // operator yang lupa mengisi `[fetch]` melihat sebabnya di log, bukan
    // menebaknya dari halaman yang diam.
    let kebijakan_fetch = file.fetch.as_ref().map_or_else(
        cdp::native_wire::KebijakanJaringan::default,
        config::FetchSection::kebijakan,
    );
    if kebijakan_fetch.mengizinkan_sesuatu() {
        tracing::info!(
            izin_host = ?kebijakan_fetch.izin(),
            "gerbang jaringan fetch siap"
        );
    } else {
        tracing::info!(
            "bagian [fetch] kosong: fetch() dari halaman akan ditolak semua dengan alasan"
        );
    }
    let ambil: Arc<dyn engine::native::ambil::Pengambil> =
        Arc::new(cdp::native_wire::PengambilJaringan::baru(kebijakan_fetch));

    let engines = launch_all(
        &resolved.machines,
        args.ready_timeout_ms,
        Some(&dingin),
        Some(Arc::clone(&ambil)),
    )
    .await?;
    let pool = EnginePool::new(resolved.default_id.clone(), engines)
        .map_err(|err| anyhow::anyhow!("konfigurasi mesin tidak sah: {err}"))?
        .dengan_batas_tab(batas_tab(&resolved.machines));

    // Aturan `[policy]` diperiksa terhadap mesin yang benar-benar terpasang
    // SEBELUM daemon melayani. Aturan yang menunjuk mesin hilang adalah salah
    // tulis config, dan salah tulis config harus gagal di sini dengan nama
    // mesinnya, bukan nanti di satu host yang kebetulan cocok.
    // Katalog dibangun dari JENIS mesin, bukan id-nya: id dipilih operator dan
    // bebas (`kiri`, `kanan`), sedangkan yang menentukan kemampuan adalah
    // jenisnya. Memakai id akan menolak config yang benar hanya karena mesinnya
    // diberi nama lain.
    let jenis = pool
        .slots()
        .map(|(_, slot)| slot.kind().to_owned())
        .collect::<Vec<_>>();
    validasi_policy(&file, &jenis)?;

    // Penanam dijalankan untuk SETIAP mesin, bukan hanya mesin bawaan. Klien
    // yang diarahkan ke mesin lain lewat policy harus mendapat mata dan tangan
    // yang sama; menyuntik satu mesin saja akan membuat kemampuan bergantung
    // pada mesin mana yang kebetulan dipilih.
    for (id, slot) in pool.slots() {
        crate::penanam::mulai(Arc::clone(slot));
        tracing::info!(mesin = %id, "penanam runtime mata-tangan dijalankan");
    }

    // State daemon dilacak sejak awal. Metrik `browser_engine_state` yang
    // dilaporkan tanpa state nyata cuma angka karangan, jadi state-nya
    // dijalankan lewat tabel transisi yang sama dengan yang diuji unit.
    let machine = Arc::new(Mutex::new(Machine::new()));
    let counters = Arc::new(Counters::new());
    apply_event(&machine, Event::Boot);
    apply_event(&machine, Event::Booted);

    // Pengamat tantangan dipasang sebelum router dibangun: dia harus melihat
    // trafik sejak frame pertama, bukan sejak handoff pertama.
    // Probe handoff masih memakai mesin bawaan: registry handoff belum menyimpan
    // mesin per sesi, jadi probe ke mesin lain belum punya alamat yang benar.
    // Batas ini tercatat di README supaya tidak dikira sudah per mesin.
    let wired = wiring::start(
        Arc::clone(pool.default_slot()),
        args.ready_timeout_ms,
        limits_handoff(&file),
        control::RegistryLimits::default(),
        // Layar handoff disimpan di samping DB store: handoff harus membawa
        // gambar halaman, dan gambar itu milik data daemon, bukan /tmp.
        Some(wiring::dir_layar(&file.store_db())),
    );

    let bind = resolved.bind.clone();
    let public_ws_url = format!("ws://{bind}/devtools/browser/{}", session_token());
    let state = Arc::new(
        ProxyState::pool(pool.clone(), public_ws_url)
            .with_observer(wired.observer.clone())
            // Aturan `[policy]` disambungkan di sini, satu tempat. Katalognya
            // dibangun dari jenis mesin yang terpasang, sama seperti
            // `validasi_policy` di atas: kemampuan ditentukan jenis, bukan id.
            .with_pemilih(crate::routing::PemilihPolicy::dari_jenis(
                file.policy(),
                &jenis,
            )),
    );
    let kegagalan = Arc::new(metrics::Kegagalan::baru());
    let app: Router = cdp::router(state).merge(metrics::router(
        pool.clone(),
        Arc::clone(&counters),
        Arc::clone(&wired.counters),
        Arc::clone(&kegagalan),
    ));

    // Batas hidup: satu tempat yang dibaca jalur pembuatan tab dan loop reap.
    // Nilai awalnya sama persis dengan yang ditegakkan saat boot, jadi tidak ada
    // jendela waktu di mana daemon melayani dengan angka yang berbeda dari yang
    // dilaporkan di log start.
    let hidup = Arc::new(Hidup::baru(Batas::baru(
        batas_tab(&resolved.machines),
        ambang_reap(&args, &file),
    )));

    // Loop reap selalu dijalankan, termasuk saat reap dimatikan (`0`). Bedanya
    // hanya perilaku: ambang `None` membuat putaran tidak memutus apa pun. Task
    // yang tidak pernah ada tidak bisa dinyalakan lewat SIGHUP, dan justru itu
    // gunanya reload: menyalakan pagar memori pada daemon yang sudah melayani.
    match hidup.idle_reap() {
        Some(idle_reap) => tracing::info!(
            idle_reap_secs = idle_reap.as_secs(),
            "loop reap menganggur aktif"
        ),
        None => tracing::warn!(
            "reap mesin menganggur dimatikan (idle_reap_secs = 0): memori tidak akan kembali sendiri"
        ),
    }
    tokio::spawn(loop_reap(
        pool.clone(),
        Arc::clone(&counters),
        Arc::clone(&wired.registry),
        Arc::clone(&wired.leases),
        Arc::clone(&hidup),
    ));

    // Supervisor anggaran memori: hanya kalau operator menulis `[budget]`.
    match file.budget() {
        Ok(Some(cfg)) => {
            tracing::info!(
                soft_mb = cfg.soft_bytes / lifecycle::budget::MB,
                high_mb = cfg.high_bytes / lifecycle::budget::MB,
                hard_mb = cfg.hard_bytes / lifecycle::budget::MB,
                poll_ms = cfg.poll_ms,
                "supervisor anggaran memori aktif"
            );
            tokio::spawn(loop_anggaran(pool.clone(), cfg, args.ready_timeout_ms));
        }
        Ok(None) => {
            tracing::info!("supervisor anggaran mati: [budget] tidak ada di config");
        }
        // Salah tulis ambang adalah config yang tidak sah, dan config yang
        // tidak sah harus terlihat saat start. Dibiarkan jalan, `hard` yang
        // tidak pernah tercapai membuat janji pemulihan jadi bohong.
        Err(err) => return Err(err),
    }

    // SIGHUP = muat ulang batas. Sinyal ini dipilih karena `systemctl reload`
    // memakai SIGHUP dan tidak membunuh proses: tab yang sedang dipakai tidak
    // ikut mati hanya karena operator menaikkan satu angka di config.
    dengar_sighup(
        pool.clone(),
        Arc::clone(&hidup),
        config_path.clone(),
        args.idle_reap_secs,
    );

    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind} gagal; port dipakai proses lain?"))?;

    for (id, slot) in pool.slots() {
        let hidup = slot.hidup();
        let pss_mb = hidup
            .as_ref()
            .and_then(|engine| engine.process_id())
            .and_then(metrics::read_pss_bytes)
            .map_or_else(
                || "?".to_string(),
                |bytes| format!("{:.1}", bytes as f64 / 1_048_576.0),
            );
        tracing::info!(
            mesin = %id,
            kind = slot.kind(),
            pid = ?hidup.as_ref().and_then(|engine| engine.process_id()),
            http = %hidup
                .as_ref()
                .map_or_else(|| "(tidak hidup)".to_owned(), |engine| engine.http_base()),
            pss_mb = %pss_mb,
            "mesin siap"
        );
    }

    for mesin in &resolved.machines {
        match mesin.max_tabs {
            // Batas yang tidak dilaporkan saat start akan dikira tidak berlaku:
            // operator menulis angka di config dan berharap melihatnya diyakini
            // oleh daemon, bukan cuma dibaca.
            Some(batas) if batas > 0 => tracing::info!(
                mesin = %mesin.id,
                max_tabs = batas,
                "batas tab ditegakkan untuk mesin ini"
            ),
            Some(_) => tracing::info!(
                mesin = %mesin.id,
                "max_tabs = 0 dibaca sebagai tanpa batas"
            ),
            None => {}
        }
    }

    pasang_control(&file, &wired).await?;

    tracing::info!(
        bind = %bind,
        control_bind = %file.control_bind(),
        handoff = %wiring::ringkas(&wired.registry),
        mesin = pool.len(),
        mesin_bawaan = pool.default_id(),
        multiplex = pool.is_multi(),
        config = %config_path.display(),
        "browserd siap"
    );
    println!("browserd mendengarkan di http://{bind}");
    println!("cdp_endpoint = \"http://{bind}\"");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server berhenti dengan error")?;

    apply_event(&machine, Event::Drain);
    apply_event(&machine, Event::Reap);

    tracing::info!(mesin = pool.len(), "mematikan semua mesin");
    matikan_semua(pool.slots().map(|(_, slot)| Arc::clone(slot)).collect()).await;
    tracing::info!("semua mesin dimatikan");

    apply_event(&machine, Event::Reaped);
    Ok(())
}

/// Batas handoff dari `[captcha]`.
///
/// Dibangun eksplisit, bukan lewat `From`, karena ketiga angka ini punya
/// bawaan di dua tempat: dokumen 03 §3.1 dan `CaptchaSection::default()`.
/// Satu titik pemetaan membuat perbedaan di antara keduanya terlihat.
fn limits_handoff(file: &FileConfig) -> policy::HandoffLimits {
    let captcha = file.captcha();
    policy::HandoffLimits {
        per_host_hour: captcha.handoff_per_host_hour,
        total_day: captcha.handoff_total_day,
        cooldown_secs: captcha.cooldown_secs,
    }
}

/// Menyalakan port kontrol di task sendiri.
///
/// Token dibuat di sini kalau belum ada, dan **tidak** pernah dicetak: yang
/// dilaporkan hanya path-nya. Owner dan agent membaca berkasnya sendiri.
async fn pasang_control(file: &FileConfig, wired: &Wired) -> Result<()> {
    let bind = file.control_bind();
    let token_path = file.control_token_path();
    let token = control::ensure_token_file(&token_path)
        .with_context(|| format!("menyiapkan token kontrol {}", token_path.display()))?;

    let control_state = Arc::new(control::ControlState {
        registry: Arc::clone(&wired.registry),
        token,
        started_at: unix_seconds().max(0) as u64,
        version: env!("CARGO_PKG_VERSION"),
        // Probe disuntikkan dari sini karena hanya daemon yang tahu alamat
        // WebSocket mesin. Router kontrol tetap tidak tahu cara bicara CDP.
        probe: Some(std::sync::Arc::new({
            // Slot, bukan alamat WebSocket. Mesin yang sudah di-reap punya port
            // baru setelah lahir lagi, jadi alamat yang diambil sekali saat
            // boot akan menunjuk port mati. Karena itu alamatnya dibaca di sini,
            // setelah mesin dipastikan hidup.
            let slot = Arc::clone(&wired.slot);
            let timeout_ms = wired.ready_timeout_ms;
            move |target_id: String| {
                let slot = Arc::clone(&slot);
                Box::pin(async move {
                    let alamat = crate::wiring::alamat_mesin(slot, timeout_ms)
                        .await
                        .map_err(|err| format!("mesin tidak bisa dipakai untuk probe: {err:#}"))?;
                    crate::wiring::probe_target(&alamat, &target_id)
                        .await
                        .map_err(|err| format!("probe gagal: {err}"))
                })
                    as std::pin::Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = Result<policy::challenge::DomFinding, String>,
                                > + Send,
                        >,
                    >
            }
        })),
    });
    let app = control::build_router(control_state);

    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind kontrol {bind} gagal; port dipakai proses lain?"))?;

    tracing::info!(control_bind = %bind, token = %token_path.display(), "port kontrol siap");
    println!("control_endpoint = \"http://{bind}\"");
    println!("control_token = \"{}\"", token_path.display());

    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!(%err, "server kontrol berhenti");
        }
    });
    Ok(())
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|durasi| durasi.as_secs() as i64)
        .unwrap_or(0)
}

/// Satu event state daemon. Transisi yang ditolak itu bug, bukan kejadian
/// normal, jadi jangan pernah ditelan tanpa jejak.
fn apply_event(machine: &Mutex<Machine>, event: Event) {
    let mut guard = machine.lock().unwrap_or_else(|err| err.into_inner());
    if let Err(err) = guard.apply(event) {
        tracing::warn!(%err, ?event, "transisi state daemon ditolak");
    }
}

/// Membuka store, menjalankan retensi, dan melaporkan isinya.
///
/// Profil yang belum ada dibuat di sini hanya kalau operator sudah menyebutnya
/// lewat config; daemon tidak mengarang identitas sendiri.
fn buka_store(file: &FileConfig) -> Result<store::Store> {
    let db = file.store_db();
    let key = file.store_key();
    let store = store::Store::open_with_key_file(&db, &key).with_context(|| {
        format!(
            "store tidak bisa dibuka ({} + {}); jalankan `browserd keygen` sekali di host ini",
            db.display(),
            key.display()
        )
    })?;

    let now = unix_seconds();
    let sweep = store.sweep(now)?;
    let profil = store.list_profiles()?;
    let cookie = store
        .count_cookies("scratch")
        .map(|jumlah| jumlah.to_string())
        .unwrap_or_else(|err| format!("tidak terbaca: {err}"));

    tracing::info!(
        db = %db.display(),
        profiles = profil.len(),
        cookies_scratch = %cookie,
        swept_session = sweep.session,
        swept_engine_run = sweep.engine_run,
        "store siap"
    );
    Ok(store)
}

/// Menyalakan satu mesin sesuai resepnya.
async fn nyalakan(
    mesin: &Mesin,
    timeout_ms: u64,
    dingin: Option<&tabstore::SumberDingin>,
    ambil: Option<Arc<dyn engine::native::ambil::Pengambil>>,
) -> Result<Arc<dyn Engine>> {
    let spec = engine::LaunchSpec::new(mesin.binary.clone(), mesin.user_data_dir.clone())
        .with_port(mesin.port)
        .with_args(mesin.args.iter().cloned());

    tracing::info!(
        mesin = %mesin.id,
        kind = %mesin.kind,
        binary = %mesin.binary.display(),
        user_data_dir = %mesin.user_data_dir.display(),
        port = mesin.port,
        "menyalakan mesin"
    );

    // Penggerak dipilih dari jenis mesin, bukan dari satu biner tetap: itulah
    // yang membuat `[engines.oxi]` bisa hidup berdampingan dengan shell dalam
    // satu proses daemon.
    // Sesi penyimpanan dingin memakai id mesin: dua mesin native tidak boleh
    // berbagi ruang tab.
    let pabrik = dingin.map(|sumber| sumber.pabrik(mesin.id.clone()));
    let penggerak = penggerak_kind_dengan(&mesin.kind, pabrik, ambil).with_context(|| {
        format!(
            "mesin '{}': jenis '{}' belum punya adapter",
            mesin.id, mesin.kind
        )
    })?;

    // Spawn proses dan polling port adalah kerja blocking. Menjalankannya
    // langsung di runtime akan menahan worker tokio selama mesin boot.
    tokio::task::spawn_blocking(move || penggerak.nyalakan(&spec, timeout_ms))
        .await
        .context("task peluncuran mesin panik")?
        .map_err(|err| anyhow::anyhow!("mesin '{}' gagal dinyalakan: {err}", mesin.id))
}

/// Menyalakan semua mesin yang dikonfigurasi.
///
/// Kalau salah satu gagal, yang sudah menyala ikut dimatikan sebelum error
/// dilaporkan. Meninggalkannya berarti ada browser hidup tanpa daemon yang
/// mengurusnya, dan proses seperti itu baru ketahuan saat RAM habis.
async fn launch_all(
    machines: &[Mesin],
    timeout_ms: u64,
    dingin: Option<&tabstore::SumberDingin>,
    ambil: Option<Arc<dyn engine::native::ambil::Pengambil>>,
) -> Result<BTreeMap<String, Arc<dyn Engine>>> {
    let mut engines: BTreeMap<String, Arc<dyn Engine>> = BTreeMap::new();
    for mesin in machines {
        match nyalakan(mesin, timeout_ms, dingin, ambil.clone()).await {
            Ok(engine) => {
                engines.insert(mesin.id.clone(), engine);
            }
            Err(err) => {
                let sudah: Vec<Arc<dyn Engine>> = engines.values().map(Arc::clone).collect();
                hentikan_semua(sudah).await;
                return Err(err);
            }
        }
    }
    Ok(engines)
}

/// Mematikan sekumpulan mesin, satu per satu lewat `spawn_blocking`.
///
/// Berurutan, bukan bersamaan: `stop` menunggu proses benar-benar hilang, dan
/// menjalankan beberapa penantian sekaligus hanya membuat log berhenti tidak
/// bisa dibaca saat salah satu menggantung.
async fn hentikan_semua(engines: Vec<Arc<dyn Engine>>) {
    for engine in engines {
        let kind = engine.kind();
        match tokio::task::spawn_blocking(move || engine.stop()).await {
            Ok(Ok(())) => tracing::info!(kind, "mesin berhenti"),
            Ok(Err(err)) => tracing::warn!(kind, %err, "mesin tidak berhenti bersih"),
            Err(err) => tracing::warn!(kind, %err, "task penghentian gagal"),
        }
    }
}

/// Berapa detik mesin boleh menganggur sebelum di-reap. `None` = tidak pernah.
///
/// `--idle-reap-secs` menang atas `[daemon] idle_reap_secs`; `0` di keduanya
/// berarti fitur ini mati. Bawaannya 60 detik, angka yang sama dengan
/// [`ReaperConfig::default`] supaya perilaku daemon tidak bergantung pada
/// bagian mana yang dibaca lebih dulu.
fn ambang_reap(args: &ServeArgs, file: &FileConfig) -> Option<Duration> {
    ambang_dari(args.idle_reap_secs, file)
}

/// Aturan yang sama tanpa `ServeArgs`, supaya reload memakai satu sumber.
///
/// `--idle-reap-secs` tetap menang setelah reload: flag baris perintah adalah
/// keputusan operator untuk proses INI, sedangkan berkas config bisa dibaca
/// ulang. Kalau berkas menang, SIGHUP akan diam-diam membatalkan flag yang
/// sudah diketik.
fn ambang_dari(flag: Option<u64>, file: &FileConfig) -> Option<Duration> {
    match flag
        .or_else(|| file.idle_reap_secs())
        .unwrap_or(DEFAULT_IDLE_REAP_SECS)
    {
        0 => None,
        detik => Some(Duration::from_secs(detik)),
    }
}

const DEFAULT_IDLE_REAP_SECS: u64 = 60;

/// Jarak antar pemeriksaan menganggur.
///
/// Bukan `ReaperConfig::poll` (250 ms): jeda itu untuk menunggu proses benar-
/// benar hilang setelah sinyal, bukan untuk mengintip slot. Dengan ambang satu
/// menit, memeriksa 4 kali per detik hanya membakar CPU tanpa satu pun klien
/// merasakannya. Lima detik tidak terasa, dan tetap jauh lebih rapat daripada
/// ambangnya.
const TICK_REAP: Duration = Duration::from_secs(5);

/// Loop reap: satu putaran tiap [`TICK_REAP`] selama daemon hidup.
///
/// Jam dibaca di sini, bukan di dalam predikat, supaya predikatnya tetap murni
/// dan bisa diuji tanpa tidur.
async fn loop_reap(
    pool: EnginePool,
    counters: Arc<Counters>,
    registry: Arc<Registry>,
    leases: Arc<Mutex<Gudang>>,
    hidup: Arc<Hidup>,
) {
    let mut ticker = tokio::time::interval(TICK_REAP);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        ticker.tick().await;
        // Ambang dibaca ulang tiap putaran, bukan dibekukan saat spawn: SIGHUP
        // menggantinya tanpa restart, dan angka yang disalin sekali akan membuat
        // reload terlihat berhasil di log tapi tidak berlaku di loop ini.
        // `None` = operator mematikan reap, jadi putaran ini tidak memutus apa
        // pun tanpa harus mematikan task-nya.
        let Some(idle_reap) = hidup.idle_reap() else {
            continue;
        };
        let cfg = ReaperConfig {
            idle_reap,
            ..ReaperConfig::default()
        };
        // Dua jam, dua satuan, dan itu disengaja: `putaran_reap` membandingkan
        // cap waktu mesin yang satuannya milidetik, sedangkan TTL handoff dan
        // lease masih dihitung dalam detik. Menyamakan keduanya ke detik
        // mengembalikan bug reap-terlalu-awal sampai satu detik penuh.
        let sekarang_ms = engine::slot::ms_sekarang();
        let sekarang = unix_seconds().max(0) as u64;
        // Handoff yang belum dijawab menahan reap. Registry belum menyimpan
        // mesin per handoff, jadi satu handoff menahan **semua** mesin: terlalu
        // luas memang, tapi arah kelirunya aman (memori ditahan), sedangkan
        // arah sebaliknya menghapus captcha yang sedang dijawab orang.
        let pending = registry.pending(sekarang).len();
        // Lease dibaca di dalam kunci yang sama sekali saja, cukup untuk satu
        // putaran: kalau lease dilepas sesudah pembacaan ini, putaran berikutnya
        // (5 detik lagi) sudah melihatnya. Menahan lock selama pemutusan akan
        // memblokir jalur yang sedang memegang tab.
        let penahan = {
            let gudang = leases.lock().unwrap_or_else(|err| err.into_inner());
            Penahan::dari_gudang(pending, &gudang, sekarang)
        };
        for id in putaran_reap(&pool, &counters, penahan, &cfg, sekarang_ms).await {
            tracing::info!(mesin = %id, "mesin di-reap karena menganggur");
        }
    }
}

/// Supervisor anggaran: satu [`Penjaga`] per mesin, satu putaran per `poll_ms`.
///
/// Yang dijaga adalah janji AC-017: satu mesin yang menembus anggaran **tidak**
/// membuat daemon keluar dan **tidak** menolak permintaan ke mesin lain. Karena
/// itu pemulihan menunggu di dalam `spawn_blocking` untuk mesin itu saja; loop
/// ini tidak memegang lock pool dan tidak menghentikan task lain.
async fn loop_anggaran(
    pool: EnginePool,
    cfg: lifecycle::budget::BudgetConfig,
    ready_timeout_ms: u64,
) {
    let pengukur = ProcPengukur;
    let mut penjaga: BTreeMap<String, Penjaga> = pool
        .ids()
        .map(|id| (id.clone(), Penjaga::baru(cfg)))
        .collect();

    // Batas bawah 100 ms: `poll_ms = 1` akan memanggil `/proc` seribu kali per
    // detik untuk membuktikan hal yang sama dengan satu kali per detik.
    let jeda = Duration::from_millis(cfg.poll_ms.max(100));
    let mut ticker = tokio::time::interval(jeda);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        ticker.tick().await;
        for (id, slot) in pool.slots() {
            // Mesin yang tidur tidak punya proses untuk diukur, dan itu bukan
            // pelanggaran anggaran. Mesin native juga tidak punya pid: ia hidup
            // di dalam proses daemon ini, jadi PSS-nya sudah termasuk PSS daemon
            // dan mengukurnya per mesin akan menghitung memori yang sama dua kali.
            let Some(pid) = slot.hidup().and_then(|engine| engine.process_id()) else {
                continue;
            };

            let putusan = match penjaga.get_mut(id) {
                Some(penjaga) => penjaga.putaran(&pengukur, pid),
                None => continue,
            };

            match putusan {
                Putusan::Diam { .. } => {}
                Putusan::TakTerukur { sebab } => tracing::warn!(
                    mesin = %id,
                    %sebab,
                    "PSS tidak terbaca: mesin dibiarkan hidup (gagal-terbuka)"
                ),
                Putusan::Pulihkan { pss, sebab } => {
                    if let Some(penjaga) = penjaga.get_mut(id) {
                        penjaga.mulai_pulih(Instant::now());
                    }
                    tracing::warn!(
                        mesin = %id,
                        pid,
                        pss_mb = pss / lifecycle::budget::MB,
                        aksi = %sebab,
                        "mesin menembus anggaran: dihentikan lalu dinyalakan ulang"
                    );

                    let hasil = pulihkan_mesin(Arc::clone(slot), ready_timeout_ms).await;
                    let lama = penjaga
                        .get_mut(id)
                        .and_then(|penjaga| penjaga.selesai_pulih(Instant::now()));

                    match (hasil, lama) {
                        (Ok(()), Some(lama)) => tracing::info!(
                            mesin = %id,
                            pulih_ms = lama.as_millis(),
                            dalam_batas = lama <= crate::anggaran::PEMULIHAN_MAKS,
                            "mesin pulih"
                        ),
                        (Ok(()), None) => {
                            tracing::warn!(mesin = %id, "mesin pulih tanpa jam tercatat")
                        }
                        (Err(err), _) => tracing::error!(
                            mesin = %id,
                            %err,
                            "pemulihan gagal: daemon tetap melayani mesin lain"
                        ),
                    }
                }
            }
        }
    }
}

/// Hentikan satu mesin lalu nyalakan ulang dari resep yang sama.
///
/// Kedua panggilan itu blocking (`matikan` menunggu proses benar-benar hilang,
/// `pastikan` menunggu mesin siap menjawab), jadi dijalankan di
/// `spawn_blocking`: menjalankannya langsung akan membekukan seluruh runtime
/// dan justru melanggar janji "mesin lain tetap dilayani".
async fn pulihkan_mesin(slot: Arc<Slot>, ready_timeout_ms: u64) -> Result<(), String> {
    let untuk_henti = Arc::clone(&slot);
    tokio::task::spawn_blocking(move || {
        untuk_henti
            .matikan()
            .map(|_| ())
            .map_err(|err| err.to_string())
    })
    .await
    .map_err(|err| format!("task penghentian gagal: {err}"))??;

    tokio::task::spawn_blocking(move || {
        let sekarang = engine::slot::ms_sekarang();
        slot.pastikan(ready_timeout_ms, sekarang)
            .map(|_| ())
            .map_err(|err| err.to_string())
    })
    .await
    .map_err(|err| format!("task penyalakan gagal: {err}"))?
}

/// Membaca batas dari berkas config. `Err` berarti: pertahankan yang lama.
///
/// Sengaja hanya membaca berkas, tidak me-resolve ulang mesin. Reload adalah
/// perubahan dua angka, bukan kesempatan menambah mesin: menyalakan mesin baru
/// butuh proses baru, port baru, dan profil baru, dan itu bukan sesuatu yang
/// boleh terjadi sebagai efek samping SIGHUP.
fn muat_batas(path: &Path, idle_flag: Option<u64>) -> Result<Batas, String> {
    let file = FileConfig::load(path).map_err(|err| format!("{err:#}"))?;
    let peta = reload::batas_tab(
        file.machines()
            .map(|(id, mesin)| (id.as_str(), mesin.max_tabs)),
    );
    Ok(Batas::baru(peta, ambang_dari(idle_flag, &file)))
}

/// Memasang penangan SIGHUP yang menerapkan batas baru tanpa restart.
///
/// Sinyal, bukan endpoint HTTP: port kontrol menerima perintah dari klien, dan
/// mengubah pagar memori bukan wewenang klien. SIGHUP juga yang dikirim
/// `systemctl reload`, jadi jalur operator tetap satu perintah yang sama.
#[cfg(unix)]
fn dengar_sighup(pool: EnginePool, hidup: Arc<Hidup>, path: PathBuf, idle_flag: Option<u64>) {
    use tokio::signal::unix::{SignalKind, signal};

    let mut hup = match signal(SignalKind::hangup()) {
        Ok(hup) => hup,
        Err(err) => {
            tracing::warn!(
                %err,
                "gagal memasang penangan SIGHUP: batas hanya bisa diubah lewat restart"
            );
            return;
        }
    };

    tokio::spawn(async move {
        loop {
            hup.recv().await;
            let hasil = muat_batas(&path, idle_flag);
            // Pool diperbarui hanya kalau pembacaan berhasil. Pada jalur gagal
            // tidak ada satu pun nilai yang disentuh, jadi batas yang sedang
            // ditegakkan tetap sama persis dengan sebelum sinyal datang.
            if let Ok(batas) = &hasil {
                let mesin = pool.ganti_batas_tab(batas.max_tabs.clone());
                tracing::info!(
                    mesin,
                    config = %path.display(),
                    "batas tab diganti dari config"
                );
            }
            match hidup.terapkan(hasil) {
                Hasil::Diterapkan { mesin, idle_reap } => tracing::info!(
                    mesin,
                    idle_reap_secs = idle_reap.map(|dur| dur.as_secs()),
                    config = %path.display(),
                    "config dimuat ulang tanpa restart"
                ),
                Hasil::Dipertahankan { sebab } => tracing::warn!(
                    %sebab,
                    config = %path.display(),
                    "config gagal dibaca; batas lama dipertahankan"
                ),
            }
        }
    });
}

/// Tanpa sinyal POSIX tidak ada `reload`; daemon tetap jalan dengan batas boot.
#[cfg(not(unix))]
fn dengar_sighup(pool: EnginePool, hidup: Arc<Hidup>, path: PathBuf, idle_flag: Option<u64>) {
    let _ = (pool, hidup, path, idle_flag);
    tracing::warn!("platform ini tidak punya SIGHUP: batas hanya berubah lewat restart");
}

/// Apa yang sedang menahan mesin supaya tidak di-reap pada putaran ini.
///
/// Dipisah dari angka mentah supaya dua alasan yang berbeda tidak bisa
/// tertukar diam-diam: handoff adalah manusia yang sedang menjawab captcha,
/// lease adalah sesi yang masih memakai tabnya. Keduanya menahan mesin, tapi
/// hanya satu yang boleh dibaca sebagai "ada pekerjaan berjalan" di log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Penahan {
    /// Handoff yang belum dijawab pada detik ini.
    pub handoff: usize,
    /// Tab yang masih dipegang lease hidup.
    pub lease: usize,
}

impl Penahan {
    #[must_use]
    pub const fn kosong() -> Self {
        Self {
            handoff: 0,
            lease: 0,
        }
    }

    #[must_use]
    pub const fn handoff(handoff: usize) -> Self {
        Self { handoff, lease: 0 }
    }

    #[must_use]
    pub const fn baru(handoff: usize, lease: usize) -> Self {
        Self { handoff, lease }
    }

    /// Membaca jumlah lease hidup dari gudang. Fungsi, bukan parameter angka,
    /// supaya tidak ada pemanggil yang bisa lupa menyapu lease kedaluwarsa
    /// lebih dulu: `target_ditahan` sudah menyaringnya.
    #[must_use]
    pub fn dari_gudang(handoff: usize, gudang: &Gudang, sekarang: u64) -> Self {
        Self {
            handoff,
            lease: gudang.target_ditahan(sekarang).len(),
        }
    }

    #[must_use]
    pub const fn total(self) -> usize {
        self.handoff + self.lease
    }

    #[must_use]
    pub const fn ada(self) -> bool {
        self.total() > 0
    }
}

/// Satu putaran pemeriksaan, tanpa timer dan tanpa jaringan.
///
/// Dipisah dari [`loop_reap`] supaya bisa diuji langsung: menguji loop berarti
/// menunggu, dan menunggu di test berarti test yang lambat dan kadang merah.
///
/// Yang dikembalikan hanya id mesin yang benar-benar berhenti. Mesin yang gagal
/// dimatikan dikembalikan ke slotnya, jadi putaran berikutnya mencoba lagi
/// alih-alih meninggalkan proses tanpa pemilik.
pub async fn putaran_reap(
    pool: &EnginePool,
    counters: &Counters,
    penahan: Penahan,
    cfg: &ReaperConfig,
    sekarang: u64,
) -> Vec<String> {
    let mut dimatikan = Vec::new();

    for (id, slot) in pool.slots() {
        // Mesin yang tidur atau sudah mati tidak punya proses untuk dimatikan.
        // `keadaan` sekaligus membuang pegangan ke proses yang sudah hilang.
        if !matches!(slot.keadaan(), Keadaan::Hidup) {
            continue;
        }

        let sesi = slot.sesi();
        let diam = slot.diam(sekarang);
        if idle_reap_due(sesi, penahan.total(), diam, cfg).is_none() {
            continue;
        }

        // Pengambilan dilakukan di dalam kunci yang sama dengan pemeriksaan
        // sesi: klien yang datang di antara dua panggilan itu tidak boleh
        // kehilangan mesinnya hanya karena reap sudah memutuskan lebih dulu.
        let Some(engine) = slot.ambil_kalau_diam(sekarang, cfg.idle_reap) else {
            continue;
        };

        match matikan_satu(&engine, ReapReason::Idle, cfg).await {
            Ok(report) => {
                counters.record_reap(&report);
                if report.outcome.released() {
                    tracing::info!(
                        mesin = %id,
                        sesi,
                        diam_secs = diam.as_secs(),
                        laporan = %report,
                        "reap selesai"
                    );
                    dimatikan.push(id.clone());
                } else {
                    // Sinyal sudah dikirim dan prosesnya masih ada: mesin
                    // dikembalikan ke slot supaya tidak berubah menjadi proses
                    // yatim yang tidak pernah diurus siapa pun.
                    slot.kembalikan(engine);
                    tracing::error!(
                        mesin = %id,
                        laporan = %report,
                        "proses mesin bertahan setelah SIGKILL; mesin dikembalikan ke slot"
                    );
                }
            }
            Err(err) => {
                slot.kembalikan(engine);
                tracing::warn!(mesin = %id, %err, "reap ditunda; mesin dikembalikan ke slot");
            }
        }
    }

    dimatikan
}

/// Menjalankan protokol reap terhadap satu mesin.
///
/// Sinyalnya dikirim lewat [`crate::reap::PgidGroup`] atas **pid mesin**, yang
/// di sini sama dengan pgid-nya karena [`ShellEngine`] melahirkan anaknya dengan
/// `process_group(0)`. Urutan `SIGTERM -> tunggu -> SIGKILL` tidak ditulis ulang
/// di sini: itu milik `lifecycle::reaper`, dan mengulangnya berarti punya dua
/// versi protokol yang bisa berbeda diam-diam.
///
/// Laporan yang dikembalikan berisi sinyal yang **benar-benar** dikirim, jadi
/// `browser_reap_signal_total` tidak diisi tebakan.
async fn matikan_satu(
    engine: &Arc<dyn Engine>,
    alasan: ReapReason,
    cfg: &ReaperConfig,
) -> Result<ReapReport, String> {
    let engine = Arc::clone(engine);
    let cfg = *cfg;

    tokio::task::spawn_blocking(move || -> Result<ReapReport, String> {
        let Some(pid) = engine.process_id() else {
            // Tanpa pid tidak ada yang bisa disinyalir. Mengarang laporan di
            // sini akan membuat metrik reap menghitung sesuatu yang tidak
            // pernah terjadi, jadi putarannya lebih baik ditunda.
            return Err("pid mesin tidak terbaca; reap ditunda".to_owned());
        };

        let group = crate::reap::PgidGroup::new(pid);
        let report =
            lifecycle::reaper::reap(&group, &cfg, alasan).map_err(|err| err.to_string())?;

        if report.outcome.released() {
            // Pegangan `ShellEngine` masih menyimpan `Child`. Tanpa `stop`,
            // `is_running` bisa menjawab "hidup" untuk proses yang sudah mati,
            // dan slot akan terus mencoba mematikannya.
            if let Err(err) = engine.stop() {
                tracing::warn!(pid, %err, "menyinkronkan pegangan mesin setelah reap gagal");
            }
        }

        Ok(report)
    })
    .await
    .map_err(|err| format!("task reap gagal: {err}"))?
}

/// Mematikan semua mesin milik daemon, termasuk yang tidur.
///
/// Lewat [`Slot::matikan`], bukan daftar pegangan yang dikumpulkan lebih dulu:
/// mesin yang baru selesai boot di antara pengumpulan dan penghentian tetap
/// ikut mati, karena pengambilan mesin terjadi di dalam slot.
async fn matikan_semua(slots: Vec<Arc<Slot>>) {
    for slot in slots {
        let id = slot.id().to_owned();
        match tokio::task::spawn_blocking(move || slot.matikan()).await {
            Ok(Ok(true)) => tracing::info!(mesin = %id, "mesin dimatikan"),
            Ok(Ok(false)) => tracing::debug!(mesin = %id, "mesin sudah tidur"),
            Ok(Err(err)) => tracing::warn!(mesin = %id, %err, "mesin tidak berhenti bersih"),
            Err(err) => tracing::warn!(mesin = %id, %err, "task penghentian gagal"),
        }
    }
}

/// Batas tab per mesin yang benar-benar ditegakkan.
///
/// Mesin tanpa `max_tabs`, dan mesin dengan `max_tabs = 0`, tidak masuk peta:
/// keduanya berarti tanpa batas, dan mesin tanpa entri tidak membayar satu
/// pemeriksaan pun di jalur pembuatan tab.
fn batas_tab(machines: &[Mesin]) -> BTreeMap<String, u32> {
    reload::batas_tab(
        machines
            .iter()
            .map(|mesin| (mesin.id.as_str(), mesin.max_tabs)),
    )
}

/// Satu mesin yang harus dinyalakan, hasil penggabungan CLI dan config.
#[derive(Debug, Clone)]
struct Mesin {
    id: String,
    kind: String,
    binary: PathBuf,
    user_data_dir: PathBuf,
    port: u16,
    args: Vec<String>,
    max_tabs: Option<u32>,
}

#[derive(Debug, Clone)]
struct Resolved {
    bind: String,
    /// Id mesin yang dipakai kalau klien tidak menyebut mesin.
    default_id: String,
    machines: Vec<Mesin>,
}

impl Resolved {
    fn resolve(args: &ServeArgs, file: &FileConfig) -> Result<Self> {
        let bind = args
            .bind
            .clone()
            .or_else(|| file.daemon.as_ref().and_then(|daemon| daemon.bind.clone()))
            .unwrap_or_else(|| format!("127.0.0.1:{}", args.port));

        // Config yang menyebut `[engines.*]` menang: mesin yang daftarnya ada
        // di berkas tidak boleh ditimpa diam-diam oleh bentuk satu mesin dari
        // CLI, karena operator akan mengira mesin kedua ikut jalan.
        let machines = if file.machines().next().is_none() {
            vec![dari_cli(args, file)?]
        } else {
            dari_config(args, file)?
        };

        let default_id = match file.engine_default() {
            Some(nama) => {
                if !machines.iter().any(|mesin| mesin.id == nama) {
                    bail!(
                        "[daemon] engine_default = '{nama}' tetapi mesin itu tidak ada; yang dikonfigurasi: {}",
                        daftar_id(&machines)
                    );
                }
                nama.to_owned()
            }
            // Tanpa petunjuk eksplisit, id terkecil menurut abjad. Deterministik,
            // dan untuk satu mesin hasilnya id mesin itu sendiri.
            None => machines
                .iter()
                .map(|mesin| mesin.id.as_str())
                .min()
                .unwrap_or_default()
                .to_owned(),
        };

        Ok(Self {
            bind,
            default_id,
            machines,
        })
    }
}

fn daftar_id(machines: &[Mesin]) -> String {
    machines
        .iter()
        .map(|mesin| mesin.id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn wajib_absolut(path: &Path, apa: &str) -> Result<()> {
    if !path.is_absolute() {
        bail!("{apa} harus path absolut, dapat: {}", path.display());
    }
    Ok(())
}

/// Penggerak untuk satu jenis mesin, termasuk mesin yang **tidak** punya biner.
///
/// `engine::slot::penggerak_untuk` sengaja tidak tahu soal mesin native: crate
/// `engine` tidak boleh bergantung pada `cdp` (arah ketergantungannya justru
/// sebaliknya, `cdp` yang memakai `engine`). Karena itu penggabungan dua daftar
/// ini hidup di daemon, satu-satunya tempat yang melihat keduanya.
fn penggerak_kind(kind: &str) -> Option<Arc<dyn Penggerak>> {
    penggerak_kind_dengan(kind, None, None)
}

/// Sama dengan [`penggerak_kind`], tapi mesin native diberi penyimpanan dingin
/// pilihan pemanggil.
///
/// Daftar "jenis apa saja yang punya adapter" memakai bentuk tanpa penyimpanan
/// karena pertanyaannya cuma ada atau tidak ada. Jalur peluncuran memakai
/// bentuk ini supaya tab yang disingkirkan mendarat di SQLite.
fn penggerak_kind_dengan(
    kind: &str,
    dingin: Option<cdp::native_wire::PabrikDingin>,
    ambil: Option<Arc<dyn engine::native::ambil::Pengambil>>,
) -> Option<Arc<dyn Penggerak>> {
    if kind == NATIVE_KIND {
        let penggerak = match (dingin, ambil) {
            (Some(pabrik), Some(ambil)) => PenggerakNative::dengan_ambil(pabrik, ambil),
            (Some(pabrik), None) => PenggerakNative::baru(pabrik),
            (None, _) => PenggerakNative::default(),
        };
        return Some(Arc::new(penggerak));
    }
    penggerak_untuk(kind)
}

/// Mesin yang hidup di dalam proses daemon: tidak ada biner yang dieksekusi.
fn tanpa_biner(kind: &str) -> bool {
    kind == NATIVE_KIND
}

/// Jenis yang benar-benar punya adapter, untuk pesan galat. Dihitung dari
/// daftar yang sama dengan `penggerak_kind`, bukan diketik ulang: daftar yang
/// diketik ulang akan basi setiap kali adapter baru masuk.
fn jenis_tersedia() -> String {
    ["shell", "oxi", "full", NATIVE_KIND]
        .iter()
        .filter(|kind| penggerak_kind(kind).is_some())
        .map(|kind| format!("'{kind}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Penemuan biner bawaan untuk satu jenis mesin. Dipakai dua jalur (CLI dan
/// config) supaya "tidak ketemu biner" tidak pernah bergantung pada jalur mana
/// yang dipakai.
fn pencarian_biner(kind: &str) -> Option<PathBuf> {
    match kind {
        "shell" => discover_shell(),
        "oxi" => discover_oxi(),
        // Tiga ejaan, satu mesin. `policy::EngineKind::dari_nama` menerima
        // ketiganya, jadi penemuan binernya pun harus menerima ketiganya:
        // kalau tidak, operator yang menulis `engine = "full"` di config dapat
        // "biner tidak ditemukan" padahal Chromium-nya terpasang.
        "full" | "chrome-full" | "chrome_full" => discover_full(),
        _ => None,
    }
}

/// Jalur lama: satu mesin dari argumen CLI dan `[engines.chrome_shell]`.
fn dari_cli(args: &ServeArgs, file: &FileConfig) -> Result<Mesin> {
    let kind = args.engine.clone();
    // `[engines.chrome_shell].binary` hanya berlaku untuk shell: menyalakan oxi
    // dengan biner shell menghasilkan boot yang gagal dengan pesan membingungkan.
    // Mesin native tidak mengeksekusi biner apa pun: `PenggerakNative`
    // mengabaikan `LaunchSpec.binary`. Memaksa operator menyetel path biner
    // untuknya hanya menghasilkan galat yang tidak bisa diperbaiki.
    let binary = if tanpa_biner(&kind) {
        PathBuf::new()
    } else {
        let binary = args
            .engine_binary
            .clone()
            .or_else(|| (kind == "shell").then(|| file.shell_binary()).flatten())
            .or_else(|| pencarian_biner(&kind))
            .with_context(|| {
                format!(
                    "biner engine '{kind}' tidak ditemukan. Set --engine-binary atau `binary` di config"
                )
            })?;
        wajib_absolut(&binary, "biner engine")?;
        binary
    };

    let user_data_dir = args
        .user_data_dir
        .clone()
        .unwrap_or_else(config::default_user_data_dir);
    wajib_absolut(&user_data_dir, "user-data-dir")?;

    let engine_port = if args.engine_port != 0 {
        args.engine_port
    } else {
        file.daemon
            .as_ref()
            .and_then(|daemon| daemon.engine_port)
            .unwrap_or(0)
    };

    // Flag bawaan `[engines.chrome_shell].args` adalah flag Chrome. Memberikannya
    // ke mesin lain hanya menghasilkan `unexpected argument` saat boot.
    let mut engine_args = if kind == "shell" {
        file.shell_args()
    } else {
        Vec::new()
    };
    engine_args.extend(args.extra_arg.iter().cloned());

    Ok(Mesin {
        id: args.engine.clone(),
        kind,
        binary,
        user_data_dir,
        port: engine_port,
        args: engine_args,
        max_tabs: file.max_tabs(),
    })
}

/// Jalur M4: setiap `[engines.<id>]` jadi satu mesin.
fn dari_config(args: &ServeArgs, file: &FileConfig) -> Result<Vec<Mesin>> {
    let jumlah = file.machines().count();
    if jumlah > 1 && args.engine_port != 0 {
        bail!(
            "--engine-port hanya berlaku untuk satu mesin; dengan {jumlah} mesin, \
             atur `port` per mesin di config (kosongkan supaya mesin memilih sendiri)"
        );
    }
    if jumlah > 1 && args.user_data_dir.is_some() {
        bail!(
            "--user-data-dir hanya berlaku untuk satu mesin: dua mesin yang berbagi \
             direktori profil akan bertabrakan karena Chrome mengunci profilnya. \
             Atur `user_data_dir` per mesin di config"
        );
    }

    let mut machines: Vec<Mesin> = Vec::with_capacity(jumlah);
    let mut dir_terpakai: Vec<(PathBuf, String)> = Vec::with_capacity(jumlah);

    for (id, section) in file.machines() {
        let kind = section.kind.clone().unwrap_or_else(|| "shell".to_owned());
        if penggerak_kind(&kind).is_none() {
            bail!(
                "mesin '{id}': jenis '{kind}' belum punya adapter; yang tersedia: {}",
                jenis_tersedia()
            );
        }

        let binary = if tanpa_biner(&kind) {
            PathBuf::new()
        } else {
            let binary = section
                .binary
                .as_ref()
                .map(|raw| config::expand_home(raw))
                .or_else(|| args.engine_binary.clone())
                .or_else(|| pencarian_biner(&kind))
                .with_context(|| {
                    format!(
                        "mesin '{id}': biner engine '{kind}' tidak ditemukan; set `binary` untuk mesin ini"
                    )
                })?;
            wajib_absolut(&binary, &format!("biner mesin '{id}'"))?;
            binary
        };

        let user_data_dir = section
            .user_data_dir
            .as_ref()
            .map(|raw| config::expand_home(raw))
            .or_else(|| args.user_data_dir.clone())
            .unwrap_or_else(|| config::machine_user_data_dir(id));
        wajib_absolut(&user_data_dir, &format!("user_data_dir mesin '{id}'"))?;

        if let Some((_, lain)) = dir_terpakai.iter().find(|(dir, _)| dir == &user_data_dir) {
            bail!(
                "mesin '{id}' dan '{lain}' memakai direktori profil yang sama ({}); \
                 Chrome mengunci profil, jadi mesin kedua tidak akan bisa start",
                user_data_dir.display()
            );
        }
        dir_terpakai.push((user_data_dir.clone(), id.clone()));

        let port = section.port.filter(|port| *port != 0).unwrap_or({
            if args.engine_port != 0 {
                args.engine_port
            } else {
                0
            }
        });

        let mut engine_args = section.args.clone();
        engine_args.extend(args.extra_arg.iter().cloned());

        machines.push(Mesin {
            id: id.clone(),
            kind,
            binary,
            user_data_dir,
            port,
            args: engine_args,
            max_tabs: section.max_tabs,
        });
    }

    Ok(machines)
}

/// Penanda unik untuk path websocket. Bukan rahasia, bukan autentikasi:
/// pengamanan port ada di bind ke loopback, bukan di nama path.
fn session_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{:x}{:x}", std::process::id(), nanos)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(err) => tracing::warn!(%err, "tidak bisa memasang handler SIGTERM"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("SIGINT diterima"),
        () = terminate => tracing::info!("SIGTERM diterima"),
    }
}

/// Melepas proses dari terminal. Memakai biner `setsid` alih-alih `pre_exec`,
/// karena `pre_exec` butuh `unsafe` dan crate ini melarangnya.
pub fn maybe_detach(args: &ServeArgs, log_dir: Option<PathBuf>) -> Result<Option<u32>> {
    if !args.detach || std::env::var_os(DETACHED_ENV).is_some() {
        return Ok(None);
    }

    let exe = std::env::current_exe().context("tidak bisa menemukan biner sendiri")?;
    let log_dir = log_dir.unwrap_or_else(|| config::home().join(".opencrabs/browser-daemon/logs"));
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("membuat direktori log {}", log_dir.display()))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let log_path = log_dir.join(format!("browserd-{stamp}.log"));
    let log = std::fs::File::create(&log_path)
        .with_context(|| format!("membuat log {}", log_path.display()))?;

    let mut command = match find_setsid() {
        Some(setsid) => {
            let mut command = std::process::Command::new(setsid);
            command.arg(&exe);
            command
        }
        None => std::process::Command::new(&exe),
    };

    command.args(rebuild_args(args));
    command.env(DETACHED_ENV, "1");
    command
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().context("menduplikasi log")?)
        .stderr(log);

    let child = command.spawn().context("gagal melepas proses")?;
    println!("browserd berjalan di latar, pid {}", child.id());
    println!("log: {}", log_path.display());
    Ok(Some(child.id()))
}

fn rebuild_args(args: &ServeArgs) -> Vec<String> {
    let mut out = vec![
        "serve".to_owned(),
        "--port".to_owned(),
        args.port.to_string(),
        "--engine".to_owned(),
        args.engine.clone(),
        "--engine-port".to_owned(),
        args.engine_port.to_string(),
        "--ready-timeout-ms".to_owned(),
        args.ready_timeout_ms.to_string(),
    ];
    if let Some(bind) = &args.bind {
        out.push("--bind".to_owned());
        out.push(bind.clone());
    }
    if let Some(binary) = &args.engine_binary {
        out.push("--engine-binary".to_owned());
        out.push(binary.display().to_string());
    }
    if let Some(dir) = &args.user_data_dir {
        out.push("--user-data-dir".to_owned());
        out.push(dir.display().to_string());
    }
    if let Some(config) = &args.config {
        out.push("--config".to_owned());
        out.push(config.display().to_string());
    }
    for extra in &args.extra_arg {
        out.push("--arg".to_owned());
        out.push(extra.clone());
    }
    out
}

fn find_setsid() -> Option<PathBuf> {
    ["/usr/bin/setsid", "/bin/setsid"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

/// Tolak start kalau `[policy]` menunjuk mesin yang tidak terpasang.
///
/// Semua pelanggaran dilaporkan sekaligus: operator yang salah menulis tiga
/// aturan cukup satu kali memperbaiki.
fn validasi_policy(file: &FileConfig, terpasang: &[String]) -> Result<()> {
    // `terpasang` berisi JENIS mesin yang hidup (mis. `shell`, `full`), bukan id.
    let katalog = policy::Catalog::dari_mesin(terpasang.iter().map(String::as_str));
    let aturan = file.policy();
    match aturan.validasi(&katalog) {
        Ok(()) => Ok(()),
        Err(salah) => {
            let daftar = salah
                .iter()
                .map(|satu| format!("  - {satu}"))
                .collect::<Vec<_>>()
                .join("\n");
            bail!(
                "[policy] tidak sah untuk mesin yang terpasang ({}):\n{daftar}",
                terpasang.join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn args() -> ServeArgs {
        ServeArgs {
            port: 9224,
            bind: None,
            engine: "shell".to_owned(),
            engine_binary: Some(PathBuf::from("/abs/shell")),
            engine_port: 0,
            user_data_dir: Some(PathBuf::from("/abs/udd")),
            ready_timeout_ms: 15_000,
            extra_arg: vec!["--mute-audio".to_owned()],
            config: None,
            idle_reap_secs: None,
            detach: false,
        }
    }

    #[test]
    fn idle_reap_secs_nol_mematikan_fitur_bukan_menjadi_batas_nol() {
        // `0` adalah cara operator mematikan reap. Kalau angka itu diperlakukan
        // sebagai ambang, batasnya jadi nol detik dan setiap mesin dimatikan
        // segera setelah klien terakhir pergi.
        let mut args = args();
        assert_eq!(
            ambang_reap(&args, &FileConfig::default()),
            Some(Duration::from_secs(60)),
            "tanpa petunjuk, bawaannya 60 detik"
        );

        args.idle_reap_secs = Some(0);
        assert_eq!(ambang_reap(&args, &FileConfig::default()), None);

        args.idle_reap_secs = Some(7);
        assert_eq!(
            ambang_reap(&args, &FileConfig::default()),
            Some(Duration::from_secs(7)),
            "CLI menang atas bawaan"
        );
    }

    #[test]
    fn path_relatif_ditolak_dengan_pesan_jelas() {
        let mut args = args();
        args.engine_binary = Some(PathBuf::from("shell-relatif"));
        let err = Resolved::resolve(&args, &FileConfig::default())
            .expect_err("path relatif harus ditolak");
        assert!(err.to_string().contains("absolut"));
    }

    #[test]
    fn jenis_mesin_native_punya_penggerak_tanpa_biner() {
        // Mesin native hidup di dalam proses daemon. Dua akibatnya diuji di
        // sini: ia harus muncul di daftar penggerak (kalau tidak, operator yang
        // menulis `kind = "rust-native"` dapat "belum punya adapter"), dan ia
        // tidak boleh memaksa operator menyetel biner yang tidak akan
        // dieksekusi.
        assert!(
            penggerak_kind(NATIVE_KIND).is_some(),
            "kind native harus punya penggerak"
        );
        assert!(
            jenis_tersedia().contains(NATIVE_KIND),
            "pesan galat harus menyebut jenis yang benar-benar ada, dapat: {}",
            jenis_tersedia()
        );
        assert!(tanpa_biner(NATIVE_KIND), "native tidak mengeksekusi biner");
        assert!(!tanpa_biner("shell"), "shell tetap butuh biner");

        // Jalur CLI tanpa biner untuk native: harus lolos resolve, bukan
        // berhenti di "biner tidak ditemukan".
        let mut args = args();
        args.engine = NATIVE_KIND.to_owned();
        args.engine_binary = None;
        args.user_data_dir = None;
        let resolved = Resolved::resolve(&args, &FileConfig::default())
            .expect("native harus bisa ditentukan tanpa biner");
        assert_eq!(resolved.machines.len(), 1);
        assert_eq!(resolved.machines[0].kind, NATIVE_KIND);
        assert!(
            resolved.machines[0].binary.as_os_str().is_empty(),
            "biner mesin native harus kosong, dapat: {}",
            resolved.machines[0].binary.display()
        );
    }

    #[test]
    fn port_cli_menang_atas_default() {
        let resolved = Resolved::resolve(&args(), &FileConfig::default()).expect("resolve");
        assert_eq!(resolved.bind, "127.0.0.1:9224");
    }

    #[test]
    fn argumen_melekat_dibangun_ulang_dengan_benar() {
        let rebuilt = rebuild_args(&args());
        assert_eq!(rebuilt[0], "serve");
        assert!(rebuilt.windows(2).any(|pair| pair == ["--port", "9224"]));
        assert!(
            rebuilt
                .windows(2)
                .any(|pair| pair == ["--arg", "--mute-audio"])
        );
    }

    #[test]
    fn token_sesi_berbeda_tiap_panggilan() {
        assert_ne!(session_token(), session_token());
    }

    fn config_dua_mesin() -> FileConfig {
        let text = r#"
[engines.chrome_shell]
binary = "/abs/shell"

[engines.chrome_shell2]
binary = "/abs/shell2"
"#;
        toml::from_str(text).expect("toml sah")
    }

    #[test]
    fn dua_mesin_dari_config_resolve_jadi_dua() {
        // `--engine-binary` dan user-data-dir dari CLI tidak boleh dipakai di
        // sini: keduanya hanya berlaku untuk satu mesin.
        let mut args = args();
        args.engine_binary = None;
        args.user_data_dir = None;

        let resolved = Resolved::resolve(&args, &config_dua_mesin()).expect("resolve");
        let ids: Vec<&str> = resolved
            .machines
            .iter()
            .map(|mesin| mesin.id.as_str())
            .collect();
        assert_eq!(ids, vec!["chrome_shell", "chrome_shell2"]);
        assert_eq!(resolved.default_id, "chrome_shell");
        // Direktori profil dipisah sendiri-sendiri, bukan satu untuk semua.
        assert_ne!(
            resolved.machines[0].user_data_dir,
            resolved.machines[1].user_data_dir
        );
        assert_eq!(
            resolved.machines[0].port, 0,
            "port biar mesin pilih sendiri"
        );
    }

    #[test]
    fn direktori_profil_sama_ditolak_sebelum_menyalakan() {
        let text = r#"
[engines.a]
binary = "/abs/shell"
user_data_dir = "/abs/sama"

[engines.b]
binary = "/abs/shell2"
user_data_dir = "/abs/sama"
"#;
        let config: FileConfig = toml::from_str(text).expect("toml sah");
        let mut args = args();
        args.user_data_dir = None;
        let err = Resolved::resolve(&args, &config).expect_err("harus ditolak");
        let pesan = err.to_string();
        assert!(
            pesan.contains("direktori profil") && pesan.contains("/abs/sama"),
            "pesan harus menyebut sebab dan path, dapat: {pesan}"
        );
    }

    #[test]
    fn jenis_mesin_tidak_dikenal_ditolak_dengan_nama_mesin() {
        let text = r#"
[engines.chrome_besar]
kind = "chrome-besar"
binary = "/abs/chrome"
"#;
        let config: FileConfig = toml::from_str(text).expect("toml sah");
        let mut args = args();
        args.user_data_dir = None;
        let err = Resolved::resolve(&args, &config).expect_err("harus ditolak");
        let pesan = err.to_string();
        assert!(
            pesan.contains("chrome_besar") && pesan.contains("chrome-besar"),
            "pesan harus menyebut mesin dan jenisnya, dapat: {pesan}"
        );
    }

    #[test]
    fn engine_port_cli_ditolak_saat_mesin_lebih_dari_satu() {
        let mut args = args();
        args.engine_binary = None;
        args.user_data_dir = None;
        args.engine_port = 43000;
        let err = Resolved::resolve(&args, &config_dua_mesin()).expect_err("harus ditolak");
        assert!(err.to_string().contains("--engine-port"), "dapat: {err}");
    }

    #[test]
    fn engine_default_yang_tidak_ada_ditolak() {
        let text = r#"
[daemon]
engine_default = "tidak_ada"

[engines.chrome_shell]
binary = "/abs/shell"
"#;
        let config: FileConfig = toml::from_str(text).expect("toml sah");
        let mut args = args();
        args.engine_binary = None;
        args.user_data_dir = None;
        let err = Resolved::resolve(&args, &config).expect_err("harus ditolak");
        assert!(
            err.to_string().contains("tidak_ada") && err.to_string().contains("chrome_shell"),
            "pesan harus menyebut nama yang salah dan yang tersedia, dapat: {err}"
        );
    }

    #[test]
    fn durasi_graceful_tidak_nol() {
        assert!(Duration::from_secs(1).as_secs() > 0);
    }

    #[test]
    fn policy_config_ditolak_saat_start_dengan_nama_mesin() {
        // Jalur yang benar-benar dipanggil daemon sebelum bind: aturan yang
        // menunjuk mesin hilang harus menghentikan start, dan pesannya memuat
        // nama mesin yang kurang.
        let file: FileConfig = toml::from_str(
            "[policy]\ndefault = \"chrome-shell\"\n[policy.rules]\n\"shop.example\" = \"chrome-full\"\n",
        )
        .expect("harus bisa diparse");

        let err = validasi_policy(&file, &["chrome-shell".to_owned()])
            .expect_err("chrome-full tidak terpasang");
        let pesan = format!("{err:#}");
        assert!(pesan.contains("chrome-full"), "{pesan}");
        assert!(
            pesan.contains("chrome-shell"),
            "pesan harus menyebut mesin yang ada: {pesan}"
        );

        // Begitu chrome-full terpasang, start lolos.
        assert!(
            validasi_policy(
                &file,
                &["chrome-shell".to_owned(), "chrome-full".to_owned()]
            )
            .is_ok()
        );
    }

    /// AC-019: `browserd serve` polos tidak boleh bisa menyentuh port browser
    /// produksi.
    ///
    /// `Resolved::resolve` memakai `args.port` kalau tidak ada `--bind` dan
    /// tidak ada `[daemon] bind`. Kalau angka bawaannya 9222, daemon
    /// pengembangan merebut port browser produksi. Uji ini mengunci rantai itu
    /// dari ujung ke ujung: argumen bawaan masuk, bind keluar. Bukan sekadar
    /// mengecek konstanta `--port` di `cli.rs`, karena rantai itulah yang
    /// benar-benar dipakai daemon saat start.
    #[test]
    fn bawaan_polos_bind_ke_port_pengembangan_bukan_port_produksi() {
        let resolved = Resolved::resolve(&args(), &FileConfig::default()).expect("resolve bawaan");
        assert_eq!(resolved.bind, "127.0.0.1:9224");
        assert!(
            !resolved.bind.ends_with(":9222"),
            "bind bawaan menyentuh port browser produksi: {}",
            resolved.bind
        );
        // Port debug mesin dibiarkan 0: mesin memilih sendiri dari
        // `DevToolsActivePort`, jadi tidak ada angka tetap yang bisa
        // bertabrakan dengan produksi.
        assert!(
            resolved.machines.iter().all(|mesin| mesin.port == 0),
            "tanpa --engine-port, mesin memilih portnya sendiri"
        );
        assert!(
            resolved.machines.iter().all(|mesin| mesin.port != 9222),
            "port debug mesin tidak boleh 9222"
        );
    }

    /// `[daemon]` yang ada tapi tanpa `bind` tetap memakai port pengembangan:
    /// kehadiran seksi itu tidak boleh memindahkan bind ke angka lain.
    #[test]
    fn config_daemon_tanpa_bind_tetap_port_pengembangan() {
        let file: FileConfig = toml::from_str("[daemon]\nengine_port = 0\n").expect("toml sah");
        let resolved = Resolved::resolve(&args(), &file).expect("resolve");
        assert_eq!(resolved.bind, "127.0.0.1:9224");
    }

    /// 9222 hanya boleh tercapai kalau operator menulisnya sendiri.
    ///
    /// Ini bukan aturan yang ditegakkan kode (operator bebas memilih bind), tapi
    /// pengunci perilaku: satu-satunya jalan ke 9222 adalah `bind` yang ditulis
    /// tangan, bukan bawaan yang lolos diam-diam.
    #[test]
    fn port_produksi_hanya_tercapai_lewat_bind_eksplisit() {
        let file: FileConfig =
            toml::from_str("[daemon]\nbind = \"127.0.0.1:9222\"\n").expect("toml sah");
        let resolved = Resolved::resolve(&args(), &file).expect("resolve");
        assert_eq!(
            resolved.bind, "127.0.0.1:9222",
            "operator yang menulis 9222 tetap dihormati"
        );
    }

    /// Profil bawaan mesin hidup di direktori cache pengembangan, terpisah dari
    /// profil browser produksi.
    #[test]
    fn profil_bawaan_mesin_ada_di_direktori_pengembangan() {
        let mut args = args();
        args.user_data_dir = None;
        let resolved = Resolved::resolve(&args, &FileConfig::default()).expect("resolve");
        let dir = &resolved.machines[0].user_data_dir;
        let akar = config::home().join(".cache/opencrabs-browser");
        assert!(
            dir.starts_with(&akar),
            "profil mesin ({}) harus di bawah {}",
            dir.display(),
            akar.display()
        );
    }
}

#[cfg(test)]
mod muat_batas_tests {
    use std::io::Write;

    use super::*;

    /// Nama berkas memuat pid dan nama uji: dua uji yang menulis berkas yang
    /// sama bisa saling menimpa saat dijalankan paralel, dan kegagalannya akan
    /// terlihat seperti bug reload.
    fn berkas(nama: &str, isi: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("ocb-reload-{}-{nama}.toml", std::process::id()));
        let mut file = std::fs::File::create(&path).expect("membuat berkas uji");
        file.write_all(isi.as_bytes()).expect("menulis berkas uji");
        path
    }

    // AC-010 pada jalur nyata: angka dibaca dari berkas yang baru ditulis, dan
    // `0` tetap berarti mati, bukan "nol detik".
    #[test]
    fn batas_dibaca_dari_berkas_dan_nol_mematikan_reap() {
        let path = berkas(
            "naik",
            r#"
[daemon]
idle_reap_secs = 0

[engines.kiri]
kind = "shell"
max_tabs = 9
"#,
        );
        let batas = muat_batas(&path, None).expect("config sah");
        assert_eq!(batas.max_tabs.get("kiri"), Some(&9));
        assert_eq!(batas.idle_reap, None, "0 mematikan reap");
        let _ = std::fs::remove_file(&path);
    }

    // AC-011: berkas rusak tidak menghasilkan batas baru sama sekali, dan
    // penerapannya mempertahankan angka yang sedang berlaku.
    #[test]
    fn berkas_rusak_mempertahankan_batas_yang_sedang_berlaku() {
        let path = berkas("rusak", "ini bukan = toml [[ [");
        let err = muat_batas(&path, None).expect_err("harus ditolak");
        assert!(
            err.contains("mem-parse config"),
            "sebab harus menyebut parse, dapat: {err}"
        );

        let hidup = Hidup::baru(Batas::baru(
            reload::batas_tab([("kiri", Some(4))]),
            Some(Duration::from_secs(60)),
        ));
        let hasil = hidup.terapkan(muat_batas(&path, None));

        assert!(
            matches!(hasil, Hasil::Dipertahankan { .. }),
            "hasil harus mempertahankan, dapat {hasil:?}"
        );
        assert_eq!(hidup.max_tabs().get("kiri"), Some(&4));
        assert_eq!(hidup.idle_reap(), Some(Duration::from_secs(60)));
        let _ = std::fs::remove_file(&path);
    }

    // Flag baris perintah tetap menang setelah reload: SIGHUP membaca berkas,
    // dan berkas tidak boleh diam-diam membatalkan flag yang sudah diketik.
    #[test]
    fn flag_idle_reap_menang_atas_berkas() {
        let path = berkas(
            "flag",
            r#"
[daemon]
idle_reap_secs = 5
"#,
        );
        let batas = muat_batas(&path, Some(0)).expect("config sah");
        assert_eq!(batas.idle_reap, None, "flag 0 mematikan reap");
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod penahan_tests {
    use super::Penahan;
    use lifecycle::Gudang;

    // AC-009 pada lapisan angka: lease hidup dihitung, lease kedaluwarsa tidak,
    // dan handoff tetap dihitung terpisah supaya log bisa membedakan keduanya.
    #[test]
    fn lease_hidup_dan_kedaluwarsa_dihitung_berbeda() {
        let mut gudang = Gudang::baru(2);
        gudang.pegang("sesi-a", "T1", 60, 1_000).expect("pegang");
        gudang.pegang("sesi-b", "T2", 30, 1_000).expect("pegang");

        let penahan = Penahan::dari_gudang(1, &gudang, 1_010);
        assert_eq!(penahan.lease, 2);
        assert_eq!(penahan.handoff, 1);
        assert_eq!(penahan.total(), 3);
        assert!(penahan.ada());

        let penahan = Penahan::dari_gudang(0, &gudang, 1_031);
        assert_eq!(penahan.lease, 1, "T2 sudah kedaluwarsa pada detik 1031");
        assert_eq!(penahan.total(), 1);
    }

    #[test]
    fn tanpa_handoff_dan_tanpa_lease_berarti_mesin_bebas() {
        let gudang = Gudang::baru(1);
        let penahan = Penahan::dari_gudang(0, &gudang, 5_000);
        assert_eq!(penahan, Penahan::kosong());
        assert!(!penahan.ada());
    }
}
