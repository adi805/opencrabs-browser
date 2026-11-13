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
        hasil["result"]["result"]["value"],
        json!(2),
        "evaluasi harus mengembalikan angka, bukan string: {hasil}"
    );
    assert_eq!(
        hasil["result"]["result"]["type"], "number",
        "tipe balik harus number: {hasil}"
    );

    mesin.stop().expect("berhenti");
}

/// AC-017 lewat protokol sungguhan: `navigator.webdriver` harus keluar sebagai
/// boolean `false` di `RemoteObject`, bukan string `"false"` dan bukan
/// `undefined`. Skrip anti-bot membandingkan `=== false`, jadi bentuknya
/// menentukan, bukan isinya saja.
#[tokio::test]
async fn runtime_evaluate_mempertahankan_tipe_dan_mengirim_boolean() {
    let mesin = nyalakan();
    let port = mesin.info().devtools_port;
    let mut soket = sambung(port, "/devtools/browser").await;
    let sesi = tab_dengan(&mut soket, "<html><body><p>x</p></body></html>").await;

    let webdriver = minta_cdp_sesi(
        &mut soket,
        10,
        "Runtime.evaluate",
        json!({"expression": "navigator.webdriver", "returnByValue": true}),
        &sesi,
    )
    .await;
    assert!(webdriver.get("error").is_none(), "evaluate: {webdriver}");
    let objek = &webdriver["result"]["result"];
    assert_eq!(
        objek["type"], "boolean",
        "navigator.webdriver harus boolean: {webdriver}"
    );
    assert_eq!(
        objek["value"],
        json!(false),
        "navigator.webdriver harus false: {webdriver}"
    );

    // String tetap string, bukan angka, dan objek dikirim by value.
    let teks = minta_cdp_sesi(
        &mut soket,
        11,
        "Runtime.evaluate",
        json!({"expression": "'halo'"}),
        &sesi,
    )
    .await;
    assert_eq!(teks["result"]["result"]["type"], "string");
    assert_eq!(teks["result"]["result"]["value"], "halo");

    let larik = minta_cdp_sesi(
        &mut soket,
        12,
        "Runtime.evaluate",
        json!({"expression": "[1, 2, 3]"}),
        &sesi,
    )
    .await;
    assert_eq!(larik["result"]["result"]["type"], "object");
    assert_eq!(
        larik["result"]["result"]["value"],
        json!([1, 2, 3]),
        "larik harus dikirim by value: {larik}"
    );

    // `undefined` tidak punya `value` sama sekali; itu bentuk yang benar.
    let kosong = minta_cdp_sesi(
        &mut soket,
        13,
        "Runtime.evaluate",
        json!({"expression": "undefined"}),
        &sesi,
    )
    .await;
    assert_eq!(kosong["result"]["result"]["type"], "undefined");
    assert!(
        kosong["result"]["result"].get("value").is_none(),
        "undefined tidak boleh punya value: {kosong}"
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

    // Halaman http yang servernya tidak bisa dihubungi harus gagal dengan
    // pesan yang menyebut URL-nya, bukan dokumen kosong yang terlihat seperti
    // sukses. Port 1 di loopback menolak koneksi seketika, jadi tes ini tidak
    // menyentuh internet sama sekali.
    let http = json!({
        "id": 5,
        "sessionId": sesi,
        "method": "Page.navigate",
        "params": {"url": "http://127.0.0.1:1/"}
    });
    soket
        .send(TsMessage::Text(http.to_string().into()))
        .await
        .expect("mengirim navigate http");
    let ditolak = tunggu_id(&mut soket, 5).await;
    let pesan = ditolak["error"]["message"].as_str().unwrap_or_default();
    assert!(
        pesan.contains("tidak bisa diambil") && pesan.contains("127.0.0.1:1"),
        "kegagalan jaringan harus menyebut URL-nya: {ditolak}"
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
        // `Page.captureScreenshot` dulu di sini; sekarang dijawab sungguhan,
        // jadi yang menguji cabang "belum ada" adalah MHTML dan PDF.
        (3, "Page.printToPDF", "PDF"),
        // `DOM.querySelector` dulu di sini; sekarang dijawab sungguhan, jadi
        // yang menguji cabang "belum ada" adalah tumpukan jaringan.
        (4, "Network.enable", "jaringan"),
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

/// Menyiapkan satu tab berisi HTML lalu mengembalikan `sessionId`-nya.
///
/// Dipakai tes di bawah supaya yang diuji benar-benar jalur protokol, bukan
/// pemanggilan fungsi internal.
async fn tab_dengan(soket: &mut Klien, html: &str) -> String {
    let buat = minta_cdp(
        soket,
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
        soket,
        2,
        "Target.attachToTarget",
        json!({"targetId": id, "flatten": true}),
    )
    .await;
    let sesi = lekat["result"]["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_owned();
    let isi = minta_cdp_sesi(
        soket,
        3,
        "Page.setDocumentContent",
        json!({"frameId": "FRAME-0", "html": html}),
        &sesi,
    )
    .await;
    assert!(isi.get("error").is_none(), "setDocumentContent: {isi}");
    sesi
}

/// AC: agen bisa membaca halaman tanpa penglihatan lewat protokol.
#[tokio::test]
async fn accessibility_get_full_ax_tree_mengirim_peran_dan_nama() {
    let mesin = nyalakan();
    let port = mesin.info().devtools_port;
    let mut soket = sambung(port, "/devtools/browser").await;
    let sesi = tab_dengan(
        &mut soket,
        "<html><body><h1>Judul</h1><a href='/x'>Beranda</a>\
         <button aria-label='Tutup'>X</button></body></html>",
    )
    .await;

    let balas = minta_cdp_sesi(
        &mut soket,
        10,
        "Accessibility.getFullAXTree",
        json!({}),
        &sesi,
    )
    .await;
    assert!(balas.get("error").is_none(), "getFullAXTree: {balas}");
    let simpul = balas["result"]["nodes"].as_array().expect("nodes array");
    let peran: Vec<&str> = simpul
        .iter()
        .filter_map(|n| n["role"]["value"].as_str())
        .collect();
    assert!(peran.contains(&"heading"), "harus ada heading: {peran:?}");
    assert!(peran.contains(&"link"), "harus ada link: {peran:?}");

    let heading = simpul
        .iter()
        .find(|n| n["role"]["value"] == "heading")
        .expect("simpul heading");
    assert_eq!(heading["name"]["value"], "Judul");
    assert_eq!(heading["name"]["type"], "computedString");
    assert!(
        heading["backendDOMNodeId"].as_u64().is_some(),
        "AX harus bisa dilacak balik ke node DOM: {heading}"
    );
    assert!(
        heading["parentId"].is_string(),
        "simpul selain akar punya induk: {heading}"
    );

    let tombol = simpul
        .iter()
        .find(|n| n["role"]["value"] == "button")
        .expect("simpul button");
    assert_eq!(
        tombol["name"]["value"], "Tutup",
        "aria-label menang atas isi teks"
    );

    mesin.stop().expect("berhenti");
}

/// AC: tangkapan layar mengeluarkan PNG dan JPEG sungguhan dengan ukuran yang
/// diminta, `clip.scale` benar-benar memperbesar keluaran, dan format yang
/// belum punya encoder (`webp`) ditolak dengan namanya.
#[tokio::test]
async fn page_capture_screenshot_melayani_png_jpeg_dan_menolak_webp() {
    let mesin = nyalakan();
    let port = mesin.info().devtools_port;
    let mut soket = sambung(port, "/devtools/browser").await;
    let sesi = tab_dengan(
        &mut soket,
        "<html><body><div style='width:50px;height:50px;background-color:#ff0000'>\
         </div></body></html>",
    )
    .await;

    let balas = minta_cdp_sesi(
        &mut soket,
        10,
        "Page.captureScreenshot",
        json!({"format": "png", "clip": {"x": 0, "y": 0, "width": 120, "height": 90}}),
        &sesi,
    )
    .await;
    assert!(balas.get("error").is_none(), "captureScreenshot: {balas}");
    let data = balas["result"]["data"].as_str().expect("data base64");
    let png = cdp::base64::dekode(data).expect("base64 yang dikirim harus sah");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "harus PNG asli");
    let lebar = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let tinggi = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    assert_eq!((lebar, tinggi), (120, 90), "ukuran mengikuti clip");
    assert_eq!(
        balas["result"]["receipt"]["kotakDilukis"], 1,
        "satu kotak berlatar harus dilukis: {balas}"
    );

    let jpeg = minta_cdp_sesi(
        &mut soket,
        11,
        "Page.captureScreenshot",
        json!({"format": "jpeg", "quality": 70}),
        &sesi,
    )
    .await;
    assert!(jpeg.get("error").is_none(), "JPEG harus dilayani: {jpeg}");
    let data = jpeg["result"]["data"].as_str().expect("data jpeg base64");
    let bytes = cdp::base64::dekode(data).expect("base64 yang dikirim harus sah");
    assert_eq!(&bytes[..3], b"\xff\xd8\xff", "harus JPEG asli");
    assert_eq!(
        jpeg["result"]["receipt"]["format"], "image/jpeg",
        "tanda terima harus menyebut JPEG: {jpeg}"
    );

    let webp = minta_cdp_sesi(
        &mut soket,
        13,
        "Page.captureScreenshot",
        json!({"format": "webp"}),
        &sesi,
    )
    .await;
    assert_eq!(
        webp["error"]["code"],
        json!(-32602),
        "webp belum punya encoder dan harus ditolak dengan nama: {webp}"
    );

    let skala = minta_cdp_sesi(
        &mut soket,
        12,
        "Page.captureScreenshot",
        json!({"clip": {"x": 0, "y": 0, "width": 10, "height": 10, "scale": 2}}),
        &sesi,
    )
    .await;
    assert!(
        skala.get("error").is_none(),
        "scale 2 harus dilayani, bukan ditolak: {skala}"
    );
    let data = skala["result"]["data"].as_str().expect("data skala base64");
    let png = cdp::base64::dekode(data).expect("base64 yang dikirim harus sah");
    let lebar = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    assert_eq!(lebar, 20, "10 px CSS pada skala 2 jadi 20 px");

    mesin.stop().expect("berhenti");
}

/// Server HTTP yang sengaja lambat, dengan pemberitahuan saat permintaannya
/// benar-benar sudah diterima.
///
/// Pemberitahuan itu penting: tanpa itu tes bisa lulus secara palsu, karena
/// menanyai soket kedua sebelum pengambilan dimulai tidak membuktikan apa pun.
fn server_lambat(tunda: Duration) -> (String, tokio::sync::oneshot::Receiver<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("port bebas");
    let alamat = listener.local_addr().expect("alamat").to_string();
    // Oneshot, bukan `std::sync::mpsc`: tesnya `#[tokio::test]` satu thread,
    // jadi menunggu saluran blocking di dalam tes akan membekukan runtime
    // yang juga melayani servernya sendiri.
    let (kabar, tunggu) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let Ok((mut soket, _)) = listener.accept() else {
            return;
        };
        // Baca kepala permintaan sampai baris kosong; kalau tidak dibaca,
        // klien bisa menunggu pengiriman kita alih-alih menjawab.
        {
            let mut pembaca = std::io::BufReader::new(soket.try_clone().expect("klon soket"));
            let mut baris = String::new();
            while std::io::BufRead::read_line(&mut pembaca, &mut baris).unwrap_or(0) > 0 {
                if baris.trim().is_empty() {
                    break;
                }
                baris.clear();
            }
        }
        let _ = kabar.send(());
        std::thread::sleep(tunda);
        let badan: &[u8] = b"<html><body><p>lambat</p></body></html>";
        let kepala = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\
             content-type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
            badan.len()
        );
        let _ = std::io::Write::write_all(&mut soket, kepala.as_bytes());
        let _ = std::io::Write::write_all(&mut soket, badan);
    });
    (alamat, tunggu)
}

