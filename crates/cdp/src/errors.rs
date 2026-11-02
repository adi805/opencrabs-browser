//! Kesalahan lapisan protokol, plus cara melaporkannya ke klien HTTP.
//!
//! Klien CDP (termasuk OpenCrabs) membaca status HTTP. Karena itu kegagalan
//! harus punya status yang jujur: 502 kalau mesin tidak menjawab, 404 kalau
//! yang diminta memang tidak ada. Semua 200 dengan pesan error di body adalah
//! cara paling cepat membuat bug tak terlacak.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    #[error("mesin tidak bisa dihubungi: {0}")]
    Upstream(String),

    #[error("respons mesin bukan HTTP yang sah")]
    Malformed,

    #[error("websocket mesin tidak bisa dibuka: {0}")]
    WebSocket(String),

    #[error("mesin '{diminta}' tidak ada; yang terpasang: {tersedia}")]
    MesinTakAda { diminta: String, tersedia: String },
}

impl CdpError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            // Mesin yang tidak ada bukan kegagalan server: klien menyebut
            // nama yang tidak terpasang, dan 404 yang menyebut daftar mesin
            // membuat salah ketik selesai dalam satu percobaan.
            Self::MesinTakAda { .. } => StatusCode::NOT_FOUND,
            Self::Malformed | Self::WebSocket(_) => StatusCode::BAD_GATEWAY,
        }
    }
}

impl IntoResponse for CdpError {
    fn into_response(self) -> Response {
        // Pesan dikirim apa adanya: ini endpoint loopback untuk operator,
        // bukan halaman publik. Menyembunyikan sebab hanya memperlambat debug.
        (self.status(), format!("{self}\n")).into_response()
    }
}
