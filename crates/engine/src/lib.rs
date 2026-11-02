//! Menyalakan dan mematikan satu mesin browser.
//!
//! Crate ini **nol pengetahuan tentang CDP**. Dia tidak tahu apa itu `Target`,
//! `sessionId`, atau `Runtime.evaluate`. Yang dia tahu cuma: ada biner, ada port
//! debug, ada proses yang harus dimatikan seluruh group-nya.
//!
//! Pemisahan ini yang membuat adapter lain (full chrome, oxi, engine palsu)
//! bisa masuk tanpa menyentuh protokol, dan protokol bisa diuji tanpa browser.
//!
//! Semua API di sini sinkron. Spawn proses dan baca berkas port tidak butuh async,
//! dan trait yang sinkron tetap bisa dipakai sebagai `dyn Engine`. Pemanggil yang
//! async cukup membungkusnya dengan `spawn_blocking`.

#![forbid(unsafe_code)]

pub mod probe;
pub mod shell;

pub use probe::discover_shell;
pub use shell::ShellEngine;

use std::path::PathBuf;

/// Resep menyalakan satu mesin. Tidak ada nilai bawaan yang tersembunyi:
/// semua yang memengaruhi proses harus terlihat di sini.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub binary: PathBuf,
    pub user_data_dir: PathBuf,
    /// 0 berarti mesin memilih port sendiri, dibaca dari `DevToolsActivePort`.
    pub port: u16,
    pub extra_args: Vec<String>,
}

impl LaunchSpec {
    pub fn new(binary: impl Into<PathBuf>, user_data_dir: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            user_data_dir: user_data_dir.into(),
            port: 0,
            extra_args: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    #[must_use]
    pub fn with_args(mut self, args: impl IntoIterator<Item = String>) -> Self {
        self.extra_args.extend(args);
        self
    }
}

/// Fakta yang dilaporkan mesin tentang dirinya sendiri, apa adanya dari
/// endpoint `/json/version` miliknya. Tidak ada yang dikarang di sini: kalau
/// mesin tidak melaporkan versi V8, nilainya `None` dan itu jujur.
#[derive(Debug, Clone)]
pub struct EngineInfo {
    pub browser: String,
    pub protocol_version: String,
    pub user_agent: String,
    pub v8_version: Option<String>,
    pub webkit_version: Option<String>,
    /// Port debug yang benar-benar dipakai mesin, bukan yang diminta.
    pub devtools_port: u16,
    /// URL websocket milik mesin. Ini **internal**: klien harus memakai URL
    /// milik daemon, bukan ini, supaya semua trafik lewat satu titik.
    pub web_socket_debugger_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("biner engine tidak ditemukan: {0}")]
    BinaryMissing(PathBuf),

    #[error("mesin tidak melaporkan port debug dalam {0} ms")]
    PortFileTimeout(u64),

    #[error("isi DevToolsActivePort tidak bisa dibaca: {0}")]
    PortFileInvalid(String),

    #[error("mesin tidak siap dalam {0} ms")]
    ReadyTimeout(u64),

    #[error("respons discovery tidak valid: {0}")]
    Discovery(String),

    #[error("state engine rusak (mutex poisoned)")]
    Poisoned,

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Kontrak satu mesin yang bisa dinyalakan, ditanya, dan dimatikan.
///
/// Semua method memakai `&self` supaya bisa disimpan sebagai `Arc<dyn Engine>`.
/// Proses anak dilindungi mutex di dalam implementasi, bukan di pemanggil.
pub trait Engine: Send + Sync {
    /// Nama pendek untuk log dan metrik, mis. `"shell"`.
    fn kind(&self) -> &'static str;

    fn info(&self) -> &EngineInfo;

    fn spec(&self) -> &LaunchSpec;

    /// Basis HTTP internal mesin, mis. `http://127.0.0.1:41234`.
    fn http_base(&self) -> String;

    /// URL websocket internal mesin, untuk daemon meneruskan pesan.
    fn web_socket_url(&self) -> &str;

    fn process_id(&self) -> Option<u32>;

    fn is_running(&self) -> bool;

    /// Mematikan seluruh process group: SIGTERM, tunggu 5 detik, lalu SIGKILL.
    /// Aman dipanggil berkali-kali.
    fn stop(&self) -> Result<(), EngineError>;
}
