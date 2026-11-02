//! Berbicara protokol. Meneruskan, menerjemahkan, dan tidak memutuskan apa pun.
//!
//! Aturan crate ini:
//!
//! - **Pesan klien tidak pernah diubah isinya.** Id, `sessionId`, urutan, dan
//!   byte-nya diteruskan apa adanya. Di v0.1 ini bukan multiplex: satu koneksi
//!   klien memetakan ke satu koneksi mesin.
//! - **Crate ini tidak tahu mesin mana yang dipakai.** Dia hanya pegang trait
//!   [`engine::Engine`], jadi engine palsu bisa dipakai di test.
//! - **Keputusan routing bukan di sini.** Itu di `policy`, dan belum masuk v0.1.

#![forbid(unsafe_code)]

pub mod errors;
pub mod framing;
pub mod ids;
pub mod passthrough;
pub mod targets;

pub use errors::CdpError;

use std::sync::Arc;

use axum::Router;
use engine::Engine;

/// Yang dibutuhkan lapisan protokol untuk melayani klien.
///
/// `public_ws_url` adalah URL yang **kita** iklankan ke klien. URL milik mesin
/// sengaja tidak pernah dibocorkan: kalau klien bisa menyambung langsung ke
/// mesin, proxy ini kehilangan seluruh gunanya.
#[derive(Clone)]
pub struct ProxyState {
    pub engine: Arc<dyn Engine>,
    pub public_ws_url: String,
}

impl ProxyState {
    pub fn new(engine: Arc<dyn Engine>, public_ws_url: impl Into<String>) -> Self {
        Self {
            engine,
            public_ws_url: public_ws_url.into(),
        }
    }
}

/// Router lengkap: discovery HTTP plus endpoint websocket.
pub fn router(state: Arc<ProxyState>) -> Router {
    targets::discovery_router(Arc::clone(&state)).merge(passthrough::ws_router(state))
}
