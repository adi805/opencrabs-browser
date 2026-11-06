//! Uji kapabilitas `OxiBrowser` pada biner sungguhan.
//!
//! Uji ini **mengukur**, bukan mengasumsikan. Yang dicetak adalah jawaban mesin
//! sendiri lewat CDP: field `/json/version`, hasil `Target.createTarget`,
//! `Runtime.evaluate`, `DOM.getDocument`, `Page.captureScreenshot`, cookie, dan
//! input. Angka dan `ok`/`gagal` di laporan itulah yang dipakai mengisi
//! [`policy::Capabilities`] untuk jenis `oxi`, sehingga katalog tidak
//! mengarang kemampuan yang tidak ada.
//!
//! Dilewati dengan pesan jelas kalau biner oxi tidak ada, supaya `cargo test`
//! tetap hijau di mesin tanpa Oxibrowser. Di CI, workflow `oxi-probe.yml`
//! memasang binernya lebih dulu lewat `cargo install oxibrowser --features
//! browser`, lalu menjalankan uji ini dengan `OXI_BIN` menunjuk biner itu.
//!
//! Kenapa launcher-nya ditulis sendiri di sini, bukan memakai adapter
//! `engine::OxiEngine`: tugas uji ini justru memvalidasi kontrak CLI pihak
//! ketiga (`serve --host --port`, bentuk `/json/version`, path websocket)
//! **sebelum** adapter dibangun di atasnya. Kalau adapter dan uji memakai kode
//! yang sama, satu salah paham yang sama akan lolos dua kali.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const LISTEN: &str = "127.0.0.1";

/// Membunuh group proses mesin walau uji gagal di tengah jalan, lalu memanen
/// anaknya. `wait()` sesudah `killpg` wajib: proses yang sudah mati tapi belum
/// dipanen tetap jadi zombie, dan itu dihitung sebagai kebocoran.
struct Reaper {
    child: Option<Child>,
}

impl Reaper {
    fn baru(child: Child) -> Self {
        Self { child: Some(child) }
    }

    /// Matikan seluruh group lalu panen anaknya. Idempoten, jadi aman dipanggil
    /// di akhir uji maupun dari `Drop`.
    fn bereskan(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
            let _ = child.wait();
        }
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        self.bereskan();
    }
}

/// Biner oxi dari `OXI_BIN`, lalu lokasi yang dipakai host ini.
fn biner_oxi() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os("OXI_BIN") {
        let path = PathBuf::from(raw);
        return path.is_file().then_some(path);
    }
    let home = std::env::var_os("HOME")?;
    let kandidat = [
        PathBuf::from(&home).join(".cache/opencrabs-browser/oxi/dist/oxibrowser"),
        PathBuf::from(&home).join(".local/bin/oxibrowser"),
        PathBuf::from("/usr/local/bin/oxibrowser"),
    ];
    kandidat.into_iter().find(|path| path.is_file())
}

/// Port loopback bebas. Dipegang lalu dilepas: jeda antara dua langkah ini
/// kecil, dan oxi menerima port eksplisit sehingga tidak ada cara lain.
fn port_bebas() -> u16 {
    let listener = TcpListener::bind((LISTEN, 0)).expect("port bebas");
    listener.local_addr().expect("alamat lokal").port()
}

/// GET minimal ke mesin lokal, dibatasi 1 MiB seperti klien di crate `engine`.
fn http_get(port: u16, path: &str) -> Result<(u16, String), String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500))
        .map_err(|err| format!("connect: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|err| format!("set_read_timeout: {err}"))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {LISTEN}:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    )
    .map_err(|err| format!("tulis request: {err}"))?;

    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                buf.extend_from_slice(&chunk[..read]);
                if buf.len() > 1 << 20 {
                    return Err("respons melebihi 1 MiB".to_owned());
                }
                if selesai(&buf) {
                    break;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(format!("timeout: {err}"));
            }
            Err(err) => return Err(format!("baca: {err}")),
        }
    }

    let text = String::from_utf8_lossy(&buf).to_string();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| "respons bukan HTTP".to_owned())?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("status tidak terbaca: {head:?}"))?;
    Ok((status, body.to_owned()))
}

