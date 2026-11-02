//! Bukti multiplex dengan dua mesin sungguhan (palsu, tapi dua soket nyata).
//!
//! Yang diuji bukan peta id (itu urusan unit test di `mux::peta`), melainkan
//! **pipa-nya**: satu koneksi klien, dua soket mesin, dan tidak ada pesan yang
//! menyeberang. Setiap mesin di sini menjawab dengan namanya sendiri di
//! `sessionId` dan `frameId`, jadi tertukarnya pesan akan terlihat sebagai
//! ketidakcocokan nama, bukan sebagai timeout yang ambigu.
//!
//! Mesin palsu ini sengaja bodoh: tidak ada DOM, tidak ada navigasi. Yang
//! dibuktikan adalah penerusan dan penerjemahan identitas. Bukti dengan
//! Chromium sungguhan ada di `browserd/tests/m4_proof.rs`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::any;
use cdp::{EnginePool, ProxyState};
use engine::{Engine, EngineError, EngineInfo, LaunchSpec};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TsMessage;

const BATAS: Duration = Duration::from_secs(5);

/// Mesin uji: satu soket websocket yang menjawab seperti Chrome, sesederhana
/// mungkin, dan menandai setiap balasannya dengan namanya sendiri.
struct MesinUji {
    port: u16,
    mati: Arc<Notify>,
    tugas: JoinHandle<()>,
}

impl MesinUji {
    async fn nyalakan(id: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mesin");
        let port = listener.local_addr().expect("alamat").port();
        let mati = Arc::new(Notify::new());
        let app = Router::new()
            .route("/devtools/browser/{*rest}", any(naik))
            .with_state((id, Arc::clone(&mati)));
        let tugas = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { port, mati, tugas }
    }

    /// Memutus soket seperti mesin yang mati: koneksi ditutup, klien tidak
    /// pernah menerima close frame yang rapi.
    fn bunuh(&self) {
        self.mati.notify_waiters();
    }
}

impl Drop for MesinUji {
    fn drop(&mut self) {
        self.tugas.abort();
    }
}

