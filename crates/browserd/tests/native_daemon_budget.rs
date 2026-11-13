//! NFR-001 di proses daemon yang sesungguhnya, bukan pool di proses uji.
//!
//! `native_99_tabs_budget_sqlite.rs` mengukur PSS **proses uji** ketika
//! `TabPool` dijalankan di dalamnya. Angka itu sah untuk pool, tapi dia tidak
//! bisa melihat daemon yang dikirim ke pengguna: daemon menjalankan tokio,
//! axum, SQLite, dan alokasi glibc-nya sendiri, dan hanya angka proses itulah
//! yang dirasakan mesin. Celahnya bukan teori: pada HEAD yang sama, dua run
//! daemon release menghasilkan 89,9 dan 89,3 MB, sementara run lain dari binary
//! yang lebih tua menghasilkan 127,8 MB. Pool yang hijau tidak mencegah daemon
//! merah, jadi pagunya harus dibaca dari proses daemon.
//!
//! Yang diukur di sini: PSS proses `browserd serve` sungguhan sesudah 99 tab
//! diisi dokumen beban lewat CDP, dibaca dari `/proc/<pid daemon>/smaps_rollup`.
//! Beban dan urutan operasinya sama dengan harness pengukur (`pss-99.mjs`):
//! `Target.createTarget`, `attachToTarget`, `Page.getFrameTree`,
//! `Page.setDocumentContent`. Pagunya memakai `tab_pool::PAGU_MB`, bukan angka
//! baru yang bisa berbeda diam-diam dari yang dijanjikan PRD.
//!
//! Penegakan pagu hanya berlaku di profil yang benar-benar dikirim. Daemon
//! profil dev terukur 110,8 dan 120,4 MB pada 99 tab, sedangkan binary release
//! 89,3 dan 89,9 MB: overhead 21-31 MB itu milik rquickjs tanpa optimasi, bukan
//! milik daemon. Karena itu profil dev hanya mencetak angkanya, dan pagunya
//! ditegakkan oleh job CI yang menjalankan tes ini dengan `--release`.
//!
//! Profil antara sempat dicoba dan dibuang: `inherits = "release"` dengan
//! `lto = false` terukur **109,3 MB** pada 99 tab. Selisih ~20 MB itu akibat
//! hilangnya LTO, bukan akibat daemon. Gate yang mengukur binary yang tidak
//! pernah dikirim cuma bisa merah permanen atau menaikkan pagu sampai tidak
//! berarti, jadi yang diukur di sini harus profil `release`.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cdp::native_wire::MAKS_TAB;
use engine::native::tab_pool::PAGU_MB;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

mod common;

/// Jumlah `div` di dokumen beban, sama dengan tes anggaran dan harness.
const JUMLAH_DIV: usize = 1500;

/// Batas menunggu daemon menjawab `/json/version` sesudah diluncurkan.
const BATAS_SIAP: Duration = Duration::from_secs(30);

struct Sementara {
    path: PathBuf,
}

impl Sementara {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ocb-daemon-budget-{}-{label}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("direktori sementara");
        Self { path }
    }

    fn isi(&self, nama: &str) -> PathBuf {
        self.path.join(nama)
    }
}

impl Drop for Sementara {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Daemon yang selalu ditutup, termasuk ketika asersi gagal di tengah jalan.
///
/// Tanpa ini, satu kegagalan asersi meninggalkan daemon hidup memegang port dan
/// ~90 MB RAM; tes berikutnya lalu gagal karena alasan yang bukan kodenya.
struct Anak {
    child: Option<std::process::Child>,
}

impl Anak {
    fn id(&self) -> u32 {
        self.child.as_ref().expect("daemon hidup").id()
    }
}

impl Drop for Anak {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            common::buang(&mut child);
        }
    }
}

fn dokumen_beban() -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(JUMLAH_DIV * 64);
    s.push_str("<!doctype html><html><head><title>beban</title></head><body>");
    for i in 0..JUMLAH_DIV {
        let _ = write!(
            s,
            "<div id=\"k{i}\" class=\"baris\" style=\"height:20px\">isi baris {i}</div>"
        );
    }
    s.push_str("</body></html>");
    s
}

/// Satu GET HTTP polos, mengembalikan (status, badan).
///
/// Cukup untuk `/json/version` dan `/json/list`; menambah klien HTTP ke
/// dev-dependency cuma untuk dua permintaan ini tidak sepadan.
fn minta(port: u16, path: &str) -> Option<(u16, String)> {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut teks = String::new();
    stream.read_to_string(&mut teks).ok()?;
    let (kepala, badan) = teks.split_once("\r\n\r\n")?;
    let status = kepala
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some((status, badan.to_owned()))
}

