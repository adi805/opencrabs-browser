//! Bukti M3 dengan mesin nyata: halaman tantangan → handoff → jawaban → solved.
//!
//! Yang diuji di sini bukan unit aturan, tapi rantainya: pengamat membaca
//! respons dokumen dari mesin sungguhan, probe DOM dijalankan di halaman yang
//! sungguh memuat tanda tantangan, handoff muncul di port kontrol, jawaban
//! owner diterapkan lewat CDP, dan `captcha_events_total` naik.
//!
//! Kalau `chrome-headless-shell` tidak ada di host ini, uji dilewati dengan
//! pesan yang menyebut apa yang kurang. Dilewati bukan berarti lulus: pesannya
//! harus terbaca di `--nocapture`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const BATAS: Duration = Duration::from_secs(60);

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

fn direktori_uji() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ocb-m3-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("membuat direktori uji");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

/// Server fixture: satu halaman tantangan, status 403.
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

fn minta(
    port: u16,
    metode: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> (u16, String) {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return (0, String::new());
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let body = body.unwrap_or("");
    let otorisasi = token.map_or(String::new(), |token| {
        format!("Authorization: Bearer {token}\r\n")
    });
    let permintaan = format!(
        "{metode} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{otorisasi}\
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

#[test]
fn alur_tantangan_sampai_handoff_dijawab() {
    let Some(biner) = biner_mesin() else {
        eprintln!(
            "SKIP m3_proof: chrome-headless-shell tidak ditemukan. Set OCB_ENGINE_BINARY \
             atau pasang di ~/.cache/ms-playwright."
        );
        return;
    };

    let dir = direktori_uji();
    let control_port = port_kosong();
    let cdp_port = port_kosong();
    // Handle thread fixture sengaja tidak di-join: servernya menerima
    // koneksi tanpa batas sampai proses uji keluar, jadi `join` akan
    // menggantung selamanya dan menyamarkan hasil uji sebagai timeout.
    let (fixture_port, _fixture) = server_tantangan();

    let kunci = dir.join("store.key");
    let db = dir.join("store.db");
    let token_path = dir.join("control.token");
    let config_path = dir.join("browser-daemon.toml");
    let profil = dir.join("profil");

    std::fs::write(
        &config_path,
        format!(
            "[daemon]\ncontrol_bind = \"127.0.0.1:{control_port}\"\n\
             control_token = \"{}\"\nstore_db = \"{}\"\nstore_key = \"{}\"\n\
             \n[captcha]\nhandoff_per_host_hour = 5\nhandoff_total_day = 20\ncooldown_secs = 600\n",
            token_path.display(),
            db.display(),
            kunci.display()
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

    let mut daemon = Command::new(env!("CARGO_BIN_EXE_browserd"))
        .args([
            "serve",
            "--bind",
            &format!("127.0.0.1:{cdp_port}"),
            "--config",
            config_path.to_str().expect("path utf8"),
            "--engine-binary",
            biner.to_str().expect("path utf8"),
            "--user-data-dir",
            profil.to_str().expect("path utf8"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("menjalankan browserd serve");

    let siap = tunggu(BATAS, || {
        let (status, _) = minta(cdp_port, "GET", "/healthz", None, None);
        let (kontrol, _) = minta(
            control_port,
            "GET",
            "/api/v1/health",
            Some(&token_dari(&token_path)),
            None,
        );
        status == 200 && kontrol == 200
    });
    assert!(siap, "daemon tidak siap dalam {BATAS:?}");
    let token = token_dari(&token_path);
    assert!(!token.is_empty(), "token kontrol kosong");

    // Navigasi lewat lapisan pass-through, bukan langsung ke mesin: bukti yang
    // dicari adalah rantai daemon, jadi jalur pintas akan menguji hal lain.
    let Some(target_id) = navigasi_ke_tantangan(cdp_port, fixture_port) else {
        buang(&mut daemon);
        panic!("navigasi CDP lewat daemon gagal");
    };
    eprintln!("target = {target_id}");

    let mulai = Instant::now();
    let muncul = tunggu(BATAS, || {
        let (status, isi) = minta(
            control_port,
            "GET",
            "/api/v1/handoff/pending",
            Some(&token),
            None,
        );
        status == 200 && isi.contains("\"id\"")
    });
    assert!(muncul, "handoff tidak muncul dalam {BATAS:?}");

    let (_, isi) = minta(
        control_port,
        "GET",
        "/api/v1/handoff/pending",
        Some(&token),
        None,
    );
    let daftar: Value = serde_json::from_str(&isi).expect("pending harus JSON");
    let handoff = &daftar["handoffs"][0];
    let id = handoff["id"].as_str().expect("id handoff").to_owned();
    let url = handoff["url"].as_str().unwrap_or_default().to_owned();
    assert!(
        url.contains(&format!("127.0.0.1:{fixture_port}")),
        "URL handoff harus halaman fixture, dapat: {url}"
    );
    assert_eq!(handoff["kind"], "captcha");

    let jawaban = json!({"type": "keys", "value": "Enter"}).to_string();
    let (status, isi) = minta(
        control_port,
        "POST",
        &format!("/api/v1/handoff/{id}/answer"),
        Some(&token),
        Some(&jawaban),
    );
    assert_eq!(status, 200, "jawaban ditolak: {isi}");

    let selesai = tunggu(BATAS, || {
        let (status, isi) = minta(
            control_port,
            "GET",
            "/api/v1/handoff/pending",
            Some(&token),
            None,
        );
        status == 200 && !isi.contains(&id)
    });
    let detik = mulai.elapsed().as_secs();

    let (_, metrik) = minta(cdp_port, "GET", "/metrics", None, None);
    eprintln!("waktu total = {detik} detik");
    buang(&mut daemon);

    assert!(selesai, "handoff tidak selesai dalam {BATAS:?}");
    assert!(
        detik < 60,
        "kriteria roadmap: handoff round-trip < 60 detik, dapat {detik}"
    );
    assert!(
        metrik.contains("captcha_events_total{kind=\"challenge\",outcome=\"detected\"}"),
        "metrik deteksi tidak ada di /metrics"
    );
}

/// Membuka soket browser lewat daemon, membuat target, lalu menavigasinya.
fn navigasi_ke_tantangan(cdp_port: u16, fixture_port: u16) -> Option<String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime tokio");

    runtime.block_on(async move {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::Message;

        let url = format!("ws://127.0.0.1:{cdp_port}/devtools/browser/uji");
        let (ws, _) = connect_async(&url).await.ok()?;
        let (mut tulis, mut baca) = ws.split();

        async fn panggil(
            tulis: &mut futures_util::stream::SplitSink<
                tokio_tungstenite::WebSocketStream<
                    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                >,
                Message,
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
            let mut pesan = json!({"id": id, "method": method, "params": params});
            if let Some(session) = session {
                pesan["sessionId"] = json!(session);
            }
            tulis
                .send(Message::Text(pesan.to_string().into()))
                .await
                .ok()?;
            loop {
                let pesan = tokio::time::timeout(Duration::from_secs(10), baca.next())
                    .await
                    .ok()??
                    .ok()?;
                let Message::Text(teks) = pesan else { continue };
                let nilai: Value = serde_json::from_str(teks.as_str()).ok()?;
                if nilai.get("id") == Some(&json!(id)) {
                    return Some(nilai);
                }
            }
        }

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
        // respons dokumen, dan uji ini akan lulus dengan alasan yang salah.
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

        // Beri waktu pengamat membaca respons dokumen sebelum soket ditutup.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = tulis.close().await;
        Some(target_id)
    })
}
