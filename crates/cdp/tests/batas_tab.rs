//! Bukti bahwa `max_tabs` benar-benar menolak tab di lapisan HTTP.
//!
//! Mesin di sini palsu tetapi **sungguhan sebagai server HTTP**: jumlah tab
//! yang dilaporkan `/json/list` bisa diubah test, dan `/json/new` mencatat
//! apakah permintaannya benar-benar sampai ke mesin. Itu yang membuat test ini
//! bisa membedakan "ditolak daemon" dari "ditolak mesin" — dua hal yang di
//! layar klien terlihat sama-sama galat.
//!
//! Yang dibuktikan:
//!
//! 1. Di bawah batas, tab tetap boleh dibuat dan permintaannya diteruskan.
//! 2. Saat batas tercapai, daemon menjawab `429` dan mesin **tidak disentuh**.
//! 3. Setelah tab ditutup, tab baru boleh dibuat lagi: batasnya jumlah tab
//!    sekarang, bukan hitungan seumur hidup mesin.
//! 4. Mesin tanpa `max_tabs` tidak pernah ditolak, walau tabnya banyak.
//! 5. Jumlah tab yang tidak terbaca tidak memblokir apa pun (gagal-terbuka).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::Method;
use axum::response::Response;
use axum::routing::{any, get};
use axum::{Json, Router};
use cdp::{EnginePool, ProxyState};
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use testing::FakeEngine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TsMessage;

/// Keadaan satu mesin palsu: berapa halaman yang dilaporkan, dan berapa
/// permintaan tab baru yang benar-benar sampai.
#[derive(Debug, Default)]
struct Keadaan {
    halaman: usize,
    dibuat: usize,
}

type Bagi = Arc<Mutex<Keadaan>>;

struct Mesin {
    port: u16,
    keadaan: Bagi,
    _tugas: tokio::task::JoinHandle<()>,
}

async fn mesin(halaman: usize) -> Mesin {
    let keadaan: Bagi = Arc::new(Mutex::new(Keadaan { halaman, dibuat: 0 }));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mesin");
    let port = listener.local_addr().expect("alamat mesin").port();
    let app = Router::new()
        .route("/devtools/browser/{*rest}", any(ws_masuk))
        .route("/json/list", get(daftar))
        .route("/json/new", any(tab_baru))
        .route("/json/close/{id}", get(tutup))
        .with_state(Portofolio {
            port,
            keadaan: Arc::clone(&keadaan),
        });
    let tugas = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Mesin {
        port,
        keadaan,
        _tugas: tugas,
    }
}

impl Mesin {
    fn halaman(&self) -> usize {
        self.keadaan.lock().expect("keadaan").halaman
    }

    fn dibuat(&self) -> usize {
        self.keadaan.lock().expect("keadaan").dibuat
    }

    fn tutup_satu(&self) {
        let mut keadaan = self.keadaan.lock().expect("keadaan");
        keadaan.halaman = keadaan.halaman.saturating_sub(1);
    }
}

#[derive(Clone)]
struct Portofolio {
    port: u16,
    keadaan: Bagi,
}

fn satu_tab(port: u16, nomor: usize) -> Value {
    json!({
        "id": format!("P{nomor}"),
        "type": "page",
        "title": format!("tab {nomor}"),
        "url": "about:blank",
        "webSocketDebuggerUrl": format!("ws://127.0.0.1:{port}/devtools/page/P{nomor}"),
    })
}

async fn daftar(State(t): State<Portofolio>) -> Json<Value> {
    let jumlah = t.keadaan.lock().expect("keadaan").halaman;
    let tabs: Vec<Value> = (1..=jumlah).map(|n| satu_tab(t.port, n)).collect();
    Json(Value::Array(tabs))
}

async fn tab_baru(State(t): State<Portofolio>, method: Method) -> Json<Value> {
    let mut keadaan = t.keadaan.lock().expect("keadaan");
    keadaan.dibuat += 1;
    keadaan.halaman += 1;
    let nomor = keadaan.halaman;
    drop(keadaan);
    let mut body = satu_tab(t.port, nomor);
    if let Some(map) = body.as_object_mut() {
        // Bentuk jawaban Chrome: berupa objek target, bukan array.
        map.insert(
            "metode".to_owned(),
            Value::String(method.as_str().to_owned()),
        );
    }
    Json(body)
}