/// Pengambilan jaringan yang lambat tidak boleh menahan kunci state.
///
/// Kalau `Page.navigate` mengambil dokumen sambil memegang kunci, seluruh tab
/// berhenti menjawab selama server lambat. Tes ini menahan satu navigasi di
/// server yang butuh dua detik, lalu menanyai soket **kedua** dengan perintah
/// yang tidak butuh jaringan: jawabannya harus datang jauh sebelum navigasi
/// selesai.
#[tokio::test]
async fn navigasi_lambat_tidak_menahan_kunci_state() {
    let mesin = nyalakan();
    let port = mesin.info().devtools_port;

    let mut a = sambung(port, "/devtools/browser").await;
    let dibuat = minta_cdp(
        &mut a,
        1,
        "Target.createTarget",
        json!({"url": "about:blank"}),
    )
    .await;
    let id = dibuat["result"]["targetId"]
        .as_str()
        .expect("targetId")
        .to_owned();
    let lekat = minta_cdp(
        &mut a,
        2,
        "Target.attachToTarget",
        json!({"targetId": id, "flatten": true}),
    )
    .await;
    let sesi = lekat["result"]["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_owned();

    let mut b = sambung(port, "/devtools/browser").await;

    let (alamat, diterima) = server_lambat(Duration::from_secs(2));
    let url = format!("http://{alamat}/lambat");
    let frame = json!({
        "id": 3,
        "sessionId": sesi,
        "method": "Page.navigate",
        "params": {"url": url},
    });
    a.send(TsMessage::Text(frame.to_string().into()))
        .await
        .expect("mengirim navigasi");

    // Tunggu sampai server benar-benar memegang permintaannya, jadi jeda di
    // bawah diukur saat pengambilan sedang berjalan. Batas waktunya ada supaya
    // versi yang mengambil di dalam kunci gagal dengan pesan, bukan menggantung
    // sampai harness membunuhnya.
    tokio::time::timeout(Duration::from_secs(5), diterima)
        .await
        .expect(
            "permintaan tidak pernah sampai ke server dalam 5 detik: \
             pengambilan berjalan sambil memegang kunci state",
        )
        .expect("saluran pemberitahuan ditutup");

    let mulai = std::time::Instant::now();
    let jawab = minta_cdp(&mut b, 4, "Browser.getVersion", json!({})).await;
    let lama = mulai.elapsed();

    assert!(
        jawab["result"]["product"].is_string(),
        "soket kedua harus dijawab: {jawab}"
    );
    assert!(
        lama < Duration::from_millis(1200),
        "jawaban tertahan {lama:?}: kunci state dipegang selama pengambilan jaringan"
    );

    let navigasi = tunggu_id(&mut a, 3).await;
    assert!(
        navigasi.get("error").is_none(),
        "navigasi gagal: {navigasi}"
    );
    let akhir = navigasi["result"]["sumber"]["urlFinal"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(akhir, url, "URL akhir harus dilaporkan: {navigasi}");

    mesin.stop().expect("berhenti");
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
