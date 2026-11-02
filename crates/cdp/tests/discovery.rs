//! Endpoint discovery saat ada lebih dari satu mesin.
//!
//! Yang diuji di sini **lapisan HTTP**, bukan websocket: penggabungan
//! `/json/list`, terjemahan prefiks `<mesin>~<id>` di `/json/close`, penolakan
//! mesin yang tidak terpasang, dan kesehatan gabungan. Uji multiplex di jalur
//! websocket ada di `tests/multiplex.rs`.
//!
//! Mesin di sini palsu tetapi **sungguhan sebagai server HTTP**: tab-nya
//! dijawab oleh axum sungguhan, jadi yang diuji benar-benar jalur terusan
//! `forward`, bukan tiruan yang mengembalikan nilai yang diharapkan.
//!
//! Dua mesin sengaja memakai id tab yang **sama** (`TAB`). Kalau id tidak
//! diberi prefiks mesin, test pertama gagal: itulah inti masalahnya.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use cdp::{EnginePool, ProxyState};
use engine::Engine;
use serde_json::{Value, json};
use testing::FakeEngine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Id tab yang dipakai kedua mesin. Sengaja sama.
const TAB: &str = "TAB";

type Catatan = Arc<Mutex<Vec<String>>>;

/// Satu mesin Chrome palsu: cukup untuk `/json/list` dan `/json/close`.
struct Mesin {
    port: u16,
    catatan: Catatan,
    _tugas: tokio::task::JoinHandle<()>,
}

async fn mesin(id: &'static str) -> Mesin {
    let catatan: Catatan = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mesin");
    let port = listener.local_addr().expect("alamat mesin").port();

    let app = Router::new()
        .route("/json/list", get(daftar))
        .route("/json/close/{id}", get(tutup))
        .with_state(Titik {
            id,
            port,
            catatan: Arc::clone(&catatan),
        });

    let tugas = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Mesin {
        port,
        catatan,
        _tugas: tugas,
    }
}

/// Yang dibutuhkan handler mesin palsu. `port` ikut disimpan supaya URL tab
/// benar-benar memuat otoritas mesin: itulah yang harus ditulis ulang proxy.
#[derive(Clone)]
struct Titik {
    id: &'static str,
    port: u16,
    catatan: Catatan,
}

async fn daftar(State(t): State<Titik>) -> Json<Value> {
    Json(json!([{
        "id": TAB,
        "type": "page",
        "title": format!("tab milik {}", t.id),
        "webSocketDebuggerUrl": format!("ws://127.0.0.1:{}/devtools/page/{TAB}", t.port),
    }]))
}

async fn tutup(State(t): State<Titik>, Path(tab): Path<String>) -> Json<Value> {
    t.catatan
        .lock()
        .expect("catatan")
        .push(format!("{}:{tab}", t.id));
    Json(json!({ "ditutup": tab, "oleh": t.id }))
}

fn catat(mesin: &Mesin) -> Vec<String> {
    mesin.catatan.lock().expect("catatan").clone()
}

/// Proxy discovery di atas kumpulan mesin yang diberikan.
///
/// Pool dibangun dari `FakeEngine` (tanpa proses) karena lapisan ini hanya
/// butuh `http_base()`; port-nya diarahkan ke server axum di atas. `Arc`
/// mesinnya dikembalikan supaya test bisa mematikannya seperti daemon
/// mematikan proses sungguhan.
struct Proxy {
    port: u16,
    mesin: BTreeMap<&'static str, Arc<FakeEngine>>,
    _tugas: tokio::task::JoinHandle<()>,
}

