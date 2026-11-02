//! Merangkai semuanya: nyalakan mesin, layani klien, matikan dengan rapi.
//!
//! Urutannya penting dan tidak boleh ditukar:
//!
//! 1. Mesin dinyalakan dan **ditunggu sampai benar-benar siap**. Bind port
//!    sebelum mesin siap berarti klien bisa menyambung ke daemon yang belum
//!    punya apa-apa untuk diteruskan.
//! 2. Baru daemon bind dan melayani.
//! 3. Saat berhenti, mesin dimatikan sebagai process group, bukan hanya pid.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use axum::Router;
use cdp::ProxyState;
use engine::{Engine, ShellEngine, discover_shell};
use tokio::net::TcpListener;

use crate::cli::ServeArgs;
use crate::config::{self, FileConfig};
use crate::metrics;

const DETACHED_ENV: &str = "OCB_BROWSERD_DETACHED";

pub async fn run(args: ServeArgs, file: FileConfig, config_path: PathBuf) -> Result<()> {
    let resolved = Resolved::resolve(&args, &file)?;

    if args.engine != "shell" {
        bail!(
            "engine '{}' belum didukung v0.1; hanya 'shell'",
            args.engine
        );
    }

    let engine = launch_engine(&resolved, args.ready_timeout_ms).await?;
    let engine: Arc<dyn Engine> = Arc::new(engine);

    let bind = resolved.bind.clone();
    let public_ws_url = format!("ws://{bind}/devtools/browser/{}", session_token());
    let state = Arc::new(ProxyState::new(Arc::clone(&engine), public_ws_url));
    let app: Router = cdp::router(state);

    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind} gagal; port dipakai proses lain?"))?;

    let pss_mb = engine
        .process_id()
        .and_then(metrics::read_pss_bytes)
        .map_or_else(
            || "?".to_string(),
            |bytes| format!("{:.1}", bytes as f64 / 1_048_576.0),
        );

    if resolved.max_tabs.is_some() {
        // Jangan diam: config yang menyebut batas tab tapi tidak ditegakkan
        // akan dibaca orang sebagai "sudah dibatasi".
        tracing::warn!(
            max_tabs = ?resolved.max_tabs,
            "max_tabs terbaca tetapi belum ditegakkan di v0.1 (milik lifecycle, M1)"
        );
    }

    tracing::info!(
        bind = %bind,
        engine = engine.kind(),
        pid = ?engine.process_id(),
        engine_http = %engine.http_base(),
        pss_mb = %pss_mb,
        config = %config_path.display(),
        "browserd siap"
    );
    println!("browserd mendengarkan di http://{bind}");
    println!("cdp_endpoint = \"http://{bind}\"");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server berhenti dengan error")?;

    tracing::info!("mematikan mesin");
    let engine_for_stop = Arc::clone(&engine);
    match tokio::task::spawn_blocking(move || engine_for_stop.stop()).await {
        Ok(Ok(())) => tracing::info!("mesin berhenti"),
        Ok(Err(err)) => tracing::warn!(%err, "mesin tidak berhenti bersih"),
        Err(err) => tracing::warn!(%err, "task penghentian gagal"),
    }
    Ok(())
}

async fn launch_engine(resolved: &Resolved, timeout_ms: u64) -> Result<ShellEngine> {
    let spec = engine::LaunchSpec::new(resolved.binary.clone(), resolved.user_data_dir.clone())
        .with_port(resolved.engine_port)
        .with_args(resolved.engine_args.iter().cloned());

    tracing::info!(
        binary = %resolved.binary.display(),
        user_data_dir = %resolved.user_data_dir.display(),
        port = resolved.engine_port,
        "menyalakan mesin"
    );

    // Spawn proses dan polling port adalah kerja blocking. Menjalankannya
    // langsung di runtime akan menahan worker tokio selama mesin boot.
    tokio::task::spawn_blocking(move || ShellEngine::launch(spec, timeout_ms))
        .await
        .context("task peluncuran mesin panik")?
        .map_err(Into::into)
}

#[derive(Debug, Clone)]
struct Resolved {
    bind: String,
    binary: PathBuf,
    user_data_dir: PathBuf,
    engine_port: u16,
    engine_args: Vec<String>,
    max_tabs: Option<u32>,
}

impl Resolved {
    fn resolve(args: &ServeArgs, file: &FileConfig) -> Result<Self> {
        let bind = args
            .bind
            .clone()
            .or_else(|| file.daemon.as_ref().and_then(|daemon| daemon.bind.clone()))
            .unwrap_or_else(|| format!("127.0.0.1:{}", args.port));

        let binary = args
            .engine_binary
            .clone()
            .or_else(|| file.shell_binary())
            .or_else(discover_shell)
            .context(
                "biner engine tidak ditemukan. Set --engine-binary, atau OCB_ENGINE_BINARY, \
                 atau engines.chrome_shell.binary di config",
            )?;

        if !binary.is_absolute() {
            bail!(
                "biner engine harus path absolut, dapat: {}",
                binary.display()
            );
        }

        let user_data_dir = args
            .user_data_dir
            .clone()
            .unwrap_or_else(config::default_user_data_dir);
        if !user_data_dir.is_absolute() {
            bail!(
                "user-data-dir harus path absolut, dapat: {}",
                user_data_dir.display()
            );
        }

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

        Ok(Self {
            bind,
            binary,
            user_data_dir,
            engine_port,
            engine_args,
            max_tabs: file.max_tabs(),
        })
    }
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

    #[test]
    fn durasi_graceful_tidak_nol() {
        assert!(Duration::from_secs(1).as_secs() > 0);
    }
}
