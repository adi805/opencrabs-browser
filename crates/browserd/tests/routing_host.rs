//! AC-007 di jalur nyata: aturan host menentukan mesin lewat **proxy**, bukan
//! lewat unit test peta.
//!
//! Yang diuji bukan peta id (itu `cdp::mux::peta::uji`) melainkan penyambungannya:
//! satu klien, dua mesin dengan **jenis berbeda**, dan satu aturan host dari
//! config. Mesin palsu di sini menandai balasannya dengan namanya sendiri, jadi
//! tab yang mendarat di mesin yang salah terlihat sebagai nama yang salah, bukan
//! sebagai timeout yang ambigu.
//!
//! Tanpa Chromium: yang diperiksa routing, bukan rendering. Bukti dengan browser
//! sungguhan ada di `browserd/tests/m4_proof.rs`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::any;
use browserd::routing::PemilihPolicy;
use cdp::mux::KODE_DITOLAK;
use cdp::{EnginePool, ProxyState};
use engine::{Engine, EngineError, EngineInfo, LaunchSpec};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TsMessage;

const BATAS: Duration = Duration::from_secs(5);

/// Satu aturan host yang mengarahkan `toko.test` ke mesin jenis `chrome-full`.
/// Ditulis dalam bentuk config nyata, lalu dibaca lewat [`FileConfig`] supaya
/// yang diuji juga bentuk berkasnya, bukan cuma struct-nya.
const ATURAN_TOKO: &str =
    "[policy]\ndefault = \"chrome-shell\"\n\n[policy.rules]\n\"toko.test\" = \"chrome-full\"\n";

/// Mesin palsu: satu soket websocket yang menjawab seperti Chrome dan menandai
/// setiap balasannya dengan namanya sendiri.
struct MesinUji {
    port: u16,
    tugas: JoinHandle<()>,
}

impl MesinUji {
    async fn nyalakan(id: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mesin");
        let port = listener.local_addr().expect("alamat").port();
        let app = Router::new()
            .route("/devtools/browser/{*rest}", any(naik))
            .with_state(id);
        let tugas = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { port, tugas }
    }
}

impl Drop for MesinUji {
    fn drop(&mut self) {
        self.tugas.abort();
    }
}

async fn naik(ws: WebSocketUpgrade, State(id): State<&'static str>) -> Response {
    ws.on_upgrade(move |socket| layani(socket, id))
}

