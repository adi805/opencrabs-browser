//! CLI yang bisa dibaca orang lain tanpa membuka dokumentasi.
//!
//! Semua nilai yang memengaruhi proses terlihat di sini atau di berkas config,
//! tidak ada default tersembunyi di dalam kode.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "browserd",
    version,
    about = "Daemon browser untuk OpenCrabs: discovery, pass-through CDP, lifecycle"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Menjalankan daemon.
    Serve(ServeArgs),
    /// Mengukur mesin apa adanya lalu keluar: PSS sebelum dan sesudah reap.
    Selftest(SelftestArgs),
    /// Mencetak versi dan keluar.
    Version,
}

#[derive(Debug, Args, Clone)]
pub struct SelftestArgs {
    /// Jenis mesin. v0.1 hanya `chrome-shell`.
    #[arg(long, default_value = "chrome-shell")]
    pub engine: String,

    /// Biner mesin. Kalau kosong: `OCB_ENGINE_BINARY`, lalu deteksi otomatis.
    #[arg(long, env = "OCB_ENGINE_BINARY")]
    pub engine_binary: Option<PathBuf>,

    /// Berapa tab dibuka bersamaan.
    #[arg(long, default_value_t = 3)]
    pub tabs: u32,

    /// URL yang dimuat tiap tab.
    #[arg(long, default_value = "about:blank")]
    pub url: String,

    /// Jalankan asersi memori; tanpa ini selftest hanya melaporkan angka.
    #[arg(long)]
    pub memory: bool,

    /// Ambang PSS puncak, MB desimal (RFC §A4: 450).
    #[arg(long, default_value_t = 450)]
    pub expect_peak_mb: u64,

    /// Ambang PSS setelah reap, MB desimal (RFC §A3: 120).
    #[arg(long, default_value_t = 120)]
    pub expect_pss_after_reap_mb: u64,

    /// Berkas tujuan JSON hasil pengukuran.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Jangan hapus direktori profil setelah selesai; berguna saat gagal boot.
    #[arg(long)]
    pub keep_profile: bool,

    /// Batas waktu menunggu mesin siap.
    #[arg(long, default_value_t = 20_000)]
    pub ready_timeout_ms: u64,
}

#[derive(Debug, Args, Clone)]
pub struct ServeArgs {
    /// Port data plane untuk klien CDP. Sengaja 9224 saat pengembangan supaya
    /// browser produksi di 9222 tidak tersentuh.
    #[arg(long, default_value_t = 9222)]
    pub port: u16,

    /// Alamat bind lengkap, menimpa `--port` dan config. Mis. `127.0.0.1:9224`.
    #[arg(long)]
    pub bind: Option<String>,

    /// Jenis mesin. v0.1 hanya mendukung `shell`.
    #[arg(long, default_value = "shell")]
    pub engine: String,

    /// Biner mesin. Kalau kosong: config, lalu deteksi otomatis.
    #[arg(long, env = "OCB_ENGINE_BINARY")]
    pub engine_binary: Option<PathBuf>,

    /// Port debug mesin. 0 berarti mesin memilih sendiri (dibaca dari
    /// `DevToolsActivePort`), dan itu yang diinginkan: tidak ada tabrakan port.
    #[arg(long, default_value_t = 0)]
    pub engine_port: u16,

    /// Direktori profil mesin. Kalau kosong: `~/.cache/opencrabs-browser/shell-profile`.
    #[arg(long, env = "OCB_USER_DATA_DIR")]
    pub user_data_dir: Option<PathBuf>,

    /// Batas waktu menunggu mesin siap menjawab.
    #[arg(long, default_value_t = 15_000)]
    pub ready_timeout_ms: u64,

    /// Argumen tambahan untuk mesin, boleh diulang.
    #[arg(long = "arg")]
    pub extra_arg: Vec<String>,

    /// Berkas konfigurasi TOML. Kalau kosong: `~/.opencrabs/browser-daemon.toml`.
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Lepas dari terminal dan menulis log ke berkas.
    #[arg(long)]
    pub detach: bool,
}
