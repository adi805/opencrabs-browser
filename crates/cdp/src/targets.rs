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
    Ok((
        status,
        [(header::CONTENT_TYPE, "application/json")],
        upstream.body,
    )
        .into_response())
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
    match tokio::time::timeout(READ_TIMEOUT, stream.read_to_end(&mut buf)).await {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => return Err(CdpError::Upstream(err.to_string())),
        Err(_) => return Err(CdpError::Upstream("timeout membaca respons".into())),
    }
    if buf.len() > MAX_BODY {
        return Err(CdpError::Malformed);
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
}
