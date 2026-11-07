//! Mesin native menjawab protokol CDP tanpa satu pun biner Chrome.
//!
//! Yang diuji di sini adalah **server sungguhan**: `NativeEngine` menyalakan
//! axum di port yang benar-benar dibuka, dan permintaan di bawah dikirim lewat
//! TCP serta websocket, bukan lewat pemanggilan fungsi internal. Jadi kalau
//! rute hilang atau frame tidak terbingkai, tes ini gagal.
//!
//! Bukti AC-013: `/json/version` menyebut `RustNativeEngine`, dan
//! `LaunchSpec.binary` sengaja menunjuk path yang tidak ada. Kalau mesin ini
//! diam-diam mengeksekusi biner, tes pertama tidak akan pernah hidup.

use std::time::Duration;

use cdp::native_wire::{KIND, NativeEngine, PRODUK, USER_AGENT};
use engine::{Engine, LaunchSpec};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as TsMessage;

/// Path yang dijamin tidak ada di host mana pun. Dipakai untuk membuktikan
/// bahwa mesin native tidak butuh biner.
const BINER_HANTU: &str = "/nonexistent/oc-browser/tidak-ada-chrome";

fn spec() -> LaunchSpec {
    LaunchSpec::new(BINER_HANTU, "/tmp/oc-browser-native-wire")
}

fn nyalakan() -> NativeEngine {
    NativeEngine::start(spec()).expect("mesin native menyala")
}

/// Permintaan HTTP mentah. `Connection: close` supaya axum menutup soket dan
/// pembacaan sampai EOF tidak menggantung.
async fn minta(port: u16, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("menyambung ke mesin native");
    let permintaan =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(permintaan.as_bytes())
        .await
        .expect("mengirim permintaan");

    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut buf))
        .await
        .expect("waktu habis membaca jawaban")
        .expect("membaca jawaban");

    let teks = String::from_utf8_lossy(&buf).into_owned();
    let (kepala, badan) = teks.split_once("\r\n\r\n").expect("jawaban HTTP utuh");
    let status = kepala
        .split_whitespace()
        .nth(1)
        .and_then(|kode| kode.parse().ok())
        .expect("status");
    (status, badan.to_owned())
}

type Klien = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

async fn sambung(port: u16, path: &str) -> Klien {
    let (soket, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}{path}"))
        .await
        .expect("menyambung ke soket mesin native");
    soket
}

/// Mengirim satu perintah dan menunggu balasan dengan `id` yang sama.
async fn minta_cdp(soket: &mut Klien, id: i64, metode: &str, params: Value) -> Value {
    let pesan = json!({"id": id, "method": metode, "params": params});
    soket
        .send(TsMessage::Text(pesan.to_string().into()))
        .await
        .expect("mengirim perintah");

    let batas = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let sisa = batas.saturating_duration_since(tokio::time::Instant::now());
        let pesan = tokio::time::timeout(sisa, soket.next())
            .await
            .expect("waktu habis menunggu balasan")
            .expect("soket tertutup")
            .expect("pesan rusak");
        let TsMessage::Text(teks) = pesan else {
            continue;
        };
        let nilai: Value = serde_json::from_str(teks.as_str()).expect("balasan JSON");
        if nilai.get("id").and_then(Value::as_i64) == Some(id) {
            return nilai;
        }
    }
}

/// Perintah dengan `sessionId` di tingkat atas frame, seperti yang dikirim
/// Puppeteer setelah `Target.attachToTarget`.
async fn minta_cdp_sesi(
    soket: &mut Klien,
    id: i64,
    metode: &str,
    params: Value,
    sesi: &str,
) -> Value {
    let pesan = json!({"id": id, "method": metode, "params": params, "sessionId": sesi});
    soket
        .send(TsMessage::Text(pesan.to_string().into()))
        .await
        .expect("mengirim perintah");
    tunggu_id(soket, id).await
}

