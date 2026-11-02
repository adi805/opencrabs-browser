//! Pintu HTTP untuk tool dan operator. Bukan untuk browser.
//!
//! Crate ini memegang dua hal: token kontrol (siapa yang boleh bicara) dan
//! daftar handoff (apa yang sedang menunggu manusia). Penerapan jawaban ke
//! browser ada di daemon, bukan di sini, supaya crate ini bisa diuji tanpa
//! Chrome dan supaya jalur CDP hanya punya satu pintu masuk.
//!
//! Rancangan lengkap: dokumen 03 §7 (protokol handoff) dan dokumen 04 §2
//! (kunci `[daemon] control_bind`).

#![forbid(unsafe_code)]

pub mod auth;
pub mod registry;
pub mod router;

pub use auth::{AuthError, Token, ensure_token_file, parse_bearer, read_token_file};
pub use registry::{
    Ack, Answer, ApplyOutcome, ApplyRequest, Handoff, Kind, NewHandoff, Registry, RegistryError,
    RegistryLimits, State, View,
};
pub use router::{ApiError, ControlState, router as build_router};
