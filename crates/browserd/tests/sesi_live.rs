//! Bukti live untuk sesi: handoff yang dijawab melanjutkan sesi yang sama,
//! handoff yang ditinggal tidak menggantung mesin, dan batas tab bisa diganti
//! lewat `SIGHUP` tanpa restart.
//!
//! Yang diuji di sini bukan aturan internal (itu sudah diuji unit dengan jam
//! yang disuntik), tapi apa yang hanya bisa dibuktikan oleh proses nyata:
//!
//! 1. Handoff yang muncul di port kontrol membawa URL dan detail yang bisa
//!    langsung dipakai menjawab.
//! 2. Jawaban owner **melanjutkan sesi yang sama**: target tidak diganti, dan
//!    konteks JavaScript yang ditulis sebelum menjawab masih ada sesudahnya.
//!    Kalau daemon diam-diam membuat tab baru, keadaan halaman hilang dan
//!    jawaban owner akan mengetik ke halaman yang salah.
//! 3. Handoff yang berakhir (dibatalkan) melepas tab dan mengembalikan mesin
//!    ke keadaan siap: tab baru boleh dibuat lagi.
//! 4. `SIGHUP` mengganti `max_tabs` **tanpa restart**: pid tetap, dan batas
//!    yang lebih ketat langsung berlaku. Config yang rusak saat reload tidak
//!    melempar batas ke tanpa-batas: nilai lama dipertahankan.
//!
//! Batas TTL handoff (900 detik) sengaja TIDAK ditunggu di sini: menunggu 15
//! menit di dalam suite bukan uji, itu tidur. Kedaluwarsa diuji dengan jam
//! semu di `crates/control/src/registry.rs`, dan di sini hanya batas atasnya
//! yang diperiksa (`expires_at - created_at`).
//!
//! Kalau `chrome-headless-shell` tidak ada di host ini, uji **dilewati** dengan
//! pesan yang menyebut apa yang kurang. Dilewati bukan berarti lulus: pesannya
//! harus terbaca dengan `--nocapture`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Batas kesabaran seluruh uji. Mesin nyata butuh waktu boot, dan host yang
/// sedang sibuk bisa jauh lebih lambat dari dugaan.
const BATAS: Duration = Duration::from_secs(120);

/// Batas satu balasan CDP. Terpisah dari [`BATAS`] karena artinya lain: `BATAS`
/// menunggu keadaan yang berubah (handoff muncul, daemon siap), sedangkan ini
/// menunggu satu perintah dijawab mesin yang mungkin masih lahir.
const BATAS_PERINTAH: Duration = Duration::from_secs(60);

/// TTL handoff yang dijanjikan `control::registry::HOLD_SECS`.
///
/// Diperiksa sebagai selisih waktu, bukan sebagai konstanta yang diimpor:
/// yang perlu dibuktikan adalah apa yang benar-benar dikirim ke owner, bukan
/// apa yang tertulis di kode.
const TTL_HANDOFF_SECS: u64 = 900;

fn biner_mesin() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os("OCB_ENGINE_BINARY") {
        let path = PathBuf::from(raw);
        if path.is_file() {
            return Some(path);
        }
    }
    engine::discover_shell().filter(|path| path.is_file())
}

fn port_kosong() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind port sementara");
    let port = listener.local_addr().expect("alamat lokal").port();
    drop(listener);
    port
}

fn direktori_uji(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ocb-sesi-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("membuat direktori uji");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

/// Fixture halaman tantangan: 403 dengan tanda Cloudflare yang dikenali probe.
///
/// `Content-Length` wajib benar; tanpa itu Chrome menunggu koneksi ditutup dan
/// pengamat baru melihat dokumen setelah batas waktu, yang membuat uji ini
/// gagal atau lulus karena alasan yang salah.
fn server_tantangan() -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
    let port = listener.local_addr().expect("alamat fixture").port();
    let handle = std::thread::spawn(move || {
        for koneksi in listener.incoming() {
            let Ok(mut stream) = koneksi else { break };
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf);
            let body = "<html><head><title>Just a moment</title></head><body>\
                <div id=\"challenge-form\"></div>\
                <div class=\"cf-turnstile\"></div>\
                <p>Verify you are human</p></body></html>";
            let respons = format!(
                "HTTP/1.1 403 Forbidden\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(respons.as_bytes());
            let _ = stream.flush();
        }
    });
    (port, handle)
}