#[tokio::test]
async fn json_version_menyebut_rust_native_engine_tanpa_biner_chrome() {
    let mesin = nyalakan();
    let port = mesin.info().devtools_port;

    let (status, badan) = minta(port, "/json/version").await;
    assert_eq!(status, 200, "GET /json/version harus 200");
    assert!(
        badan.contains("RustNativeEngine"),
        "AC-013 butuh `RustNativeEngine` di /json/version, dapat: {badan}"
    );
    assert!(
        badan.contains(USER_AGENT),
        "user agent yang dilaporkan harus milik mesin ini"
    );

    // Identitas yang dilaporkan `Engine`, bukan cuma yang tertulis di body.
    assert_eq!(mesin.kind(), KIND);
    assert_eq!(mesin.info().browser, PRODUK);
    assert_eq!(mesin.info().protocol_version, "1.3");

    // Bukti tidak ada proses anak: mesin ini hidup di dalam proses tes.
    assert!(
        mesin.process_id().is_none(),
        "mesin native tidak boleh melaporkan pid anak; reaper akan menembak pid itu"
    );
    assert_eq!(mesin.spec().binary.to_str(), Some(BINER_HANTU));

    mesin.stop().expect("mesin berhenti");
    assert!(!mesin.is_running(), "setelah stop, mesin harus mati");
}

#[tokio::test]
async fn daftar_target_kosong_lalu_terisi_setelah_create_target() {
    let mesin = nyalakan();
    let port = mesin.info().devtools_port;

    let (status, badan) = minta(port, "/json/list").await;
    assert_eq!(status, 200);
    let awal: Value = serde_json::from_str(&badan).expect("JSON /json/list");
    assert_eq!(awal.as_array().map(Vec::len), Some(0), "belum ada tab");

    let mut soket = sambung(port, "/devtools/browser").await;
    let jawab = minta_cdp(
        &mut soket,
        1,
        "Target.createTarget",
        json!({"url": "about:blank"}),
    )
    .await;
    assert!(
        jawab.get("error").is_none(),
        "createTarget harus berhasil: {jawab}"
    );
    let id = jawab["result"]["targetId"].as_str().expect("targetId");

    let (_, badan) = minta(port, "/json/list").await;
    let daftar: Value = serde_json::from_str(&badan).expect("JSON /json/list");
    let tab = daftar.as_array().expect("array");
    assert_eq!(tab.len(), 1);
    assert_eq!(tab[0]["id"].as_str(), Some(id));
    assert!(
        tab[0]["webSocketDebuggerUrl"]
            .as_str()
            .is_some_and(|url| url.contains(id)),
        "setiap tab harus punya URL soket sendiri: {}",
        tab[0]
    );

    mesin.stop().expect("berhenti");
}

