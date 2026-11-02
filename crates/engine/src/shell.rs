//! Adapter untuk `chrome-headless-shell`.
//!
//! Yang dijaga di sini: proses anak **selalu** dimatikan sebagai satu group.
//! Anak dari shell (zygote, renderer) tidak ikut mati kalau hanya pid induk
//! yang di-SIGTERM, dan itulah sumber memori yang tidak pernah kembali.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use crate::{Engine, EngineError, EngineInfo, LaunchSpec};

pub const DEFAULT_READY_TIMEOUT_MS: u64 = 15_000;

/// Batas keras ukuran respons discovery. Endpoint ini kecil; respons raksasa
/// berarti yang menjawab bukan mesin yang kita kira.
const MAX_RESPONSE_BYTES: usize = 1 << 20;

const TERM_GRACE: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Flag yang sudah terbukti di benchmark sebelumnya. Sengaja tidak memakai
/// `--headless`: biner ini headless secara bawaan.
const SHELL_FLAGS: &[&str] = &[
    "--no-sandbox",
    "--disable-dev-shm-usage",
    "--disable-gpu",
    "--mute-audio",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-background-networking",
    "--disable-sync",
    "--disable-extensions",
    "--window-size=1920,1080",
];

struct Proc {
    child: Child,
    pgid: i32,
    stopped: bool,
}

pub struct ShellEngine {
    spec: LaunchSpec,
    info: EngineInfo,
    proc: Mutex<Proc>,
}

impl ShellEngine {
    /// Menyalakan shell dan menunggu sampai benar-benar bisa menjawab.
    ///
    /// Kalau gagal di tengah jalan, proses yang sudah telanjur hidup dimatikan
    /// sebelum error dikembalikan. Tidak boleh ada proses yatim dari kegagalan.
    pub fn launch(spec: LaunchSpec, timeout_ms: u64) -> Result<Self, EngineError> {
        if !spec.binary.is_file() {
            return Err(EngineError::BinaryMissing(spec.binary.clone()));
        }
        std::fs::create_dir_all(&spec.user_data_dir)?;

        let port_file = spec.user_data_dir.join("DevToolsActivePort");
        // Berkas dari run sebelumnya membuat kita membaca port yang sudah mati.
        let _ = std::fs::remove_file(&port_file);

        let mut cmd = Command::new(&spec.binary);
        cmd.arg(format!("--remote-debugging-port={}", spec.port));
        cmd.arg("--remote-debugging-address=127.0.0.1");
        cmd.arg(format!("--user-data-dir={}", spec.user_data_dir.display()));
        cmd.args(SHELL_FLAGS);
        cmd.args(&spec.extra_args);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        // Group sendiri: SIGTERM ke group ini ikut mematikan seluruh anaknya.
        cmd.process_group(0);

        let child = cmd.spawn()?;
        let pgid = child.id() as i32;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);

        let prepared = (|| -> Result<EngineInfo, EngineError> {
            let port = if spec.port == 0 {
                wait_for_port_file(&port_file, deadline, timeout_ms)?
            } else {
                spec.port
            };
            let value = wait_for_version(port, deadline, timeout_ms)?;
            info_from_json(&value, port)
        })();

        match prepared {
            Ok(info) => Ok(Self {
                spec,
                info,
                proc: Mutex::new(Proc {
                    child,
                    pgid,
                    stopped: false,
                }),
            }),
            Err(err) => {
                let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
                let mut child = child;
                let _ = child.wait();
                Err(err)
            }
        }
    }
}

