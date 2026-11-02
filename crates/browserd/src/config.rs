//! Konfigurasi TOML.
//!
//! Berkas yang sama dengan rancangan di dokumen 04 (`~/.opencrabs/browser-daemon.toml`).
//! Hanya bagian yang dipakai v0.1 yang di-parse; bagian lain diabaikan dengan
//! sengaja supaya berkas config yang sudah ditulis untuk milestone berikutnya
//! tidak membuat daemon hari ini gagal start.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct FileConfig {
    #[serde(default)]
    pub daemon: Option<DaemonSection>,
    #[serde(default)]
    pub engines: Option<EnginesSection>,
}

#[derive(Debug, Deserialize)]
pub struct DaemonSection {
    pub bind: Option<String>,
    pub engine_port: Option<u16>,
    pub log_dir: Option<String>,
    /// Berkas database store. Bawaan: `~/.opencrabs/browser-daemon/store.db`.
    pub store_db: Option<String>,
    /// Berkas kunci store. Bawaan: `~/.opencrabs/browser-daemon/store.key`.
    /// Tanpa kunci ini daemon menolak start: cookie tidak pernah ditulis
    /// plaintext sebagai kompromi.
    pub store_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct EnginesSection {
    pub chrome_shell: Option<EngineSection>,
}

#[derive(Debug, Deserialize)]
pub struct EngineSection {
    pub binary: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Dibaca tapi belum ditegakkan: pembatasan tab milik `lifecycle` (M1).
    pub max_tabs: Option<u32>,
}

impl FileConfig {
    /// Berkas yang tidak ada berarti "pakai nilai bawaan", bukan error.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("membaca config {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("mem-parse config {}", path.display()))
    }

    #[must_use]
    pub fn shell_binary(&self) -> Option<PathBuf> {
        let raw = self
            .engines
            .as_ref()?
            .chrome_shell
            .as_ref()?
            .binary
            .as_ref()?;
        Some(expand_home(raw))
    }

    /// Direktori log saat `--detach`. `None` = pakai bawaan
    /// `~/.opencrabs/browser-daemon/logs`.
    #[must_use]
    pub fn log_dir(&self) -> Option<PathBuf> {
        let raw = self.daemon.as_ref()?.log_dir.as_ref()?;
        Some(expand_home(raw))
    }

    /// Batas tab dari config. Nilainya **belum ditegakkan** v0.1 karena
    /// penegakan milik crate `lifecycle` (M1); daemon melaporkannya saat start
    /// supaya tidak ada yang mengira batas ini sudah berlaku.
    #[must_use]
    pub fn max_tabs(&self) -> Option<u32> {
        self.engines.as_ref()?.chrome_shell.as_ref()?.max_tabs
    }

    #[must_use]
    pub fn store_db(&self) -> PathBuf {
        self.daemon
            .as_ref()
            .and_then(|daemon| daemon.store_db.as_ref())
            .map_or_else(super::storecmd::default_db, |raw| expand_home(raw))
    }

    #[must_use]
    pub fn store_key(&self) -> PathBuf {
        self.daemon
            .as_ref()
            .and_then(|daemon| daemon.store_key.as_ref())
            .map_or_else(super::storecmd::default_key, |raw| expand_home(raw))
    }

    #[must_use]
    pub fn shell_args(&self) -> Vec<String> {
        self.engines
            .as_ref()
            .and_then(|engines| engines.chrome_shell.as_ref())
            .map(|section| section.args.clone())
            .unwrap_or_default()
    }
}

#[must_use]
pub fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
}

/// `~/x` diperluas. Path absolut dibiarkan. Path relatif **ditolak** nanti saat
/// dipakai, karena config relatif selalu berarti hal berbeda tergantung siapa
/// yang menjalankan daemon.
#[must_use]
pub fn expand_home(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        return home().join(rest);
    }
    PathBuf::from(value)
}

#[must_use]
pub fn default_config_path() -> PathBuf {
    home().join(".opencrabs/browser-daemon.toml")
}

#[must_use]
pub fn default_user_data_dir() -> PathBuf {
    home().join(".cache/opencrabs-browser/shell-profile")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bagian_yang_belum_dipakai_tidak_merusak_parse() {
        // Berkas ini memuat [budget], [lifecycle], [policy], [captcha] yang
        // belum dipakai v0.1. Daemon harus tetap bisa membacanya.
        let text = r#"
[daemon]
bind = "127.0.0.1:9224"
engine_port = 0

[budget]
soft_mb = 600

[lifecycle]
idle_reap_secs = 60

[engines.chrome_shell]
binary = "~/bin/shell"
max_tabs = 6
args = ["--no-sandbox", "--mute-audio"]
"#;
        let config: FileConfig = toml::from_str(text).expect("harus bisa diparse");
        let daemon = config.daemon.as_ref().expect("ada bagian daemon");
        assert_eq!(daemon.bind.as_deref(), Some("127.0.0.1:9224"));
        assert_eq!(config.shell_args(), vec!["--no-sandbox", "--mute-audio"]);
        let binary = config.shell_binary().expect("binary terbaca");
        assert!(
            binary.is_absolute(),
            "~ harus diperluas, dapat: {}",
            binary.display()
        );
    }

    #[test]
    fn config_kosong_tidak_panic() {
        let config: FileConfig = toml::from_str("").expect("harus bisa diparse");
        assert!(config.shell_binary().is_none());
        assert!(config.shell_args().is_empty());
    }
}