/// Alur yang dipakai agen sungguhan: buat tab, attach, isi dokumen, baca
/// dokumen, lalu evaluasi skrip. Semua lewat satu soket browser dengan
/// `sessionId`, seperti Puppeteer.
#[tokio::test]
async fn alur_agen_lengkap_dari_create_target_sampai_evaluasi() {
    let mesin = nyalakan();
    let port = mesin.info().devtools_port;
    let mut soket = sambung(port, "/devtools/browser").await;

    let buat = minta_cdp(
        &mut soket,
        1,
        "Target.createTarget",
        json!({"url": "about:blank"}),
    )
    .await;
    let id = buat["result"]["targetId"]
        .as_str()
        .expect("targetId")
        .to_owned();

    let lekat = minta_cdp(
        &mut soket,
        2,
        "Target.attachToTarget",
        json!({"targetId": id, "flatten": true}),
    )
    .await;
    let sesi = lekat["result"]["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_owned();

    // Terpasang berarti `/json/list` melihatnya.
    let (_, badan) = minta(port, "/json/list").await;
    let daftar: Value = serde_json::from_str(&badan).expect("JSON");
    assert_eq!(
        daftar[0]["attached"],
        json!(true),
        "tab harus terlihat terpasang"
    );

    let isi = json!({
        "id": 3,
        "sessionId": sesi,
        "method": "Page.setDocumentContent",
        "params": {
            "frameId": "FRAME-0",
            "html": "<html><head><title>Kasir</title></head><body><h1 id='judul'>Halo</h1></body></html>"
        }
    });
    soket
        .send(TsMessage::Text(isi.to_string().into()))
        .await
        .expect("mengirim setDocumentContent");
    let terima = tunggu_id(&mut soket, 3).await;
    assert!(
        terima.get("error").is_none(),
        "setDocumentContent: {terima}"
    );
    assert_eq!(
        terima["result"]["receipt"]["status"], "Changed",
        "dokumen kosong yang diisi harus menghasilkan perubahan: {terima}"
    );

    let dok = minta_cdp_sesi(&mut soket, 4, "DOM.getDocument", json!({"depth": 2}), &sesi).await;
    assert!(dok.get("error").is_none(), "DOM.getDocument: {dok}");
    let root = &dok["result"]["root"];
    assert_eq!(root["nodeName"].as_str(), Some("#document"));

    let eval = json!({
        "id": 5,
        "sessionId": sesi,
        "method": "Runtime.evaluate",
        "params": {"expression": "1 + 1"}
    });
    soket
        .send(TsMessage::Text(eval.to_string().into()))
        .await
        .expect("mengirim evaluate");
    let hasil = tunggu_id(&mut soket, 5).await;
    assert!(hasil.get("error").is_none(), "Runtime.evaluate: {hasil}");
    assert_eq!(
        hasil["result"]["result"]["value"], "2",
        "evaluasi harus mengembalikan nilai skrip: {hasil}"
    );

    mesin.stop().expect("berhenti");
}

/// `Page.navigate` ke `data:text/html` mengisi dokumen **dan** melaporkan
/// perubahan; dua kali navigasi ke URL yang sama melaporkan
/// `NoObservableChange` (AC-020), karena tidak ada yang berubah.
#[tokio::test]
async fn navigasi_melaporkan_perubahan_dan_menolak_skema_tanpa_jaringan() {
    let mesin = nyalakan();
    let port = mesin.info().devtools_port;
    let mut soket = sambung(port, "/devtools/browser").await;

    let buat = minta_cdp(
        &mut soket,
        1,
        "Target.createTarget",
        json!({"url": "about:blank"}),
    )
    .await;
    let id = buat["result"]["targetId"]
        .as_str()
        .expect("targetId")
        .to_owned();
    let lekat = minta_cdp(
        &mut soket,
        2,
        "Target.attachToTarget",
        json!({"targetId": id}),
    )
    .await;
    let sesi = lekat["result"]["sessionId"]
        .as_str()
        .expect("sesi")
        .to_owned();

    let url = "data:text/html,<html><head><title>A</title></head><body><p>isi</p></body></html>";
    let nav = json!({
        "id": 3,
        "sessionId": sesi,
        "method": "Page.navigate",
        "params": {"url": url}
    });
    soket
        .send(TsMessage::Text(nav.to_string().into()))
        .await
        .expect("mengirim navigate");
    let pertama = tunggu_id(&mut soket, 3).await;
    assert!(pertama.get("error").is_none(), "navigate: {pertama}");
    assert_eq!(pertama["result"]["receipt"]["status"], "Changed");

    let ulang = json!({
        "id": 4,
        "sessionId": sesi,
        "method": "Page.navigate",
        "params": {"url": url}
    });
    soket
        .send(TsMessage::Text(ulang.to_string().into()))
        .await
        .expect("mengirim navigate ulang");
    let kedua = tunggu_id(&mut soket, 4).await;
    assert_eq!(
        kedua["result"]["receipt"]["status"], "NoObservableChange",
        "URL dan dokumen sama berarti tidak ada yang berubah: {kedua}"
    );

    // Skema yang butuh jaringan ditolak dengan pesan yang menyebut jalan
    // keluarnya, bukan halaman kosong yang terlihat seperti sukses.
    let http = json!({
        "id": 5,
        "sessionId": sesi,
        "method": "Page.navigate",
        "params": {"url": "https://example.com/"}
    });
    soket
        .send(TsMessage::Text(http.to_string().into()))
        .await
        .expect("mengirim navigate http");
    let ditolak = tunggu_id(&mut soket, 5).await;
    let pesan = ditolak["error"]["message"].as_str().unwrap_or_default();
    assert!(
        pesan.contains("Page.setDocumentContent"),
        "penolakan harus menyebut jalan keluarnya: {ditolak}"
    );

    mesin.stop().expect("berhenti");
}

/// Metode yang belum punya lapisan dijawab galat yang menyebut lapisannya.
/// Kalau ini berubah jadi `{}` tanpa implementasi, tes gagal.
#[tokio::test]
async fn metode_belum_ada_menjawab_galat_yang_menyebut_lapisannya() {
    let mesin = nyalakan();
    let port = mesin.info().devtools_port;
    let mut soket = sambung(port, "/devtools/browser").await;

    let buat = minta_cdp(
        &mut soket,
        1,
        "Target.createTarget",
        json!({"url": "about:blank"}),
    )
    .await;
    let id = buat["result"]["targetId"]
        .as_str()
        .expect("targetId")
        .to_owned();
    let lekat = minta_cdp(
        &mut soket,
        2,
        "Target.attachToTarget",
        json!({"targetId": id}),
    )
    .await;
    let sesi = lekat["result"]["sessionId"]
        .as_str()
        .expect("sesi")
        .to_owned();

    for (nomor, metode, lapisan) in [
        (3, "Page.captureScreenshot", "eyes"),
        (4, "DOM.querySelector", "selektor"),
        (5, "Input.dispatchMouseEvent", "hands"),
    ] {
        let frame = json!({
            "id": nomor,
            "sessionId": sesi,
            "method": metode,
            "params": {}
        });
        soket
            .send(TsMessage::Text(frame.to_string().into()))
            .await
            .expect("mengirim metode yang belum ada");
        let jawab = tunggu_id(&mut soket, nomor).await;
        assert_eq!(
            jawab["error"]["code"].as_i64(),
            Some(-32601),
            "{metode} harus dijawab method-not-found: {jawab}"
        );
        let pesan = jawab["error"]["message"].as_str().unwrap_or_default();
        assert!(
            pesan.contains(lapisan),
            "{metode} harus menyebut lapisan '{lapisan}': {pesan}"
        );
    }

    mesin.stop().expect("berhenti");
}

/// Batas 99 tab ditegakkan di lapisan protokol, dan galatnya jelas.
#[tokio::test]
async fn tab_ke_seratus_ditolak_dengan_galat_kapasitas() {
    let mesin = nyalakan();
    let port = mesin.info().devtools_port;
    let mut soket = sambung(port, "/devtools/browser").await;

    for nomor in 0..99 {
        let jawab = minta_cdp(
            &mut soket,
            nomor + 1,
            "Target.createTarget",
            json!({"url": "about:blank"}),
        )
        .await;
        assert!(
            jawab.get("error").is_none(),
            "tab ke-{} harus berhasil: {jawab}",
            nomor + 1
        );
    }

    let lebih = minta_cdp(
        &mut soket,
        100,
        "Target.createTarget",
        json!({"url": "about:blank"}),
    )
    .await;
    let pesan = lebih["error"]["message"].as_str().unwrap_or_default();
    assert!(
        pesan.contains("99"),
        "galat kapasitas harus menyebut batasnya: {lebih}"
    );
    assert_eq!(mesin.state().jumlah_tab(), 99, "tidak boleh ada tab ke-100");

    mesin.stop().expect("berhenti");
}

/// Soket per halaman tidak membawa `sessionId`; mesin menempelkannya sendiri.
#[tokio::test]
async fn soket_halaman_bicara_tanpa_session_id() {
    let mesin = nyalakan();
    let port = mesin.info().devtools_port;
    let mut browser = sambung(port, "/devtools/browser").await;
    let buat = minta_cdp(
        &mut browser,
        1,
        "Target.createTarget",
        json!({"url": "about:blank"}),
    )
    .await;
    let id = buat["result"]["targetId"]
        .as_str()
        .expect("targetId")
        .to_owned();

    let mut halaman = sambung(port, &format!("/devtools/page/{id}")).await;
    let eval = minta_cdp(
        &mut halaman,
        1,
        "Runtime.evaluate",
        json!({"expression": "'halo'"}),
    )
    .await;
    assert!(
        eval.get("error").is_none(),
        "soket halaman harus menjawab tanpa sessionId eksplisit: {eval}"
    );
    assert_eq!(eval["result"]["result"]["value"], "halo");

    mesin.stop().expect("berhenti");
}

/// AC-014 versi kecil: `stop` menutup listener, jadi port tidak bisa dipakai
/// lagi. Satu soket yang masih terbuka tidak boleh menahan server hidup.
#[tokio::test]
async fn stop_menutup_port_dan_tidak_menyisakan_soket() {
    let mesin = nyalakan();
    let port = mesin.info().devtools_port;
    let soket = sambung(port, "/devtools/browser").await;
    mesin.stop().expect("berhenti");
    drop(soket);

    // Memberi waktu tugas axum selesai; tanpa tidur panjang, cukup satu
    // putaran runtime supaya abort-nya mendarat.
    tokio::task::yield_now().await;
    for _ in 0..50 {
        if TcpStream::connect(("127.0.0.1", port)).await.is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("port {port} masih menerima koneksi setelah stop");
}

async fn tunggu_id(soket: &mut Klien, id: i64) -> Value {
    let batas = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let sisa = batas.saturating_duration_since(tokio::time::Instant::now());
        let pesan = tokio::time::timeout(sisa, soket.next())
            .await
            .expect("waktu habis menunggu balasan")
            .expect("soket tertutup")
            .expect("pesan rusak");
        let TsMessage::Text(teks) = pesan else {
            continue;
        };
        let nilai: Value = serde_json::from_str(teks.as_str()).expect("balasan JSON");
        if nilai.get("id").and_then(Value::as_i64) == Some(id) {
            return nilai;
        }
    }
}
