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
    /// Membuat kunci enkripsi store (sekali saja) lalu keluar.
    Keygen(KeygenArgs),
    /// Cookie: daftar, ekspor, impor, dan bukti selamat restart.
    Cookies(CookiesArgs),
    /// Mencetak versi dan keluar.
    Version,
}

/// Field yang sama untuk semua perintah cookie.
#[derive(Debug, Args, Clone)]
pub struct CookieArgs {
    /// Profil pemilik cookie. Satu profil = satu identitas beku.
    #[arg(long, default_value = "scratch")]
    pub profile: String,

    /// Berkas database store.
    #[arg(long, default_value_os_t = crate::storecmd::default_db())]
    pub db: PathBuf,

    /// Berkas kunci store. Mode wajib 0600; kunci tidak lengkap berarti store
    /// tidak bisa dibuka, dan itu disengaja.
    #[arg(long, default_value_os_t = crate::storecmd::default_key())]
    pub key_file: PathBuf,

    /// Websocket CDP mesin, mis. `ws://127.0.0.1:41234/devtools/browser/<id>`.
    /// Wajib untuk ekspor dan impor; tidak dipakai `ls`.
    #[arg(long)]
    pub cdp: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub struct KeygenArgs {
    /// Berkas kunci yang dibuat. Berkas yang sudah ada tidak pernah ditimpa.
    #[arg(long, default_value_os_t = crate::storecmd::default_key())]
    pub key_file: PathBuf,

    /// Berkas database store yang ikut disiapkan.
    #[arg(long, default_value_os_t = crate::storecmd::default_db())]
    pub db: PathBuf,
}

#[derive(Debug, Args, Clone)]
pub struct CookiesArgs {
    #[command(subcommand)]
    pub action: CookieAction,
}

#[derive(Debug, Subcommand, Clone)]
pub enum CookieAction {
    /// Daftar cookie tersimpan. Nilai tidak pernah dicetak.
    Ls(CookieLsArgs),
    /// Ambil cookie dari mesin yang sedang jalan, simpan ke store.
    Export(CookieSyncArgs),
    /// Tanam cookie dari store ke mesin yang sedang jalan.
    Import(CookieSyncArgs),
    /// Bukti M2: cookie dummy harus kembali setelah mesin dimatikan dan
    /// dinyalakan ulang dengan direktori profil baru.
    Proof(ProofArgs),
}

#[derive(Debug, Args, Clone)]
pub struct CookieLsArgs {
    #[command(flatten)]
    pub umum: CookieArgs,

    /// Saring satu host.
    #[arg(long)]
    pub host: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub struct CookieSyncArgs {
    #[command(flatten)]
    pub umum: CookieArgs,
}

#[derive(Debug, Args, Clone)]
pub struct ProofArgs {
    #[command(flatten)]
    pub umum: CookieArgs,

    /// Biner mesin. Kalau kosong: `OCB_ENGINE_BINARY`, lalu deteksi otomatis.
    #[arg(long, env = "OCB_ENGINE_BINARY")]
    pub engine_binary: Option<PathBuf>,

    /// Jangan hapus direktori profil sementara setelah selesai.
    #[arg(long)]
    pub keep_profile: bool,
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
