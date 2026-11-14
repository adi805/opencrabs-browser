//! Mesin native Rust: parser HTML, perhitungan layout, dan isolat JavaScript.
//!
//! Ini **fondasi**, bukan mesin penuh. Yang ada di sini sudah jalan dan diuji:
//! [`dom::Document::parse_html`] membangun pohon arena dari HTML sungguhan
//! lewat `html5ever`, [`selectors`] mencocokkan selektor CSS terhadap pohon
//! itu, [`css::Kaskade`] menyelesaikan stylesheet jadi deklarasi pemenang per
//! elemen, [`gaya::PohonGaya`] menurunkan properti yang waris dan menghitung
//! `em`/`rem`/`%` terhadap konteks yang benar, [`layout::hitung`] menempatkan
//! kotak lewat `taffy`, [`axtree::pohon`]
//! membaca isi halaman sebagai pohon aksesibilitas, [`paint::lukis`]
//! merasterkan kotak menjadi PNG lewat `tiny-skia`, dan [`js::Isolat`]
//! menjalankan skrip lewat `rquickjs` dengan pagu memori.
//!
//! [`teks::UkuranTeks`] mengukur lebar teks dari rantai face TrueType yang
//! ditanam di dalam biner (`crates/engine/assets/`) ditambah face `@font-face`
//! yang byte-nya diserahkan pemanggil, memilih face per code point dan
//! membentuk glyph dengan `rustybuzz`, dan node teks menghasilkan kotak layout
//! yang dibungkus pada lebar induknya.
//!
//! Yang **belum** ada disebut namanya di [`CELAH`], bukan disembunyikan.
//! Memanggil ini "mesin browser yang selesai" akan jadi klaim palsu.
//!
//! Permukaan CDP-nya **tidak** ada di sini. Crate ini sengaja nol pengetahuan
//! protokol; yang menerjemahkan frame JSON-RPC jadi panggilan ke
//! [`tab_pool::TabPool`] adalah `cdp::native_wire`, dan yang menyalakannya
//! sebagai mesin adalah `cdp::native_wire::NativeEngine`.

pub mod ambil;
pub mod axtree;
pub mod bingkai;
pub mod css;
pub mod dom;
pub mod gaya;
pub mod js;
pub mod layout;
pub mod paint;
pub mod selectors;
pub mod tab_pool;
pub mod teks;