fn minta(port: u16, metode: &str, path: &str, token: Option<&str>) -> (u16, String) {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return (0, String::new());
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let otorisasi = token.map_or(String::new(), |token| {
        format!("Authorization: Bearer {token}\r\n")
    });
    let permintaan = format!(
        "{metode} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{otorisasi}\
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

fn kirim_json(port: u16, path: &str, token: &str, body: &str) -> (u16, String) {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return (0, String::new());
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let permintaan = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
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

fn tunggu<F: FnMut() -> bool>(batas: Duration, mut cek: F) -> bool {
    let mulai = Instant::now();
    while mulai.elapsed() < batas {
        if cek() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

fn buang(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn token_dari(path: &Path) -> String {
    std::fs::read_to_string(path)
        .map(|isi| isi.trim().to_owned())
        .unwrap_or_default()
}

/// Satu daemon nyata di port sementara, siap menerima perintah.
struct Daemon {
    child: Child,
    cdp_port: u16,
    control_port: u16,
    token: String,
    config: PathBuf,
}

impl Daemon {
    /// Menulis config, membuat kunci store, lalu menyalakan `browserd serve`.
    ///
    /// `max_tabs` ikut ditulis supaya uji reload punya angka awal yang jelas;
    /// `idle_reap_secs` sengaja tidak ditulis (reap mati) agar mesin tidak
    /// dimatikan di tengah uji karena tidak ada yang menyentuhnya.
    fn nyalakan(dir: &Path, biner: &Path, max_tabs: u32) -> Self {
        let cdp_port = port_kosong();
        let control_port = port_kosong();
        let kunci = dir.join("store.key");
        let db = dir.join("store.db");
        let token_path = dir.join("control.token");
        let config = dir.join("browser-daemon.toml");
        std::fs::write(
            &config,
            config_isi(control_port, &token_path, &db, &kunci, biner, max_tabs),
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
                "--bind",
                &format!("127.0.0.1:{cdp_port}"),
                "--config",
                config.to_str().expect("path utf8"),
                "--engine-binary",
                biner.to_str().expect("path utf8"),
                "--user-data-dir",
                dir.join("profil").to_str().expect("path utf8"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("menjalankan browserd serve");

        let mut daemon = Self {
            child,
            cdp_port,
            control_port,
            token: String::new(),
            config,
        };
        let siap = tunggu(BATAS, || {
            if let Ok(Some(status)) = daemon.child.try_wait() {
                panic!("daemon keluar lebih awal: {status}");
            }
            minta(cdp_port, "GET", "/healthz", None).0 == 200
                && minta(
                    control_port,
                    "GET",
                    "/api/v1/health",
                    Some(&token_dari(&token_path)),
                )
                .0 == 200
        });
        assert!(siap, "daemon tidak siap dalam {BATAS:?}");
        daemon.token = token_dari(&token_path);
        assert!(!daemon.token.is_empty(), "token kontrol kosong");
        daemon
    }

    /// Menulis ulang config lalu mengirim `SIGHUP`, persis seperti
    /// `systemctl reload`: prosesnya tetap, yang dibaca ulang hanya batasnya.
    fn tulis_dan_reload(&self, isi: &str) {
        std::fs::write(&self.config, isi).expect("menulis ulang config uji");
        let pid = self.child.id().to_string();
        let status = Command::new("/bin/kill")
            .args(["-HUP", &pid])
            .status()
            .expect("mengirim SIGHUP");
        assert!(status.success(), "SIGHUP ditolak: {status}");
    }

    fn masih_hidup(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn pending(&self) -> Value {
        let (status, isi) = minta(
            self.control_port,
            "GET",
            "/api/v1/handoff/pending",
            Some(&self.token),
        );
        assert_eq!(status, 200, "pending harus 200: {isi}");
        serde_json::from_str(&isi).expect("pending harus JSON")
    }

    fn daftar_tab(&self) -> Value {
        let (status, isi) = minta(self.cdp_port, "GET", "/json/list", None);
        assert_eq!(status, 200, "/json/list harus 200: {isi}");
        serde_json::from_str(&isi).expect("/json/list harus JSON")
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        buang(&mut self.child);
    }
}

fn config_isi(
    control_port: u16,
    token_path: &Path,
    db: &Path,
    kunci: &Path,
    biner: &Path,
    max_tabs: u32,
) -> String {
    format!(
        "[daemon]\ncontrol_bind = \"127.0.0.1:{control_port}\"\n\
         control_token = \"{}\"\nstore_db = \"{}\"\nstore_key = \"{}\"\n\
         \n[captcha]\nhandoff_per_host_hour = 5\nhandoff_total_day = 20\ncooldown_secs = 600\n\
         \n[engines.chrome_shell]\nbinary = \"{}\"\nmax_tabs = {max_tabs}\n",
        token_path.display(),
        db.display(),
        kunci.display(),
        biner.display()
    )
}

/// Membuka soket browser lewat daemon, membuat target, menavigasinya ke
/// fixture tantangan, lalu menulis penanda di konteks halaman.
///
/// Penanda ditulis **sesudah** navigasi: dokumen baru menghapus semua variabel
/// global, jadi penanda yang ditulis sebelum navigasi bukan bukti apa pun.
/// Yang dicari sesudah jawaban owner adalah penanda ini masih ada, dan itu
/// berarti tab yang sama masih memuat dokumen yang sama.
fn buka_dan_navigasi(cdp_port: u16, fixture_port: u16) -> Option<(String, String)> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime tokio");

    runtime.block_on(async move {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;

        let url = format!("ws://127.0.0.1:{cdp_port}/devtools/browser/uji");
        let (ws, _) = connect_async(&url).await.ok()?;
        let (mut tulis, mut baca) = ws.split();

        let dibuat = panggil(
            &mut tulis,
            &mut baca,
            1,
            "Target.createTarget",
            json!({"url": "about:blank"}),
            None,
        )
        .await?;
        let target_id = dibuat["result"]["targetId"].as_str()?.to_owned();

        let sesi = panggil(
            &mut tulis,
            &mut baca,
            2,
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
            None,
        )
        .await?;
        let session_id = sesi["result"]["sessionId"].as_str()?.to_owned();

        // Tanpa Network dan Page aktif, pengamat tidak akan pernah melihat
        // respons dokumen, dan uji ini akan gagal karena alasan yang salah.
        panggil(
            &mut tulis,
            &mut baca,
            3,
            "Network.enable",
            json!({}),
            Some(&session_id),
        )
        .await?;
        panggil(
            &mut tulis,
            &mut baca,
            4,
            "Page.enable",
            json!({}),
            Some(&session_id),
        )
        .await?;
        panggil(
            &mut tulis,
            &mut baca,
            5,
            "Page.navigate",
            json!({"url": format!("http://127.0.0.1:{fixture_port}/")}),
            Some(&session_id),
        )
        .await?;

        // Dokumen harus selesai dimuat sebelum penanda ditulis; kalau tidak,
        // navigasi yang datang belakangan akan menghapusnya dan uji ini gagal
        // seolah-olah daemon mengganti tab.
        let siap = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let nilai = panggil(
                    &mut tulis,
                    &mut baca,
                    6,
                    "Runtime.evaluate",
                    json!({"expression": "document.readyState", "returnByValue": true}),
                    Some(&session_id),
                )
                .await;
                if nilai
                    .as_ref()
                    .and_then(|n| n["result"]["result"]["value"].as_str())
                    == Some("complete")
                {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        })
        .await
        .ok()?;
        if !siap {
            return None;
        }

        panggil(
            &mut tulis,
            &mut baca,
            7,
            "Runtime.evaluate",
            json!({"expression": "window.__sesi = 'hidup'", "returnByValue": true}),
            Some(&session_id),
        )
        .await?;

        let _ = tulis.close().await;
        Some((target_id, session_id))
    })
}

/// Membaca satu ekspresi JavaScript di target yang **sudah ada**.
///
/// Lampiran baru (session id baru) sengaja dipakai: yang dibuktikan bukan
/// "sesi CDP yang sama masih terbuka" (soket uji sudah ditutup), tapi "tab dan
/// dokumen yang sama masih hidup", dan itu justru harus terbaca dari lampiran
/// baru.
fn eval_di(cdp_port: u16, target_id: &str, ekspresi: &str) -> Option<String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime tokio");

    runtime.block_on(async move {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;

        let url = format!("ws://127.0.0.1:{cdp_port}/devtools/browser/uji");
        let (ws, _) = connect_async(&url).await.ok()?;
        let (mut tulis, mut baca) = ws.split();

        let sesi = panggil(
            &mut tulis,
            &mut baca,
            1,
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
            None,
        )
        .await?;
        let session_id = sesi["result"]["sessionId"].as_str()?.to_owned();

        let nilai = panggil(
            &mut tulis,
            &mut baca,
            2,
            "Runtime.evaluate",
            json!({"expression": ekspresi, "returnByValue": true}),
            Some(&session_id),
        )
        .await?;
        let _ = tulis.close().await;
        nilai["result"]["result"]["value"]
            .as_str()
            .map(str::to_owned)
    })
}

#[allow(clippy::too_many_arguments)]
async fn panggil(
    tulis: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::Message,
    >,
    baca: &mut futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    id: i64,
    method: &str,
    params: Value,
    session: Option<&str>,
) -> Option<Value> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let mut pesan = json!({"id": id, "method": method, "params": params});
    if let Some(session) = session {
        pesan["sessionId"] = json!(session);
    }
    tulis
        .send(Message::Text(pesan.to_string().into()))
        .await
        .ok()?;
    loop {
        // Batas per perintah sengaja longgar: `Target.createTarget` ikut
        // menunggu proses mesin lahir, dan di host yang sedang menjalankan
        // beberapa Chromium sekaligus itu bisa jauh lebih lama daripada satu
        // balasan protokol biasa. Batas yang terlalu ketat di sini bukan
        // menemukan bug, cuma memindahkan beban host ke hasil uji.
        let pesan = match tokio::time::timeout(BATAS_PERINTAH, baca.next()).await {
            Ok(Some(Ok(pesan))) => pesan,
            Ok(Some(Err(galat))) => {
                eprintln!("CDP {method} (id {id}): soket gagal: {galat}");
                return None;
            }
            Ok(None) => {
                eprintln!("CDP {method} (id {id}): soket ditutup sebelum dijawab");
                return None;
            }
            Err(_) => {
                eprintln!("CDP {method} (id {id}): tidak dijawab dalam {BATAS_PERINTAH:?}");
                return None;
            }
        };
        let Message::Text(teks) = pesan else { continue };
        let nilai: Value = serde_json::from_str(teks.as_str()).ok()?;
        if nilai.get("id") == Some(&json!(id)) {
            return Some(nilai);
        }
    }
}

fn lewati(tag: &str) -> bool {
    eprintln!(
        "SKIP sesi_live::{tag}: chrome-headless-shell tidak ditemukan. Set \
         OCB_ENGINE_BINARY atau pasang di ~/.cache/ms-playwright."
    );
    true
}

mod common;

#[test]
fn handoff_dijawab_melanjutkan_tab_yang_sama() {
    let Some(biner) = biner_mesin() else {
        lewati("handoff_dijawab_melanjutkan_tab_yang_sama");
        return;
    };
    // Uji live diserialkan supaya tidak berebut CPU dengan uji live lain yang
    // berjalan paralel. Alasan lengkapnya di `tests/common/mod.rs`.
    let _kunci = common::kunci_live("sesi_live::jawab");

    let dir = direktori_uji("jawab");
    let (fixture_port, _fixture) = server_tantangan();
    let daemon = Daemon::nyalakan(&dir, &biner, 4);

    let Some((target_id, session_id)) = buka_dan_navigasi(daemon.cdp_port, fixture_port) else {
        panic!("navigasi CDP lewat daemon gagal");
    };
    eprintln!("target = {target_id} sesi = {session_id}");

    let muncul = tunggu(BATAS, || {
        let (status, isi) = minta(
            daemon.control_port,
            "GET",
            "/api/v1/handoff/pending",
            Some(&daemon.token),
        );
        status == 200 && isi.contains("\"id\"")
    });
    assert!(muncul, "handoff tidak muncul dalam {BATAS:?}");

    let daftar = daemon.pending();
    let handoff = &daftar["handoffs"][0];
    let id = handoff["id"].as_str().expect("id handoff").to_owned();
    let url = handoff["url"].as_str().unwrap_or_default().to_owned();
    let dibuat = handoff["created_at"].as_u64().expect("created_at");
    let kedaluwarsa = handoff["expires_at"].as_u64().expect("expires_at");

    // Konteks siap jawab: owner harus bisa membuka halaman dan tahu apa yang
    // sedang ditunggu tanpa menggali log.
    assert!(
        url.contains(&format!("127.0.0.1:{fixture_port}")),
        "URL handoff harus halaman fixture, dapat: {url}"
    );
    assert_eq!(handoff["kind"], "captcha", "jenis handoff harus captcha");
    assert!(
        handoff["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "detail handoff kosong: {handoff}"
    );
    assert_eq!(
        kedaluwarsa - dibuat,
        TTL_HANDOFF_SECS,
        "TTL handoff harus {TTL_HANDOFF_SECS} detik, dapat {}",
        kedaluwarsa - dibuat
    );
    assert_eq!(
        handoff["session_id"].as_str(),
        Some(session_id.as_str()),
        "handoff harus menunjuk sesi yang sedang memegang tab"
    );

    // Empat field konteks (AC-014). Diperiksa satu per satu, bukan "ada
    // sesuatu": handoff yang datang dengan judul kosong atau selector karangan
    // tetap membuat owner menggali log, dan itu yang mau dihindari.
    assert_eq!(
        handoff["title"].as_str(),
        Some("Just a moment"),
        "judul handoff harus judul dokumen halaman tantangan: {handoff}"
    );
    let selector = handoff["expected_selector"]
        .as_str()
        .expect("selector titik henti wajib ada");
    assert!(
        selector.contains("#challenge-form"),
        "selector harus menunjuk form tantangan yang dilihat probe: {selector}"
    );
    let layar = handoff["screenshot"]
        .as_str()
        .expect("layar handoff wajib ada");
    let path_layar = std::path::Path::new(layar);
    assert!(
        path_layar.exists(),
        "berkas layar yang dirujuk handoff tidak ada: {layar}"
    );
    let bytes = std::fs::read(path_layar).expect("membaca layar");
    assert!(
        bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "layar harus PNG asli, bukan base64 yang tersimpan mentah ({} byte)",
        bytes.len()
    );
    assert!(
        layar.contains("handoff"),
        "layar harus disimpan di direktori handoff, dapat: {layar}"
    );

    let (status, isi) = kirim_json(
        daemon.control_port,
        &format!("/api/v1/handoff/{id}/answer"),
        &daemon.token,
        &json!({"type": "keys", "value": "Enter"}).to_string(),
    );
    assert_eq!(status, 200, "jawaban ditolak: {isi}");

    let selesai = tunggu(BATAS, || {
        let (status, isi) = minta(
            daemon.control_port,
            "GET",
            "/api/v1/handoff/pending",
            Some(&daemon.token),
        );
        status == 200 && !isi.contains(&id)
    });
    assert!(selesai, "handoff tidak selesai dalam {BATAS:?}");

    // Tab yang sama harus masih ada, dan konteksnya harus utuh: kalau daemon
    // mengganti tab atau memuat ulang halaman, variabel ini hilang.
    let tab = daemon.daftar_tab();
    assert!(
        tab.to_string().contains(&target_id),
        "target {target_id} hilang sesudah jawaban: {tab}"
    );
    let penanda = eval_di(daemon.cdp_port, &target_id, "window.__sesi");
    assert_eq!(
        penanda.as_deref(),
        Some("hidup"),
        "konteks halaman hilang sesudah handoff dijawab: {penanda:?}"
    );
}

#[test]
fn handoff_dibatalkan_melepas_tab_dan_mesin_siap_lagi() {
    let Some(biner) = biner_mesin() else {
        lewati("handoff_dibatalkan_melepas_tab_dan_mesin_siap_lagi");
        return;
    };
    let _kunci = common::kunci_live("sesi_live::batal");

    let dir = direktori_uji("batal");
    let (fixture_port, _fixture) = server_tantangan();
    let daemon = Daemon::nyalakan(&dir, &biner, 4);

    let Some((target_id, _session_id)) = buka_dan_navigasi(daemon.cdp_port, fixture_port) else {
        panic!("navigasi CDP lewat daemon gagal");
    };

    let muncul = tunggu(BATAS, || {
        daemon.pending()["handoffs"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
    });
    assert!(muncul, "handoff tidak muncul dalam {BATAS:?}");
    let daftar = daemon.pending();
    let id = daftar["handoffs"][0]["id"]
        .as_str()
        .expect("id handoff")
        .to_owned();

    // "Abort" adalah jalur yang sama dengan handoff kedaluwarsa: state
    // dibersihkan dan tab dilepas. Yang tidak bisa ditunggu di sini adalah
    // timer 900 detiknya; itu diuji dengan jam semu di `control::registry`.
    let (status, isi) = kirim_json(
        daemon.control_port,
        &format!("/api/v1/handoff/{id}/answer"),
        &daemon.token,
        &json!({"type": "abort"}).to_string(),
    );
    assert_eq!(status, 200, "abort ditolak: {isi}");

    let bersih = tunggu(BATAS, || {
        daemon.pending()["handoffs"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty)
    });
    assert!(
        bersih,
        "handoff masih menunggu sesudah abort: {:?}",
        daemon.pending()
    );

    // Mesin harus kembali siap: tab baru boleh dibuat, dan yang lebih penting,
    // permintaannya benar-benar diteruskan ke mesin (bukan dijawab daemon).
    let (status, isi) = minta(daemon.cdp_port, "PUT", "/json/new?about:blank", None);
    assert_eq!(status, 200, "mesin tidak siap lagi sesudah abort: {isi}");
    let tab = daemon.daftar_tab();
    assert!(
        tab.as_array().is_some_and(|a| a.len() >= 2),
        "tab baru harus terdaftar: {tab}"
    );
    eprintln!("tab sesudah abort: {tab} (target lama {target_id})");
}

#[test]
fn sighup_mengubah_batas_tab_tanpa_restart() {
    let Some(biner) = biner_mesin() else {
        lewati("sighup_mengubah_batas_tab_tanpa_restart");
        return;
    };
    let _kunci = common::kunci_live("sesi_live::sighup");

    let dir = direktori_uji("sighup");
    let mut daemon = Daemon::nyalakan(&dir, &biner, 3);
    let pid = daemon.child.id();

    let (status, isi) = minta(daemon.cdp_port, "PUT", "/json/new?about:blank", None);
    assert_eq!(status, 200, "tab pertama harus boleh: {isi}");

    // Batas diperketat: 1 tab, dan satu tab sudah ada. Kalau SIGHUP tidak
    // berlaku, permintaan ini akan tetap diteruskan ke mesin.
    let ketat = config_isi(
        daemon.control_port,
        &dir.join("control.token"),
        &dir.join("store.db"),
        &dir.join("store.key"),
        &biner,
        1,
    );
    daemon.tulis_dan_reload(&ketat);
    let ditolak = tunggu(Duration::from_secs(20), || {
        minta(daemon.cdp_port, "PUT", "/json/new?about:blank", None).0 == 429
    });
    let (status, isi) = minta(daemon.cdp_port, "PUT", "/json/new?about:blank", None);
    assert!(
        ditolak,
        "batas 1 tab tidak berlaku sesudah SIGHUP (status {status}): {isi}"
    );
    assert!(
        isi.contains("max_tabs"),
        "tolakan harus menyebut max_tabs supaya bisa ditindaklanjuti: {isi}"
    );

    // Dilonggarkan lagi: bukti bahwa reload dua arah, bukan efek samping satu
    // kali baca config.
    let longgar = config_isi(
        daemon.control_port,
        &dir.join("control.token"),
        &dir.join("store.db"),
        &dir.join("store.key"),
        &biner,
        4,
    );
    daemon.tulis_dan_reload(&longgar);
    let diterima = tunggu(Duration::from_secs(20), || {
        minta(daemon.cdp_port, "PUT", "/json/new?about:blank", None).0 == 200
    });
    assert!(diterima, "batas 4 tab tidak berlaku sesudah SIGHUP kedua");

    assert_eq!(daemon.child.id(), pid, "pid tidak boleh berubah");
    assert!(daemon.masih_hidup(), "daemon mati sesudah SIGHUP");
    assert_eq!(
        minta(daemon.cdp_port, "GET", "/healthz", None).0,
        200,
        "daemon harus tetap melayani sesudah reload"
    );
}

#[test]
fn config_rusak_saat_reload_mempertahankan_batas_lama() {
    let Some(biner) = biner_mesin() else {
        lewati("config_rusak_saat_reload_mempertahankan_batas_lama");
        return;
    };
    let _kunci = common::kunci_live("sesi_live::rusak");

    let dir = direktori_uji("rusak");
    let mut daemon = Daemon::nyalakan(&dir, &biner, 1);

    let (status, isi) = minta(daemon.cdp_port, "PUT", "/json/new?about:blank", None);
    assert_eq!(status, 200, "tab pertama harus boleh: {isi}");
    let (status, _) = minta(daemon.cdp_port, "PUT", "/json/new?about:blank", None);
    assert_eq!(status, 429, "batas 1 tab harus sudah berlaku");

    // Config yang tidak bisa diparse TIDAK boleh diartikan "tanpa batas":
    // itu akan membuat satu salah tulis melempar pagar tab ke laut.
    daemon.tulis_dan_reload("ini bukan toml {{{");
    std::thread::sleep(Duration::from_millis(700));
    let (status, isi) = minta(daemon.cdp_port, "PUT", "/json/new?about:blank", None);
    assert_eq!(
        status, 429,
        "batas lama harus dipertahankan saat config rusak, dapat {status}: {isi}"
    );
    assert!(daemon.masih_hidup(), "daemon mati karena config rusak");
    assert_eq!(
        minta(daemon.cdp_port, "GET", "/healthz", None).0,
        200,
        "daemon harus tetap melayani"
    );
}
