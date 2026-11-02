//! Penerusan websocket CDP: klien ke daemon, daemon ke mesin.
//!
//! v0.1 sengaja **bukan** multiplex. Satu koneksi klien menjadi satu koneksi
//! mesin, pesan lewat tanpa disentuh. Alasannya: seluruh penghematan memori yang
//! terukur datang dari lifecycle (menutup tab dan mengembalikan memori), bukan
//! dari multiplex. Multiplex menambah tempat fragilitas protokol, jadi dia
//! masuk setelah pass-through terbukti tidak merusak automation.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::any;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TsMessage;

use crate::{CdpError, ProxyState, framing};

pub fn ws_router(state: Arc<ProxyState>) -> Router {
    Router::new()
        .route("/devtools/browser/{*rest}", any(upgrade))
        .route("/devtools/page/{*rest}", any(upgrade))
        .with_state(state)
}

async fn upgrade(ws: WebSocketUpgrade, State(state): State<Arc<ProxyState>>) -> Response {
    ws.on_upgrade(move |socket| async move {
        match proxy(socket, state).await {
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

pub async fn proxy(client: WebSocket, state: Arc<ProxyState>) -> Result<Stats, CdpError> {
    let engine_url = state.engine.web_socket_url().to_owned();
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
                        if let Err(err) = engine_tx.send(framing::axum_to_ts(message)).await {
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
