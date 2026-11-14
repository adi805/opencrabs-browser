//! Mesin native sebagai [`engine::Engine`], lengkap dengan server CDP-nya.
//!
//! Mesin ini tidak punya proses anak. Chrome, chrome-headless-shell, dan oxi
//! semuanya lahir sebagai proses terpisah yang di-proxy daemon; mesin native
//! hidup **di dalam** proses daemon dan menjawab protokolnya sendiri di port
//! debug-nya sendiri. Karena itu [`Engine::process_id`] mengembalikan `None`:
//! mengembalikan pid daemon akan membuat reaper mengirim SIGKILL ke daemon itu
//! sendiri.
//!
//! Yang dijual di sini hanya yang benar-benar jalan. Mesin ini **menerima**
//! `Input.dispatchMouseEvent` dan menjalankan pendengar halaman yang
//! didaftarkan skrip lewat `addEventListener`, dan sejak W8 juga **mengirim**
//! peristiwa CDP ke klien: `Runtime.consoleAPICalled` dan
//! `Runtime.executionContextCreated` sesudah `Runtime.enable`,
//! `Page.loadEventFired` sesudah `Page.enable`, serta
//! `Network.requestWillBeSent`/`Network.responseReceived` untuk dokumen yang
//! benar-benar diambil dari jaringan (lihat [`peristiwa`]). Peristiwa hanya
//! dikirim untuk domain yang sudah di-`enable` oleh sesi itu, dan sisanya
//! dijawab dengan galat yang menyebut lapisannya, bukan dengan jawaban kosong
//! yang terlihat seperti sukses.
//!
//! Dokumen `http`/`https` diambil lewat [`jaringan`] **sebelum** kunci state
//! dipegang: satu navigasi yang lambat tidak boleh membekukan tab lain.

pub mod ambil;
pub mod gaya;
pub mod jaringan;
pub mod peristiwa;
pub mod state;

pub use ambil::{KebijakanJaringan, Keputusan, PengambilJaringan};

pub use state::{
    ColdStoreObj, DokumenAwal, Galat, LembarJaringan, MAKS_TAB, NativeState, PabrikDingin, Sidik,
    banding, dokumen_awal, pabrik_memori, percent_decode,
};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::Uri;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use engine::native::ambil::Pengambil;
use engine::{Engine, EngineError, EngineInfo, LaunchSpec, Penggerak};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::oneshot;

/// Nama jenis mesin, dipakai `[engines.<id>] kind` di config.
pub const KIND: &str = "rust-native";

/// Identitas yang dilaporkan ke klien. Inilah string yang dicari AC-013 di
/// `/json/version`.
pub const PRODUK: &str = "RustNativeEngine/0.2.0";

/// Versi protokol yang dijawab mesin ini.
pub const PROTOKOL: &str = "1.3";

/// User agent yang dilaporkan. Bukan penyamaran: mesin ini memang bukan Chrome,
/// dan menyamar sebagai Chrome akan jadi klaim palsu di lapisan identitas.
pub const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) RustNativeEngine/0.2.0";

/// State yang dibagi ke seluruh handler HTTP.
#[derive(Clone)]
struct App {
    state: NativeState,
    ws_browser: String,
    /// `host:port` milik server ini, untuk menyusun URL soket per halaman.
    authority: String,
}

/// Mesin native: state tab di memori, server CDP di task tokio.
pub struct NativeEngine {
    info: EngineInfo,
    spec: LaunchSpec,
    state: NativeState,
    http: String,
    ws: String,
    hidup: Arc<AtomicBool>,
    tutup: Mutex<Option<oneshot::Sender<()>>>,
    tugas: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl NativeEngine {
    /// Menyalakan server CDP di `spec.port` (`0` = port bebas).
    ///
    /// Sinkron, seperti penggerak lain, tapi butuh runtime tokio yang sedang
    /// aktif karena servernya hidup di dalam proses daemon. Dipanggil dari
    /// `spawn_blocking`, `Handle::current()` tetap tersedia.
    pub fn start(spec: LaunchSpec) -> Result<Self, EngineError> {
        Self::start_dengan(spec, pabrik_memori(), None)
    }

    /// Sama dengan [`Self::start`], tapi penyimpanan dingin dan gerbang
    /// jaringannya dipilih pemanggil.
    ///
    /// Daemon memakai ini supaya tab yang disingkirkan mendarat di SQLite dan
    /// `fetch()` di dalam isolat lewat kebijakannya. Tanpa `pengambil`, setiap
    /// tab memakai [`engine::native::ambil::PengambilTolak`]: `fetch` menolak
    /// semua dengan alasan, bukan diam-diam mengizinkan.
    pub fn start_dengan(
        spec: LaunchSpec,
        pabrik: PabrikDingin,
        pengambil: Option<Arc<dyn Pengambil>>,
    ) -> Result<Self, EngineError> {
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            EngineError::Io(std::io::Error::other(
                "mesin native butuh runtime tokio: server CDP-nya hidup di dalam proses daemon",
            ))
        })?;

