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
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use axum::Router;
use cdp::{EnginePool, ProxyState};
use engine::{Engine, ShellEngine, discover_shell};
use lifecycle::{Counters, Event, Machine};
use tokio::net::TcpListener;

use crate::cli::ServeArgs;
use crate::config::{self, FileConfig};
use crate::metrics;
use crate::{wiring, wiring::Wired};

const DETACHED_ENV: &str = "OCB_BROWSERD_DETACHED";

pub async fn run(args: ServeArgs, file: FileConfig, config_path: PathBuf) -> Result<()> {
    let resolved = Resolved::resolve(&args, &file)?;

    if args.engine != "shell" {
        bail!(
            "engine '{}' belum didukung v0.1; hanya 'shell'",
            args.engine
        );
    }

    // Store dibuka SEBELUM mesin dinyalakan: kunci yang hilang atau rusak harus
    // terlihat sebagai daemon yang tidak mau start, bukan sebagai daemon sehat
    // yang diam-diam kehilangan semua cookie saat restart berikutnya.
    // Disimpan hidup selama daemon berjalan: perintah cookie (export/import)
    // dan sesi berikutnya memakai koneksi yang sama.
    let _store = buka_store(&file)?;

    let engines = launch_all(&resolved.machines, args.ready_timeout_ms).await?;
    let pool = EnginePool::new(resolved.default_id.clone(), engines)
        .map_err(|err| anyhow::anyhow!("konfigurasi mesin tidak sah: {err}"))?;

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
        pool.default_engine().web_socket_url().to_owned(),
        limits_handoff(&file),
        control::RegistryLimits::default(),
    );

    let bind = resolved.bind.clone();
    let public_ws_url = format!("ws://{bind}/devtools/browser/{}", session_token());
    let state = Arc::new(
        ProxyState::pool(pool.clone(), public_ws_url).with_observer(wired.observer.clone()),
    );
    let app: Router = cdp::router(state).merge(metrics::router(
        pool.clone(),
        Arc::clone(&counters),
        Arc::clone(&wired.counters),
        Arc::clone(&machine),
    ));

    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind} gagal; port dipakai proses lain?"))?;

    for (id, engine) in pool.engines() {
        let pss_mb = engine
            .process_id()
            .and_then(metrics::read_pss_bytes)
            .map_or_else(
                || "?".to_string(),
                |bytes| format!("{:.1}", bytes as f64 / 1_048_576.0),
            );
        tracing::info!(
            mesin = %id,
            kind = engine.kind(),
            pid = ?engine.process_id(),
            http = %engine.http_base(),
            pss_mb = %pss_mb,
            "mesin siap"
        );
    }

    for mesin in &resolved.machines {
        if mesin.max_tabs.is_some() {
            // Jangan diam: config yang menyebut batas tab tapi tidak ditegakkan
            // akan dibaca orang sebagai "sudah dibatasi".
            tracing::warn!(
                mesin = %mesin.id,
                max_tabs = ?mesin.max_tabs,
                "max_tabs terbaca tetapi belum ditegakkan di v0.1 (milik lifecycle, M1)"
            );
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
    hentikan_semua(
        pool.engines()
            .map(|(_, engine)| Arc::clone(engine))
            .collect(),
    )
    .await;
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
            let engine_ws = wired.engine_ws.clone();
            move |target_id: String| {
                let engine_ws = engine_ws.clone();
                Box::pin(async move {
                    crate::wiring::probe_target(&engine_ws, &target_id)
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
async fn nyalakan(mesin: &Mesin, timeout_ms: u64) -> Result<Arc<dyn Engine>> {
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

    // Spawn proses dan polling port adalah kerja blocking. Menjalankannya
    // langsung di runtime akan menahan worker tokio selama mesin boot.
    tokio::task::spawn_blocking(move || ShellEngine::launch(spec, timeout_ms))
        .await
        .context("task peluncuran mesin panik")?
        .map(|engine| Arc::new(engine) as Arc<dyn Engine>)
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
) -> Result<BTreeMap<String, Arc<dyn Engine>>> {
    let mut engines: BTreeMap<String, Arc<dyn Engine>> = BTreeMap::new();
    for mesin in machines {
        match nyalakan(mesin, timeout_ms).await {
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

/// Jalur lama: satu mesin dari argumen CLI dan `[engines.chrome_shell]`.
fn dari_cli(args: &ServeArgs, file: &FileConfig) -> Result<Mesin> {
    let binary = args
        .engine_binary
        .clone()
        .or_else(|| file.shell_binary())
        .or_else(discover_shell)
        .context(
            "biner engine tidak ditemukan. Set --engine-binary, atau OCB_ENGINE_BINARY, \
             atau engines.chrome_shell.binary di config",
        )?;
    wajib_absolut(&binary, "biner engine")?;

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

    let mut engine_args = file.shell_args();
    engine_args.extend(args.extra_arg.iter().cloned());

    Ok(Mesin {
        id: args.engine.clone(),
        kind: args.engine.clone(),
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
        if kind != "shell" {
            bail!("mesin '{id}': jenis '{kind}' belum didukung v0.1; hanya 'shell'");
        }

        let binary = section
            .binary
            .as_ref()
            .map(|raw| config::expand_home(raw))
            .or_else(|| args.engine_binary.clone())
            .or_else(discover_shell)
            .with_context(|| {
                format!("mesin '{id}': biner engine tidak ditemukan; set `binary` untuk mesin ini")
            })?;
        wajib_absolut(&binary, &format!("biner mesin '{id}'"))?;

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
            detach: false,
        }
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
[engines.chrome_full]
kind = "chrome-full"
binary = "/abs/chrome"
"#;
        let config: FileConfig = toml::from_str(text).expect("toml sah");
        let mut args = args();
        args.user_data_dir = None;
        let err = Resolved::resolve(&args, &config).expect_err("harus ditolak");
        let pesan = err.to_string();
        assert!(
            pesan.contains("chrome_full") && pesan.contains("chrome-full"),
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
}
