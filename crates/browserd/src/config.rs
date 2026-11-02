//! Konfigurasi TOML.
//!
//! Berkas yang sama dengan rancangan di dokumen 04 (`~/.opencrabs/browser-daemon.toml`).
//! Hanya bagian yang dipakai v0.1 yang di-parse; bagian lain diabaikan dengan
//! sengaja supaya berkas config yang sudah ditulis untuk milestone berikutnya
//! tidak membuat daemon hari ini gagal start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct FileConfig {
    #[serde(default)]
    pub daemon: Option<DaemonSection>,
    #[serde(default)]
    pub engines: Option<EnginesSection>,
    /// Bagian yang sudah dibaca sejak M3: pemilihan mesin dan batas handoff.
    #[serde(default)]
    pub policy: Option<policy::PolicySection>,
    #[serde(default)]
    pub captcha: Option<policy::CaptchaSection>,
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
    /// Alamat port kontrol (handoff). Bawaan `127.0.0.1:9223`.
    ///
    /// Dipisah dari port CDP karena dua alasan: klien CDP tidak boleh bisa
    /// menjawab handoff tanpa token, dan port CDP harus tetap menerima
    /// websocket apa adanya sementara port ini hanya bicara JSON.
    pub control_bind: Option<String>,
    /// Berkas token kontrol. Bawaan
    /// `~/.opencrabs/browser-daemon/control.token`.
    pub control_token: Option<String>,
    /// Id mesin bawaan kalau lebih dari satu mesin dikonfigurasi. Kosong
    /// berarti `chrome_shell`, lalu id terkecil. Lihat
    /// [`FileConfig::engine_default`].
    pub engine_default: Option<String>,
    /// Berapa detik mesin boleh menganggur sebelum dimatikan. `0` berarti
    /// tidak pernah. Bawaan 60 detik, sama dengan `ReaperConfig::default()`
    /// di crate `lifecycle`: dua angka berbeda di dua tempat akan membuat
    /// perilaku daemon bergantung pada mana yang dibaca lebih dulu.
    ///
    /// Mesin yang dimatikan di sini **bukan** mesin yang hilang: resepnya
    /// (`LaunchSpec`) tetap dipegang `Slot`, jadi permintaan berikutnya
    /// menyalakannya lagi tanpa me-restart daemon.
    pub idle_reap_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct EnginesSection {
    /// Peta id mesin -> pengaturan mesin.
    ///
    /// Bentuk ini dipilih supaya `[engines.chrome_shell]` yang sudah ada tetap
    /// terbaca tanpa perubahan, dan mesin kedua cukup ditambah sebagai
    /// `[engines.chrome_shell2]`. Id-nya **dipakai apa adanya** di `/json/list`
    /// (`<id>~<id-target>`) dan di label metrik, jadi id itu bagian dari
    /// kontrak, bukan sekadar nama.
    #[serde(flatten)]
    pub daftar: BTreeMap<String, EngineSection>,
}