fn selesai(buf: &[u8]) -> bool {
    let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let head = String::from_utf8_lossy(&buf[..pos]);
    let panjang = head.lines().find_map(|line| {
        let (nama, nilai) = line.split_once(':')?;
        nama.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| nilai.trim().parse::<usize>().ok())
            .flatten()
    });
    panjang.is_some_and(|n| buf.len() - pos - 4 >= n)
}

/// Satu percakapan CDP di atas satu socket websocket.
struct Cdp {
    aliran: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: u64,
}

impl Cdp {
    async fn sambung(port: u16) -> Result<Self, String> {
        let url = format!("ws://{LISTEN}:{port}/ws");
        let (aliran, _) = tokio_tungstenite::connect_async(&url)
            .await
            .map_err(|err| format!("websocket {url}: {err}"))?;
        Ok(Self { aliran, next_id: 1 })
    }

    /// Mengirim satu perintah dan menunggu balasan dengan id yang sama.
    /// Event di antaranya diabaikan: uji ini menilai jawaban, bukan aliran event.
    async fn perintah(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let pesan = json!({"id": id, "method": method, "params": params});
        self.aliran
            .send(Message::Text(pesan.to_string().into()))
            .await
            .map_err(|err| format!("{method}: kirim: {err}"))?;

        let batas = Instant::now() + Duration::from_secs(20);
        loop {
            let sisa = batas.saturating_duration_since(Instant::now());
            if sisa.is_zero() {
                return Err(format!("{method}: tidak ada jawaban dalam 20 detik"));
            }
            let pesan = tokio::time::timeout(sisa, self.aliran.next())
                .await
                .map_err(|_| format!("{method}: timeout menunggu jawaban"))?
                .ok_or_else(|| format!("{method}: socket ditutup"))?
                .map_err(|err| format!("{method}: baca: {err}"))?;
            let teks = match pesan {
                Message::Text(teks) => teks.to_string(),
                Message::Binary(bin) => String::from_utf8_lossy(&bin).to_string(),
                _ => continue,
            };
            let value: Value = match serde_json::from_str(&teks) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(format!("{method}: galat CDP: {error}"));
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

/// Satu baris hasil yang bisa dibaca manusia dan mesin.
fn baris(laporan: &mut String, nama: &str, hasil: Result<String, String>) {
    match hasil {
        Ok(detail) => {
            println!("PROBE {nama}: ok {detail}");
            laporan.push_str(&format!("| `{nama}` | ok | {detail} |\n"));
        }
        Err(sebab) => {
            println!("PROBE {nama}: GAGAL {sebab}");
            laporan.push_str(&format!("| `{nama}` | GAGAL | {sebab} |\n"));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_kapabilitas() {
    let Some(biner) = biner_oxi() else {
        eprintln!(
            "Dilewati: biner oxibrowser tidak ditemukan. Pasang dengan \
             `cargo install oxibrowser --version 0.28.0 --features browser --locked`, \
             atau set OXI_BIN."
        );
        return;
    };

    let port = port_bebas();
    let profil = std::env::temp_dir().join(format!("ocb-oxi-probe-{}", std::process::id()));
    std::fs::create_dir_all(&profil).expect("folder profil");

    let mut cmd = Command::new(&biner);
    cmd.args(["serve", "--host", LISTEN, "--port", &port.to_string()]);
    // Fixture uji dilayani dari loopback, dan oxi memblokir IP privat secara
    // bawaan (perlindungan SSRF). Tanpa flag ini navigasi ke fixture akan
    // ditolak, dan yang gagal bukan browsernya melainkan caranya diuji.
    cmd.arg("--allow-private-ips");
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    let log = std::fs::File::create(profil.join("oxi-stderr.log")).expect("log stderr");
    cmd.stderr(Stdio::from(log));
    cmd.process_group(0);
    let child: Child = cmd.spawn().expect("oxibrowser serve harus bisa dinyalakan");
    let mut reaper = Reaper::baru(child);

    // Tunggu sampai mesin menjawab discovery. Batas 30 detik: biner debug dari
    // CI lebih lambat daripada biner rilis, dan uji ini bukan lomba kecepatan.
    let batas = Instant::now() + Duration::from_secs(30);
    let version = loop {
        match http_get(port, "/json/version") {
            Ok((200, body)) => break body,
            _ if Instant::now() >= batas => {
                let mut log = String::new();
                if let Ok(isi) = std::fs::read_to_string(profil.join("oxi-stderr.log")) {
                    log = isi.lines().rev().take(20).collect::<Vec<_>>().join("\n");
                }
                panic!("oxi tidak menjawab /json/version dalam 30 detik. stderr:\n{log}");
            }
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    };

    let mut laporan = String::new();
    println!("PROBE /json/version: {version}");
    let value: Value = serde_json::from_str(&version).expect("/json/version harus JSON");
    baris(&mut laporan, "json_version", Ok(version.clone()));

    // Nama field diperiksa apa adanya: Chrome memakai PascalCase, oxi memakai
    // snake_case. Adapter yang menganggap keduanya sama akan gagal boot, dan
    // kegagalan itu harus terlihat di laporan ini, bukan nanti di daemon.
    for field in ["Browser", "browser"] {
        if let Some(isi) = value.get(field).and_then(Value::as_str) {
            baris(&mut laporan, &format!("field_{field}"), Ok(isi.to_owned()));
        }
    }
    baris(
        &mut laporan,
        "webSocketDebuggerUrl",
        value
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "tidak ada".to_owned()),
    );

    let (status, daftar) = http_get(port, "/json/list").expect("/json/list harus terjawab");
    baris(
        &mut laporan,
        "json_list",
        (status == 200)
            .then_some(daftar.chars().take(200).collect::<String>())
            .ok_or_else(|| format!("status {status}")),
    );

    let mut cdp = Cdp::sambung(port)
        .await
        .expect("websocket /ws harus terbuka");

    let target = cdp
        .perintah("Target.createTarget", json!({"url": "about:blank"}))
        .await;
    let target_id = target
        .as_ref()
        .ok()
        .and_then(|value| value.get("targetId"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    baris(
        &mut laporan,
        "Target.createTarget",
        target.map(|value| value.to_string()),
    );

    let mut sesi = None;
    if let Some(target_id) = target_id {
        let attached = cdp
            .perintah(
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
            )
            .await;
        if let Ok(value) = &attached {
            sesi = value
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        baris(
            &mut laporan,
            "Target.attachToTarget",
            attached.map(|value| value.to_string()),
        );
    }

    let sesi_param = |method: &str, mut params: Value| -> Value {
        if let Some(sesi) = &sesi {
            params["sessionId"] = json!(sesi);
        }
        let _ = method;
        params
    };

    // Panel aksesibilitas dan DOM perintah butuh target yang terpasang.
    baris(
        &mut laporan,
        "DOM.getDocument",
        cdp.perintah("DOM.getDocument", sesi_param("DOM.getDocument", json!({})))
            .await
            .map(|value| {
                value
                    .get("root")
                    .map(|root| root.to_string())
                    .unwrap_or_else(|| value.to_string())
            }),
    );

    let eval = cdp
        .perintah(
            "Runtime.evaluate",
            sesi_param(
                "Runtime.evaluate",
                json!({"expression": "6 * 7", "returnByValue": true}),
            ),
        )
        .await;
    let hasil_eval = eval
        .as_ref()
        .ok()
        .and_then(|value| value.get("result"))
        .and_then(|result| result.get("value"))
        .cloned();
    baris(
        &mut laporan,
        "Runtime.evaluate",
        match (eval, hasil_eval) {
            (Ok(_), Some(nilai)) => Ok(format!("6*7 = {nilai}")),
            (Err(sebab), _) => Err(sebab),
            (Ok(_), None) => Err("tidak ada field result.value".to_owned()),
        },
    );

    // Navigasi ke fixture lokal: satu-satunya cara membuktikan layout dan
    // jaringan jalan, bukan cuma mesin JS.
    let (url, server) = sajikan_fixture().await;
    let nav = cdp
        .perintah(
            "Page.navigate",
            sesi_param("Page.navigate", json!({"url": url})),
        )
        .await;
    baris(
        &mut laporan,
        "Page.navigate",
        nav.map(|value| value.to_string()),
    );

    tokio::time::sleep(Duration::from_millis(500)).await;

    let judul = cdp
        .perintah(
            "Runtime.evaluate",
            sesi_param(
                "Runtime.evaluate",
                json!({"expression": "document.title", "returnByValue": true}),
            ),
        )
        .await;
    baris(
        &mut laporan,
        "document.title",
        judul.map(|value| {
            value
                .get("result")
                .map(|r| r.to_string())
                .unwrap_or_default()
        }),
    );

    baris(
        &mut laporan,
        "Page.captureScreenshot",
        cdp.perintah(
            "Page.captureScreenshot",
            sesi_param("Page.captureScreenshot", json!({"format": "png"})),
        )
        .await
        .map(|value| {
            let panjang = value
                .get("data")
                .and_then(Value::as_str)
                .map(|data| data.len())
                .unwrap_or(0);
            format!("{panjang} karakter base64")
        }),
    );

    baris(
        &mut laporan,
        "Network.getCookies",
        cdp.perintah(
            "Network.getCookies",
            sesi_param("Network.getCookies", json!({"urls": [url]})),
        )
        .await
        .map(|value| value.to_string()),
    );

    baris(
        &mut laporan,
        "Input.dispatchMouseEvent",
        async {
            let tekan = cdp
                .perintah(
                    "Input.dispatchMouseEvent",
                    sesi_param(
                        "Input.dispatchMouseEvent",
                        json!({"type": "mousePressed", "x": 10, "y": 10, "button": "left", "clickCount": 1}),
                    ),
                )
                .await;
            let lepas = cdp
                .perintah(
                    "Input.dispatchMouseEvent",
                    sesi_param(
                        "Input.dispatchMouseEvent",
                        json!({"type": "mouseReleased", "x": 10, "y": 10, "button": "left", "clickCount": 1}),
                    ),
                )
                .await;
            tekan.and(lepas).map(|value| value.to_string())
        }
        .await,
    );

    server.abort();
    reaper.bereskan();

    let ringkas = format!(
        "# Bukti kapabilitas OxiBrowser\n\nBiner: `{}`\nPort: {}\n\n| Probe | Hasil | Detail |\n|---|---|---|\n{laporan}",
        biner.display(),
        port
    );
    println!("PROBE_LAPORAN_MULAI\n{ringkas}\nPROBE_LAPORAN_SELESAI");
}

/// Fixture sekecil mungkin: judul, satu tombol, satu input. Kalau oxi tidak
/// bisa membuka ini, ia tidak bisa membuka apa pun.
async fn sajikan_fixture() -> (String, tokio::task::JoinHandle<()>) {
    use axum::Router;
    use axum::response::Html;
    use axum::routing::get;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("port fixture");
    let port = listener.local_addr().expect("alamat fixture").port();
    let app = Router::new().route(
        "/",
        get(|| async {
            Html(
                r#"<!doctype html><html><head><title>Halaman Probe Oxi</title></head>
<body><h1 id="judul">Probe</h1><button id="tombol">Klik</button>
<input id="isian" type="text"><div id="hasil">belum</div>
<script>
document.getElementById('tombol').addEventListener('click', function () {
  document.getElementById('hasil').textContent = 'sudah diklik';
});
</script></body></html>"#,
            )
        }),
    );
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://127.0.0.1:{port}/"), server)
}
