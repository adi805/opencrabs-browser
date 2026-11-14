//! Peristiwa DOM dari mesin native: pendengar halaman, `element.click()`, dan
//! hit-test `Input.dispatchMouseEvent`.
//!
//! Yang diuji di sini adalah rantai penuh: klien mengirim koordinat lewat CDP,
//! mesin menghitung layout untuk mencari elemen di titik itu, lalu
//! **menjalankan fungsi JavaScript yang didaftarkan halaman**. Kalau salah satu
//! mata rantai hilang, pendengar tidak pernah dipanggil dan hitungan di bawah
//! tetap nol. Karena itu tes ini tidak cukup memeriksa balasan CDP: ia membaca
//! variabel yang hanya bisa berubah kalau skrip halaman benar-benar berjalan.
//!
//! Sisi negatifnya sama penting: klik di luar kotak **tidak boleh** memanggil
//! pendengar. Mesin yang memanggil pendengar untuk semua klik akan lulus tes
//! positif tanpa pernah menghitung apa pun.

use std::time::Duration;

use cdp::native_wire::NativeEngine;
use engine::{Engine, LaunchSpec};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as TsMessage;

const BINER_HANTU: &str = "/nonexistent/oc-browser/tidak-ada-chrome";

type Klien = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

fn spec() -> LaunchSpec {
    LaunchSpec::new(BINER_HANTU, "/tmp/oc-browser-native-events")
}