/// Sisi websocket mesin palsu: mencatat `Target.createTarget` yang benar-benar
/// sampai. Hitungan itu yang membedakan "ditolak daemon" dari "ditolak mesin".
async fn ws_masuk(ws: WebSocketUpgrade, State(t): State<Portofolio>) -> Response {
    ws.on_upgrade(move |socket| layani_ws(socket, t))
}

async fn layani_ws(mut socket: WebSocket, t: Portofolio) {
    while let Some(Ok(AxumMessage::Text(teks))) = socket.next().await {
        let Ok(nilai) = serde_json::from_str::<Value>(teks.as_str()) else {
            continue;
        };
        let id = nilai.get("id").cloned().unwrap_or(Value::Null);
        let method = nilai
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let balasan = if method == "Target.createTarget" {
            let mut keadaan = t.keadaan.lock().expect("keadaan");
            keadaan.dibuat += 1;
            keadaan.halaman += 1;
            let nomor = keadaan.halaman;
            drop(keadaan);
            json!({"id": id, "result": {"targetId": format!("P{nomor}")}})
        } else {
            json!({"id": id, "result": {}})
        };
        if socket
            .send(AxumMessage::Text(balasan.to_string().into()))
            .await
            .is_err()
        {
            break;
        }
    }
}

async fn tutup(State(t): State<Portofolio>, Path(_id): Path<String>) -> Json<Value> {
    let mut keadaan = t.keadaan.lock().expect("keadaan");
    keadaan.halaman = keadaan.halaman.saturating_sub(1);
    Json(json!({"ditutup": true}))
}

/// Proxy discovery di atas satu mesin, dengan batas tab tertentu.
struct Proxy {
    port: u16,
    _tugas: tokio::task::JoinHandle<()>,
}

async fn proxy(port_mesin: u16, batas: Option<u32>) -> Proxy {
    let engine: Arc<dyn Engine> = Arc::new(FakeEngine::at_port(port_mesin));
    let mut mesin_peta: BTreeMap<String, Arc<dyn Engine>> = BTreeMap::new();
    mesin_peta.insert("mesin".to_owned(), engine);
    let pool = EnginePool::new("mesin", mesin_peta).expect("pool");
    let pool = match batas {
        Some(batas) => {
            let mut peta = BTreeMap::new();
            peta.insert("mesin".to_owned(), batas);
            pool.dengan_batas_tab(peta)
        }
        None => pool,
    };

    nyalakan(pool).await
}

/// Pool dua mesin yang menunjuk mesin palsu yang sama. Cukup untuk membuktikan
/// jalur multiplex: yang diuji penegakan batasnya, bukan pemisahan antar mesin.
async fn proxy_dua(port_mesin: u16, batas: Option<u32>) -> Proxy {
    let mut mesin_peta: BTreeMap<String, Arc<dyn Engine>> = BTreeMap::new();
    for id in ["a", "b"] {
        mesin_peta.insert(
            id.to_owned(),
            Arc::new(FakeEngine::at_port(port_mesin)) as Arc<dyn Engine>,
        );
    }
    let mut pool = EnginePool::new("a", mesin_peta).expect("pool dua mesin");
    if let Some(batas) = batas {
        let mut peta = BTreeMap::new();
        peta.insert("a".to_owned(), batas);
        pool = pool.dengan_batas_tab(peta);
    }
    nyalakan(pool).await
}

async fn nyalakan(pool: EnginePool) -> Proxy {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
    let port = listener.local_addr().expect("alamat proxy").port();
    let state = ProxyState::pool(
        pool,
        format!("ws://127.0.0.1:{port}/devtools/browser/klien"),
    );
    let app = cdp::router(Arc::new(state));
    let tugas = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Proxy {
        port,
        _tugas: tugas,
    }
}

