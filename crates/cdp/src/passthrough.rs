//! Penerusan websocket CDP: klien ke daemon, daemon ke mesin.
//!
//! v0.1 sengaja **bukan** multiplex. Satu koneksi klien menjadi satu koneksi
//! mesin, pesan lewat tanpa disentuh. Alasannya: seluruh penghematan memori yang
//! terukur datang dari lifecycle (menutup tab dan mengembalikan memori), bukan
//! dari multiplex. Multiplex menambah tempat fragilitas protokol, jadi dia
//! masuk setelah pass-through terbukti tidak merusak automation.

use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TsMessage;

use crate::{CdpError, ProxyState, framing};

pub fn ws_router(state: Arc<ProxyState>) -> Router {
    Router::new()
        .route("/devtools/browser/{*rest}", any(upgrade_browser))
        .route("/devtools/page/{*rest}", any(upgrade_page))
        .with_state(state)
}

/// Soket browser titipan: di sinilah `Target.*` dan `sessionId` berlaku.
///
/// Satu mesin tetap lewat jalur pass-through lama, yang sudah dipakai produksi.
/// Multiplex hanya dipakai kalau memang ada lebih dari satu mesin: menaruh
/// penerjemahan identitas di jalur satu mesin berarti mempertaruhkan yang sudah
/// jalan tanpa alasan.
async fn upgrade_browser(ws: WebSocketUpgrade, State(state): State<Arc<ProxyState>>) -> Response {
    if state.engines.is_multi() {
        let pool = state.engines.clone();
        let observer = state.observer.clone();
        return ws.on_upgrade(move |socket| async move {
            match crate::mux::proxy_multiplex(socket, pool, observer).await {
                Ok(stats) => tracing::info!(
                    to_engine = stats.to_engine,
                    from_engine = stats.from_engine,
                    "sesi CDP multiplex selesai"
                ),
                Err(err) => tracing::warn!(%err, "sesi CDP multiplex gagal"),
            }
        });
    }
    upgrade(
        ws,
        state.engine().web_socket_url().to_owned(),
        state.observer.clone(),
    )
}

/// Soket halaman harus benar-benar mendarat di halaman itu. Kalau diteruskan
/// ke soket browser, `Page.navigate` tanpa `sessionId` ditolak di sana, dan
/// klien melihat kegagalan yang tidak ada hubungannya dengan permintaannya.
async fn upgrade_page(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ProxyState>>,
    Path(rest): Path<String>,
) -> Response {
    let id = rest.trim_start_matches('/');
    // Id halaman boleh berprefiks mesin (`<mesin>~<id>`) karena itu bentuk
    // yang kita iklankan di `/json/list`. Tanpa prefiks, halaman dianggap milik
    // mesin bawaan, sehingga URL lama tetap bekerja.
    let (mesin, upstream) = crate::engines::pecah_id(id);
    let engine = match mesin {
        Some(mesin) => match state.engines.get(mesin) {
            Some(engine) => engine,
            None => {
                return CdpError::Upstream(format!("mesin '{mesin}' tidak terpasang"))
                    .into_response();
            }
        },
        None => state.engine(),
    };
    let port = engine.info().devtools_port;
    if upstream.is_empty() || port == 0 {
        return CdpError::Upstream("target halaman tidak bisa dipetakan ke mesin".to_owned())
            .into_response();
    }
    upgrade(
        ws,
        format!("ws://127.0.0.1:{port}/devtools/page/{upstream}"),
        state.observer.clone(),
    )
}

fn upgrade(
    ws: WebSocketUpgrade,
    engine_url: String,
    observer: Option<Arc<dyn crate::FrameObserver>>,
) -> Response {
    ws.on_upgrade(move |socket| async move {
        match proxy_with_observer(socket, engine_url, observer).await {
            Ok(stats) => tracing::info!(
                to_engine = stats.to_engine,
                from_engine = stats.from_engine,
                "sesi CDP selesai"
            ),
            Err(err) => tracing::warn!(%err, "sesi CDP gagal"),
        }
    })
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub to_engine: u64,
    pub from_engine: u64,
}

pub async fn proxy(client: WebSocket, engine_url: String) -> Result<Stats, CdpError> {
    proxy_with_observer(client, engine_url, None).await
}

/// Sama dengan [`proxy`], plus pengamat yang membaca setiap pesan dari mesin.
///
/// Pesan yang diteruskan tetap pesan yang sama persis. Observer hanya menerima
/// teksnya, dan panik di dalamnya ditangkap supaya satu bug di fitur pengamatan
/// tidak memutus sesi yang sedang dipakai.
pub async fn proxy_with_observer(
    client: WebSocket,
    engine_url: String,
    observer: Option<Arc<dyn crate::FrameObserver>>,
) -> Result<Stats, CdpError> {
    let (engine_socket, _response) = connect_async(engine_url.as_str())
        .await
        .map_err(|err| CdpError::WebSocket(err.to_string()))?;

    let (mut client_tx, mut client_rx) = client.split();
    let (mut engine_tx, mut engine_rx) = engine_socket.split();

    let mut stats = Stats::default();

    loop {
        tokio::select! {
            incoming = client_rx.next() => {
                let Some(incoming) = incoming else { break };
                match incoming {
                    Ok(message) => {
                        stats.to_engine += 1;
                        let message = framing::axum_to_ts(message);
                        // Pengamat juga membaca arah klien. Tanpa itu
                        // pemetaan sesi ke target hilang: mesin hanya
                        // mengirim `sessionId` di balasan attach, sedangkan
                        // `targetId` cuma ada di permintaan klien.
                        if let Some(observer) = observer.as_ref()
                            && let Some(teks) = framing::text_of(&message)
                        {
                            observer.observe(teks);
                        }
                        if let Err(err) = engine_tx.send(message).await {
                            tracing::debug!(%err, "mesin menolak pesan");
                            break;
                        }
                    }
                    Err(err) => {
                        tracing::debug!(%err, "klien memutus koneksi");
                        break;
                    }
                }
            }
            outgoing = engine_rx.next() => {
                let Some(outgoing) = outgoing else { break };
                match outgoing {
                    Ok(message) => {
                        stats.from_engine += 1;
                        if let Some(observer) = observer.as_ref()
                            && let Some(teks) = framing::text_of(&message)
                        {
                            // Observer berjalan di jalur panas dan tidak
                            // boleh menahan pesan: kontraknya hanya parsing
                            // ringan lalu `try_send` ke antrean pekerja.
                            observer.observe(teks);
                        }
                        let Some(message) = framing::ts_to_axum(message) else {
                            tracing::debug!("pesan tanpa padanan dilewati");
                            continue;
                        };
                        if client_tx.send(message).await.is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        tracing::debug!(%err, "mesin memutus koneksi");
                        break;
                    }
                }
            }
        }
    }

    // Menutup rapi: kirim close ke kedua sisi supaya tab tidak menggantung
    // menunggu klien yang sebenarnya sudah pergi.
    let _ = engine_tx.send(TsMessage::Close(None)).await;
    let _ = client_tx.send(AxumMessage::Close(None)).await;

    Ok(stats)
}
