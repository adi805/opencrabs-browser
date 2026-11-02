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

/// Pembaca pesan yang lewat, tanpa hak mengubahnya.
///
/// Dipanggil sinkron di dalam jalur penerusan, jadi implementasinya wajib
/// cepat: parsing ringan dan `try_send` ke antrean. Pekerjaan yang butuh
/// perjalanan bolak-balik ke mesin (mis. probe DOM) dikerjakan di tempat lain,
/// bukan di sini, karena menahannya di jalur ini berarti memperlambat setiap
/// pesan klien.
///
/// `observe` tidak mengembalikan apa pun dan tidak menerima pesan yang bisa
/// diubah: satu-satunya cara observer merusak penerusan adalah panik, dan itu
/// ditangkap pemanggil.
pub trait FrameObserver: Send + Sync {
    fn observe(&self, payload: &str);
}

/// Yang dibutuhkan lapisan protokol untuk melayani klien.
///
/// `public_ws_url` adalah URL yang **kita** iklankan ke klien. URL milik mesin
/// sengaja tidak pernah dibocorkan: kalau klien bisa menyambung langsung ke
/// mesin, proxy ini kehilangan seluruh gunanya.
#[derive(Clone)]
pub struct ProxyState {
    pub engine: Arc<dyn Engine>,
    pub public_ws_url: String,
    /// `None` berarti tidak ada yang mengamati: perilaku persis sama dengan
    /// sebelum fitur ini ada.
    pub observer: Option<Arc<dyn FrameObserver>>,
}

impl ProxyState {
    pub fn new(engine: Arc<dyn Engine>, public_ws_url: impl Into<String>) -> Self {
        Self {
            engine,
            public_ws_url: public_ws_url.into(),
            observer: None,
        }
    }

    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn FrameObserver>) -> Self {
        self.observer = Some(observer);
        self
    }
}

/// Router lengkap: discovery HTTP plus endpoint websocket.
pub fn router(state: Arc<ProxyState>) -> Router {
    targets::discovery_router(Arc::clone(&state)).merge(passthrough::ws_router(state))
}
