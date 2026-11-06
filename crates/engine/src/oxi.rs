//! Adapter untuk OxiBrowser (`oxibrowser serve`).
//!
//! OxiBrowser bicara CDP, tapi tidak sama dengan `chrome-headless-shell` di
//! tiga hal yang menyentuh adapter ini secara langsung:
//!
//! 1. **Port dipilih pemanggil.** Shell menulis `DevToolsActivePort` dan boleh
//!    diberi port 0; oxi hanya mencetak alamatnya ke stdout. Karena itu adapter
//!    ini yang memilih port bebas, lalu memberikannya lewat `--port`.
//! 2. **Nama field `/json/version` camelCase** (`browser`, `userAgent`,
//!    `webSocketDebuggerUrl`), bukan PascalCase bergaris seperti Chrome
//!    (`Browser`, `User-Agent`). Pembaca field di sini menerima keduanya, supaya
//!    satu adapter bisa dipakai untuk dua mesin tanpa `if kind == ...`.
//! 3. **Profil disimpan sebagai berkas cookie**, bukan direktori user data.
//!    `--cookie-file` diarahkan ke `user_data_dir/cookies.json` supaya arti
//!    `LaunchSpec` tetap sama untuk kedua mesin.
//!
//! Yang **tidak** disamakan: filter SSRF. Oxi menolak alamat privat secara
//! bawaan, dan adapter ini tidak mematikannya sendiri. Halaman uji lokal harus
//! meminta `--allow-private-ips` lewat `LaunchSpec::extra_args`, sehingga
//! keputusan itu terlihat di konfigurasi, bukan tersembunyi di kode.

use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::proses::{Proc, stderr_ke_file};
use crate::shell::{POLL_INTERVAL, http_get};
use crate::{Engine, EngineError, EngineInfo, LaunchSpec};

pub const DEFAULT_READY_TIMEOUT_MS: u64 = 30_000;

/// Berkas stderr oxi. Dipisah dari milik shell supaya dua mesin yang berbagi
/// satu direktori kerja tidak saling menimpa log.
pub const STDERR_LOG: &str = "oxi-stderr.log";

/// Nama berkas cookie di dalam `user_data_dir`.
const COOKIE_FILE: &str = "cookies.json";

pub struct OxiEngine {
    spec: LaunchSpec,
    info: EngineInfo,
    proc: Mutex<Proc>,
}

impl OxiEngine {
    /// Menyalakan oxi dan menunggu sampai `/json/version` menjawab.
    ///
    /// Kegagalan di tengah jalan mematikan seluruh group proses sebelum error
    /// dikembalikan: mesin yang gagal siap tidak boleh tertinggal hidup.
    pub fn launch(spec: LaunchSpec, timeout_ms: u64) -> Result<Self, EngineError> {
        if !spec.binary.is_file() {
            return Err(EngineError::BinaryMissing(spec.binary.clone()));
        }
        std::fs::create_dir_all(&spec.user_data_dir)?;

        let port = if spec.port == 0 {
            port_bebas()?
        } else {
            spec.port
        };

        let mut cmd = Command::new(&spec.binary);
        cmd.args(argumen(&spec, port));
        stderr_ke_file(&mut cmd, &spec.user_data_dir.join(STDERR_LOG));
        let mut proc = Proc::spawn(cmd)?;

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let siap = (|| -> Result<EngineInfo, EngineError> {
            let body = wait_for_version(port, deadline, timeout_ms)?;
            info_dari_json(&body, port)
        })();

        match siap {
            Ok(info) => Ok(Self {
                spec,
                info,
                proc: Mutex::new(proc),
            }),
            Err(err) => {
                proc.kill_group();
                Err(err)
            }
        }
    }
}

/// Argumen `serve`. Dipisah dari `launch` supaya resep yang dikirim ke biner
/// bisa diperiksa test tanpa menyalakan proses.
fn argumen(spec: &LaunchSpec, port: u16) -> Vec<String> {
    let mut args = vec![
        "serve".to_owned(),
        // Loopback eksplisit. Oxi memperingatkan sendiri kalau bind ke alamat
        // non-loopback tanpa token, dan itu bukan keadaan yang kita inginkan.
        "--host".to_owned(),
        "127.0.0.1".to_owned(),
        "--port".to_owned(),
        port.to_string(),
        "--cookie-file".to_owned(),
        spec.user_data_dir.join(COOKIE_FILE).display().to_string(),
    ];
    args.extend(spec.extra_args.iter().cloned());
    args
}