        let listener = std::net::TcpListener::bind(("127.0.0.1", spec.port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let authority = format!("127.0.0.1:{port}");
        let http = format!("http://{authority}");
        let ws = format!("ws://{authority}/devtools/browser");

        let state = match pengambil {
            Some(pengambil) => NativeState::dengan_ambil(pabrik, pengambil),
            None => NativeState::dengan_dingin(pabrik),
        };
        let app = router(App {
            state: state.clone(),
            ws_browser: ws.clone(),
            authority,
        });

        let (tutup, tunggu) = oneshot::channel();
        let hidup = Arc::new(AtomicBool::new(true));
        let hidup_tugas = Arc::clone(&hidup);
        let tugas = handle.spawn(async move {
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(listener) => listener,
                Err(err) => {
                    tracing::error!(%err, "listener mesin native tidak bisa dipasang");
                    hidup_tugas.store(false, Ordering::SeqCst);
                    return;
                }
            };
            let hasil = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = tunggu.await;
                })
                .await;
            if let Err(err) = hasil {
                tracing::error!(%err, "server CDP native berhenti dengan galat");
            }
            hidup_tugas.store(false, Ordering::SeqCst);
        });

        let info = EngineInfo {
            browser: PRODUK.to_owned(),
            protocol_version: PROTOKOL.to_owned(),
            user_agent: USER_AGENT.to_owned(),
            // QuickJS bukan V8 dan bukan WebKit. Mengosongkan kedua kolom itu
            // lebih jujur daripada menaruh versi yang tidak dipakai.
            v8_version: None,
            webkit_version: None,
            devtools_port: port,
            web_socket_debugger_url: ws.clone(),
        };

        Ok(Self {
            info,
            spec,
            state,
            http,
            ws,
            hidup,
            tutup: Mutex::new(Some(tutup)),
            tugas: Mutex::new(Some(tugas)),
        })
    }

    /// State tab, untuk tes dan untuk pemanggil yang ingin bicara langsung.
    #[must_use]
    pub fn state(&self) -> &NativeState {
        &self.state
    }
}

impl Engine for NativeEngine {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn info(&self) -> &EngineInfo {
        &self.info
    }

    fn spec(&self) -> &LaunchSpec {
        &self.spec
    }

    fn http_base(&self) -> String {
        self.http.clone()
    }

    fn web_socket_url(&self) -> &str {
        &self.ws
    }

    /// Selalu `None`: mesin ini tidak punya proses anak.
    fn process_id(&self) -> Option<u32> {
        None
    }

    fn is_running(&self) -> bool {
        self.hidup.load(Ordering::SeqCst)
    }

    fn stop(&self) -> Result<(), EngineError> {
        if let Some(tutup) = self.kunci(&self.tutup).take() {
            let _ = tutup.send(());
        }
        if let Some(tugas) = self.kunci(&self.tugas).take() {
            tugas.abort();
        }
        self.hidup.store(false, Ordering::SeqCst);
        Ok(())
    }
}

impl NativeEngine {
    fn kunci<'a, T>(&self, isi: &'a Mutex<T>) -> std::sync::MutexGuard<'a, T> {
        isi.lock().unwrap_or_else(|err| err.into_inner())
    }
}

/// Penggerak `kind = "rust-native"`.
///
/// Membawa pabrik penyimpanan dingin: daemon menyerahkan adapter SQLite di
/// sini, sehingga tab yang disingkirkan selamat dari restart. Tanpa itu pool
/// memakai penyimpanan di memori, yang hanya benar untuk tes.
#[derive(Clone)]
pub struct PenggerakNative {
    dingin: PabrikDingin,
    pengambil: Option<Arc<dyn Pengambil>>,
}

impl Default for PenggerakNative {
    fn default() -> Self {
        Self::baru(pabrik_memori())
    }
}