impl Engine for ShellEngine {
    fn kind(&self) -> &'static str {
        "shell"
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
        self.proc.lock().ok().map(|guard| guard.child.id())
    }

    fn is_running(&self) -> bool {
        let Ok(mut guard) = self.proc.lock() else {
            return false;
        };
        if guard.stopped {
            return false;
        }
        matches!(guard.child.try_wait(), Ok(None))
    }

    fn stop(&self) -> Result<(), EngineError> {
        let mut guard = self.proc.lock().map_err(|_| EngineError::Poisoned)?;
        if guard.stopped {
            return Ok(());
        }
        let pgid = Pid::from_raw(guard.pgid);

        // SIGTERM dulu: shell sempat menutup profil dan menyimpan cookie.
        let _ = killpg(pgid, Signal::SIGTERM);
        let deadline = Instant::now() + TERM_GRACE;
        while Instant::now() < deadline {
            match guard.child.try_wait() {
                Ok(Some(_)) => {
                    guard.stopped = true;
                    return Ok(());
                }
                Ok(None) => std::thread::sleep(POLL_INTERVAL),
                Err(err) => return Err(EngineError::Io(err)),
            }
        }

        let _ = killpg(pgid, Signal::SIGKILL);
        let _ = guard.child.wait();
        guard.stopped = true;
        Ok(())
    }
}

fn wait_for_port_file(path: &Path, deadline: Instant, timeout_ms: u64) -> Result<u16, EngineError> {
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Some(port) = parse_devtools_port(&text)
        {
            return Ok(port);
        }
        if Instant::now() >= deadline {
            return Err(EngineError::PortFileTimeout(timeout_ms));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_version(port: u16, deadline: Instant, timeout_ms: u64) -> Result<String, EngineError> {
    loop {
        let last = match http_get(port, "/json/version") {
            Ok(body) => return Ok(body),
            Err(err) => err.to_string(),
        };
        if Instant::now() >= deadline {
            // Kegagalan terakhir ikut dilaporkan: "timeout" tanpa sebab
            // memaksa orang menebak, dan tebakan biasanya salah.
            tracing::warn!(hint = %last, "mesin tidak menjawab discovery");
            return Err(EngineError::ReadyTimeout(timeout_ms));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Baris pertama `DevToolsActivePort` adalah port. Baris kedua adalah path
/// websocket, yang tidak kita pakai karena URL lengkap datang dari
/// `/json/version`.
pub fn parse_devtools_port(contents: &str) -> Option<u16> {
    contents.lines().next()?.trim().parse::<u16>().ok()
}

fn info_from_json(body: &str, port: u16) -> Result<EngineInfo, EngineError> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|err| EngineError::Discovery(format!("bukan JSON: {err}")))?;

    let field = |key: &str| {
        value
            .get(key)
            .and_then(|item| item.as_str())
            .map(str::to_owned)
    };

    let browser =
        field("Browser").ok_or_else(|| EngineError::Discovery("field Browser tidak ada".into()))?;
    let user_agent = field("User-Agent")
        .ok_or_else(|| EngineError::Discovery("field User-Agent tidak ada".into()))?;
    let web_socket_debugger_url = field("webSocketDebuggerUrl")
        .ok_or_else(|| EngineError::Discovery("field webSocketDebuggerUrl tidak ada".into()))?;

    Ok(EngineInfo {
        browser,
        protocol_version: field("Protocol-Version").unwrap_or_else(|| "1.3".into()),
        user_agent,
        v8_version: field("V8-Version"),
        webkit_version: field("WebKit-Version"),
        devtools_port: port,
        web_socket_debugger_url,
    })
}

/// GET minimal ke mesin lokal. Ditulis manual karena yang dibutuhkan hanya
/// HTTP/1.1 tanpa TLS ke loopback, dan menambah klien HTTP penuh untuk ini
/// berarti menambah permukaan yang harus dijaga.
fn http_get(port: u16, path: &str) -> Result<String, EngineError> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;

    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    )?;

    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                buf.extend_from_slice(&chunk[..read]);
                if buf.len() > MAX_RESPONSE_BYTES {
                    return Err(EngineError::Discovery("respons melebihi 1 MiB".into()));
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(EngineError::Discovery(format!("timeout membaca: {err}")));
            }
            Err(err) => return Err(EngineError::Io(err)),
        }
    }

    let (head, body) = split_http_response(&buf)
        .ok_or_else(|| EngineError::Discovery("respons bukan HTTP".into()))?;
    let status_line = String::from_utf8_lossy(head);
    let status_line = status_line.lines().next().unwrap_or_default();
    if !status_line.contains(" 200") {
        return Err(EngineError::Discovery(format!(
            "status bukan 200: {status_line}"
        )));
    }
    String::from_utf8(body.to_vec())
        .map_err(|err| EngineError::Discovery(format!("body bukan UTF-8: {err}")))
}