#[derive(Debug, Deserialize)]
pub struct EngineSection {
    /// Jenis mesin. v0.1 hanya `shell`. Nilai lain **ditolak saat start**
    /// dengan pesan yang menyebut mesin mana: mengabaikannya diam-diam berarti
    /// operator mengira dua mesin jalan padahal satu.
    pub kind: Option<String>,
    pub binary: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Port debug mesin. Kosong atau 0 berarti mesin memilih sendiri, dan itu
    /// yang diinginkan: dua mesin dengan port tetap akan bertabrakan.
    pub port: Option<u16>,
    /// Direktori profil mesin. Kosong berarti
    /// `~/.cache/opencrabs-browser/<id>-profile`.
    ///
    /// Dua mesin **tidak boleh** berbagi direktori ini: Chrome mengunci
    /// profil, dan mesin kedua gagal start dengan pesan yang tidak menyebut
    /// sebabnya.
    pub user_data_dir: Option<String>,
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
            .daftar
            .get("chrome_shell")?
            .binary
            .as_ref()?;
        Some(expand_home(raw))
    }

    /// Semua mesin yang dikonfigurasi, urut id.
    ///
    /// Urutan `BTreeMap` dipakai apa adanya supaya mesin dinyalakan, dilaporkan
    /// di metrik, dan digabung di `/json/list` dalam urutan yang sama setiap
    /// kali. Urutan yang berubah-ubah membuat dua tangkapan metrik tidak bisa
    /// dibandingkan tanpa berpikir.
    pub fn machines(&self) -> impl Iterator<Item = (&String, &EngineSection)> {
        self.engines
            .iter()
            .flat_map(|section| section.daftar.iter())
    }

    /// Mesin yang jadi bawaan saat klien tidak menyebut mesin.
    ///
    /// Urutan keputusan, semuanya eksplisit: `[daemon] engine_default`, lalu
    /// `chrome_shell` kalau ada (nama itu yang dipakai config lama), lalu id
    /// terkecil menurut urutan abjad. Tidak ada yang mengarang mesin: kalau
    /// `engine_default` menyebut mesin yang tidak ada, daemon menolak start.
    #[must_use]
    pub fn engine_default(&self) -> Option<&str> {
        if let Some(nama) = self
            .daemon
            .as_ref()
            .and_then(|daemon| daemon.engine_default.as_ref())
        {
            return Some(nama.as_str());
        }
        if self.machines().any(|(id, _)| id == "chrome_shell") {
            return Some("chrome_shell");
        }
        self.machines().map(|(id, _)| id.as_str()).min()
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
        self.engines.as_ref()?.daftar.get("chrome_shell")?.max_tabs
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

    /// Alamat port kontrol. Bawaannya loopback: handoff berisi URL dan path
    /// screenshot milik sesi owner, dan tidak ada alasan port itu terbuka ke
    /// jaringan.
    #[must_use]
    pub fn control_bind(&self) -> String {
        self.daemon
            .as_ref()
            .and_then(|daemon| daemon.control_bind.clone())
            .unwrap_or_else(|| "127.0.0.1:9223".to_owned())
    }

    #[must_use]
    pub fn control_token_path(&self) -> PathBuf {
        self.daemon
            .as_ref()
            .and_then(|daemon| daemon.control_token.as_ref())
            .map_or_else(
                || home().join(".opencrabs/browser-daemon/control.token"),
                |raw| expand_home(raw),
            )
    }

    /// Berapa detik mesin boleh menganggur sebelum di-reap. `None` berarti
    /// tidak pernah: `0` di config adalah cara operator mematikan fitur ini,
    /// dan itu harus terbaca sebagai "mati", bukan "batas nol detik".
    #[must_use]
    pub fn idle_reap_secs(&self) -> Option<u64> {
        self.daemon
            .as_ref()
            .and_then(|daemon| daemon.idle_reap_secs)
    }

    /// Batas handoff dari `[captcha]`, atau bawaan dokumen 03 §3.1.
    #[must_use]
    pub fn captcha(&self) -> policy::CaptchaSection {
        self.captcha.clone().unwrap_or_default()
    }

    /// Aturan pemilihan mesin dari `[policy]`.
    #[must_use]
    pub fn policy(&self) -> policy::PolicySection {
        self.policy.clone().unwrap_or_default()
    }

    #[must_use]
    pub fn shell_args(&self) -> Vec<String> {
        self.engines
            .as_ref()
            .and_then(|engines| engines.daftar.get("chrome_shell"))
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

/// Direktori profil untuk mesin ber-id `id`.
///
/// Satu direktori per mesin, bukan satu untuk semua: Chrome mengunci profil
/// yang sedang dipakai, jadi dua mesin yang berbagi direktori akan membuat
/// mesin kedua gagal start dan pesannya tidak menyebut sebabnya.
#[must_use]
pub fn machine_user_data_dir(id: &str) -> PathBuf {
    home().join(format!(".cache/opencrabs-browser/{id}-profile"))
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
    fn bagian_kontrol_dan_captcha_terbaca() {
        let text = r#"
[daemon]
control_bind = "127.0.0.1:9333"
control_token = "/tmp/uji/token"

[captcha]
handoff_per_host_hour = 2
handoff_total_day = 7
cooldown_secs = 30

[policy]
default = "shell"

[policy.rules]
"shop.example" = "chrome-full"
"#;
        let config: FileConfig = toml::from_str(text).expect("harus bisa diparse");
        assert_eq!(config.control_bind(), "127.0.0.1:9333");
        assert_eq!(
            config.control_token_path(),
            PathBuf::from("/tmp/uji/token"),
            "path absolut harus dipakai apa adanya"
        );
        let captcha = config.captcha();
        assert_eq!(captcha.handoff_per_host_hour, 2);
        assert_eq!(captcha.handoff_total_day, 7);
        assert_eq!(captcha.cooldown_secs, 30);
        assert_eq!(
            config
                .policy()
                .rules
                .get("shop.example")
                .map(String::as_str),
            Some("chrome-full")
        );
    }

    #[test]
    fn bawaan_kontrol_loopback() {
        let config: FileConfig = toml::from_str("").expect("harus bisa diparse");
        assert_eq!(config.control_bind(), "127.0.0.1:9223");
        assert!(
            config.control_token_path().is_absolute(),
            "token harus di path absolut, dapat: {}",
            config.control_token_path().display()
        );
        assert_eq!(config.captcha().handoff_per_host_hour, 5);
    }

    #[test]
    fn config_kosong_tidak_panic() {
        let config: FileConfig = toml::from_str("").expect("harus bisa diparse");
        assert!(config.shell_binary().is_none());
        assert!(config.shell_args().is_empty());
    }

    #[test]
    fn dua_mesin_terbaca_sebagai_peta_urut_id() {
        // Bentuk M4: setiap bagian di bawah [engines] satu mesin, dan id-nya
        // bagian itu sendiri. Id dipakai di `/json/list` dan label metrik, jadi
        // urutannya harus stabil.
        let text = r#"
[engines.chrome_shell2]
kind = "shell"
binary = "/opt/b/shell2"
port = 43002
user_data_dir = "/var/tmp/dua"

[engines.chrome_shell]
binary = "/opt/b/shell"
args = ["--mute-audio"]
max_tabs = 4
"#;
        let config: FileConfig = toml::from_str(text).expect("harus bisa diparse");
        let ids: Vec<&str> = config.machines().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["chrome_shell", "chrome_shell2"]);

        let kedua = config
            .machines()
            .find(|(id, _)| *id == "chrome_shell2")
            .map(|(_, section)| section)
            .expect("mesin kedua ada");
        assert_eq!(kedua.kind.as_deref(), Some("shell"));
        assert_eq!(kedua.port, Some(43002));
        assert_eq!(kedua.user_data_dir.as_deref(), Some("/var/tmp/dua"));

        // Config lama tetap terbaca lewat aksesornya sendiri.
        assert_eq!(config.shell_args(), vec!["--mute-audio"]);
        assert_eq!(config.max_tabs(), Some(4));
        assert!(
            config
                .shell_binary()
                .expect("binary terbaca")
                .ends_with("shell")
        );
    }

    #[test]
    fn mesin_bawaan_bisa_ditunjuk_eksplisit() {
        let text = r#"
[daemon]
engine_default = "chrome_shell2"

[engines.chrome_shell]
binary = "/opt/b/shell"

[engines.chrome_shell2]
binary = "/opt/b/shell2"
"#;
        let config: FileConfig = toml::from_str(text).expect("harus bisa diparse");
        assert_eq!(config.engine_default(), Some("chrome_shell2"));
    }

    #[test]
    fn tanpa_penunjuk_bawaan_chrome_shell_yang_dipilih() {
        let text = r#"
[engines.aaa]
binary = "/opt/b/a"

[engines.chrome_shell]
binary = "/opt/b/shell"
"#;
        let config: FileConfig = toml::from_str(text).expect("harus bisa diparse");
        // `chrome_shell` menang atas urutan abjad: nama itu yang dipakai config
        // lama, jadi mesin yang selama ini jadi bawaan tidak berpindah diam-diam.
        assert_eq!(config.engine_default(), Some("chrome_shell"));
    }

    #[test]
    fn direktori_profil_bawaan_berbeda_per_mesin() {
        // Dua mesin dengan direktori sama tidak akan bisa menyala bersamaan.
        let satu = machine_user_data_dir("chrome_shell");
        let dua = machine_user_data_dir("chrome_shell2");
        assert_ne!(satu, dua);
        assert!(satu.is_absolute(), "{}", satu.display());
    }
}