impl PenggerakNative {
    /// Penggerak tanpa gerbang jaringan: `fetch()` menolak semua.
    ///
    /// Dipakai tes dan daemon yang tidak mengonfigurasi `[fetch]`. Menolak,
    /// bukan mengizinkan, karena daemon yang lupa dikonfigurasi tidak boleh
    /// menjadi proxy terbuka.
    #[must_use]
    pub fn baru(dingin: PabrikDingin) -> Self {
        Self {
            dingin,
            pengambil: None,
        }
    }

    /// Penggerak dengan gerbang jaringan untuk `fetch()` di dalam isolat.
    #[must_use]
    pub fn dengan_ambil(dingin: PabrikDingin, pengambil: Arc<dyn Pengambil>) -> Self {
        Self {
            dingin,
            pengambil: Some(pengambil),
        }
    }
}

impl std::fmt::Debug for PenggerakNative {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PenggerakNative")
    }
}

impl Penggerak for PenggerakNative {
    fn nyalakan(
        &self,
        spec: &LaunchSpec,
        _timeout_ms: u64,
    ) -> Result<Arc<dyn Engine>, EngineError> {
        // `spec.binary` diabaikan dengan sengaja: tidak ada biner yang
        // dieksekusi. Kolom itu tetap ada karena kontrak `LaunchSpec` dipakai
        // bersama semua mesin.
        NativeEngine::start_dengan(
            spec.clone(),
            Arc::clone(&self.dingin),
            self.pengambil.as_ref().map(Arc::clone),
        )
        .map(|engine| Arc::new(engine) as Arc<dyn Engine>)
    }
}

fn router(app: App) -> Router {
    Router::new()
        .route("/json/version", get(versi))
        .route("/json", get(daftar))
        .route("/json/list", get(daftar))
        .route("/json/new", any(target_baru))
        .route("/json/activate/{id}", any(aktifkan))
        .route("/json/close/{id}", any(tutup_target))
        .route("/json/protocol", get(protokol))
        .route("/devtools/browser", get(ws_browser))
        .route("/devtools/page/{id}", get(ws_halaman))
        .route("/healthz", get(sehat))
        .with_state(app)
}

async fn versi(State(app): State<App>) -> Json<Value> {
    Json(json!({
        "Browser": PRODUK,
        "Protocol-Version": PROTOKOL,
        "User-Agent": USER_AGENT,
        "V8-Version": "",
        "WebKit-Version": "",
        "webSocketDebuggerUrl": app.ws_browser,
    }))
}

async fn daftar(State(app): State<App>) -> Json<Value> {
    let items = app
        .state
        .target()
        .into_iter()
        .map(|mut target| {
            if let Some(obj) = target.as_object_mut() {
                let id = obj
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                obj.insert(
                    "webSocketDebuggerUrl".to_owned(),
                    json!(format!("ws://{}/devtools/page/{id}", app.authority)),
                );
                obj.insert("devtoolsFrontendUrl".to_owned(), json!(""));
            }
            target
        })
        .collect::<Vec<_>>();
    Json(Value::Array(items))
}

/// `PUT /json/new?<url>`. Chrome menerima URL telanjang di query, bukan
/// `?url=`, jadi dua bentuk itu diterima.
async fn target_baru(State(app): State<App>, uri: Uri) -> Response {
    let url = match uri.query() {
        Some(query) if query.contains('=') => query
            .split('&')
            .find_map(|bagian| bagian.strip_prefix("url="))
            .map(percent_decode)
            .unwrap_or_default(),
        Some(query) => percent_decode(query),
        None => String::new(),
    };
    let (ambilan, gaya) = match ambil_untuk_url(&url).await {
        Some(j) => (Some(j.ambilan), j.gaya),
        None => (None, LembarJaringan::default()),
    };
    match app.state.buka_dengan(&url, ambilan, gaya) {
        Ok(id) => Json(json!({
            "id": id,
            "type": "page",
            "title": "",
            "url": url,
            "webSocketDebuggerUrl": format!("ws://{}/devtools/page/{id}", app.authority),
        }))
        .into_response(),
        Err(galat) => jawab_galat(galat),
    }
}

async fn aktifkan(State(app): State<App>, Path(id): Path<String>) -> Response {
    match app.state.aktifkan(&id) {
        Ok(()) => (axum::http::StatusCode::OK, "Target activated").into_response(),
        Err(galat) => jawab_galat(galat),
    }
}

async fn tutup_target(State(app): State<App>, Path(id): Path<String>) -> Response {
    match app.state.tutup(&id) {
        Ok(()) => (axum::http::StatusCode::OK, "Target is closing").into_response(),
        Err(galat) => jawab_galat(galat),
    }
}

