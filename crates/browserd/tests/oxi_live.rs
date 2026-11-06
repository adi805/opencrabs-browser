//! Milestone 5: adapter `oxi` diuji dengan mesin sungguhan.
//!
//! Dua hal berbeda dibuktikan di sini, dan keduanya perlu:
//!
//! 1. **Daemon mau menerima oxi.** `[engines.oxi] kind = "oxi"` dulu ditolak
//!    `bail!("jenis '{kind}' belum didukung v0.1")`. Uji ini menyalakan daemon
//!    dari config seperti itu dan membaca `/json/version` serta `/json/list`
//!    dari port data plane, jadi yang diuji adalah jalur yang benar-benar
//!    dipakai operator, bukan pemanggilan fungsi internal.
//! 2. **Oxi bisa dipakai untuk kerja nyata.** `Runtime.evaluate` mengembalikan
//!    angka yang benar, DOM terbaca, `Page.captureScreenshot` menghasilkan PNG
//!    yang bisa dikenali, dan group proses benar-benar hilang setelah `stop()`.
//!
//! Kenapa tidak cukup dengan uji kapabilitas mentah (`oxi_probe.rs`): probe itu
//! berbicara ke biner oxi langsung. Yang dijanjikan milestone 5 adalah adapter
//! **kita** bekerja, dan adapter bisa saja salah membaca field discovery, salah
//! memilih port, atau salah mematikan proses meski binernya sendiri sehat.
//!
//! Dilewati dengan pesan jelas kalau biner oxi tidak ada. Oxi tidak menerbitkan
//! biner Linux, jadi di host pengembangan normal uji ini memang dilewati;
//! tempatnya berjalan adalah CI (`oxi-probe.yml`), yang membangun binernya
//! lebih dulu.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::response::Html;
use axum::routing::get;
use browserd::penanam::Sesi;
use engine::{Engine, LaunchSpec, OxiEngine, discover_oxi};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;
use serde_json::{Value, json};

const BATAS_BOOT: Duration = Duration::from_secs(60);

/// Membunuh group mesin walau uji gagal di tengah jalan.
struct Reaper {
    pgid: Option<i32>,
}

impl Reaper {
    fn new() -> Self {
        Self { pgid: None }
    }

    fn arm(&mut self, pid: u32) {
        self.pgid = Some(pid as i32);
    }

    /// Lepas penjaga setelah group-nya terbukti mati, supaya `Drop` tidak
    /// mengirim sinyal ke pgid yang mungkin sudah dipakai ulang.
    fn disarm(&mut self) {
        self.pgid = None;
    }

    /// Benar kalau group proses sudah tidak ada. Dipakai sebagai bukti, bukan
    /// sebagai klaim: `stop()` yang mengaku sukses tidak membuktikan apa pun.
    fn mati(&self) -> bool {
        match self.pgid {
            None => true,
            Some(pgid) => matches!(killpg(Pid::from_raw(pgid), None), Err(Errno::ESRCH)),
        }
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        if let Some(pgid) = self.pgid {
            let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
        }
    }
}

struct Profil(PathBuf);

impl Drop for Profil {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn profil_sementara(label: &str) -> Profil {
    let cap = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("ocb-oxi-{label}-{}-{cap}", std::process::id()));
    std::fs::create_dir_all(&path).expect("profil sementara harus bisa dibuat");
    Profil(path)
}

fn port_kosong() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind port sementara");
    let port = listener.local_addr().expect("alamat lokal").port();
    drop(listener);
    port
}

/// Satu permintaan HTTP polos ke port data plane. Port CDP tidak memakai
/// otorisasi, jadi tidak ada token yang perlu dikarang di sini.
fn minta(port: u16, metode: &str, path: &str) -> (u16, String) {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return (0, String::new());
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
    let permintaan = format!(
        "{metode} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(permintaan.as_bytes()).is_err() {
        return (0, String::new());
    }
    let mut respons = String::new();
    let _ = stream.read_to_string(&mut respons);
    let status = respons
        .split_whitespace()
        .nth(1)
        .and_then(|kode| kode.parse().ok())
        .unwrap_or(0);
    let isi = respons
        .split_once("\r\n\r\n")
        .map_or(String::new(), |(_, isi)| isi.to_owned());
    (status, isi)
}