async fn naik(
    ws: WebSocketUpgrade,
    State((id, mati)): State<(&'static str, Arc<Notify>)>,
) -> Response {
    ws.on_upgrade(move |socket| layani(socket, id, mati))
}

async fn layani(mut socket: WebSocket, id: &'static str, mati: Arc<Notify>) {
    loop {
        let pesan = tokio::select! {
            () = mati.notified() => break,
            pesan = socket.next() => pesan,
        };
        let Some(Ok(AxumMessage::Text(teks))) = pesan else {
            break;
        };
        let Ok(nilai) = serde_json::from_str::<Value>(teks.as_str()) else {
            continue;
        };
        let Some(id_pesan) = nilai.get("id").and_then(Value::as_i64) else {
            continue;
        };
        let method = nilai
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let sesi = nilai
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();

        // `Page.navigate` mengirim event sebelum balasan, seperti Chrome.
        // Urutan ini yang membuat test bisa membuktikan event tidak tertukar.
        if method == "Page.navigate" {
            let event = json!({
                "method": "Page.loadEventFired",
                "sessionId": sesi,
                "params": {"timestamp": 1},
            });
            if socket
                .send(AxumMessage::Text(event.to_string().into()))
                .await
                .is_err()
            {
                break;
            }
        }

        let diterima = nilai.get("params").cloned().unwrap_or(Value::Null);
        let balasan = match method {
            "Target.createTarget" => json!({"id": id_pesan, "result": {
                "targetId": format!("{id}-TARGET"),
                // Dipantulkan supaya test bisa melihat bentuk pesan yang
                // benar-benar keluar dari proxy, bukan menebaknya.
                "diterima": diterima,
            }}),
            "Target.attachToTarget" => {
                json!({"id": id_pesan, "result": {"sessionId": format!("{id}-SESI")}})
            }
            "Target.getTargets" => json!({"id": id_pesan, "result": {"targetInfos": [{
                "targetId": format!("{id}-PAGE"),
                "type": "page",
                "url": "about:blank",
            }]}}),
            "Page.navigate" => {
                json!({"id": id_pesan, "result": {"frameId": format!("{id}-FRAME")}})
            }
            _ => json!({"id": id_pesan, "result": {}}),
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

/// Mesin untuk sisi proxy: hanya alamat, tanpa proses.
struct Alamat {
    info: EngineInfo,
    spec: LaunchSpec,
}

impl Alamat {
    fn baru(id: &str, port: u16) -> Self {
        Self {
            info: EngineInfo {
                browser: format!("MesinUji/{id}"),
                protocol_version: "1.3".to_owned(),
                user_agent: "uji".to_owned(),
                v8_version: None,
                webkit_version: None,
                devtools_port: port,
                web_socket_debugger_url: format!("ws://127.0.0.1:{port}/devtools/browser/uji"),
            },
            spec: LaunchSpec::new(
                std::path::PathBuf::from("/tidak/ada"),
                std::path::PathBuf::from("/tmp/ocb-mux"),
            ),
        }
    }
}

impl Engine for Alamat {
    fn kind(&self) -> &'static str {
        "uji"
    }
    fn info(&self) -> &EngineInfo {
        &self.info
    }
    fn spec(&self) -> &LaunchSpec {
        &self.spec
    }
    fn http_base(&self) -> String {
        format!("http://127.0.0.1:{}", self.info.devtools_port)
    }
    fn web_socket_url(&self) -> &str {
        &self.info.web_socket_debugger_url
    }
    fn process_id(&self) -> Option<u32> {
        None
    }
    fn is_running(&self) -> bool {
        true
    }
    fn stop(&self) -> Result<(), EngineError> {
        Ok(())
    }
}

/// Proxy yang sudah menyala, plus dua mesin di belakangnya.
struct Rig {
    url: String,
    a: MesinUji,
    /// Tidak pernah dibaca test, tetapi harus tetap hidup selama rig hidup.
    _b: MesinUji,
    _proxy: JoinHandle<()>,
}

async fn rig() -> Rig {
    let a = MesinUji::nyalakan("a").await;
    let b = MesinUji::nyalakan("b").await;

    let mut mesin: BTreeMap<String, Arc<dyn Engine>> = BTreeMap::new();
    mesin.insert(
        "a".to_owned(),
        Arc::new(Alamat::baru("a", a.port)) as Arc<dyn Engine>,
    );
    mesin.insert(
        "b".to_owned(),
        Arc::new(Alamat::baru("b", b.port)) as Arc<dyn Engine>,
    );
    let pool = EnginePool::new("a", mesin).expect("pool dua mesin");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
    let port = listener.local_addr().expect("alamat proxy").port();
    let state = ProxyState::pool(
        pool,
        format!("ws://127.0.0.1:{port}/devtools/browser/klien"),
    );
    let app = cdp::router(Arc::new(state));
    let proxy = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Rig {
        url: format!("ws://127.0.0.1:{port}/devtools/browser/klien"),
        a,
        _b: b,
        _proxy: proxy,
    }
}

type Klien =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn klien(rig: &Rig) -> Klien {
    let (soket, _) = connect_async(rig.url.as_str()).await.expect("menyambung");
    soket
}

async fn kirim(soket: &mut Klien, nilai: Value) {
    soket
        .send(TsMessage::Text(nilai.to_string().into()))
        .await
        .expect("mengirim");
}

/// Membaca pesan sampai predikatnya cocok. Pesan lain dilewati, karena event
/// mesin boleh datang kapan saja dan urutannya tidak dijanjikan.
async fn tunggu(soket: &mut Klien, f: impl Fn(&Value) -> bool) -> Value {
    let batas = tokio::time::Instant::now() + BATAS;
    loop {
        let sisa = batas.saturating_duration_since(tokio::time::Instant::now());
        let pesan = tokio::time::timeout(sisa, soket.next())
            .await
            .expect("waktu habis menunggu pesan")
            .expect("soket tertutup")
            .expect("pesan rusak");
        let TsMessage::Text(teks) = pesan else {
            continue;
        };
        let nilai: Value = serde_json::from_str(teks.as_str()).expect("json");
        if f(&nilai) {
            return nilai;
        }
    }
}

async fn buka_tab(soket: &mut Klien, id: i64, mesin: &str) -> (String, String) {
    kirim(
        soket,
        json!({"id": id, "method": "Target.createTarget", "params": {
            "url": "about:blank",
            "openCrabsOptions": {"engine": mesin},
        }}),
    )
    .await;
    let balasan = tunggu(soket, |n| n["id"] == id).await;
    let tid = balasan["result"]["targetId"]
        .as_str()
        .expect("targetId")
        .to_owned();

    kirim(
        soket,
        json!({"id": id + 100, "method": "Target.attachToTarget", "params": {"targetId": tid}}),
    )
    .await;
    let balasan = tunggu(soket, |n| n["id"] == id + 100).await;
    let sid = balasan["result"]["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_owned();
    (tid, sid)
}

#[tokio::test]
async fn dua_tab_dua_mesin_tidak_tertukar() {
    let rig = rig().await;
    let mut soket = klien(&rig).await;

    let (tid_a, sid_a) = buka_tab(&mut soket, 1, "a").await;
    let (tid_b, sid_b) = buka_tab(&mut soket, 2, "b").await;
    assert_ne!(tid_a, tid_b, "dua mesin tidak boleh berbagi targetId");
    assert_ne!(sid_a, sid_b, "dua mesin tidak boleh berbagi sessionId");

    // Navigasi tab A: event yang kembali harus milik sesi A, bukan sesi B.
    kirim(
        &mut soket,
        json!({"id": 10, "method": "Page.navigate", "sessionId": sid_a, "params": {"url": "a"}}),
    )
    .await;
    let event = tunggu(&mut soket, |n| n["method"] == "Page.loadEventFired").await;
    assert_eq!(
        event["sessionId"],
        sid_a.as_str(),
        "event mendarat di sesi yang salah"
    );

    let balasan = tunggu(&mut soket, |n| n["id"] == 10).await;
    assert_eq!(
        balasan["result"]["frameId"], "a-FRAME",
        "balasan datang dari mesin yang salah"
    );

    // Dan sebaliknya untuk tab B.
    kirim(
        &mut soket,
        json!({"id": 11, "method": "Page.navigate", "sessionId": sid_b, "params": {"url": "b"}}),
    )
    .await;
    let event = tunggu(&mut soket, |n| n["method"] == "Page.loadEventFired").await;
    assert_eq!(event["sessionId"], sid_b.as_str());
    let balasan = tunggu(&mut soket, |n| n["id"] == 11).await;
    assert_eq!(balasan["result"]["frameId"], "b-FRAME");
}

#[tokio::test]
async fn mesin_mati_hanya_melepas_sesi_miliknya() {
    let rig = rig().await;
    let mut soket = klien(&rig).await;
    let (_, sid_a) = buka_tab(&mut soket, 1, "a").await;
    let (tid_b, sid_b) = buka_tab(&mut soket, 2, "b").await;

    rig.a.bunuh();

    let lepas = tunggu(&mut soket, |n| n["method"] == "Target.detachedFromTarget").await;
    assert_eq!(
        lepas["params"]["sessionId"],
        sid_a.as_str(),
        "sesi mesin mati yang dilepas"
    );
    assert_ne!(
        lepas["params"]["sessionId"],
        sid_b.as_str(),
        "sesi mesin hidup tidak boleh dilepas"
    );

    // Tab di mesin B harus tetap bekerja setelah mesin A pergi.
    kirim(
        &mut soket,
        json!({"id": 20, "method": "Page.navigate", "sessionId": sid_b, "params": {"url": "b"}}),
    )
    .await;
    let balasan = tunggu(&mut soket, |n| n["id"] == 20).await;
    assert_eq!(balasan["result"]["frameId"], "b-FRAME", "mesin B ikut mati");
    assert!(
        tid_b.starts_with('t'),
        "targetId klien, bukan id mesin: {tid_b}"
    );
}

#[tokio::test]
async fn sesi_karangan_dijawab_bukan_ditelan() {
    let rig = rig().await;
    let mut soket = klien(&rig).await;

    kirim(
        &mut soket,
        json!({"id": 30, "method": "Page.navigate", "sessionId": "s99", "params": {}}),
    )
    .await;
    let balasan = tunggu(&mut soket, |n| n["id"] == 30).await;
    assert_eq!(
        balasan["error"]["code"],
        cdp::mux::KODE_SESI_TAK_DIKENAL,
        "sesi tak dikenal harus dijawab, bukan didiamkan: {balasan}"
    );
}

#[tokio::test]
async fn get_targets_digabung_dari_dua_mesin() {
    let rig = rig().await;
    let mut soket = klien(&rig).await;

    kirim(&mut soket, json!({"id": 40, "method": "Target.getTargets"})).await;
    let balasan = tunggu(&mut soket, |n| n["id"] == 40).await;
    let infos = balasan["result"]["targetInfos"].as_array().expect("array");
    assert_eq!(infos.len(), 2, "satu target per mesin: {balasan}");

    // Dua targetId berbeda membuktikan penggabungan tidak menimpa salah satu.
    let ids: Vec<&str> = infos
        .iter()
        .filter_map(|i| i["targetId"].as_str())
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
}

#[tokio::test]
async fn set_discover_targets_dijawab_sekali() {
    let rig = rig().await;
    let mut soket = klien(&rig).await;

    kirim(
        &mut soket,
        json!({"id": 50, "method": "Target.setDiscoverTargets", "params": {"discover": true}}),
    )
    .await;
    tunggu(&mut soket, |n| n["id"] == 50).await;

    // Balasan kedua untuk id yang sama akan membuat klien CDP menganggap
    // protokolnya rusak, jadi kelebihannya harus dibuang.
    let sisa = tokio::time::timeout(Duration::from_millis(300), soket.next()).await;
    if let Ok(Some(Ok(TsMessage::Text(teks)))) = sisa {
        let nilai: Value = serde_json::from_str(teks.as_str()).expect("json");
        assert_ne!(
            nilai["id"], 50,
            "balasan siaran diteruskan dua kali: {nilai}"
        );
    }
}

#[tokio::test]
async fn mesin_yang_tidak_terpasang_ditolak_dengan_nama() {
    let rig = rig().await;
    let mut soket = klien(&rig).await;

    kirim(
        &mut soket,
        json!({"id": 60, "method": "Target.createTarget", "params": {
            "url": "about:blank",
            "openCrabsOptions": {"engine": "oxi"},
        }}),
    )
    .await;
    let balasan = tunggu(&mut soket, |n| n["id"] == 60).await;
    let pesan = balasan["error"]["message"].as_str().unwrap_or_default();
    assert!(
        pesan.contains("oxi"),
        "pesan harus menyebut mesin yang diminta: {pesan}"
    );
    assert!(
        pesan.contains("a, b"),
        "pesan harus menyebut yang tersedia: {pesan}"
    );
}

#[tokio::test]
async fn opencrabs_options_tidak_sampai_ke_mesin() {
    let rig = rig().await;
    let mut soket = klien(&rig).await;

    kirim(
        &mut soket,
        json!({"id": 70, "method": "Target.createTarget", "params": {
            "url": "about:blank",
            "openCrabsOptions": {"engine": "b", "profile": "toko", "ttlSecs": 60},
        }}),
    )
    .await;
    let balasan = tunggu(&mut soket, |n| n["id"] == 70).await;

    // Mesin memantulkan params yang diterimanya: di sinilah terlihat bahwa
    // opsi internal dibuang sebelum keluar, bukan cuma dihilangkan dari
    // dokumen.
    let diterima = &balasan["result"]["diterima"];
    assert_eq!(
        diterima["url"], "about:blank",
        "params sah harus tetap sampai"
    );
    assert!(
        diterima.get("openCrabsOptions").is_none(),
        "opsi internal bocor ke mesin: {diterima}"
    );
}

#[tokio::test]
async fn tutup_tab_mesin_a_tidak_memengaruhi_tab_mesin_b() {
    // Skenario roadmap §6: "tutup salah satu tidak memengaruhi yang lain".
    // Ditutup lewat `Target.closeTarget` karena itu jalur yang dipakai klien
    // OpenCrabs, bukan `/json/close` (jalur HTTP-nya diuji di tests/discovery.rs).
    let rig = rig().await;
    let mut soket = klien(&rig).await;
    let (tid_a, _) = buka_tab(&mut soket, 1, "a").await;
    let (_, sid_b) = buka_tab(&mut soket, 2, "b").await;

    kirim(
        &mut soket,
        json!({"id": 40, "method": "Target.closeTarget", "params": {"targetId": tid_a}}),
    )
    .await;
    let balasan = tunggu(&mut soket, |n| n["id"] == 40).await;
    assert!(
        balasan["error"].is_null(),
        "menutup tab mesin A harus sukses, bukan ditolak: {balasan}"
    );

    // Tab B tetap menjawab sesudah tab A hilang.
    kirim(
        &mut soket,
        json!({"id": 41, "method": "Page.navigate", "sessionId": sid_b, "params": {"url": "b"}}),
    )
    .await;
    let balasan = tunggu(&mut soket, |n| n["id"] == 41).await;
    assert_eq!(
        balasan["result"]["frameId"], "b-FRAME",
        "tab mesin B ikut terpengaruh: {balasan}"
    );
}