/// Permintaan HTTP mentah; klien CDP memakai `PUT /json/new`, dan itu yang
/// harus diuji, bukan bentuk yang lebih nyaman untuk test.
async fn minta(port: u16, metode: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("menyambung ke proxy");
    let permintaan =
        format!("{metode} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
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

#[tokio::test]
async fn di_bawah_batas_tab_tetap_diteruskan_ke_mesin() {
    let mesin = mesin(1).await;
    let proxy = proxy(mesin.port, Some(2)).await;

    let (status, badan) = minta(proxy.port, "PUT", "/json/new?about:blank").await;
    assert_eq!(status, 200, "badan: {badan}");
    assert_eq!(mesin.dibuat(), 1, "permintaan harus sampai ke mesin");
    assert_eq!(mesin.halaman(), 2);
}

#[tokio::test]
async fn saat_batas_tercapai_tab_ditolak_dan_mesin_tidak_disentuh() {
    let mesin = mesin(2).await;
    let proxy = proxy(mesin.port, Some(2)).await;

    let (status, badan) = minta(proxy.port, "PUT", "/json/new").await;
    assert_eq!(status, 429, "penuh harus 429, bukan 5xx: {badan}");
    assert_eq!(mesin.dibuat(), 0, "mesin tidak boleh menerima permintaan");
    assert_eq!(mesin.halaman(), 2, "jumlah tab mesin tidak berubah");
    // Pesannya harus bisa ditindaklanjuti tanpa membuka config.
    assert!(badan.contains("max_tabs"), "badan: {badan}");
    assert!(badan.contains("2 dari batas 2"), "badan: {badan}");
}

#[tokio::test]
async fn setelah_tab_ditutup_tab_baru_boleh_lagi() {
    let mesin = mesin(2).await;
    let proxy = proxy(mesin.port, Some(2)).await;

    let (status, _) = minta(proxy.port, "PUT", "/json/new").await;
    assert_eq!(status, 429, "penuh dulu");

    mesin.tutup_satu();
    let (status, badan) = minta(proxy.port, "PUT", "/json/new").await;
    assert_eq!(status, 200, "satu tab ditutup, jatahnya kembali: {badan}");
    assert_eq!(mesin.dibuat(), 1);
}

#[tokio::test]
async fn daftar_tab_tetap_dijawab_saat_batas_penuh() {
    let mesin = mesin(3).await;
    let proxy = proxy(mesin.port, Some(2)).await;

    // Batas tab tidak boleh membuat klien buta: klien yang ingin menutup tab
    // justru perlu melihat daftarnya.
    let (status, badan) = minta(proxy.port, "GET", "/json/list").await;
    assert_eq!(status, 200, "badan: {badan}");
    let daftar: Value = serde_json::from_str(&badan).expect("JSON");
    assert_eq!(daftar.as_array().expect("array").len(), 3);
}

#[tokio::test]
async fn mesin_tanpa_batas_tidak_pernah_ditolak() {
    let mesin = mesin(10).await;
    let proxy = proxy(mesin.port, None).await;

    let (status, badan) = minta(proxy.port, "PUT", "/json/new").await;
    assert_eq!(status, 200, "tanpa batas berarti tanpa penolakan: {badan}");
    assert_eq!(mesin.dibuat(), 1);
}

#[tokio::test]
async fn hitungan_yang_tidak_terbaca_tidak_memblokir_tab() {
    // Port 1 tidak ada yang mendengarkan: meniru mesin yang mati atau sedang
    // dalam keadaan aneh. Batas tidak boleh berubah menjadi "browser mati".
    let state_pool = {
        let engine: Arc<dyn Engine> = Arc::new(FakeEngine::at_port(1));
        let mut peta: BTreeMap<String, Arc<dyn Engine>> = BTreeMap::new();
        peta.insert("mesin".to_owned(), engine);
        let mut batas = BTreeMap::new();
        batas.insert("mesin".to_owned(), 1);
        EnginePool::new("mesin", peta)
            .expect("pool")
            .dengan_batas_tab(batas)
    };
    let penjaga = cdp::batas::Penjaga::dari_pool(&state_pool, "mesin").expect("penjaga");
    assert!(
        penjaga.periksa().await.is_none(),
        "hitungan tidak terbaca harus gagal-terbuka, bukan menolak"
    );
}
// AC-020 arah kedua: mesin tujuan yang TIDAK DIKENAL harus ditolak, bukan
// diteruskan ke mesin bawaan. Salah meneruskan di sini berarti `/json/close/x`
// menutup tab di mesin yang salah, dan itu kehilangan pekerjaan, bukan sekadar
// salah alamat.
#[tokio::test]
async fn mesin_tujuan_tidak_dikenal_ditolak_dan_tidak_diteruskan() {
    let mesin = mesin(2).await;
    let proxy = proxy_dua(mesin.port, None).await;

    let (status, badan) = minta(proxy.port, "GET", "/json/close/asing~P1").await;
    assert_eq!(status, 404, "mesin asing harus ditolak, dapat: {badan}");
    assert!(
        badan.contains("asing"),
        "pesan harus menyebut mesin yang diminta: {badan}"
    );
    assert!(
        badan.contains("a") && badan.contains("b"),
        "pesan harus menyebut mesin yang benar-benar terpasang: {badan}"
    );
    assert_eq!(
        mesin.halaman(),
        2,
        "permintaan ke mesin asing tidak boleh sampai ke mesin mana pun"
    );
}

type Klien = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

async fn sambung(port: u16) -> Klien {
    let (soket, _) = connect_async(format!("ws://127.0.0.1:{port}/devtools/browser/klien"))
        .await
        .expect("menyambung ke proxy");
    soket
}

/// Membaca pesan sampai balasan dengan `id` itu tiba; event mesin dilewati.
async fn tunggu(soket: &mut Klien, id: i64) -> Value {
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
        let nilai: Value = serde_json::from_str(teks.as_str()).expect("JSON");
        if nilai.get("id").and_then(Value::as_i64) == Some(id) {
            return nilai;
        }
    }
}

async fn buat_tab(soket: &mut Klien, id: i64, mesin: Option<&str>) -> Value {
    let mut params = json!({"url": "about:blank"});
    if let Some(mesin) = mesin {
        params["openCrabsOptions"] = json!({"engine": mesin});
    }
    let pesan = json!({"id": id, "method": "Target.createTarget", "params": params});
    soket
        .send(TsMessage::Text(pesan.to_string().into()))
        .await
        .expect("mengirim createTarget");
    tunggu(soket, id).await
}

fn kode(nilai: &Value) -> i64 {
    nilai["error"]["code"].as_i64().unwrap_or_default()
}

#[tokio::test]
async fn create_target_di_soket_browser_ditolak_saat_penuh() {
    let mesin = mesin(1).await;
    let proxy = proxy(mesin.port, Some(1)).await;
    let mut klien = sambung(proxy.port).await;

    let balasan = buat_tab(&mut klien, 1, None).await;
    assert_eq!(kode(&balasan), -32010, "balasan: {balasan}");
    assert!(
        balasan["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("1 dari batas 1"),
        "pesan harus menyebut angkanya: {balasan}"
    );
    assert_eq!(mesin.dibuat(), 0, "pesan tidak boleh sampai ke mesin");
    assert_eq!(mesin.halaman(), 1);
}

#[tokio::test]
async fn create_target_dilewatkan_setelah_tab_ditutup() {
    let mesin = mesin(1).await;
    let proxy = proxy(mesin.port, Some(1)).await;
    let mut klien = sambung(proxy.port).await;

    assert_eq!(kode(&buat_tab(&mut klien, 1, None).await), -32010);
    mesin.tutup_satu();

    let balasan = buat_tab(&mut klien, 2, None).await;
    assert!(
        balasan.get("error").is_none(),
        "satu tab ditutup, jatahnya kembali: {balasan}"
    );
    assert_eq!(mesin.dibuat(), 1, "permintaan kedua harus diteruskan");
}

#[tokio::test]
async fn multiplex_menolak_create_target_ke_mesin_yang_penuh() {
    let mesin = mesin(1).await;
    let proxy = proxy_dua(mesin.port, Some(1)).await;
    let mut klien = sambung(proxy.port).await;

    let balasan = buat_tab(&mut klien, 1, Some("a")).await;
    assert_eq!(kode(&balasan), -32010, "balasan: {balasan}");
    assert_eq!(mesin.dibuat(), 0, "mesin tidak boleh menerima permintaan");

    // Mesin "b" tidak punya batas: batas milik satu mesin tidak boleh menular
    // ke mesin lain.
    let balasan = buat_tab(&mut klien, 2, Some("b")).await;
    assert!(
        balasan.get("error").is_none(),
        "mesin tanpa batas tetap boleh: {balasan}"
    );
    assert_eq!(mesin.dibuat(), 1);
}