/// Daftar metode yang **benar-benar** dijawab mesin ini.
///
/// Bukan salinan `browser_protocol.json` Chrome: menyajikan skema penuh untuk
/// domain yang tidak ada akan membuat klien mencoba metode yang pasti gagal.
async fn protokol() -> Json<Value> {
    Json(json!({
        "version": { "major": "1", "minor": "3" },
        "note": "skema ini hanya memuat metode yang dijawab RustNativeEngine",
        "domains": [
            { "domain": "Browser", "commands": [{ "name": "getVersion" }] },
            { "domain": "Target", "commands": [
                { "name": "getTargets" },
                { "name": "createTarget" },
                { "name": "closeTarget" },
                { "name": "attachToTarget" },
                { "name": "detachFromTarget" },
                { "name": "activateTarget" },
                { "name": "setDiscoverTargets" },
                { "name": "setAutoAttach" }
            ]},
            { "domain": "Page", "commands": [
                { "name": "navigate" },
                { "name": "reload" },
                { "name": "setDocumentContent" },
                { "name": "getFrameTree" },
                { "name": "bringToFront" },
                { "name": "captureScreenshot" },
                { "name": "enable" },
                { "name": "disable" }
            ]},
            { "domain": "DOM", "commands": [
                { "name": "getDocument" },
                { "name": "querySelector" },
                { "name": "querySelectorAll" },
                { "name": "getOuterHTML" },
                { "name": "resolveNode" },
                { "name": "enable" },
                { "name": "disable" }
            ]},
            { "domain": "Runtime", "commands": [
                { "name": "evaluate" },
                { "name": "enable" },
                { "name": "disable" }
            ]},
            { "domain": "Input", "commands": [
                { "name": "dispatchMouseEvent" }
            ]},
            { "domain": "Log", "commands": [{ "name": "enable" }, { "name": "clear" }]},
            { "domain": "Accessibility", "commands": [
                { "name": "getFullAXTree" }
            ]}
        ]
    }))
}

async fn sehat(State(app): State<App>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "kind": KIND,
        "tabs": app.state.jumlah_tab(),
    }))
}

async fn ws_browser(ws: WebSocketUpgrade, State(app): State<App>) -> Response {
    ws.on_upgrade(move |socket| layani(socket, app.state, None))
}

async fn ws_halaman(
    ws: WebSocketUpgrade,
    State(app): State<App>,
    Path(id): Path<String>,
) -> Response {
    match app.state.sesi_halaman(&id) {
        Ok(sesi) => ws.on_upgrade(move |socket| layani(socket, app.state, Some(sesi))),
        Err(galat) => jawab_galat(galat),
    }
}

/// Satu sesi websocket: satu frame masuk, satu jawaban keluar.
async fn layani(socket: WebSocket, state: NativeState, sesi: Option<String>) {
    let (mut tx, mut rx) = socket.split();
    while let Some(pesan) = rx.next().await {
        let teks = match pesan {
            Ok(AxumMessage::Text(teks)) => teks.as_str().to_owned(),
            Ok(AxumMessage::Close(_)) | Err(_) => break,
            Ok(_) => continue,
        };
        // Jaringan diambil di sini, sebelum kunci state: frame yang tidak
        // menavigasi ke `http`/`https` tidak menyentuh jaringan sama sekali.
        let ambilan = ambil_untuk_frame(&teks).await;
        let (dokumen, gaya) = match ambilan {
            Some(j) => (Some(j.ambilan), j.gaya),
            None => (None, LembarJaringan::default()),
        };
        let jawaban = state.tangani_ambil(&teks, sesi.as_deref(), dokumen, gaya);
        // Peristiwa dikirim sebelum balasan: klien yang menunggu balasan
        // sebuah `evaluate` tidak boleh kehilangan `console.log` yang lahir
        // dari evaluasi itu sendiri, dan urutan ini membuat satu putaran
        // baca sudah memuat keduanya.
        for peristiwa in state.ambil_peristiwa(sesi.as_deref()) {
            if tx.send(AxumMessage::Text(peristiwa.into())).await.is_err() {
                return;
            }
        }
        if let Some(jawaban) = jawaban
            && tx.send(AxumMessage::Text(jawaban.into())).await.is_err()
        {
            break;
        }
    }
}

/// Dokumen jaringan beserta lembar gaya eksternalnya.
///
/// Dua putaran, bukan satu: URL stylesheet baru diketahui setelah HTML diurai,
/// dan `href` relatif harus diselesaikan terhadap URL **setelah** pengalihan.
struct JaringanDokumen {
    ambilan: Result<jaringan::Ambilan, Galat>,
    gaya: LembarJaringan,
}