async fn layani(mut socket: WebSocket, id: &'static str) {
    while let Some(Ok(AxumMessage::Text(teks))) = socket.next().await {
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
        // Nama mesin masuk ke targetId: itu satu-satunya cara test tahu mesin
        // mana yang benar-benar menerima permintaan.
        let balasan = match method {
            "Target.createTarget" => json!({"id": id_pesan, "result": {
                "targetId": format!("{id}-TARGET"),
                // Proxy mengganti `targetId` dengan id router (`t1`), jadi nama
                // mesin ikut di field lain yang tidak disentuh peta. Tanpa ini
                // test tidak bisa tahu mesin mana yang menjawab.
                "mesin": id,
            }}),
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

/// Mesin untuk sisi pool: alamat saja, tanpa proses.
struct Alamat {
    jenis: &'static str,
    info: EngineInfo,
    spec: LaunchSpec,
}

impl Alamat {
    fn baru(id: &str, jenis: &'static str, port: u16) -> Self {
        Self {
            jenis,
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
                std::path::PathBuf::from("/tmp/ocb-routing"),
            ),
        }
    }
}

impl Engine for Alamat {
    fn kind(&self) -> &'static str {
        self.jenis
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

/// Proxy yang sudah menyala, dua mesin di belakangnya, satu aturan host.
struct Rig {
    url: String,
    _shell: MesinUji,
    _full: MesinUji,
    _proxy: JoinHandle<()>,
}

/// Rig dengan mesin `shell` (jenis `chrome-shell`) dan `full` (jenis
/// `chrome-full`). `terpasang` menentukan jenis yang dilihat `[policy]`, jadi
/// rig bisa menyerupai daemon yang hanya memasang sebagian mesin.
async fn rig(aturan: &str, terpasang: &[&str]) -> Rig {
    let shell = MesinUji::nyalakan("shell").await;
    let full = MesinUji::nyalakan("full").await;

    let mut mesin: BTreeMap<String, Arc<dyn Engine>> = BTreeMap::new();
    for (id, jenis, port) in [
        ("shell", "chrome-shell", shell.port),
        ("full", "chrome-full", full.port),
    ] {
        mesin.insert(
            id.to_owned(),
            Arc::new(Alamat::baru(id, jenis, port)) as Arc<dyn Engine>,
        );
    }
    let pool = EnginePool::new("shell", mesin).expect("pool dua mesin");

    let jenis: Vec<String> = terpasang.iter().map(|jenis| (*jenis).to_owned()).collect();
    let file: browserd::config::FileConfig = toml::from_str(aturan).expect("config uji");
    let pemilih = PemilihPolicy::dari_jenis(file.policy(), &jenis);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
    let port = listener.local_addr().expect("alamat proxy").port();
    let state = ProxyState::pool(
        pool,
        format!("ws://127.0.0.1:{port}/devtools/browser/klien"),
    )
    .with_pemilih(pemilih);
    let app = cdp::router(Arc::new(state));
    let proxy = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Rig {
        url: format!("ws://127.0.0.1:{port}/devtools/browser/klien"),
        _shell: shell,
        _full: full,
        _proxy: proxy,
    }
}

type Klien =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn klien(rig: &Rig) -> Klien {
    let (soket, _) = connect_async(rig.url.as_str()).await.expect("menyambung");
    soket
}

/// Membuka tab dengan url tertentu, tanpa `openCrabsOptions` kecuali diminta.
/// Mengembalikan balasan mesin apa adanya supaya test bisa memeriksa mesinnya.
async fn buka(soket: &mut Klien, url: &str, mesin: Option<&str>) -> Value {
    let mut params = json!({"url": url});
    if let Some(mesin) = mesin {
        params["openCrabsOptions"] = json!({"engine": mesin});
    }
    let permintaan = json!({"id": 1, "method": "Target.createTarget", "params": params});
    soket
        .send(TsMessage::Text(permintaan.to_string().into()))
        .await
        .expect("mengirim");

    // Event mesin boleh datang kapan saja; yang dicari balasan id 1.
    loop {
        let pesan = tokio::time::timeout(BATAS, soket.next())
            .await
            .expect("balasan tidak datang sampai batas")
            .expect("soket terbuka")
            .expect("pesan sah");
        let TsMessage::Text(teks) = pesan else {
            continue;
        };
        let nilai: Value = serde_json::from_str(&teks).expect("balasan json");
        if nilai.get("id").and_then(Value::as_i64) == Some(1) {
            return nilai;
        }
    }
}

fn mesin_dari(balasan: &Value) -> String {
    balasan["result"]["mesin"]
        .as_str()
        .unwrap_or_else(|| panic!("balasan tidak menyebut mesinnya: {balasan}"))
        .to_owned()
}

#[tokio::test]
async fn host_beraturan_mendarat_di_instance_yang_ditunjuk() {
    let rig = rig(ATURAN_TOKO, &["chrome-shell", "chrome-full"]).await;
    let mut soket = klien(&rig).await;

    let balasan = buka(&mut soket, "http://toko.test/halaman", None).await;
    assert_eq!(
        mesin_dari(&balasan),
        "full",
        "aturan chrome-full harus mendarat di instance jenis itu: {balasan}"
    );
}

#[tokio::test]
async fn host_tanpa_aturan_memakai_mesin_bawaan() {
    let rig = rig(ATURAN_TOKO, &["chrome-shell", "chrome-full"]).await;
    let mut soket = klien(&rig).await;

    let balasan = buka(&mut soket, "http://lain.test/", None).await;
    assert_eq!(mesin_dari(&balasan), "shell", "{balasan}");
}

#[tokio::test]
async fn permintaan_eksplisit_klien_menang_atas_aturan_host() {
    let rig = rig(ATURAN_TOKO, &["chrome-shell", "chrome-full"]).await;
    let mut soket = klien(&rig).await;

    let balasan = buka(&mut soket, "http://toko.test/", Some("shell")).await;
    assert_eq!(
        mesin_dari(&balasan),
        "shell",
        "mesin yang diminta klien tidak boleh ditimpa aturan: {balasan}"
    );
}

#[tokio::test]
async fn aturan_ke_jenis_yang_tidak_terpasang_dijawab_galat_bukan_diam() {
    // Daemon hanya memasang `chrome-shell`, sedangkan aturannya menuntut
    // `chrome-full`. Klien harus menerima galat yang menyebut jenisnya, bukan
    // tab di mesin bawaan.
    let rig = rig(ATURAN_TOKO, &["chrome-shell"]).await;
    let mut soket = klien(&rig).await;

    let balasan = buka(&mut soket, "http://toko.test/", None).await;
    assert_eq!(
        balasan["error"]["code"].as_i64(),
        Some(KODE_DITOLAK),
        "harus ditolak, bukan dilayani mesin lain: {balasan}"
    );
    let pesan = balasan["error"]["message"].as_str().unwrap_or_default();
    assert!(
        pesan.contains("chrome-full"),
        "pesan harus menyebut jenis yang hilang: {pesan}"
    );
}