pub use bingkai::{Muatan, asal, rencana, selesaikan};
pub use css::{FontFace, Kaskade, LembarLuar, Tautan};
pub use dom::{Document, ModeBayang, Node, NodeId, NodeKind};
pub use js::{Isolat, NilaiJs, PaguIsolat};
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
    "`@media` dijalankan: prelude-nya dievaluasi terhadap viewport yang \
     sedang dipakai (`width`/`height` beserta `min-`/`max-` dan varian \
     `device-`, `orientation`, `prefers-color-scheme`, \
     `prefers-reduced-motion`), lalu isi blok yang cocok masuk kaskade di \
     posisi blok itu. Yang belum: `@supports` belum dijalankan (dicatat di \
     `diabaikan`), dan kueri ber-`not` atau berfitur di luar daftar itu \
     dianggap tak dikenal lalu dilewati dengan alasannya, bukan diterapkan \
     dengan arti karangan",
    "teks diukur dan dibentuk dari rantai dua face yang ditanam di \
     `crates/engine/assets/` (`DejaVuSans.ttf` untuk Latin, Yunani, Kiril, \
     Arab, dan Ibrani; `NotoSansDevanagari-Regular.ttf` untuk Devanagari), \
     dengan pemilihan face per code point dan shaping HarfBuzz lewat \
     `rustybuzz`, jadi teks Arab bersambung dan matra Indic tergambar di \
     posisi yang benar. `@font-face` diunduh dan dipakai: `font-family` yang \
     menamai face unduhan menang atas rantai tertanam, byte-nya dibatasi \
     8 MiB per berkas dan 24 MiB per dokumen. Yang belum: `font-weight` dan \
     `font-style` tidak memilih di antara beberapa face sekeluarga (yang \
     dipakai face pertama yang memetakan code point-nya), aksara di luar face \
     tertanam maupun unduhan (CJK, Thai, emoji berwarna) tetap diukur selebar \
     `.notdef`, dan byte webfont tidak ikut tab dingin sehingga tab yang \
     dihidupkan lagi memakai face tertanam sampai navigasi berikutnya",
    "peristiwa pointer (`mousedown`, `mouseup`, `click`, `mousemove`) dan \
     pendengar halaman untuk `click`/`input`/`change`/`focus`/`blur`/`keydown` \
     sudah ada, tapi peristiwa **papan ketik dan sentuh** belum: \
     `Input.dispatchKeyEvent` dan `Input.dispatchTouchEvent` masih dijawab \
     galat, jadi `keydown` hanya bisa dipicu skrip halaman lewat \
     `dispatchEvent`, bukan lewat input sungguhan. `focus()`/`blur()` hanya \
     mengirim peristiwanya: tidak ada state fokus, jadi \
     `document.activeElement` tidak bergerak. Fase tangkap, `once`/`passive`, \
     dan `composedPath` belum ada, jadi `addEventListener(tipe, fn, true)` \
     diperlakukan sama dengan `false`",
    "raster melukis latar kotak dan glyph teks, termasuk gradien \
     (`linear`/`radial`), garis `border: solid` dengan `border-radius`, isi \
     `<img>` PNG/JPEG dari byte yang diserahkan pemanggil, tebal sintetis untuk \
     `font-weight` 600 ke atas, peregangan spasi `justify`, dan pemotongan \
     glyph pada `overflow: hidden`. Yang belum: `box-shadow`/`text-shadow`, \
     `border-style` selain `solid` (dilaporkan, tidak dilukis), `border` per \
     sisi (`border-left` dan sejenisnya), `object-fit` selain `fill`, dan tebal \
     sungguhan dari face bold (yang ada hanya tebal sintetis dari satu face)",
    "`Page.captureScreenshot` mengeluarkan `png`, `jpeg`, dan `webp`; format \
     lain di luar tiga itu ditolak dengan nama formatnya. `webp` di sini \
     lossless (VP8L) karena encoder-nya Rust murni: Chrome memakai WebP lossy \
     untuk `quality` yang sama, jadi berkas dari mesin ini bisa lebih besar \
     daripada Chrome pada halaman berfoto. Yang sudah dihormati: `quality` \
     (jpeg), `optimizeForSpeed`, `clip.scale` (render ulang pada kerapatan \
     itu, bukan pembesaran gambar hasil), `captureBeyondViewport`, dan \
     `fromSurface` (dicatat, hasilnya sama karena mesin software cuma punya \
     satu permukaan)",
    "pohon aksesibilitas belum mencakup dokumen anak yang tidak dimuat: isi \
     `<iframe>` ikut ke pohon hanya kalau pemanggil sudah menyerahkan HTML-nya \
     lewat `Document::pasang_bingkai`, karena mesin ini tidak membuka soket \
     sendiri. Iframe lintas asal, `sandbox`, `allow`, dan `csp` tidak dibaca, \
     jadi dokumen anak yang lolos memuat dengan hak penuh, dan kedalaman \
     bingkai dibatasi 8. `aria-labelledby` berantai dan status \
     *focusable*/*selected*/*expanded* sudah dihitung dari atribut dokumen \
     sejak W7; yang belum adalah *focused*, karena mesin ini belum punya \
     state fokus",
    "`fetch()` di dalam isolat hanya bisa lewat jalur yang dipasang pemanggil \
     (`native::ambil::Pengambil`); tanpa hook ia menolak semua. Jawabannya \
     objek biasa, bukan `Promise`: `await fetch(...)` dan `await r.text()` \
     jalan karena await pada nilai non-thenable mengembalikan nilai itu \
     sendiri, tapi `fetch(...).then(...)`, `Promise.all` dan `AbortController` \
     tidak. Badan diangkut sebagai teks UTF-8 lossy, jadi `arrayBuffer()`, \
     stream, dan cookie tidak ada",
    "nilai balik `Runtime.evaluate` membawa tipe JS (boolean, number, string, \
     object) dan objek dikirim *by value*, tapi belum ada `objectId`, \
     `Runtime.callFunctionOn`, maupun `awaitPromise`: klien yang menyimpan \
     pegangan objek atau menunggu promise tidak bisa memakainya",
    "`navigator` di dalam isolat adalah objek biasa, bukan instance \
     `Navigator`, jadi pemeriksa yang membaca prototipe bisa membedakannya dari \
     browser sungguhan. Buffer `console` hidup di global `__log` \
     (non-enumerable, jadi tidak muncul di `Object.keys(globalThis)`, tapi \
     namanya bisa ditebak). `document` sendiri sudah pohon sungguhan sejak \
     jembatan W4b: mutasi dari skrip halaman terlihat oleh `DOM.getDocument` \
     dan oleh raster (`native::js::tests::test_js_runtime_dom_mutations`), tapi \
     cakupannya masih inti DOM (cari, buat, sisip, hapus, teks, atribut, \
     `children`/`parentNode`, `title`), belum `classList`, `innerHTML` setter, \
     `dataset`, maupun `insertAdjacentHTML`",
    "mesin mengirim peristiwa CDP untuk domain yang sudah di-`enable` \
     (`Runtime.consoleAPICalled`, `Runtime.executionContextCreated`, \
     `Page.loadEventFired`, `Network.requestWillBeSent`/`responseReceived`), \
     tapi antreannya baru dikuras sesudah klien mengirim satu frame: klien yang \
     diam menunggu peristiwa tidak menerima apa pun sampai ia bicara lagi, \
     karena tidak ada jalur dorong dari mesin ke soket. `Runtime.exceptionThrown`, \
     `Page.frameNavigated`, `DOM.documentUpdated`, dan `Log.entryAdded` belum \
     ada, dan `Network.*` hanya melaporkan dokumen navigasi, bukan subresource",
    "`<iframe>` diambil oleh daemon, bukan oleh crate ini: `engine` tidak \
     membuka soket, jadi byte dokumen anak tetap diserahkan pemanggil lewat \
     `native::bingkai::rencana` lalu `Document::pasang_bingkai`. Yang \
     mengisinya adalah `cdp::native_wire::bingkai`, dan batasnya di sana: satu \
     tingkat saja (iframe di dalam iframe tidak diambil), hanya same-origin, \
     maksimal 8 iframe per dokumen, 4 MiB per dokumen, 12 MiB total, dan 8 \
     detik per pengambilan. Yang sudah berjalan: kueri selektor, layout, \
     raster, pohon AX, dan `DOM.getDocument` menembus dokumen anak, `srcdoc` \
     menang atas `src`, dan iframe lintas asal ditolak beserta alasannya. Yang \
     belum: `sandbox`/`allow`/`csp` tidak dibaca, dan \
     `contentDocument`/`contentWindow` tidak ada di isolat, jadi skrip halaman \
     tidak bisa menyentuh dokumen anak",
    "shadow root tertutup tidak terlihat skrip halaman, dan itu memang \
     spesifikasinya, bukan celah: `selectors.rs` punya dua jalur, \
     `pilih_terang` untuk skrip halaman yang berhenti di batas pohon, dan \
     `pilih` untuk otomasi yang menembusnya. Yang belum: `<slot>` dan \
     `assignedNodes` (isi bayangan tidak diproyeksikan), `:host`/`::slotted`/ \
     `:host-context` belum dikenali parser selektor, dan `elementFromPoint` \
     belum menembus bayangan",
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

    #[error("operasi DOM ditolak: {0}")]
    Dom(String),

    /// Skrip berjalan lebih lama dari pagu waktunya lalu dibatalkan.
    ///
    /// Dibedakan dari [`Self::Script`] karena penyebabnya bukan kesalahan
    /// skrip: skrip yang benar pun bisa melewati batas kalau halaman memang
    /// berat. Pemanggil yang ingin menaikkan batas perlu tahu itu.
    #[error("skrip melewati batas waktu {0} dan dibatalkan")]
    BatasWaktu(String),
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

    pub(crate) fn batas_waktu(batas: std::time::Duration) -> Self {
        Self::BatasWaktu(format!("{} ms", batas.as_millis()))
    }

    pub(crate) fn raster(pesan: impl Into<String>) -> Self {
        Self::Raster(pesan.into())
    }

    pub(crate) fn dom(pesan: impl Into<String>) -> Self {
        Self::Dom(pesan.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC-001: an HTML5 page with custom tags and nested stylesheet styling
    /// produces a valid node tree **plus** computed styles and boxes.
    ///
    /// The three assertions are deliberately separate, because the earlier
    /// `dom.rs` test proved parsing only: a tree with no computed style and no
    /// box would have passed it. Here a custom element gets its colour from a
    /// stylesheet rule, inherits it into a nested child, and both end up with
    /// real boxes, so neither half can regress unnoticed.
    #[test]
    fn test_dom_parsing_dan_styling() {
        let html = "<!DOCTYPE html><html><head><style>\
             page-kartu { display: flex; flex-direction: column; width: 300px; \
             color: #123456; font-size: 20px }\
             page-kartu > isi-baris { width: 100%; height: 2em }\
             </style></head><body>\
             <page-kartu id=\"kartu\"><isi-baris id=\"baris\">halo</isi-baris>\
             </page-kartu></body></html>";

        let doc = Document::parse_html(html).expect("html harus terurai");

        // 1. Node tree: custom tags survive parsing as elements, nested.
        let kartu = doc.cari_id("kartu").expect("custom tag harus ada");
        let baris = doc
            .cari_id("baris")
            .expect("custom tag bersarang harus ada");
        assert_eq!(doc.node(kartu).and_then(Node::tag), Some("page-kartu"));
        assert_eq!(doc.node(baris).and_then(Node::tag), Some("isi-baris"));
        assert_eq!(
            doc.node(baris).and_then(|n| n.parent),
            Some(kartu),
            "isi-baris harus anak dari page-kartu"
        );

        // 2. Computed styles: stylesheet rule applied, inheritance carried.
        let laporan =
            layout::hitung(&doc, Viewport::new(800.0, 600.0)).expect("layout harus jalan");
        let gaya_baris = laporan
            .gaya_terhitung(baris)
            .expect("anak harus punya gaya terhitung");
        assert_eq!(
            gaya_baris.warna.as_deref(),
            Some("#123456"),
            "color dari aturan stylesheet harus waris ke anak"
        );
        assert_eq!(
            gaya_baris.ukuran_font, 20.0,
            "font-size 20px dari induk harus waris"
        );

        // 3. Boxes: both elements are laid out, and `2em` resolved against
        // the inherited 20px, so the child is 40px tall and 100% wide.
        let kotak_kartu = laporan.kotak_untuk(kartu).expect("kartu harus punya kotak");
        let kotak_baris = laporan.kotak_untuk(baris).expect("baris harus punya kotak");
        assert_eq!(kotak_kartu.width, 300.0, "width 300px dari stylesheet");
        assert_eq!(
            kotak_baris.height, 40.0,
            "2em terhadap font-size warisan 20px harus 40px"
        );
        assert_eq!(
            kotak_baris.width, 300.0,
            "100% harus mengikuti lebar induk 300px"
        );
    }
}