async fn sambung(port: u16) -> Klien {
    let (soket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/devtools/browser"))
            .await
            .expect("menyambung ke soket mesin native");
    soket
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

async fn kirim(soket: &mut Klien, id: i64, metode: &str, params: Value, sesi: &str) -> Value {
    let pesan = json!({"id": id, "method": metode, "params": params, "sessionId": sesi});
    soket
        .send(TsMessage::Text(pesan.to_string().into()))
        .await
        .expect("mengirim perintah");
    tunggu_id(soket, id).await
}

/// Membuat satu tab berisi HTML uji, lalu mengembalikan id sesinya.
async fn tab_dengan(soket: &mut Klien, html: &str) -> String {
    let buat = json!({"id": 1, "method": "Target.createTarget", "params": {"url": "about:blank"}});
    soket
        .send(TsMessage::Text(buat.to_string().into()))
        .await
        .expect("createTarget");
    let jawab = tunggu_id(soket, 1).await;
    let id = jawab["result"]["targetId"]
        .as_str()
        .expect("targetId")
        .to_owned();

    let lekat = json!({"id": 2, "method": "Target.attachToTarget", "params": {"targetId": id}});
    soket
        .send(TsMessage::Text(lekat.to_string().into()))
        .await
        .expect("attachToTarget");
    let jawab = tunggu_id(soket, 2).await;
    let sesi = jawab["result"]["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_owned();

    let jawab = kirim(
        soket,
        3,
        "Page.setDocumentContent",
        json!({"frameId": sesi, "html": html}),
        &sesi,
    )
    .await;
    assert!(
        jawab.get("error").is_none(),
        "setDocumentContent harus berhasil: {jawab}"
    );
    sesi
}

async fn eval_angka(soket: &mut Klien, id: i64, ekspresi: &str, sesi: &str) -> Value {
    let jawab = kirim(
        soket,
        id,
        "Runtime.evaluate",
        json!({"expression": ekspresi, "returnByValue": true}),
        sesi,
    )
    .await;
    assert!(jawab.get("error").is_none(), "evaluate gagal: {jawab}");
    jawab["result"]["result"]["value"].clone()
}

/// Halaman uji: satu kotak 200x100 di pojok kiri atas, tanpa margin.
const HALAMAN: &str = r#"<!doctype html><html><head><title>uji</title></head>
<body><div id="tombol" style="width:200px;height:100px">klik</div></body></html>"#;

/// Pendengar yang didaftarkan halaman benar-benar dipanggil oleh klik CDP.
///
/// Ini tes yang menuntut seluruh rantai: hit-test layout, pemilihan node, dan
/// eksekusi `addEventListener` milik skrip halaman.
#[tokio::test]
async fn test_mouse_click_dispatch() {
    let mesin = NativeEngine::start(spec()).expect("mesin native menyala");
    let port = mesin.info().devtools_port;
    let mut soket = sambung(port).await;
    let sesi = tab_dengan(&mut soket, HALAMAN).await;

    let daftar = eval_angka(
        &mut soket,
        4,
        "window.__klik = 0; window.__jenis = ''; \
         document.getElementById('tombol').addEventListener('click', function (e) { \
           window.__klik++; window.__jenis = e.type; window.__sasaran = e.target.id; }); \
         'terpasang'",
        &sesi,
    )
    .await;
    assert_eq!(daftar, json!("terpasang"), "pendengar harus terpasang");

    // Titik (10,10) ada di dalam kotak 200x100 di pojok kiri atas.
    for (id, jenis) in [(10, "mousePressed"), (11, "mouseReleased")] {
        let jawab = kirim(
            &mut soket,
            id,
            "Input.dispatchMouseEvent",
            json!({"type": jenis, "x": 10, "y": 10, "button": "left", "clickCount": 1}),
            &sesi,
        )
        .await;
        assert!(
            jawab.get("error").is_none(),
            "{jenis} harus dijawab mesin, bukan galat: {jawab}"
        );
    }

    let klik = eval_angka(&mut soket, 12, "window.__klik", &sesi).await;
    assert_eq!(
        klik,
        json!(1),
        "pendengar click harus dipanggil tepat sekali"
    );
    let jenis = eval_angka(&mut soket, 13, "window.__jenis", &sesi).await;
    assert_eq!(jenis, json!("click"), "tipe peristiwa harus `click`");
    let sasaran = eval_angka(&mut soket, 14, "window.__sasaran", &sesi).await;
    assert_eq!(
        sasaran,
        json!("tombol"),
        "peristiwa harus menyasar elemen yang di-hit-test"
    );

    mesin.stop().expect("berhenti");
}

/// Klik di luar semua kotak tidak boleh memanggil pendengar mana pun.
///
/// Tanpa sisi negatif ini, mesin yang memanggil semua pendengar untuk setiap
/// klik akan lulus tes positif di atas.
#[tokio::test]
async fn klik_di_luar_kotak_tidak_memanggil_pendengar() {
    let mesin = NativeEngine::start(spec()).expect("mesin native menyala");
    let port = mesin.info().devtools_port;
    let mut soket = sambung(port).await;
    let sesi = tab_dengan(&mut soket, HALAMAN).await;

    eval_angka(
        &mut soket,
        4,
        "window.__klik = 0; \
         document.addEventListener; \
         document.getElementById('tombol').addEventListener('click', function () { window.__klik++; }); \
         'ok'",
        &sesi,
    )
    .await;

    // Kotak berakhir di x=200, y=100. Titik ini di luarnya.
    for (id, jenis) in [(10, "mousePressed"), (11, "mouseReleased")] {
        kirim(
            &mut soket,
            id,
            "Input.dispatchMouseEvent",
            json!({"type": jenis, "x": 700, "y": 500, "button": "left", "clickCount": 1}),
            &sesi,
        )
        .await;
    }

    let klik = eval_angka(&mut soket, 12, "window.__klik", &sesi).await;
    assert_eq!(
        klik,
        json!(0),
        "klik di luar kotak tidak boleh menyentuh pendengar"
    );

    mesin.stop().expect("berhenti");
}

/// `element.click()` dari skrip halaman memanggil pendengar yang sama.
#[tokio::test]
async fn element_click_memanggil_pendengar() {
    let mesin = NativeEngine::start(spec()).expect("mesin native menyala");
    let port = mesin.info().devtools_port;
    let mut soket = sambung(port).await;
    let sesi = tab_dengan(&mut soket, HALAMAN).await;

    let hasil = eval_angka(
        &mut soket,
        4,
        "window.__klik = 0; \
         var t = document.getElementById('tombol'); \
         t.addEventListener('click', function (e) { window.__klik++; window.__tipe = e.type; }); \
         t.click(); \
         window.__klik",
        &sesi,
    )
    .await;
    assert_eq!(hasil, json!(1), "element.click() harus memanggil pendengar");

    mesin.stop().expect("berhenti");
}

/// `removeEventListener` menghentikan pendengar, dan peristiwa merambat ke induk.
#[tokio::test]
async fn peristiwa_merambat_dan_remove_menghentikannya() {
    let mesin = NativeEngine::start(spec()).expect("mesin native menyala");
    let port = mesin.info().devtools_port;
    let mut soket = sambung(port).await;
    let sesi = tab_dengan(
        &mut soket,
        r#"<!doctype html><html><head><title>uji</title></head><body>
           <div id="induk" style="width:200px;height:100px"><span id="anak">x</span></div>
           </body></html>"#,
    )
    .await;

    let hasil = eval_angka(
        &mut soket,
        4,
        "window.__induk = 0; window.__anak = 0; window.__lepas = 0; \
         var induk = document.getElementById('induk'); \
         var anak = document.getElementById('anak'); \
         induk.addEventListener('click', function () { window.__induk++; }); \
         var sekali = function () { window.__anak++; }; \
         anak.addEventListener('click', sekali); \
         anak.click(); \
         anak.removeEventListener('click', sekali); \
         anak.click(); \
         [window.__induk, window.__anak]",
        &sesi,
    )
    .await;
    assert_eq!(
        hasil,
        json!([2, 1]),
        "dua klik: induk menerima keduanya (merambat), anak hanya yang pertama"
    );

    mesin.stop().expect("berhenti");
}

/// Pendengar `focus`, `blur`, `input`, `change`, dan `keydown` terpasang dan
/// bisa dipicu, jadi daftar tipe di PRD bukan janji kosong.
#[tokio::test]
async fn pendengar_tipe_lain_bisa_dipicu() {
    let mesin = NativeEngine::start(spec()).expect("mesin native menyala");
    let port = mesin.info().devtools_port;
    let mut soket = sambung(port).await;
    let sesi = tab_dengan(
        &mut soket,
        r#"<!doctype html><html><head><title>uji</title></head><body>
           <input id="kotak" style="width:100px;height:20px">
           </body></html>"#,
    )
    .await;

    let hasil = eval_angka(
        &mut soket,
        4,
        "window.__jejak = []; \
         var k = document.getElementById('kotak'); \
         ['focus', 'blur', 'input', 'change', 'keydown'].forEach(function (tipe) { \
           k.addEventListener(tipe, function (e) { window.__jejak.push(e.type); }); \
         }); \
         k.focus(); k.blur(); \
         k.dispatchEvent(new Event('input')); \
         k.dispatchEvent(new Event('change')); \
         k.dispatchEvent(new Event('keydown')); \
         window.__jejak.join(',')",
        &sesi,
    )
    .await;
    assert_eq!(
        hasil,
        json!("focus,blur,input,change,keydown"),
        "kelima tipe peristiwa harus sampai ke pendengar halaman"
    );

    mesin.stop().expect("berhenti");
}