/// PSS satu proses dalam MB, dibaca dari `smaps_rollup` proses itu.
fn pss_mb(pid: u32) -> f64 {
    let teks = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
        .expect("smaps_rollup proses daemon harus terbaca di Linux");
    for baris in teks.lines() {
        if let Some(sisa) = baris.strip_prefix("Pss:") {
            let kb: f64 = sisa
                .split_whitespace()
                .next()
                .expect("nilai Pss")
                .parse()
                .expect("Pss dalam kB");
            return kb / 1024.0;
        }
    }
    panic!("baris Pss tidak ditemukan di smaps_rollup pid {pid}");
}

/// Klien CDP minimal: satu websocket, satu permintaan per panggilan.
struct Cdp {
    aliran: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: u64,
}

impl Cdp {
    async fn sambung(url: &str) -> Result<Self, String> {
        let (aliran, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|err| format!("websocket {url}: {err}"))?;
        Ok(Self { aliran, next_id: 1 })
    }

    async fn perintah(
        &mut self,
        method: &str,
        params: Value,
        sesi: Option<&str>,
    ) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut pesan = json!({"id": id, "method": method, "params": params});
        if let Some(sesi) = sesi {
            pesan["sessionId"] = json!(sesi);
        }
        self.aliran
            .send(Message::Text(pesan.to_string().into()))
            .await
            .map_err(|err| format!("{method}: kirim: {err}"))?;

