//! Re-ekspor base64 dari `cdp`.
//!
//! Implementasinya tinggal di `cdp::base64` karena mesin native mengeluarkan
//! PNG di crate itu dan tidak boleh ada dua tabel alfabet yang bisa menyimpang.
//! Nama modul ini dipertahankan supaya pemanggil lama (`wiring.rs`) tidak perlu
//! berubah.

pub use cdp::base64::{dekode, kodekan};