struct Daemon {
    child: Child,
    cdp_port: u16,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // SIGTERM lebih dulu supaya daemon mematikan mesinnya sebagai process
        // group. Membunuh induknya saja meninggalkan oxi yatim.
        let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGTERM);
        let mulai = Instant::now();
        while mulai.elapsed() < Duration::from_secs(20) {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Menyalakan daemon dari config yang menyebut mesin **jenis oxi**.
fn nyalakan_daemon(dir: &Path, biner: &Path) -> Daemon {
    let control_port = port_kosong();
    let cdp_port = port_kosong();
    let kunci = dir.join("store.key");
    let db = dir.join("store.db");
    let token = dir.join("control.token");
    let config = dir.join("browser-daemon.toml");
    std::fs::write(
        &config,
        format!(
            "[daemon]\ncontrol_bind = \"127.0.0.1:{control_port}\"\n\
             control_token = \"{}\"\nstore_db = \"{}\"\nstore_key = \"{}\"\n\
             \n[engines.oxi]\nkind = \"oxi\"\nbinary = \"{}\"\n\
             args = [\"--allow-private-ips\"]\n",
            token.display(),
            db.display(),
            kunci.display(),
            biner.display()
        ),
    )
    .expect("menulis config uji");

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
            "--config",
            config.to_str().expect("path utf8"),
            "--bind",
            &format!("127.0.0.1:{cdp_port}"),
            "--user-data-dir",
            dir.join("profil").to_str().expect("path utf8"),
            "--ready-timeout-ms",
            "60000",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("menjalankan browserd");

    Daemon { child, cdp_port }
}

impl Daemon {
    /// Menunggu port data plane menjawab, lalu mengembalikan isi
    /// `/json/version`. Gagal boot dilaporkan dengan stderr daemon, bukan dengan
    /// "timeout" kosong.
    fn tunggu_siap(&mut self) -> String {
        let mulai = Instant::now();
        while mulai.elapsed() < BATAS_BOOT {
            let (status, isi) = minta(self.cdp_port, "GET", "/json/version");
            if status == 200 && !isi.is_empty() {
                return isi;
            }
            if let Ok(Some(kode)) = self.child.try_wait() {
                let mut log = String::new();
                if let Some(mut err) = self.child.stderr.take() {
                    let _ = err.read_to_string(&mut log);
                }
                panic!("daemon mati dengan status {kode} sebelum siap; stderr:\n{log}");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        panic!("daemon belum siap dalam {BATAS_BOOT:?}");
    }
}

async fn sajikan_fixture() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/halaman.html",
        get(|| async {
            Html(
                "<!doctype html><html><head><title>Halaman Oxi</title></head>\
                 <body><h1 id=\"judul\">Oxi lewat adapter kita</h1>\
                 <p id=\"isi\">empat puluh dua</p></body></html>",
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("port lokal");
    let alamat = listener.local_addr().expect("alamat lokal");
    let tugas = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{alamat}"), tugas)
}

async fn evaluasi(sesi: &mut Sesi, sesi_id: &str, ekspresi: &str) -> Value {
    let hasil = sesi
        .panggil(
            "Runtime.evaluate",
            json!({ "expression": ekspresi, "returnByValue": true, "awaitPromise": false }),
            Some(sesi_id),
        )
        .await
        .expect("Runtime.evaluate harus dijawab");
    if let Some(pesan) = hasil["exceptionDetails"]["exception"]["description"].as_str() {
        panic!("skrip halaman melempar: {pesan}");
    }
    hasil["result"]["value"].clone()
}

/// Jalur operator: daemon menerima mesin jenis oxi dan melayaninya.
#[tokio::test(flavor = "multi_thread")]
async fn daemon_melayani_mesin_oxi_dari_config() {
    let Some(biner) = discover_oxi() else {
        eprintln!("dilewati: biner oxibrowser tidak ditemukan (set OXI_BIN)");
        return;
    };
    let _kunci = common::kunci_live("oxi_daemon");

    let profil = profil_sementara("daemon");
    let mut daemon = nyalakan_daemon(&profil.0, &biner);
    let versi = daemon.tunggu_siap();

    let nilai: Value = serde_json::from_str(&versi).expect("version harus JSON");
    let browser = nilai["browser"]
        .as_str()
        .or_else(|| nilai["Browser"].as_str())
        .expect("harus melaporkan nama browser")
        .to_owned();
    assert!(
        browser.to_ascii_lowercase().contains("oxi"),
        "daemon harus meneruskan fakta mesin oxi, dapat: {browser}"
    );

    // `/json/list` memaksa jalur bangun-tab lewat adapter; kalau adapter salah
    // membaca discovery, permintaan ini yang gagal lebih dulu.
    let (status, isi) = minta(daemon.cdp_port, "GET", "/json/list");
    assert_eq!(status, 200, "body: {isi}");
    let target: Value = serde_json::from_str(&isi).expect("list harus JSON");
    assert!(
        target.as_array().is_some_and(|daftar| !daftar.is_empty()),
        "mesin oxi harus melaporkan minimal satu target: {isi}"
    );
}

/// Jalur adapter: mesin oxi dipakai untuk kerja nyata, lalu dimatikan bersih.
#[tokio::test(flavor = "multi_thread")]
async fn oxi_bicara_cdp_ke_halaman_nyata_lalu_mati_bersih() {
    let Some(biner) = discover_oxi() else {
        eprintln!("dilewati: biner oxibrowser tidak ditemukan (set OXI_BIN)");
        return;
    };
    let _kunci = common::kunci_live("oxi_live");

    let profil = profil_sementara("cdp");
    // Fixture disajikan di loopback, dan oxi menolak alamat privat secara
    // bawaan. Flag ini datang dari `extra_args`, bukan dari adapter: kalau
    // adapter sendiri yang mematikannya, filter SSRF produksi akan hilang
    // tanpa ada yang menyadarinya.
    let spec =
        LaunchSpec::new(biner, profil.0.clone()).with_args(["--allow-private-ips".to_owned()]);
    let mesin = OxiEngine::launch(spec, BATAS_BOOT.as_millis() as u64).expect("oxi harus siap");
    let mut reaper = Reaper::new();
    reaper.arm(mesin.process_id().expect("mesin punya pid"));

    assert_eq!(mesin.kind(), "oxi");
    assert_ne!(
        mesin.info().devtools_port,
        9222,
        "port mesin tidak boleh port produksi"
    );

    let (basis, tugas) = sajikan_fixture().await;
    let mut sesi = Sesi::sambung(mesin.web_socket_url())
        .await
        .expect("websocket oxi harus terbuka");

    let hasil = sesi
        .panggil("Target.createTarget", json!({ "url": "about:blank" }), None)
        .await
        .expect("tab kosong harus bisa dibuat");
    let target = hasil["targetId"]
        .as_str()
        .expect("mesin harus memberi targetId")
        .to_owned();
    let sesi_id = sesi.tanam(&target).await.expect("suntikan runtime");

    sesi.panggil(
        "Page.navigate",
        json!({ "url": format!("{basis}/halaman.html") }),
        Some(&sesi_id),
    )
    .await
    .expect("navigasi harus diterima");

    let batas = Instant::now() + Duration::from_secs(20);
    loop {
        let siap = evaluasi(
            &mut sesi,
            &sesi_id,
            "document.readyState === 'complete' && document.body ? 1 : 0",
        )
        .await;
        if siap.as_i64() == Some(1) {
            break;
        }
        assert!(Instant::now() < batas, "halaman tidak selesai dimuat");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert_eq!(
        evaluasi(&mut sesi, &sesi_id, "6 * 7").await,
        json!(42),
        "JavaScript harus berjalan di mesin oxi"
    );
    assert_eq!(
        evaluasi(&mut sesi, &sesi_id, "document.title").await,
        json!("Halaman Oxi"),
        "judul halaman dibaca dari DOM yang benar"
    );

    let tangkapan = sesi
        .panggil(
            "Page.captureScreenshot",
            json!({ "format": "png" }),
            Some(&sesi_id),
        )
        .await
        .expect("tangkapan layar harus dijawab");
    let data = tangkapan["data"]
        .as_str()
        .expect("harus mengembalikan data base64");
    assert!(
        data.starts_with("iVBORw0KGgo"),
        "PNG base64 diawali tanda tangan yang dikenal, dapat: {}",
        &data[..data.len().min(16)]
    );

    let pid = mesin.process_id().expect("pid");
    mesin.stop().expect("mesin harus berhenti");
    assert!(!mesin.is_running(), "sesudah stop mesin tidak hidup");
    assert!(
        reaper.mati(),
        "group proses {pid} masih ada sesudah stop: anak mesin ikut mati atau tidak"
    );
    reaper.disarm();
    tugas.abort();
}
