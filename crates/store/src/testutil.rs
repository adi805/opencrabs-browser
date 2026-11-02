//! Direktori sementara untuk test, tanpa menambah dependensi.
//!
//! `tempfile` akan menjadi dependensi test yang sah, tapi crate ini sengaja
//! memakai std dulu: satu dependensi lebih sedikit berarti satu rantai pasok
//! lebih sedikit yang harus diaudit. Nama direktori memuat pid dan penghitung
//! atomik supaya test paralel tidak saling menimpa.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static URUT: AtomicU64 = AtomicU64::new(0);

pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> Self {
        let nomor = URUT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ocb-store-{}-{}-{nomor}",
            std::process::id(),
            label
        ));
        std::fs::create_dir_all(&path).expect("membuat direktori sementara");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