fn split_http_response(buf: &[u8]) -> Option<(&[u8], &[u8])> {
    let sep = b"\r\n\r\n";
    let pos = buf.windows(sep.len()).position(|window| window == sep)?;
    Some((&buf[..pos], &buf[pos + sep.len()..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn port_dibaca_dari_baris_pertama() {
        assert_eq!(
            parse_devtools_port("41234\n/devtools/browser/abc\n"),
            Some(41234)
        );
        assert_eq!(parse_devtools_port("41234"), Some(41234));
    }

    #[test]
    fn port_file_sampah_balik_none() {
        assert_eq!(parse_devtools_port(""), None);
        assert_eq!(parse_devtools_port("bukan-angka\n/abc"), None);
        assert_eq!(parse_devtools_port("99999\n"), None, "port di luar rentang");
    }

    #[test]
    fn body_dipisah_dari_header() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{\"a\":1}";
        let (head, body) = split_http_response(raw).expect("ada pemisah");
        assert!(String::from_utf8_lossy(head).contains("200"));
        assert_eq!(body, b"{\"a\":1}");
    }

    #[test]
    fn respons_tanpa_pemisah_ditolak() {
        assert!(split_http_response(b"HTTP/1.1 200 OK").is_none());
    }

    /// Menyalakan server HTTP sekali-tembak: membuktikan klien manual kita
    /// benar-benar bisa membaca `/json/version`, bukan hanya dikira bisa.
    #[test]
    fn http_get_membaca_json_version() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("port bebas");
        let port = listener.local_addr().expect("alamat lokal").port();

        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("satu koneksi");
            // Seluruh request harus dibaca sampai baris kosong. Kalau sisanya
            // dibiarkan di buffer, `close()` mengirim RST dan respons yang
            // sudah ditulis dibuang kernel sebelum klien sempat membacanya.
            use std::io::BufRead;
            let mut reader = BufReader::new(socket.try_clone().expect("klon socket"));
            let mut request = String::new();
            loop {
                let mut line = String::new();
                let read = reader.read_line(&mut line).expect("baris permintaan");
                request.push_str(&line);
                if read == 0 || line == "\r\n" || line == "\n" {
                    break;
                }
            }
            assert!(
                request.starts_with("GET /json/version HTTP/1.1"),
                "request: {request:?}"
            );
            let body = r#"{"Browser":"HeadlessChrome/test","Protocol-Version":"1.3","User-Agent":"ua","webSocketDebuggerUrl":"ws://127.0.0.1/x"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .expect("tulis respons");
        });

        let body = http_get(port, "/json/version").expect("harus terbaca");
        server.join().expect("server selesai");

        let info = info_from_json(&body, port).expect("field lengkap");
        assert_eq!(info.browser, "HeadlessChrome/test");
        assert_eq!(info.devtools_port, port);
        assert_eq!(info.v8_version, None, "field yang tidak dikirim tetap None");
    }

    #[test]
    fn respons_tanpa_websocket_ditolak() {
        let body = r#"{"Browser":"x","User-Agent":"y"}"#;
        let err = info_from_json(body, 1).expect_err("harus ditolak");
        assert!(err.to_string().contains("webSocketDebuggerUrl"));
    }
}
