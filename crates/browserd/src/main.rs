//! Titik masuk `browserd`.

use browserd::{cli, config, daemon, selftest, storecmd};

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let parsed = cli::Cli::parse();

    match parsed.command {
        cli::Command::Version => {
            println!("browserd {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        cli::Command::Selftest(args) => {
            init_tracing();
            match selftest::run(args) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("browserd selftest: {err:#}");
                    ExitCode::FAILURE
                }
            }
        }
        cli::Command::Keygen(args) => match storecmd::keygen(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("browserd keygen: {err:#}");
                ExitCode::FAILURE
            }
        },
        cli::Command::Cookies(args) => match run_cookies(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("browserd cookies: {err:#}");
                ExitCode::FAILURE
            }
        },
        cli::Command::Serve(args) => {
            init_tracing();
            match run(args) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    // `{err:#}` mencetak seluruh rantai penyebab. Pesan "gagal
                    // start" tanpa sebab adalah cara termahal untuk debug.
                    eprintln!("browserd: {err:#}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn run_cookies(args: cli::CookiesArgs) -> anyhow::Result<()> {
    match args.action {
        cli::CookieAction::Ls(a) => storecmd::ls(&a.umum, a.host.as_deref()),
        cli::CookieAction::Export(a) => storecmd::export(&a.umum),
        cli::CookieAction::Import(a) => storecmd::import(&a.umum),
        cli::CookieAction::Proof(a) => {
            init_tracing();
            storecmd::proof(&a)
        }
    }
}

fn run(args: cli::ServeArgs) -> anyhow::Result<()> {
    // Config dibaca sekali, sebelum fork: jalur config yang salah harus
    // terlihat sebagai error di terminal, bukan sebagai proses latar yang
    // mati diam-diam.
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(config::default_config_path);
    let file = config::FileConfig::load(&config_path)?;

    if daemon::maybe_detach(&args, file.log_dir())?.is_some() {
        return Ok(());
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(daemon::run(args, file, config_path))
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_env("OCB_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,cdp=debug,engine=debug"));

    // Log ke stderr: stdout dipakai untuk pesan yang dibaca operator
    // (`cdp_endpoint = ...`), supaya `--detach` dan pemakaian interaktif
    // punya perilaku yang sama.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}