impl std::fmt::Debug for OxiEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OxiEngine")
            .field("spec", &self.spec)
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl Engine for OxiEngine {
    fn kind(&self) -> &'static str {
        "oxi"
    }

    fn info(&self) -> &EngineInfo {
        &self.info
    }

    fn spec(&self) -> &LaunchSpec {
        &self.spec
    }

    fn http_base(&self) -> String {
        format!("http://127.0.0.1:{}", self.info.devtools_port)
    }

    fn web_socket_url(&self) -> &str {
        &self.info.web_socket_debugger_url
    }

    fn process_id(&self) -> Option<u32> {
        self.proc.lock().ok().map(|guard| guard.pid())
    }

    fn is_running(&self) -> bool {
        let Ok(mut guard) = self.proc.lock() else {
            return false;
        };
        guard.is_running()
    }

    fn stop(&self) -> Result<(), EngineError> {
        let mut guard = self.proc.lock().map_err(|_| EngineError::Poisoned)?;
        guard.stop()
    }
}

/// Port bebas dari kernel.
///
/// Ada jeda antara `drop` listener dan oxi melakukan `bind`, dan port itu bisa
/// direbut proses lain di jeda tersebut. Itu diterima: kegagalannya terlihat
/// sebagai boot gagal dengan pesan bind dari oxi, bukan sebagai pesan yang
/// salah alamat. Alternatifnya (memaksa `--port 0`) tidak bisa dipakai karena
/// oxi tidak menulis port hasil bind ke berkas mana pun.
fn port_bebas() -> Result<u16, EngineError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn wait_for_version(port: u16, deadline: Instant, timeout_ms: u64) -> Result<String, EngineError> {
    loop {
        let terakhir = match http_get(port, "/json/version") {
            Ok(body) => return Ok(body),
            Err(err) => err.to_string(),
        };
        if Instant::now() >= deadline {
            tracing::warn!(hint = %terakhir, "oxi tidak menjawab discovery");
            return Err(EngineError::ReadyTimeout(timeout_ms));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Membaca `/json/version` dari dua gaya penamaan field.
///
/// Chrome: `Browser`, `User-Agent`, `Protocol-Version`, `webSocketDebuggerUrl`.
/// Oxi: `browser`, `userAgent`, `protocolVersion`, `webSocketDebuggerUrl`.
/// Perbedaan ini bukan detail kosmetik: satu nama yang salah tebak membuat boot
/// gagal dengan pesan "field tidak ada" padahal mesinnya sehat.
pub fn info_dari_json(body: &str, port: u16) -> Result<EngineInfo, EngineError> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|err| EngineError::Discovery(format!("bukan JSON: {err}")))?;

    let field = |keys: &[&str]| -> Option<String> {
        keys.iter().find_map(|key| {
            value
                .get(*key)
                .and_then(|item| item.as_str())
                .map(str::to_owned)
        })
    };

    let browser = field(&["Browser", "browser", "product"])
        .ok_or_else(|| EngineError::Discovery("field Browser/browser tidak ada".into()))?;
    let user_agent = field(&["User-Agent", "userAgent", "user_agent"])
        .ok_or_else(|| EngineError::Discovery("field User-Agent/userAgent tidak ada".into()))?;
    let web_socket_debugger_url = field(&[
        "webSocketDebuggerUrl",
        "web_socket_debugger_url",
        "webSocketUrl",
    ])
    .ok_or_else(|| EngineError::Discovery("field webSocketDebuggerUrl tidak ada".into()))?;

    Ok(EngineInfo {
        browser,
        protocol_version: field(&["Protocol-Version", "protocolVersion", "protocol_version"])
            .unwrap_or_else(|| "1.3".into()),
        user_agent,
        v8_version: field(&["V8-Version", "v8Version", "v8_version"]),
        webkit_version: field(&["WebKit-Version", "webkitVersion", "webkit_version"]),
        devtools_port: port,
        web_socket_debugger_url,
    })
}

/// Apakah `path` terlihat seperti biner oxi. Dipakai `probe::discover_oxi`,
/// dan sengaja hanya memeriksa nama: menjalankan biner untuk bertanya versi
/// berarti menyalakan mesin hanya untuk sebuah tebakan.
#[must_use]
pub fn nama_biner_cocok(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "oxibrowser")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_uji() -> LaunchSpec {
        LaunchSpec::new("/usr/local/bin/oxibrowser", "/tmp/oxi-uji")
    }

    #[test]
    fn argumen_menyebut_serve_loopback_dan_cookie_di_direktori_profil() {
        let args = argumen(&spec_uji(), 9333);
        assert_eq!(args[0], "serve");
        assert!(args.windows(2).any(|w| w == ["--host", "127.0.0.1"]));
        assert!(args.windows(2).any(|w| w == ["--port", "9333"]));
        assert!(
            args.windows(2)
                .any(|w| w == ["--cookie-file", "/tmp/oxi-uji/cookies.json"]),
            "cookie harus di dalam user_data_dir: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--allow-private-ips"),
            "filter SSRF tidak boleh dimatikan sendiri oleh adapter"
        );
        assert!(
            !args.iter().any(|a| a == "--auth-token"),
            "token tidak dikarang adapter"
        );
    }

    #[test]
    fn extra_args_diteruskan_apa_adanya() {
        let spec = spec_uji().with_args(["--allow-private-ips".to_owned()]);
        let args = argumen(&spec, 9333);
        assert!(args.iter().any(|a| a == "--allow-private-ips"));
    }

    /// Bentuk nyata dari oxi 0.28.0 (`oxibrowser-cdp/src/protocol.rs:75`).
    #[test]
    fn membaca_json_version_gaya_oxi() {
        let body = r#"{"browser":"OxiBrowser/0.28.0","protocolVersion":"1.3","userAgent":"OxiBrowser/0.28.0","v8Version":"OxiBrowser/0.28.0","webkitVersion":"OxiBrowser/0.28.0","webSocketDebuggerUrl":"ws://127.0.0.1:9333/ws"}"#;
        let info = info_dari_json(body, 9333).expect("field oxi lengkap");
        assert_eq!(info.browser, "OxiBrowser/0.28.0");
        assert_eq!(info.web_socket_debugger_url, "ws://127.0.0.1:9333/ws");
        assert_eq!(info.devtools_port, 9333);
    }

    /// Bentuk chrome-headless-shell, dibaca oleh pembaca yang sama.
    #[test]
    fn membaca_json_version_gaya_chrome() {
        let body = r#"{"Browser":"HeadlessChrome/156.0","Protocol-Version":"1.3","User-Agent":"Mozilla/5.0","V8-Version":"15.6","WebKit-Version":"537.36","webSocketDebuggerUrl":"ws://127.0.0.1:9224/devtools/browser/x"}"#;
        let info = info_dari_json(body, 9224).expect("field chrome lengkap");
        assert_eq!(info.browser, "HeadlessChrome/156.0");
        assert_eq!(info.user_agent, "Mozilla/5.0");
        assert_eq!(info.v8_version.as_deref(), Some("15.6"));
    }

    #[test]
    fn field_wajib_hilang_ditolak_dengan_nama_fieldnya() {
        let err =
            info_dari_json(r#"{"browser":"x","userAgent":"y"}"#, 1).expect_err("harus ditolak");
        assert!(err.to_string().contains("webSocketDebuggerUrl"), "{err}");
    }

    #[test]
    fn port_bebas_benar_benar_bisa_dipakai() {
        let port = port_bebas().expect("port");
        assert!(port > 0);
        // Bisa di-bind ulang berarti kernel sudah melepasnya.
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("port bisa dipakai ulang");
        drop(listener);
    }

    #[test]
    fn nama_biner_dikenali_tanpa_menjalankannya() {
        assert!(nama_biner_cocok(Path::new("/a/b/oxibrowser")));
        assert!(!nama_biner_cocok(Path::new("/a/b/chrome-headless-shell")));
        assert!(!nama_biner_cocok(Path::new("/a/b/oxibrowser-unsigned")));
    }
}