        let batas = Instant::now() + Duration::from_secs(60);
        loop {
            let sisa = batas.saturating_duration_since(Instant::now());
            if sisa.is_zero() {
                return Err(format!("{method}: tidak ada jawaban dalam 60 detik"));
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

fn port_bebas() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("port bebas");
    listener.local_addr().expect("alamat lokal").port()
}

/// Meluncurkan `browserd serve` dengan mesin native, penyimpanan dingin di
/// direktori sementara, dan reap dimatikan supaya tab tidak menghilang di
/// tengah pengukuran.
fn luncurkan(dir: &Sementara) -> (Anak, u16) {
    let cdp_port = port_bebas();
    let kontrol_port = port_bebas();
    let db = dir.isi("store.db");
    let kunci = dir.isi("store.key");
    let token = dir.isi("control.token");
    let config = dir.isi("browserd.toml");

    let isi = format!(
        "[daemon]\n\
         bind = \"127.0.0.1:{cdp_port}\"\n\
         control_bind = \"127.0.0.1:{kontrol_port}\"\n\
         idle_reap_secs = 0\n\
         store_db = \"{}\"\n\
         store_key = \"{}\"\n\
         control_token = \"{}\"\n\
         \n\
         [engines.native]\n\
         kind = \"rust-native\"\n\
         max_tabs = {MAKS_TAB}\n\
         \n\
         [policy]\n\
         default = \"native\"\n",
        db.display(),
        kunci.display(),
        token.display()
    );
    std::fs::write(&config, isi).expect("menulis config uji");

    let keygen = Command::new(env!("CARGO_BIN_EXE_browserd"))
        .args([
            "keygen",
            "--key-file",
            kunci.to_str().expect("path utf8"),
            "--db",
            db.to_str().expect("path utf8"),
        ])
        .output()
        .expect("menjalankan keygen");
    assert!(
        keygen.status.success(),
        "keygen gagal: {}",
        String::from_utf8_lossy(&keygen.stderr)
    );

    let child = Command::new(env!("CARGO_BIN_EXE_browserd"))
        .args([
            "serve",
            "--bind",
            &format!("127.0.0.1:{cdp_port}"),
            "--config",
            config.to_str().expect("path utf8"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("menjalankan browserd serve");

    let daemon = Anak { child: Some(child) };
    let batas = Instant::now() + BATAS_SIAP;
    loop {
        if matches!(minta(cdp_port, "/json/version"), Some((200, _))) {
            break;
        }
        assert!(
            Instant::now() < batas,
            "daemon tidak menjawab /json/version dalam {BATAS_SIAP:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    (daemon, cdp_port)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_sembilan_puluh_sembilan_tab_di_bawah_pagu_pss() {
    // Uji live lain memakai kunci yang sama: angka PSS yang diukur sambil
    // Chromium lain berebut RAM adalah angka host, bukan angka daemon.
    let _kunci = common::kunci_live("native_daemon_budget");

    let dir = Sementara::new("native");
    let (daemon, port) = luncurkan(&dir);
    let pid = daemon.id();

    let (_, badan) = minta(port, "/json/version").expect("badan /json/version");
    let versi: Value = serde_json::from_str(&badan).expect("JSON /json/version");
    let ws_url = versi
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .expect("webSocketDebuggerUrl dari daemon")
        .to_owned();
    let browser = versi
        .get("Browser")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_owned();
    assert!(
        browser.starts_with("RustNativeEngine"),
        "yang diukur harus mesin native, dapat {browser}"
    );

    let html = dokumen_beban();
    let pss_awal = pss_mb(pid);
    let mulai = Instant::now();
    let mut cdp = Cdp::sambung(&ws_url).await.expect("websocket CDP daemon");

    let mut node_terakhir = 0;
    for nomor in 0..MAKS_TAB {
        let target = cdp
            .perintah("Target.createTarget", json!({"url": "about:blank"}), None)
            .await
            .unwrap_or_else(|err| panic!("tab ke-{}: {err}", nomor + 1));
        let target_id = target
            .get("targetId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("tab ke-{}: createTarget tanpa targetId", nomor + 1))
            .to_owned();
        let sesi = cdp
            .perintah(
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
                None,
            )
            .await
            .unwrap_or_else(|err| panic!("tab ke-{}: {err}", nomor + 1))
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("tab ke-{}: attach tanpa sessionId", nomor + 1))
            .to_owned();
        let pohon = cdp
            .perintah("Page.getFrameTree", json!({}), Some(&sesi))
            .await
            .unwrap_or_else(|err| panic!("tab ke-{}: {err}", nomor + 1));
        let frame = pohon
            .get("frameTree")
            .and_then(|pohon| pohon.get("frame"))
            .and_then(|frame| frame.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("tab ke-{}: getFrameTree tanpa frame.id", nomor + 1))
            .to_owned();
        let isi = cdp
            .perintah(
                "Page.setDocumentContent",
                json!({"frameId": frame, "html": html}),
                Some(&sesi),
            )
            .await
            .unwrap_or_else(|err| panic!("tab ke-{}: {err}", nomor + 1));
        // Respons daemon berupa tanda terima perubahan, bukan jumlah node
        // mentah; jumlah node dokumen ada di `receipt.sesudah.node`.
        let tanda = isi
            .get("receipt")
            .and_then(|tanda| tanda.get("sesudah"))
            .and_then(|sesudah| sesudah.get("node"))
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("tab ke-{}: receipt tanpa sesudah.node", nomor + 1));
        node_terakhir = tanda;
    }

    let detik = mulai.elapsed().as_secs_f64();
    let pss_akhir = pss_mb(pid);
    let (_, daftar) = minta(port, "/json/list").expect("badan /json/list");
    let target = serde_json::from_str::<Value>(&daftar)
        .map(|value| value.as_array().map(Vec::len).unwrap_or(0))
        .unwrap_or(0);

    let laporan = format!(
        "daemon pid {pid}: PSS {pss_akhir:.1} MB (awal {pss_awal:.1} MB) untuk {target} target, \
         dokumen {node_terakhir} node, {MAKS_TAB} tab dalam {detik:.1} detik, pagu {PAGU_MB:.0} MB"
    );
    println!("{laporan}");

    assert_eq!(target, MAKS_TAB, "daemon harus memegang {MAKS_TAB} target");
    assert!(
        node_terakhir >= 3000,
        "dokumen beban harus seukuran harness (>=3000 node), dapat {node_terakhir}"
    );
    // Profil dev diukur, tidak ditegakkan: daemon dev terukur 110-120 MB
    // pada 99 tab karena rquickjs tanpa optimasi, bukan karena regresi, dan
    // gate yang merah permanen tidak berguna. Penegakan pagu ada di job CI
    // yang menjalankan tes ini dengan `--release`, yaitu profil binary yang
    // dikirim.
    if cfg!(debug_assertions) {
        println!(
            "UKUR dev: PSS {pss_akhir:.1} MB di luar penegakan pagu; pagu {PAGU_MB:.0} MB \
             ditegakkan lewat `cargo test -p browserd --test native_daemon_budget --release`"
        );
    } else {
        assert!(
            pss_akhir <= PAGU_MB,
            "PSS daemon {pss_akhir:.1} MB melewati pagu {PAGU_MB:.0} MB: {laporan}"
        );
    }
}