async fn proxy_dengan(mesin: &[(&'static str, u16)]) -> Proxy {
    let mut peta: BTreeMap<String, Arc<dyn Engine>> = BTreeMap::new();
    let mut pegang: BTreeMap<&'static str, Arc<FakeEngine>> = BTreeMap::new();
    for (id, port) in mesin {
        let engine = Arc::new(FakeEngine::at_port(*port));
        pegang.insert(*id, Arc::clone(&engine));
        peta.insert((*id).to_owned(), engine as Arc<dyn Engine>);
    }
    let bawaan = mesin.first().map_or("a", |(id, _)| *id);
    let pool = EnginePool::new(bawaan, peta).expect("pool");

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
        mesin: pegang,
        _tugas: tugas,
    }
}

/// GET mentah. Crate ini tidak menarik klien HTTP baru hanya untuk test, dan
/// jawaban axum selalu ber-`Content-Length`, jadi membaca sampai EOF cukup.
async fn ambil(port: u16, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("menyambung ke proxy");
    let permintaan =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(permintaan.as_bytes())
        .await
        .expect("mengirim permintaan");

    let mut buf = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        stream.read_to_end(&mut buf),
    )
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

fn badan_json(badan: &str) -> Value {
    serde_json::from_str(badan).unwrap_or_else(|err| panic!("bukan JSON ({err}): {badan}"))
}

#[tokio::test]
async fn daftar_gabungan_memberi_prefiks_mesin() {
    let a = mesin("a").await;
    let b = mesin("b").await;
    let proxy = proxy_dengan(&[("a", a.port), ("b", b.port)]).await;
    let port = proxy.port;

    let (status, badan) = ambil(port, "/json/list").await;
    assert_eq!(status, 200, "badan: {badan}");

    let daftar = badan_json(&badan);
    let daftar = daftar.as_array().expect("array tab");
    assert_eq!(daftar.len(), 2, "dua mesin, dua tab: {badan}");

    let ids: Vec<&str> = daftar
        .iter()
        .filter_map(|tab| tab.get("id").and_then(Value::as_str))
        .collect();
    assert!(ids.contains(&"a~TAB"), "id mesin a berprefiks: {ids:?}");
    assert!(ids.contains(&"b~TAB"), "id mesin b berprefiks: {ids:?}");

    for tab in daftar {
        let id = tab["id"].as_str().expect("id tab");
        let url = tab
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .expect("url websocket");
        assert!(
            url.ends_with(&format!("/devtools/page/{id}")),
            "url menunjuk id berprefiks yang sama dengan field id: {url}"
        );
        assert!(
            !url.contains(&format!(":{}", a.port)) && !url.contains(&format!(":{}", b.port)),
            "port mesin tidak boleh bocor ke klien: {url}"
        );
    }
}

#[tokio::test]
async fn tutup_lewat_prefiks_hanya_ke_mesin_itu() {
    let a = mesin("a").await;
    let b = mesin("b").await;
    let proxy = proxy_dengan(&[("a", a.port), ("b", b.port)]).await;
    let port = proxy.port;

    let (status, badan) = ambil(port, "/json/close/b~TAB").await;
    assert_eq!(status, 200, "badan: {badan}");
    assert!(badan.contains("\"oleh\":\"b\""), "jawaban mesin b: {badan}");

    assert!(catat(&a).is_empty(), "mesin a tidak boleh disentuh");
    assert_eq!(catat(&b), vec!["b:TAB".to_owned()], "id asli diteruskan");
}

#[tokio::test]
async fn tab_tanpa_prefiks_diarahkan_ke_mesin_bawaan() {
    let a = mesin("a").await;
    let b = mesin("b").await;
    let proxy = proxy_dengan(&[("a", a.port), ("b", b.port)]).await;
    let port = proxy.port;

    let (status, _) = ambil(port, "/json/close/TAB").await;
    assert_eq!(status, 200);
    assert_eq!(catat(&a), vec!["a:TAB".to_owned()]);
    assert!(catat(&b).is_empty());
}

#[tokio::test]
async fn mesin_tak_dikenal_ditolak_dengan_daftar() {
    let a = mesin("a").await;
    let b = mesin("b").await;
    let proxy = proxy_dengan(&[("a", a.port), ("b", b.port)]).await;
    let port = proxy.port;

    let (status, badan) = ambil(port, "/json/close/zzz~TAB").await;
    assert_eq!(status, 404, "badan: {badan}");
    assert!(badan.contains("zzz"), "menyebut yang diminta: {badan}");
    assert!(
        badan.contains("a, b"),
        "menyebut yang tersedia, bukan diam-diam memakai bawaan: {badan}"
    );
    assert!(catat(&a).is_empty() && catat(&b).is_empty());
}

#[tokio::test]
async fn mesin_mati_dilewati_daftar_tetap_jalan() {
    let a = mesin("a").await;
    let b = mesin("b").await;
    // Port yang sudah pasti kosong: bind lalu tutup.
    let mati = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port_mati = mati.local_addr().expect("alamat").port();
    drop(mati);

    let proxy = proxy_dengan(&[("a", port_mati), ("b", b.port)]).await;
    let port = proxy.port;
    drop(a);

    let (status, badan) = ambil(port, "/json/list").await;
    assert_eq!(status, 200, "satu mesin mati bukan alasan gagal: {badan}");
    let daftar = badan_json(&badan);
    let daftar = daftar.as_array().expect("array");
    assert_eq!(daftar.len(), 1, "hanya tab mesin yang hidup: {badan}");
    assert_eq!(daftar[0]["id"], "b~TAB");
}

#[tokio::test]
async fn semua_mesin_mati_jadi_502() {
    let (port_a, port_b) = {
        let a = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let b = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let hasil = (
            a.local_addr().expect("alamat").port(),
            b.local_addr().expect("alamat").port(),
        );
        drop(a);
        drop(b);
        hasil
    };

    let proxy = proxy_dengan(&[("a", port_a), ("b", port_b)]).await;
    let port = proxy.port;
    let (status, badan) = ambil(port, "/json/list").await;
    assert_eq!(status, 502, "semua gagal: {badan}");
    assert!(badan.contains("tidak ada mesin yang menjawab"), "{badan}");
}

#[tokio::test]
async fn healthz_menyebut_tiap_mesin_dan_503_kalau_ada_yang_mati() {
    let a = mesin("a").await;
    let b = mesin("b").await;

    let proxy = proxy_dengan(&[("a", a.port), ("b", b.port)]).await;
    let port = proxy.port;
    let (status, badan) = ambil(port, "/healthz").await;
    assert_eq!(status, 200, "dua mesin hidup: {badan}");
    let nilai = badan_json(&badan);
    assert_eq!(
        nilai["engines"].as_array().map(Vec::len),
        Some(2),
        "tiap mesin dilaporkan: {badan}"
    );

    // Mematikan mesin lewat `stop()`, bukan dengan menjatuhkan servernya:
    // `is_running()` adalah keadaan proses, dan itulah yang dibaca `/healthz`.
    // Menjatuhkan server HTTP saja tidak mengubah keadaan proses.
    proxy.mesin["a"].stop().expect("mematikan mesin a");
    let (status, badan) = ambil(port, "/healthz").await;
    assert_eq!(status, 503, "satu mesin mati harus 503: {badan}");
    let nilai = badan_json(&badan);
    assert_eq!(nilai["ok"], false, "jawaban jujur: {badan}");
    assert_eq!(
        nilai["engines"][0]["running"], false,
        "mesin yang mati dilaporkan mati: {badan}"
    );
}
