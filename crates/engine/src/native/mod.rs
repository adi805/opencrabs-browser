//! Mesin native Rust: parser HTML, perhitungan layout, dan isolat JavaScript.
//!
//! Ini **fondasi**, bukan mesin penuh. Yang ada di sini sudah jalan dan diuji:
//! [`dom::Document::parse_html`] membangun pohon arena dari HTML sungguhan
//! lewat `html5ever`, [`layout::hitung`] menempatkan kotak lewat `taffy`, dan
//! [`js::Isolat`] menjalankan skrip lewat `rquickjs` dengan pagu memori.
//!
//! Yang **belum** ada disebut namanya di [`CELAH`], bukan disembunyikan:
//! selektor CSS, kaskade stylesheet, pewarisan properti, pengukuran teks,
//! peristiwa DOM, raster, dan snapshot tab. Memanggil ini "mesin browser yang
//! selesai" akan jadi klaim palsu.

pub mod dom;
pub mod js;
pub mod layout;
pub mod tab_pool;

pub use dom::{Document, Node, NodeId, NodeKind};
pub use js::{Isolat, PaguIsolat};
pub use layout::{Kotak, LaporanLayout, Viewport};
pub use tab_pool::{
    AlasanSingkir, BASELINE_MB, ColdStore, MemoryColdStore, PAGU_MB, PSS_PER_TAB_MB, PoolError,
    Snapshot, TabId, TabPool, tab_panas_maksimum,
};

use std::fmt;

/// Celah yang **diketahui** dan disengaja pada fondasi ini.
///
/// Daftar ini adalah kontrak: menambah kemampuan berarti menghapus barisnya
/// di sini dan menambah tesnya, bukan sekadar menambah kode.
pub const CELAH: &[&str] = &[
    "selektor CSS (`selectors`) belum ada; pencarian node hanya lewat atribut id",
    "kaskade stylesheet `<style>`/`<link>` belum ada; hanya atribut `style` inline",
    "pewarisan properti dan nilai non-px (em, rem, %) belum ada",
    "pengukuran teks belum ada; node teks tidak menghasilkan kotak layout",
    "peristiwa DOM (klik, input, fokus) belum ada; itu tugas lapisan hands",
    "rasterisasi bitmap (`tiny-skia`) belum ada; itu tugas lapisan eyes",
    "snapshot tab ke `crates/store` belum ada; itu tugas tab pool",
];

#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    #[error("parser HTML mengembalikan galat I/O: {0}")]
    Parse(String),

    #[error("taffy menolak operasi layout: {0}")]
    Layout(String),

    #[error("runtime JavaScript tidak bisa dibuat: {0}")]
    Runtime(String),

    #[error("skrip JavaScript gagal: {0}")]
    Script(String),
}

impl NativeError {
    /// Membungkus galat pustaka luar tanpa memaksa pemanggil tahu tipenya.
    pub(crate) fn parse(err: impl fmt::Debug) -> Self {
        Self::Parse(format!("{err:?}"))
    }

    pub(crate) fn layout(err: impl fmt::Debug) -> Self {
        Self::Layout(format!("{err:?}"))
    }

    pub(crate) fn runtime(err: impl fmt::Debug) -> Self {
        Self::Runtime(format!("{err:?}"))
    }

    pub(crate) fn script(err: impl fmt::Debug) -> Self {
        Self::Script(format!("{err:?}"))
    }
}