/// Mengambil dokumen untuk satu URL kalau URL itu memang butuh jaringan.
async fn ambil_untuk_url(url: &str) -> Option<JaringanDokumen> {
    if !jaringan::butuh_jaringan(url) {
        return None;
    }
    Some(ambil_dokumen(url).await)
}

/// Mengambil dokumen lalu stylesheet-nya.
///
/// Kalau dokumennya gagal diambil, tidak ada HTML untuk diurai, jadi tidak ada
/// tautan yang bisa diambil: `gaya` dibiarkan kosong alih-alih menebak URL.
async fn ambil_dokumen(url: &str) -> JaringanDokumen {
    let ambilan = jaringan::muat(url, jaringan::Batas::default()).await;
    let gaya = match &ambilan {
        Ok(a) => {
            let hasil = gaya::ambil(&a.url, &a.html).await;
            let mut catatan = hasil.catatan;
            let font = unduh_font(&a.url, &a.html, &hasil.lembar, &mut catatan).await;
            LembarJaringan {
                lembar: hasil.lembar,
                catatan,
                font,
            }
        }
        Err(_) => LembarJaringan::default(),
    };
    JaringanDokumen { ambilan, gaya }
}

/// Mengunduh byte setiap `@font-face` dokumen lalu memasangnya.
///
/// Face yang byte-nya tidak bisa diurai `ttf_parser` tetap dicatat: blok
/// `@font-face` yang menunjuk berkas rusak adalah masalah halaman, dan
/// melaporkannya lebih berguna daripada diam-diam memakai face tertanam.
async fn unduh_font(
    dasar: &str,
    html: &str,
    lembar: &[engine::native::css::LembarLuar],
    catatan: &mut Vec<String>,
) -> std::sync::Arc<engine::native::teks::KumpulanFont> {
    // Dokumen diurai ulang di sini, bukan diserahkan pemanggil: `@font-face`
    // boleh datang dari `<style>` sebaris, dan pohon yang dipakai kaskade belum
    // ada saat pengunduhan ini berjalan. Penguraian ulang HTML halaman adalah
    // satu-satunya cara melihat kedua sumber tanpa menyimpan pohon setengah
    // jadi. Kalau HTML-nya tidak bisa diurai, tidak ada face yang bisa
    // dipanggil, jadi kumpulan kosong.
    let dokumen = match engine::native::Document::parse_html(html) {
        Ok(d) => d,
        Err(_) => return std::sync::Arc::new(engine::native::teks::KumpulanFont::baru()),
    };
    let face = engine::native::css::font_face_dokumen(&dokumen, lembar);
    if face.is_empty() {
        return std::sync::Arc::new(engine::native::teks::KumpulanFont::baru());
    }
    let hasil = gaya::ambil_font(dasar, &face).await;
    catatan.extend(hasil.catatan);
    let mut kumpulan = engine::native::teks::KumpulanFont::baru();
    for (keluarga, byte) in hasil.face {
        let panjang = byte.len();
        if !kumpulan.pasang(&keluarga, byte) {
            catatan.push(format!(
                "`@font-face` `{keluarga}` dilewati: {panjang} byte yang diunduh bukan font \
                 yang bisa diurai, atau anggaran font dokumen sudah habis"
            ));
        }
    }
    std::sync::Arc::new(kumpulan)
}

/// Sama dengan [`ambil_untuk_url`], tapi membaca URL dari frame JSON-RPC./// Sama dengan [`ambil_untuk_url`], tapi membaca URL dari frame JSON-RPC.
///
/// Frame yang bukan navigasi jaringan mengembalikan `None`, termasuk frame
/// yang bukan JSON: frame rusak dijawab oleh penerjemah, bukan di sini.
async fn ambil_untuk_frame(teks: &str) -> Option<JaringanDokumen> {
    let frame: Value = serde_json::from_str(teks).ok()?;
    let url = match frame.get("method")?.as_str()? {
        "Page.navigate" | "Target.createTarget" => {
            frame.get("params")?.get("url")?.as_str()?.to_owned()
        }
        _ => return None,
    };
    ambil_untuk_url(&url).await
}

fn jawab_galat(galat: Galat) -> Response {
    let status = axum::http::StatusCode::BAD_REQUEST;
    (
        status,
        Json(json!({ "error": galat.pesan, "code": galat.kode })),
    )
        .into_response()
}
