//! Endpoint discovery HTTP yang dibaca klien CDP.
//!
//! Dua peran berbeda, dan bedanya penting:
//!
//! - `/json/version` **dibangun oleh kita**, karena `webSocketDebuggerUrl` harus
//!   menunjuk ke daemon, bukan ke mesin. Kalau URL mesin yang dibocorkan, klien
//!   akan menyambung langsung ke mesin dan melewati seluruh proxy.
//! - `/json/list`, `/json/new`, `/json/close`, `/json/activate`, `/json/protocol`
//!   **diteruskan** ke mesin. Data target adalah kebenaran mesin, bukan karangan
//!   kita. Meneruskan juga berarti tidak ada state kedua yang bisa basi.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use serde_json::{Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::framing::{parse_status, split_http_response};
use crate::{CdpError, ProxyState};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BODY: usize = 8 * 1024 * 1024;

pub fn discovery_router(state: Arc<ProxyState>) -> Router {
    Router::new()
        .route("/json/version", get(version))
        .route("/json/list", get(list))
        .route("/json", get(list))
        .route("/json/protocol", get(protocol))
        .route("/json/new", any(new_target))
        .route("/json/activate/{id}", any(activate))
        .route("/json/close/{id}", any(close))
        .route("/healthz", get(healthz))
        .with_state(state)
}

/// Versi dan URL websocket **milik daemon**.
///
/// Catatan penting: klien OpenCrabs (`chromey`) mem-parse `V8-Version` dan
/// `WebKit-Version` sebagai `String` wajib. Kalau kuncinya tidak dikirim,
/// koneksi gagal dengan galat deserialisasi, bukan dengan galat yang jelas.
/// Karena itu kunci selalu ada; kalau mesin tidak melaporkan, isinya string
/// kosong. Itu jujur (tidak mengklaim versi apa pun) dan tetap bisa di-parse.
async fn version(State(state): State<Arc<ProxyState>>) -> Response {
    let info = state.engine.info();
    let mut body = Map::new();
    body.insert("Browser".into(), info.browser.clone().into());
    body.insert(
        "Protocol-Version".into(),
        info.protocol_version.clone().into(),
    );
    body.insert("User-Agent".into(), info.user_agent.clone().into());
    body.insert(
        "V8-Version".into(),
        info.v8_version.clone().unwrap_or_default().into(),
    );
    body.insert(
        "WebKit-Version".into(),
        info.webkit_version.clone().unwrap_or_default().into(),
    );
    body.insert(
        "webSocketDebuggerUrl".into(),
        state.public_ws_url.clone().into(),
    );
    axum::Json(Value::Object(body)).into_response()
}

