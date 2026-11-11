//! Mesin native Rust: parser HTML, perhitungan layout, dan isolat JavaScript.
//!
//! Ini **fondasi**, bukan mesin penuh. Yang ada di sini sudah jalan dan diuji:
//! [`dom::Document::parse_html`] membangun pohon arena dari HTML sungguhan
//! lewat `html5ever`, [`selectors`] mencocokkan selektor CSS terhadap pohon
//! itu, [`layout::hitung`] menempatkan kotak lewat `taffy`, [`axtree::pohon`]
//! membaca isi halaman sebagai pohon aksesibilitas, [`paint::lukis`]
//! merasterkan kotak menjadi PNG lewat `tiny-skia`, dan [`js::Isolat`]
//! menjalankan skrip lewat `rquickjs` dengan pagu memori.
//!
//! Yang **belum** ada disebut namanya di [`CELAH`], bukan disembunyikan:
//! kaskade stylesheet, pewarisan properti, pengukuran teks,
//! peristiwa DOM, pengambilan dokumen dari jaringan, tipe nilai balik
//! JavaScript, dan peristiwa CDP. Memanggil ini "mesin browser yang selesai"
//! akan jadi klaim palsu.
//!
//! Permukaan CDP-nya **tidak** ada di sini. Crate ini sengaja nol pengetahuan
//! protokol; yang menerjemahkan frame JSON-RPC jadi panggilan ke
//! [`tab_pool::TabPool`] adalah `cdp::native_wire`, dan yang menyalakannya
//! sebagai mesin adalah `cdp::native_wire::NativeEngine`.

pub mod axtree;
pub mod dom;
pub mod js;
pub mod layout;
pub mod paint;
pub mod selectors;
pub mod tab_pool;

pub use dom::{Document, Node, NodeId, NodeKind};
pub use js::{Isolat, PaguIsolat};
pub use layout::{Kotak, LaporanLayout, Viewport};
pub use selectors::{GalatSelektor, Kombinator, Kompleks, OpAtribut, Pola, Sederhana};
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
    "kaskade stylesheet `<style>`/`<link>` belum ada; hanya atribut `style` inline",
    "pewarisan properti dan nilai non-px (em, rem, %) belum ada",
    "pengukuran teks belum ada; node teks tidak menghasilkan kotak layout",
    "peristiwa DOM (klik, input, fokus) belum ada; itu tugas lapisan hands",
    "raster hanya melukis latar kotak dari atribut `style` inline; teks, gambar, \
     gradien, bayangan, garis (`border`), dan `border-radius` tidak dilukis, jadi \
     halaman yang isinya teks saja menghasilkan kanvas kosong",
    "`Page.captureScreenshot` hanya PNG pada skala 1 dengan viewport bawaan \
     800x600: `captureBeyondViewport`, `optimizeForSpeed`, `fromSurface`, dan \
     `clip.scale` tidak didukung, dan JPEG ditolak alih-alih dikirim sebagai PNG",
    "pohon aksesibilitas belum menghitung `aria-labelledby` berantai, status \
     *focusable*/*selected*/*expanded*, dan konten di dalam `<iframe>`",
    "pengambilan dokumen dari jaringan (HTTP/TLS) belum ada; HTML masuk lewat \
     `Page.setDocumentContent`, dan `Page.navigate` hanya melayani `about:blank` \
     serta `data:text/html`",
    "nilai balik `Runtime.evaluate` dijadikan string; tipe JS (number, boolean, \
     object) tidak dipertahankan",
    "`document` di dalam isolat adalah objek biasa, bukan DOM sungguhan: skrip \
     yang menulis `document.title` tidak mengubah pohon DOM",
    "mesin tidak mengirim peristiwa CDP (`Runtime.consoleAPICalled`, `Page.loadEventFired`, \
     `Network.*`); klien yang menunggu event akan menunggu selamanya",
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

    #[error("rasterisasi gagal: {0}")]
    Raster(String),
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

    pub(crate) fn raster(pesan: impl Into<String>) -> Self {
        Self::Raster(pesan.into())
    }
}
