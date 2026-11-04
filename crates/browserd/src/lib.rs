//! Isi `browserd` sebagai pustaka.
//!
//! Biner dan pustaka berbagi modul yang sama. Pemisahannya ada supaya bagian
//! yang tidak butuh proses hidup (perencanaan gerakan mouse, pembacaan event
//! CDP, penyusunan jawaban handoff) bisa diuji dengan `cargo test --lib`.
//! Pengujian lewat biner hanya bisa menguji apa yang tercapai dari argumen
//! baris perintah, dan itu terlalu sempit untuk logika yang menentukan apakah
//! klik owner mendarat di tombol yang benar.

#![forbid(unsafe_code)]

pub mod cdp_client;
pub mod challenge;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod handoff;
pub mod klasifikasi;
pub mod metrics;
pub mod penanam;
pub mod reap;
pub mod runtime;
pub mod selftest;
pub mod storecmd;
pub mod wiring;