async fn healthz(State(state): State<Arc<ProxyState>>) -> Response {
    let running = state.engine.is_running();
    let pid = state.engine.process_id();
    let body = format!(
        "{{\"ok\":{},\"engine\":\"{}\",\"pid\":{}}}\n",
        running,
        state.engine.kind(),
        pid.map_or_else(|| "null".to_string(), |pid| pid.to_string())
    );
    (
        if running {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

async fn list(State(state): State<Arc<ProxyState>>) -> Result<Response, CdpError> {
    forward_json(&state, &Method::GET, "/json/list").await
}

async fn protocol(State(state): State<Arc<ProxyState>>) -> Result<Response, CdpError> {
    forward_json(&state, &Method::GET, "/json/protocol").await
}

// Method diteruskan apa adanya: Chrome memakai PUT untuk /json/new sejak
// beberapa versi, dan menebak GET akan mematahkannya.
async fn new_target(
    State(state): State<Arc<ProxyState>>,
    method: Method,
    uri: Uri,
) -> Result<Response, CdpError> {
    forward_json(&state, &method, &path_and_query(&uri)).await
}

async fn activate(
    State(state): State<Arc<ProxyState>>,
    method: Method,
    Path(id): Path<String>,
) -> Result<Response, CdpError> {
    forward_json(&state, &method, &format!("/json/activate/{id}")).await
}

async fn close(
    State(state): State<Arc<ProxyState>>,
    method: Method,
    Path(id): Path<String>,
) -> Result<Response, CdpError> {
    forward_json(&state, &method, &format!("/json/close/{id}")).await
}

struct Upstream {
    status: u16,
    body: String,
}

async fn forward_json(
    state: &ProxyState,
    method: &Method,
    path_and_query: &str,
) -> Result<Response, CdpError> {
    let upstream = forward(state, method, path_and_query).await?;
    let status = StatusCode::from_u16(upstream.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let body = rewrite_discovery(
        &upstream.body,
        public_ws_base(state),
        &engine_authority(state),
    );
    Ok((status, [(header::CONTENT_TYPE, "application/json")], body).into_response())
}

/// Basis websocket milik daemon: `public_ws_url` tanpa sufiks `/devtools/...`.
fn public_ws_base(state: &ProxyState) -> &str {
    state
        .public_ws_url
        .find("/devtools/")
        .map_or(state.public_ws_url.as_str(), |cut| {
            &state.public_ws_url[..cut]
        })
}

/// `host:port` mesin. Ini alamat yang tidak boleh muncul di respons kita:
/// klien yang membacanya akan menyambung langsung ke mesin dan melewati
/// seluruh proxy, termasuk lifecycle yang belum ada di v0.1.
fn engine_authority(state: &ProxyState) -> String {
    state
        .engine
        .http_base()
        .split("://")
        .nth(1)
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_owned()
}

/// Menitipkan alamat daemon ke data target milik mesin.
///
/// Nama target, id, judul, dan tipe tetap apa adanya: itu kebenaran mesin.
/// Yang diganti hanya alamat, karena alamat lama menunjuk ke tempat yang
/// seharusnya tidak dijangkau klien.
fn rewrite_discovery(body: &str, ws_base: &str, engine_authority: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_owned();
    };
    let daemon_authority = ws_base.split("://").nth(1).unwrap_or_default();
    rewrite_value(&mut value, ws_base, daemon_authority, engine_authority);
    serde_json::to_string(&value).unwrap_or_else(|_| body.to_owned())
}

fn rewrite_value(
    value: &mut serde_json::Value,
    ws_base: &str,
    daemon_authority: &str,
    engine_authority: &str,
) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                rewrite_value(item, ws_base, daemon_authority, engine_authority);
            }
        }
        serde_json::Value::Object(map) => {
            let id = map
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            if let Some(serde_json::Value::String(url)) = map.get_mut("webSocketDebuggerUrl") {
                *url = match &id {
                    Some(id) => format!("{ws_base}/devtools/page/{id}"),
                    None => url.replace(engine_authority, daemon_authority),
                };
            }
            if let Some(serde_json::Value::String(url)) = map.get_mut("devtoolsFrontendUrl") {
                *url = url.replace(engine_authority, daemon_authority);
            }
            for nested in map.values_mut() {
                rewrite_value(nested, ws_base, daemon_authority, engine_authority);
            }
        }
        _ => {}
    }
}

async fn forward(
    state: &ProxyState,
    method: &Method,
    path_and_query: &str,
) -> Result<Upstream, CdpError> {
    let port = state.engine.info().devtools_port;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));

    let mut stream = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => return Err(CdpError::Upstream(format!(":{port}: {err}"))),
        Err(_) => return Err(CdpError::Upstream(format!(":{port} timeout menyambung"))),
    };

    let request = format!(
        "{method} {path_and_query} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|err| CdpError::Upstream(err.to_string()))?;

    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 8192];
    // Chrome tidak menutup socket walau diminta (`Connection: close`
    // diabaikan), jadi berhenti saat body sudah lengkap menurut
    // Content-Length. Menunggu EOF berarti setiap permintaan menghabiskan
    // READ_TIMEOUT penuh dan dilaporkan gagal padahal datanya sudah datang.
    loop {
        let read = match tokio::time::timeout(READ_TIMEOUT, stream.read(&mut chunk)).await {
            Ok(Ok(read)) => read,
            Ok(Err(err)) => return Err(CdpError::Upstream(err.to_string())),
            Err(_) => return Err(CdpError::Upstream("timeout membaca respons".into())),
        };
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        if buf.len() > MAX_BODY {
            return Err(CdpError::Malformed);
        }
        if crate::framing::is_complete(&buf) {
            break;
        }
    }

    let (head, body) = split_http_response(&buf).ok_or(CdpError::Malformed)?;
    let status = parse_status(head).ok_or(CdpError::Malformed)?;
    Ok(Upstream {
        status,
        body: String::from_utf8_lossy(body).into_owned(),
    })
}

fn path_and_query(uri: &Uri) -> String {
    match uri.query() {
        Some(query) => format!("{}?{query}", uri.path()),
        None => uri.path().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_dipertahankan_saat_diteruskan() {
        let uri: Uri = "/json/new?https://example.com/a?b=1"
            .parse()
            .expect("uri sah");
        assert_eq!(path_and_query(&uri), "/json/new?https://example.com/a?b=1");
    }

    #[test]
    fn tanpa_query_tetap_jadi_path_saja() {
        let uri: Uri = "/json/new".parse().expect("uri sah");
        assert_eq!(path_and_query(&uri), "/json/new");
    }

    // Fixture ini disalin dari respons chrome-headless-shell sungguhan:
    // port mesin ikut terbawa di dua tempat, dan itu yang harus hilang.
    const CHROME_LIST: &str = r#"[
      {
        "description": "",
        "devtoolsFrontendUrl": "https://chrome-devtools-frontend.appspot.com/serve_rev/@abc/inspector.html?ws=127.0.0.1:41234/devtools/page/AAA111",
        "id": "AAA111",
        "title": "about:blank",
        "type": "page",
        "url": "about:blank",
        "webSocketDebuggerUrl": "ws://127.0.0.1:41234/devtools/page/AAA111"
      }
    ]"#;

    #[test]
    fn url_mesin_diganti_url_daemon() {
        let rewritten = rewrite_discovery(CHROME_LIST, "ws://127.0.0.1:9224", "127.0.0.1:41234");
        assert!(
            !rewritten.contains("41234"),
            "port mesin masih bocor: {rewritten}"
        );
        assert!(rewritten.contains("ws://127.0.0.1:9224/devtools/page/AAA111"));
        assert!(rewritten.contains("?ws=127.0.0.1:9224/devtools/page/AAA111"));
        // Nama, id, dan judul tidak disentuh.
        assert!(rewritten.contains("\"id\":\"AAA111\""));
        assert!(rewritten.contains("\"title\":\"about:blank\""));
        assert!(rewritten.contains("\"type\":\"page\""));
    }

    #[test]
    fn objek_target_tunggal_juga_ditulis_ulang() {
        let body =
            r#"{"id":"BBB222","webSocketDebuggerUrl":"ws://127.0.0.1:41234/devtools/page/BBB222"}"#;
        let rewritten = rewrite_discovery(body, "ws://127.0.0.1:9224", "127.0.0.1:41234");
        assert!(rewritten.contains("ws://127.0.0.1:9224/devtools/page/BBB222"));
    }

    #[test]
    fn body_bukan_json_dikembalikan_apa_adanya() {
        let body = "<html>bukan json</html>";
        assert_eq!(
            rewrite_discovery(body, "ws://127.0.0.1:9224", "127.0.0.1:41234"),
            body
        );
    }

    #[test]
    fn basis_ws_strip_sufiks_devtools() {
        let engine: std::sync::Arc<dyn engine::Engine> =
            std::sync::Arc::new(testing::FakeEngine::at_port(41234));
        let state = ProxyState::new(engine, "ws://127.0.0.1:9224/devtools/browser/abc123");
        assert_eq!(public_ws_base(&state), "ws://127.0.0.1:9224");
        assert_eq!(engine_authority(&state), "127.0.0.1:41234");
    }
}
