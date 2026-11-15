//! Isolat JavaScript per tab lewat `rquickjs`.
//!
//! Satu tab panas memiliki satu isolat sendiri. Itu bukan kerapian, itu batas
//! keamanan: halaman A tidak boleh bisa membaca variabel halaman B. Karena itu
//! tidak ada isolat global yang dipakai bersama.
//!
//! Setiap runtime dipasangi pagu memori dan pagu stack, sehingga skrip halaman
//! yang rakus atau rekursif berhenti dengan galat, bukan memakan seluruh RAM
//! proses.
//!
//! `Isolat` **tidak** `Send`/`Sync`: milik satu thread, dan itu disengaja.
//! Pemanggil yang butuh lintas thread membungkusnya sendiri dengan
//! `spawn_blocking`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rquickjs::{Context, Function, Runtime};

use super::NativeError;
use super::ambil::{JawabanAmbil, Pengambil, PengambilTolak, PermintaanAmbil};
use super::css::LembarLuar;
use super::dom::{Document, ModeBayang, NodeId, NodeKind};
use super::layout::{self, LaporanLayout, Viewport};

/// Konteks halaman yang dibaca skrip: URL dan ukuran viewport.
///
/// Dipisah dari [`Isolat`] karena isinya milik **tab**, bukan milik runtime
/// JavaScript. [`super::tab_pool::TabPool`] yang mengisi, isolat yang membaca.
///
/// Layout dihitung **malas**, dan itu keputusan memori: `TabPanas` sengaja
/// tidak menyimpan [`LaporanLayout`] karena biayanya 0,551 MB per tab untuk
/// data yang tidak dibaca siapa pun (lihat catatan di sana). Menyimpannya di
/// sini untuk semua tab akan mengembalikan biaya yang sudah dibuang. Yang
/// disimpan hanya kalau skrip benar-benar memanggil `getComputedStyle` atau
/// `getBoundingClientRect`, dan dibuang begitu pohon berubah.
pub struct Halaman {
    url: String,
    viewport: Viewport,
    lembar: Vec<LembarLuar>,
    /// Dinaikkan setiap operasi jembatan yang mengubah pohon.
    ///
    /// Dipakai sebagai kunci cache layout: tanpa ini, `getBoundingClientRect`
    /// sesudah `appendChild` akan menjawab kotak versi lama, dan nilai basi
    /// yang diam itu kelas kesalahan yang paling sulit ditemukan.
    versi: AtomicU64,
    /// Cache layout, dikunci `(versi pohon, lebar, tinggi)`.
    ///
    /// Viewport ikut jadi kunci karena laporan yang sama tidak sah untuk
    /// ukuran jendela lain: `width: 100%` dan `@media` menghasilkan kotak
    /// berbeda, dan memakai ulang laporan lebar lama adalah cara paling halus
    /// membuat tangkapan layar salah tanpa satu pun galat.
    layout: Mutex<Option<((u64, u32, u32), Arc<LaporanLayout>)>>,
    /// Face `@font-face` milik dokumen, kalau ada.
    ///
    /// Ikut ke perhitungan layout malas karena kotak teks yang diukur dengan
    /// face tertanam lalu dilukis dengan webfont meluap atau menyisakan
    /// celah, dan `getBoundingClientRect` akan melaporkan angka yang tidak
    /// cocok dengan tangkapan layar.
    font: Option<Arc<super::teks::KumpulanFont>>,
}

impl Halaman {
    /// Konteks satu halaman. `lembar` adalah stylesheet eksternal milik
    /// dokumen, urut sesuai `<link>`-nya.
    #[must_use]
    pub fn baru(url: impl Into<String>, viewport: Viewport, lembar: Vec<LembarLuar>) -> Self {
        Self {
            url: url.into(),
            viewport,
            lembar,
            versi: AtomicU64::new(0),
            layout: Mutex::new(None),
            font: None,
        }
    }

    /// Memasang face webfont dokumen supaya layout malas memakai ukuran yang
    /// sama dengan lapisan lukis.
    #[must_use]
    pub fn dengan_font(mut self, font: Arc<super::teks::KumpulanFont>) -> Self {
        self.font = Some(font);
        self
    }

    /// URL dokumen ini. Dipakai `location` dan resolusi `src` skrip.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Ukuran viewport yang dipakai `window.innerWidth`/`innerHeight`.
    #[must_use]
    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    /// Menandai pohon berubah, sehingga cache layout tidak dipakai lagi.
    fn naikkan_versi(&self) {
        self.versi.fetch_add(1, Ordering::Relaxed);
    }

    /// Layout untuk versi pohon saat ini pada viewport `self.viewport`.
    fn layout(&self, dok: &Document) -> Option<Arc<LaporanLayout>> {
        self.layout_untuk(dok, self.viewport)
    }

    /// Layout untuk versi pohon saat ini pada viewport mana pun.
    ///
    /// Dipublikasikan supaya jalur tangkapan layar memakai laporan yang
    /// **sama** dengan yang menjawab `getBoundingClientRect`, bukan menata
    /// ulang: satu halaman 1.206 elemen butuh belasan detik untuk ditata, dan
    /// tangkapan berulang tanpa cache membayar biaya itu di setiap iterasi.
    pub fn layout_untuk(&self, dok: &Document, viewport: Viewport) -> Option<Arc<LaporanLayout>> {
        let kunci = (
            self.versi.load(Ordering::Relaxed),
            viewport.width.to_bits(),
            viewport.height.to_bits(),
        );
        let mut simpan = self.layout.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((kunci_cache, laporan)) = simpan.as_ref()
            && *kunci_cache == kunci
        {
            return Some(Arc::clone(laporan));
        }
        let laporan = match &self.font {
            Some(font) => layout::hitung_dengan_font(dok, viewport, &self.lembar, Arc::clone(font)),
            None => layout::hitung_dengan(dok, viewport, &self.lembar),
        };
        let laporan = Arc::new(laporan.ok()?);
        *simpan = Some((kunci, Arc::clone(&laporan)));
        Some(laporan)
    }
}

/// Sumber satu `<script>` di dokumen.
enum SumberSkrip {
    /// Teks di antara `<script>` dan `</script>`.
    Sebaris(String),
    /// Atribut `src`, apa adanya. Resolusi relatifnya urusan pemanggil.
    Luar(String),
}

struct SkripHalaman {
    sumber: SumberSkrip,
}

/// Hasil satu putaran eksekusi skrip halaman.
///
/// Tiga kolom, bukan satu boolean: "dijalankan" tanpa "dilewati" dan "galat"
/// akan membuat halaman yang setengah jalan terlihat seperti halaman yang
/// beres.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LaporanSkrip {
    /// Jumlah skrip yang **dipanggil**, termasuk yang melempar galat.
    ///
    /// Yang gagal tetap dihitung: yang gagal adalah penyelesaiannya, bukan
    /// pemanggilannya, dan menghitungnya sebagai nol membuat halaman yang
    /// skripnya error terlihat seperti halaman tanpa skrip.
    pub dijalankan: usize,
    /// Skrip yang sengaja tidak dijalankan, beserta alasannya.
    pub dilewati: Vec<String>,
    /// Skrip yang dijalankan lalu melempar galat. Halaman tetap lanjut.
    pub galat: Vec<String>,
}

impl LaporanSkrip {
    /// Benar kalau ada skrip yang berjalan tanpa galat sama sekali.
    #[must_use]
    pub fn bersih(&self) -> bool {
        self.dijalankan > 0 && self.galat.is_empty()
    }
}

/// Mengumpulkan `<script>` dalam urutan dokumen.
///
/// Yang ditelusuri hanya anak pohon terang: isi `<template>` di browser juga
/// tidak dieksekusi sampai dipakai, dan isi shadow root punya isolat sendiri.
fn kumpulkan_skrip(dok: &Document, laporan: &mut LaporanSkrip) -> Vec<SkripHalaman> {
    let mut hasil = Vec::new();
    let mut tumpuk = vec![dok.root()];
    while let Some(kini) = tumpuk.pop() {
        let Some(node) = dok.node(kini) else {
            continue;
        };
        if node.tag() == Some("script") {
            let tipe = node.attr("type").unwrap_or("").trim().to_ascii_lowercase();
            if tipe == "module" {
                laporan
                    .dilewati
                    .push("<script type=\"module\"> dilewati: pemuat impor belum ada".to_owned());
            } else if tipe.is_empty()
                || tipe == "text/javascript"
                || tipe == "application/javascript"
                || tipe == "text/ecmascript"
                || tipe == "application/ecmascript"
            {
                match node.attr("src").map(str::trim).filter(|s| !s.is_empty()) {
                    Some(src) => hasil.push(SkripHalaman {
                        sumber: SumberSkrip::Luar(src.to_owned()),
                    }),
                    None => hasil.push(SkripHalaman {
                        sumber: SumberSkrip::Sebaris(dok.teks(kini)),
                    }),
                }
            } else {
                laporan.dilewati.push(format!(
                    "<script type={tipe:?}> dilewati: bukan MIME JavaScript"
                ));
            }
            // Isi `<script>` bukan markup: anaknya teks, dan menelusurinya
            // hanya akan menemukan node teks yang tidak perlu dijalankan.
            continue;
        }
        for &anak in node.children.iter().rev() {
            tumpuk.push(anak);
        }
    }
    hasil
}

/// Pagu keras untuk satu isolat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaguIsolat {
    pub memori_bytes: usize,
    pub stack_bytes: usize,
    /// Batas waktu satu evaluasi skrip.
    ///
    /// Pagu memori menghentikan skrip yang rakus, tapi **tidak** menghentikan
    /// `while (true) {}`: skrip seperti itu tidak mengalokasikan apa pun, jadi
    /// ia berputar selamanya dan tab itu membeku tanpa galat. Mekanismenya
    /// diambil dari Luau (`lua_callbacks(L)->interrupt`, dipanggil berkala oleh
    /// interpreter): QuickJS menyediakan jalur yang sama lewat
    /// `JS_SetInterruptHandler`, dan `rquickjs` mengeksposnya sebagai
    /// [`Runtime::set_interrupt_handler`]. Handler yang mengembalikan `true`
    /// membatalkan skrip yang sedang berjalan.
    ///
    /// Nol berarti tanpa batas, dan itu hanya dipakai tes yang memang perlu
    /// berjalan lama.
    pub batas_waktu: Duration,
}

impl Default for PaguIsolat {
    /// 8 MB heap per tab. Di atas biaya terukur 3,10 MB per tab, cukup untuk
    /// halaman biasa, dan tetap di bawah anggaran 25 tab panas.
    fn default() -> Self {
        Self {
            memori_bytes: 8 * 1024 * 1024,
            stack_bytes: 512 * 1024,
            batas_waktu: Duration::from_secs(5),
        }
    }
}

impl PaguIsolat {
    #[must_use]
    pub fn new(memori_bytes: usize, stack_bytes: usize) -> Self {
        Self {
            memori_bytes,
            stack_bytes,
            batas_waktu: Duration::from_secs(5),
        }
    }

    /// Pagu dengan batas waktu pilihan. `Duration::ZERO` mematikan batas itu.
    #[must_use]
    pub fn dengan_batas_waktu(mut self, batas: Duration) -> Self {
        self.batas_waktu = batas;
        self
    }
}

/// Titik nol bersama untuk semua tenggat isolat.
///
/// `Instant` tidak bisa disimpan di atomik, jadi yang disimpan adalah jumlah
/// milidetik sejak titik nol ini. Satu titik nol untuk seluruh proses membuat
/// perbandingan tenggat cukup satu `load` tanpa kunci: handler interrupt
/// dipanggil berkala oleh interpreter, dan mengunci mutex di jalur itu akan
/// membebani setiap putaran skrip halaman.
fn sekarang_ms() -> u64 {
    static AWAL: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let awal = AWAL.get_or_init(Instant::now);
    // `Instant` monotonik, jadi durasi ini tidak pernah negatif dan tidak
    // terpengaruh perubahan jam sistem. Saturasi di u64::MAX hanya untuk
    // memenuhi tipe, tidak tercapai dalam praktik.
    u64::try_from(awal.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Jembatan minimum yang dipasang sebelum skrip halaman dijalankan.
///
/// Cakupannya sengaja sempit: hanya `console` dan alias `window`. Tidak ada
/// `fetch`, tidak ada `require`, tidak ada akses berkas maupun jaringan dari
/// dalam isolat. Mesin ini sudah bisa memuat dokumen `http`/`https`, tapi itu
/// jalur navigasi di `cdp::native_wire::jaringan`, bukan izin bagi skrip
/// halaman untuk membuka koneksi sendiri; `fetch` tetap tercatat di
/// [`super::CELAH`] sampai ada jalur yang melewati kebijakan daemon.
const PEMBUKA: &str = r#"
// Buffer log dipasang non-enumerable: `Object.keys(globalThis)` di browser
// sungguhan tidak memuatnya, dan AC-018 menuntut mesin ini tidak menambah
// global yang bisa dipakai sidik jari. Nama `__log` tetap bisa ditebak kalau
// skrip mencarinya langsung; itu tercatat di CELAH, bukan disembunyikan.
Object.defineProperty(globalThis, "__log", {
  value: [],
  enumerable: false,
  writable: true,
  configurable: false,
});
globalThis.console = {
  log: function () {
    var bagian = [];
    for (var i = 0; i < arguments.length; i++) bagian.push(String(arguments[i]));
    globalThis.__log.push(bagian.join(" "));
  },
};
globalThis.window = globalThis;
globalThis.document = globalThis.document || {};
// Di browser sungguhan `document.title` selalu berupa string; tanpa `<title>`
// nilainya string kosong. Dibiarkan `undefined`, `document.title === ""` jadi
// salah dan skrip halaman mengambil cabang yang keliru.
globalThis.document.title = "";
// FR-009: identitas tanpa kebocoran `webdriver`. Nilainya boolean, bukan
// string kosong, karena skrip anti-bot membandingkan `=== false`. UA menyebut
// mesin ini apa adanya, bukan menyamar sebagai Chrome: menyamar akan jadi
// klaim palsu yang tidak bisa dipertahankan. Objek ini objek biasa, bukan
// instance `Navigator`; itu tercatat di CELAH bersama `document`.
globalThis.navigator = {
  webdriver: false,
  userAgent: "Mozilla/5.0 (X11; Linux x86_64) OpenCrabsBrowser/0.2.0",
  language: "en-US",
  languages: ["en-US", "en"],
  platform: "Linux x86_64",
  vendor: "",
  hardwareConcurrency: 4,
};
"#;

/// Lapisan DOM di atas satu fungsi jembatan Rust.
///
/// Yang dikerjakan di JavaScript hanya pembungkusan: nama properti, prototype,
/// dan identitas objek. Semua isi pohon tetap milik Rust, jadi tidak ada dua
/// salinan dokumen yang bisa menyimpang. Nama jembatan dihapus di baris kedua,
/// sebelum `document` dipasang, sehingga tidak ada global tambahan yang bisa
/// dipakai skrip halaman untuk menyidik mesin ini.
///
/// Batas yang **tidak** disembunyikan: `querySelectorAll` mengembalikan `Array`
/// biasa alih-alih `NodeList` hidup, tidak ada `document.write`, dan tidak ada
/// `style`/CSSOM yang bisa ditulis. Semuanya tercatat di [`super::CELAH`].
/// Nama global tempat dispatcher peristiwa dipasang. Non-enumerable supaya
/// tidak muncul di `Object.keys(globalThis)`; lihat komentar di akhir
/// [`JEMBATAN_DOM`] untuk alasan ia tidak dihapus seperti `__dom`.
const NAMA_KIRIM: &str = "__kirim";

const JEMBATAN_DOM: &str = r##"
(function () {
  var jembatan = globalThis.__dom;
  delete globalThis.__dom;
  if (typeof jembatan !== "function") return;

  function panggil(op, a, b, c) {
    var mentah = jembatan(
      op,
      a === undefined || a === null ? "" : String(a),
      b === undefined || b === null ? "" : String(b),
      c === undefined || c === null ? "" : String(c)
    );
    var nilai = JSON.parse(mentah);
    if (nilai && typeof nilai === "object" && typeof nilai.__galat === "string") {
      throw new Error(nilai.__galat);
    }
    return nilai;
  }

  // Nomor node disimpan di WeakMap, bukan sebagai properti `__id`: properti
  // enumerable akan terlihat lewat `Object.keys(el)`, dan elemen DOM sungguhan
  // tidak punya properti semacam itu.
  var nomor = new WeakMap();
  var bungkusCache = new Map();

  function idDari(simpul) {
    var id = nomor.get(simpul);
    if (id === undefined) throw new TypeError("objek ini bukan node dari dokumen");
    return id;
  }

  function bungkus(id) {
    if (id === null || id === undefined || id < 0) return null;
    var ada = bungkusCache.get(id);
    if (ada) return ada;
    var jenis = panggil("nodeType", id);
    var baru =
      jenis === 3 ? new SimpulTeks(id)
      : jenis === 11 ? new SimpulBayang(id)
      : new SimpulElemen(id);
    bungkusCache.set(id, baru);
    return baru;
  }

  function SimpulTeks(id) {
    nomor.set(this, id);
  }

  function SimpulElemen(id) {
    nomor.set(this, id);
  }

  Object.defineProperties(SimpulTeks.prototype, {
    nodeType: { get: function () { return 3; } },
    nodeName: { get: function () { return "#text"; } },
    data: {
      get: function () { return panggil("text", idDari(this)); },
      set: function (v) { panggil("setText", idDari(this), v); },
    },
    nodeValue: {
      get: function () { return panggil("text", idDari(this)); },
      set: function (v) { panggil("setText", idDari(this), v); },
    },
    textContent: {
      get: function () { return panggil("text", idDari(this)); },
      set: function (v) { panggil("setText", idDari(this), v); },
    },
    parentNode: {
      get: function () { return bungkus(panggil("parent", idDari(this))); },
    },
  });

  Object.defineProperties(SimpulElemen.prototype, {
    nodeType: { get: function () { return 1; } },
    tagName: {
      get: function () { return String(panggil("tag", idDari(this))).toUpperCase(); },
    },
    nodeName: {
      get: function () { return String(panggil("tag", idDari(this))).toUpperCase(); },
    },
    id: {
      get: function () { var v = panggil("attr", idDari(this), "id"); return v === null ? "" : v; },
      set: function (v) { panggil("setAttr", idDari(this), "id", v); },
    },
    className: {
      get: function () { var v = panggil("attr", idDari(this), "class"); return v === null ? "" : v; },
      set: function (v) { panggil("setAttr", idDari(this), "class", v); },
    },
    textContent: {
      get: function () { return panggil("text", idDari(this)); },
      set: function (v) { panggil("setText", idDari(this), v); },
    },
    childNodes: {
      get: function () {
        return (panggil("children", idDari(this)) || []).map(bungkus);
      },
    },
    children: {
      get: function () {
        var anak = panggil("children", idDari(this)) || [];
        var hasil = [];
        for (var i = 0; i < anak.length; i++) {
          if (panggil("nodeType", anak[i]) === 1) hasil.push(bungkus(anak[i]));
        }
        return hasil;
      },
    },
    firstChild: {
      get: function () {
        var anak = panggil("children", idDari(this)) || [];
        return anak.length ? bungkus(anak[0]) : null;
      },
    },
    lastChild: {
      get: function () {
        var anak = panggil("children", idDari(this)) || [];
        return anak.length ? bungkus(anak[anak.length - 1]) : null;
      },
    },
    parentNode: {
      get: function () { return bungkus(panggil("parent", idDari(this))); },
    },
  });

  // Anak yang berupa elemen. `children` menyaring elemen di Rust, tapi
  // `firstElementChild` tetap butuh larik nomornya sendiri supaya
  // `childElementCount` tidak memanggil jembatan dua kali.
  function anakElemen(objek) {
    var anak = panggil("children", idDari(objek)) || [];
    var hasil = [];
    for (var i = 0; i < anak.length; i++) {
      if (panggil("nodeType", anak[i]) === 1) hasil.push(anak[i]);
    }
    return hasil;
  }

  Object.defineProperties(SimpulElemen.prototype, {
    firstElementChild: {
      get: function () { var a = anakElemen(this); return a.length ? bungkus(a[0]) : null; },
    },
    lastElementChild: {
      get: function () { var a = anakElemen(this); return a.length ? bungkus(a[a.length - 1]) : null; },
    },
    childElementCount: {
      get: function () { return anakElemen(this).length; },
    },
    // `innerText` di browser sadar-layout (ia menghormati `display:none` dan
    // menyusun baris). Di sini nilainya sama dengan `textContent`, dan itu
    // disebut di CELAH: nilai yang lebih kecil dari seharusnya lebih jujur
    // daripada susunan baris yang dikarang.
    innerText: {
      get: function () { return panggil("text", idDari(this)); },
      set: function (v) { panggil("setText", idDari(this), v); },
    },
    // Setter `innerHTML` mengurai fragmen lalu mengganti seluruh anak. Uraiannya
    // di Rust (`Document::ganti_dalam_html`), karena parser HTML5 ada di sana
    // dan menyalinnya ke JavaScript berarti dua parser yang bisa menyimpang.
    innerHTML: {
      get: function () { return panggil("html", idDari(this)); },
      set: function (v) { panggil("setHtml", idDari(this), v); },
    },
    // `outerHTML` getter. Setter-nya di DOM berarti mengganti simpul itu
    // sendiri di induknya dengan hasil uraian; itu belum ada dan disebut di
    // CELAH, bukan diam-diam diabaikan.
    outerHTML: {
      get: function () { return panggil("outer", idDari(this)); },
    },
  });

  // --- classList, dataset, dan saudara ------------------------------------
  //
  // Semuanya bekerja di atas atribut yang sudah ada (`attr`/`setAttr`/
  // `delAttr`), jadi tidak ada state kedua yang bisa menyimpang dari pohon.
  // `classList` sengaja objek baru tiap akses: DOM sungguhan mengembalikan
  // `DOMTokenList` hidup yang sama, tapi satu-satunya hal yang dibaca skrip
  // darinya adalah isinya, dan isi itu selalu dibaca ulang dari atribut.
  function daftarKelas(el) {
    var mentah = panggil("attr", idDari(el), "class");
    if (mentah === null || mentah === undefined) return [];
    var bagian = String(mentah).split(/\s+/);
    var hasil = [];
    for (var i = 0; i < bagian.length; i++) {
      if (bagian[i]) hasil.push(bagian[i]);
    }
    return hasil;
  }

  function tulisKelas(el, daftar) {
    var unik = [];
    for (var i = 0; i < daftar.length; i++) {
      if (unik.indexOf(daftar[i]) < 0) unik.push(daftar[i]);
    }
    if (unik.length) panggil("setAttr", idDari(el), "class", unik.join(" "));
    else panggil("delAttr", idDari(el), "class");
  }

  function Kelas(el) {
    this.__el = el;
  }
  Object.defineProperties(Kelas.prototype, {
    length: { get: function () { return daftarKelas(this.__el).length; } },
    value: { get: function () { return daftarKelas(this.__el).join(" "); } },
  });
  Kelas.prototype.item = function (i) { return daftarKelas(this.__el)[i] || null; };
  Kelas.prototype.contains = function (n) {
    return daftarKelas(this.__el).indexOf(String(n)) >= 0;
  };
  Kelas.prototype.add = function () {
    var d = daftarKelas(this.__el);
    for (var i = 0; i < arguments.length; i++) {
      var n = String(arguments[i]);
      if (n && d.indexOf(n) < 0) d.push(n);
    }
    tulisKelas(this.__el, d);
  };
  Kelas.prototype.remove = function () {
    var buang = Array.prototype.map.call(arguments, String);
    var d = daftarKelas(this.__el).filter(function (n) { return buang.indexOf(n) < 0; });
    tulisKelas(this.__el, d);
  };
  Kelas.prototype.toggle = function (n, paksa) {
    var nama = String(n);
    var ada = daftarKelas(this.__el).indexOf(nama) >= 0;
    var harus_ada = paksa === undefined ? !ada : !!paksa;
    if (harus_ada && !ada) this.add(nama);
    if (!harus_ada && ada) this.remove(nama);
    return harus_ada;
  };
  Kelas.prototype.replace = function (lama, baru) {
    var d = daftarKelas(this.__el);
    var i = d.indexOf(String(lama));
    if (i < 0) return false;
    d[i] = String(baru);
    tulisKelas(this.__el, d);
    return true;
  };
  Kelas.prototype.forEach = function (fn, ini) {
    var d = daftarKelas(this.__el);
    for (var i = 0; i < d.length; i++) fn.call(ini, d[i], i, this);
  };
  Kelas.prototype.toString = function () { return daftarKelas(this.__el).join(" "); };

  Object.defineProperty(SimpulElemen.prototype, "classList", {
    get: function () { return new Kelas(this); },
  });

  // `dataset` memakai Proxy karena namanya sembarang: `data-ubah-nama` dibaca
  // sebagai `.ubahNama`. Pemetaan camelCase <-> tanda hubung ada di sini, dan
  // nilainya tetap di atribut, jadi tidak ada salinan yang bisa basi.
  //
  // Batas yang jujur: `Object.keys(el.dataset)` kosong karena tidak ada operasi
  // jembatan yang mendaftar atribut, dan itu disebut di CELAH.
  function namaData(kunci) {
    return "data-" + String(kunci).replace(/[A-Z]/g, function (c) {
      return "-" + c.toLowerCase();
    });
  }

  function datasetDari(el) {
    return new Proxy({}, {
      get: function (_t, kunci) {
        if (typeof kunci !== "string") return undefined;
        var v = panggil("attr", idDari(el), namaData(kunci));
        return v === null ? undefined : v;
      },
      set: function (_t, kunci, nilai) {
        panggil("setAttr", idDari(el), namaData(kunci), String(nilai));
        return true;
      },
      deleteProperty: function (_t, kunci) {
        panggil("delAttr", idDari(el), namaData(kunci));
        return true;
      },
      has: function (_t, kunci) {
        if (typeof kunci !== "string") return false;
        return panggil("attr", idDari(el), namaData(kunci)) !== null;
      },
    });
  }

  Object.defineProperty(SimpulElemen.prototype, "dataset", {
    get: function () { return datasetDari(this); },
  });

  // Anak elemen dari sebuah **nomor** node, bukan objek. Dipakai pencarian
  // saudara, yang sudah memegang nomor induk dan tidak perlu membungkusnya.
  function anakElemenDari(id) {
    var anak = panggil("children", id) || [];
    var hasil = [];
    for (var i = 0; i < anak.length; i++) {
      if (panggil("nodeType", anak[i]) === 1) hasil.push(anak[i]);
    }
    return hasil;
  }

  function saudara(el, arah) {
    var induk = panggil("parent", idDari(el));
    if (induk < 0) return null;
    var anak = anakElemenDari(induk);
    var sendiri = idDari(el);
    for (var i = 0; i < anak.length; i++) {
      if (anak[i] === sendiri) {
        var j = i + arah;
        return j >= 0 && j < anak.length ? bungkus(anak[j]) : null;
      }
    }
    return null;
  }

  Object.defineProperties(SimpulElemen.prototype, {
    nextElementSibling: { get: function () { return saudara(this, 1); } },
    previousElementSibling: { get: function () { return saudara(this, -1); } },
    // `ownerDocument` selalu dokumen yang memuat isolat ini: satu tab satu
    // dokumen, jadi tidak ada dokumen lain yang bisa jadi pemilik.
    ownerDocument: { get: function () { return dokumen; } },
    isConnected: { get: function () { return panggil("connected", idDari(this)); } },
  });

  SimpulElemen.prototype.matches = function (s) {
    return panggil("matches", idDari(this), String(s));
  };
  // `closest` naik lewat `parent`, bukan lewat `parentNode` yang membungkus:
  // jalur itu tidak membuat objek baru untuk leluhur yang tidak dipakai.
  SimpulElemen.prototype.closest = function (s) {
    var selektor = String(s);
    var kini = idDari(this);
    while (kini >= 0) {
      if (panggil("matches", kini, selektor)) return objekDari(kini);
      kini = panggil("parent", kini);
    }
    return null;
  };

  SimpulElemen.prototype.insertAdjacentHTML = function (posisi, html) {
    panggil("insertHtml", idDari(this), String(posisi), String(html));
  };

  SimpulElemen.prototype.cloneNode = function (dalam) {
    return bungkus(panggil("clone", idDari(this), dalam ? "1" : "0"));
  };

  SimpulElemen.prototype.replaceChildren = function () {
    var anak = (panggil("children", idDari(this)) || []).slice();
    for (var i = 0; i < anak.length; i++) panggil("remove", anak[i]);
    for (var j = 0; j < arguments.length; j++) {
      var baru = arguments[j];
      if (baru === null || baru === undefined) continue;
      panggil("append", idDari(this), idDari(baru));
    }
  };

  SimpulElemen.prototype.getElementsByTagName = function (tag) {
    return (panggil("tagAll", idDari(this), String(tag)) || []).map(bungkus);
  };

  SimpulElemen.prototype.getBoundingClientRect = function () {
    var r = panggil("rect", idDari(this));
    return {
      x: r.x, y: r.y, width: r.width, height: r.height,
      top: r.top, left: r.left, right: r.right, bottom: r.bottom,
      toJSON: function () {
        return { x: r.x, y: r.y, width: r.width, height: r.height,
                 top: r.top, left: r.left, right: r.right, bottom: r.bottom };
      },
    };
  };

  Object.defineProperty(SimpulElemen.prototype, "shadowRoot", {
    get: function () { return bungkus(panggil("shadowRoot", idDari(this))); },
  });

  SimpulElemen.prototype.getAttribute = function (nama) {
    return panggil("attr", idDari(this), nama);
  };
  SimpulElemen.prototype.setAttribute = function (nama, nilai) {
    panggil("setAttr", idDari(this), nama, nilai);
  };
  SimpulElemen.prototype.removeAttribute = function (nama) {
    panggil("delAttr", idDari(this), nama);
  };
  SimpulElemen.prototype.hasAttribute = function (nama) {
    return panggil("attr", idDari(this), nama) !== null;
  };
  SimpulElemen.prototype.appendChild = function (anak) {
    panggil("append", idDari(this), idDari(anak));
    return anak;
  };
  SimpulElemen.prototype.insertBefore = function (anak, acuan) {
    if (acuan === null || acuan === undefined) {
      panggil("append", idDari(this), idDari(anak));
    } else {
      panggil("insertBefore", idDari(this), idDari(anak), idDari(acuan));
    }
    return anak;
  };
  SimpulElemen.prototype.removeChild = function (anak) {
    panggil("remove", idDari(anak));
    return anak;
  };
  SimpulElemen.prototype.remove = function () {
    panggil("remove", idDari(this));
  };
  SimpulElemen.prototype.querySelector = function (s) {
    return bungkus(panggil("selectDalamTerang", idDari(this), s));
  };
  SimpulElemen.prototype.querySelectorAll = function (s) {
    return (panggil("selectAllDalamTerang", idDari(this), s) || []).map(bungkus);
  };

  // --- shadow DOM ---------------------------------------------------------
  //
  // Root-nya nyata di pohon Rust; yang dijaga di sini hanya siapa yang boleh
  // melihatnya. `shadowRoot` tidak menjawab untuk mode "closed", dan
  // `querySelector` tidak menyeberang batas pohon. Sisi otomasi memakai
  // operasi jembatan yang lain, dan itulah yang menembus keduanya.
  function SimpulBayang(id) {
    nomor.set(this, id);
  }

  Object.defineProperties(SimpulBayang.prototype, {
    nodeType: { get: function () { return 11; } },
    nodeName: { get: function () { return "#document-fragment"; } },
    host: { get: function () { return bungkus(panggil("bayangHost", idDari(this))); } },
    mode: { get: function () { return String(panggil("bayangMode", idDari(this))); } },
    childNodes: {
      get: function () { return (panggil("children", idDari(this)) || []).map(bungkus); },
    },
    children: {
      get: function () {
        var anak = panggil("children", idDari(this)) || [];
        var hasil = [];
        for (var i = 0; i < anak.length; i++) {
          if (panggil("nodeType", anak[i]) === 1) hasil.push(bungkus(anak[i]));
        }
        return hasil;
      },
    },
    firstChild: {
      get: function () {
        var anak = panggil("children", idDari(this)) || [];
        return anak.length ? bungkus(anak[0]) : null;
      },
    },
    lastChild: {
      get: function () {
        var anak = panggil("children", idDari(this)) || [];
        return anak.length ? bungkus(anak[anak.length - 1]) : null;
      },
    },
    textContent: { get: function () { return panggil("text", idDari(this)); } },
  });
  SimpulBayang.prototype.appendChild = function (anak) {
    panggil("append", idDari(this), idDari(anak));
    return anak;
  };
  SimpulBayang.prototype.insertBefore = function (anak, acuan) {
    if (acuan === null || acuan === undefined) {
      panggil("append", idDari(this), idDari(anak));
    } else {
      panggil("insertBefore", idDari(this), idDari(anak), idDari(acuan));
    }
    return anak;
  };
  SimpulBayang.prototype.removeChild = function (anak) {
    panggil("remove", idDari(anak));
    return anak;
  };
  SimpulBayang.prototype.querySelector = function (s) {
    return bungkus(panggil("selectDalamTerang", idDari(this), s));
  };
  SimpulBayang.prototype.querySelectorAll = function (s) {
    return (panggil("selectAllDalamTerang", idDari(this), s) || []).map(bungkus);
  };
  SimpulBayang.prototype.getElementById = function (s) {
    return bungkus(panggil("byidDalam", idDari(this), s));
  };

  SimpulElemen.prototype.attachShadow = function (init) {
    var mode = init && init.mode;
    if (mode !== "open" && mode !== "closed") {
      throw new TypeError('attachShadow butuh mode "open" atau "closed"');
    }
    return bungkus(panggil("attachShadow", idDari(this), mode));
  };

  // --- peristiwa -----------------------------------------------------------
  //
  // Pendengar disimpan di sini, di dalam isolat tab ini, bukan di Rust: nilainya
  // fungsi JavaScript, dan menyalinnya ke Rust berarti memindahkan closure
  // antar-runtime. Kuncinya nomor node, yang stabil selama dokumen tidak
  // diganti; mengganti dokumen membuang isolat ini seluruhnya, jadi pendengar
  // halaman lama tidak bisa menembak elemen halaman baru.
  var pendengar = new Map();

  function petaPendengar(id) {
    var peta = pendengar.get(id);
    if (!peta) {
      peta = new Map();
      pendengar.set(id, peta);
    }
    return peta;
  }

  // Peristiwa minimum. Bukan `EventTarget` lengkap: tidak ada fase tangkap,
  // tidak ada `composedPath`, dan `once`/`passive` diabaikan. Semua itu
  // tercatat di CELAH, bukan disembunyikan di balik nama yang terdengar lengkap.
  function Peristiwa(tipe, opsi) {
    opsi = opsi || {};
    this.type = String(tipe);
    this.target = null;
    this.currentTarget = null;
    this.bubbles = opsi.bubbles === undefined ? false : !!opsi.bubbles;
    this.cancelable = opsi.cancelable === undefined ? false : !!opsi.cancelable;
    this.defaultPrevented = false;
    this.isTrusted = false;
    this.detail = opsi.detail === undefined ? null : opsi.detail;
    this.timeStamp = 0;
    this.__berhenti = false;
    this.__berhentiSeketika = false;
  }
  Peristiwa.prototype.preventDefault = function () {
    if (this.cancelable) this.defaultPrevented = true;
  };
  Peristiwa.prototype.stopPropagation = function () {
    this.__berhenti = true;
  };
  Peristiwa.prototype.stopImmediatePropagation = function () {
    this.__berhenti = true;
    this.__berhentiSeketika = true;
  };
  // `document.createEvent` mengembalikan peristiwa kosong yang diisi lewat
  // `initEvent`. Tanpa ini, pustaka lama yang masih memakai pola 2010 gagal
  // sebelum sempat memanggil apa pun.
  Peristiwa.prototype.initEvent = function (tipe, bubbles, cancelable) {
    this.type = String(tipe);
    this.bubbles = !!bubbles;
    this.cancelable = !!cancelable;
  };
  globalThis.Event = Peristiwa;

  // Nomor node dokumen, diisi setelah `dokumen` dibuat. Dipakai `sebar` untuk
  // mengenali simpul teratas tanpa membungkusnya jadi elemen.
  var docId = -1;

  // Membungkus satu nomor node jadi objek yang bisa dipakai pendengar.
  // Dokumen sendiri bukan elemen: `document` sudah objeknya, jadi dipakai
  // apa adanya supaya `e.currentTarget === document` benar.
  function objekDari(id) {
    return id === docId ? dokumen : bungkus(id);
  }

  // Merambat dari sasaran ke akar. `currentTarget` bergerak, `target` tetap:
  // itulah bedanya bagi pendengar yang memeriksa keduanya.
  //
  // Perambatan memakai **nomor** node, bukan objek, supaya simpul yang tidak
  // punya pembungkus (dokumen) tetap bisa dilewati tanpa mengarang elemen.
  function sebar(ev, id) {
    if (id < 0) return 0;
    var dijalankan = 0;
    var simpulId = id;
    var pertama = true;
    while (simpulId >= 0) {
      var objek = objekDari(simpulId);
      if (!objek) break;
      if (pertama) {
        ev.target = objek;
        pertama = false;
      }
      ev.currentTarget = objek;
      var peta = pendengar.get(simpulId);
      if (peta) {
        var daftar = peta.get(ev.type);
        if (daftar && daftar.length) {
          // Salinan: pendengar yang melepas dirinya sendiri saat dipanggil
          // tidak boleh menggeser larik yang sedang diiterasi.
          var salinan = daftar.slice();
          for (var i = 0; i < salinan.length; i++) {
            salinan[i].call(objek, ev);
            dijalankan++;
            if (ev.__berhentiSeketika) break;
          }
        }
      }
      if (ev.__berhenti || ev.__berhentiSeketika) break;
      if (!ev.bubbles) break;
      simpulId = panggil("parent", simpulId);
    }
    ev.currentTarget = null;
    return dijalankan;
  }

  // Satu-satunya pintu dari Rust: `kirim(tipe, id, detailJson)`.
  function kirim(tipe, id, detailJson) {
    var ev = new Peristiwa(tipe, {
      bubbles: tipe !== "focus" && tipe !== "blur",
      cancelable: true,
    });
    if (detailJson) {
      var detail = JSON.parse(detailJson);
      if (detail && typeof detail === "object") {
        for (var k in detail) ev[k] = detail[k];
      }
    }
    return sebar(ev, id);
  }

  SimpulElemen.prototype.addEventListener = function (tipe, fn) {
    if (typeof fn !== "function") return;
    var daftar = petaPendengar(idDari(this)).get(String(tipe));
    if (!daftar) {
      daftar = [];
      petaPendengar(idDari(this)).set(String(tipe), daftar);
    }
    for (var i = 0; i < daftar.length; i++) {
      if (daftar[i] === fn) return;
    }
    daftar.push(fn);
  };
  SimpulElemen.prototype.removeEventListener = function (tipe, fn) {
    var peta = pendengar.get(idDari(this));
    if (!peta) return;
    var daftar = peta.get(String(tipe));
    if (!daftar) return;
    for (var i = 0; i < daftar.length; i++) {
      if (daftar[i] === fn) {
        daftar.splice(i, 1);
        return;
      }
    }
  };
  SimpulElemen.prototype.dispatchEvent = function (ev) {
    if (!ev || typeof ev.type !== "string") {
      throw new TypeError("dispatchEvent butuh Event");
    }
    return sebar(ev, idDari(this)) > 0;
  };
  SimpulElemen.prototype.click = function () {
    kirim("click", idDari(this), '{"button":0}');
  };
  // `focus()` dan `blur()` hanya mengirim peristiwanya. Tidak ada state fokus
  // di mesin ini, jadi `document.activeElement` tidak bergerak; itu tercatat
  // di CELAH.
  SimpulElemen.prototype.focus = function () {
    kirim("focus", idDari(this), "");
  };
  SimpulElemen.prototype.blur = function () {
    kirim("blur", idDari(this), "");
  };

  var dokumen = {
    nodeType: 9,
    nodeName: "#document",
    getElementById: function (s) { return bungkus(panggil("byidTerang", s)); },
    querySelector: function (s) { return bungkus(panggil("selectTerang", s)); },
    querySelectorAll: function (s) {
      return (panggil("selectAllTerang", s) || []).map(bungkus);
    },
    createElement: function (t) { return bungkus(panggil("create", t)); },
    createTextNode: function (t) { return bungkus(panggil("createText", t)); },
    createEvent: function () { return new Peristiwa("", {}); },
    getElementsByTagName: function (tag) {
      return (panggil("tagAll", docId, String(tag)) || []).map(bungkus);
    },
  };

  Object.defineProperties(dokumen, {
    documentElement: { get: function () { return bungkus(panggil("root")); } },
    head: { get: function () { return bungkus(panggil("selectTerang", "head")); } },
    body: { get: function () { return bungkus(panggil("selectTerang", "body")); } },
    title: {
      get: function () { return panggil("title"); },
      set: function (v) { panggil("setTitle", v); },
    },
    childNodes: {
      get: function () {
        var akar = panggil("root");
        return akar < 0 ? [] : [bungkus(akar)];
      },
    },
  });

  // Pendengar tingkat dokumen. Halaman nyata memasang `click` di `document`
  // jauh lebih sering daripada di tiap elemen, jadi jalur ini ikut dibangun;
  // tanpa itu delegasi peristiwa tidak mungkin.
  docId = panggil("docid");
  dokumen.addEventListener = function (tipe, fn) {
    if (typeof fn !== "function") return;
    var peta = petaPendengar(docId);
    var daftar = peta.get(String(tipe));
    if (!daftar) {
      daftar = [];
      peta.set(String(tipe), daftar);
    }
    for (var i = 0; i < daftar.length; i++) {
      if (daftar[i] === fn) return;
    }
    daftar.push(fn);
  };
  dokumen.removeEventListener = function (tipe, fn) {
    var peta = pendengar.get(docId);
    if (!peta) return;
    var daftar = peta.get(String(tipe));
    if (!daftar) return;
    for (var i = 0; i < daftar.length; i++) {
      if (daftar[i] === fn) {
        daftar.splice(i, 1);
        return;
      }
    }
  };
  dokumen.dispatchEvent = function (ev) {
    if (!ev || typeof ev.type !== "string") {
      throw new TypeError("dispatchEvent butuh Event");
    }
    return sebar(ev, docId) > 0;
  };

  globalThis.document = dokumen;
  globalThis.Element = SimpulElemen;

  // `window.addEventListener` adalah permukaan yang dipakai hampir setiap
  // pustaka, dan di sini `window` adalah objek global. Pendengar tingkat
  // jendela dan tingkat dokumen berbagi satu peta: peristiwa `load` dikirim
  // ke dokumen, dan pendengar yang dipasang lewat `window` ikut menerimanya.
  // Memisahkan dua peta berarti dua jalur penyebaran yang harus diuji
  // terpisah tanpa ada halaman nyata yang membedakannya.
  globalThis.addEventListener = function (tipe, fn) {
    dokumen.addEventListener(tipe, fn);
  };
  globalThis.removeEventListener = function (tipe, fn) {
    dokumen.removeEventListener(tipe, fn);
  };

  // `readyState` bertransisi `loading` -> `interactive` -> `complete` seperti
  // spesifikasi. Nilainya dibaca pustaka sebelum menyentuh DOM; tanpa kolom
  // ini, `document.readyState === 'complete'` bernilai `undefined` dan
  // pustaka menunggu selamanya.
  var keadaanSiap = "loading";
  Object.defineProperty(dokumen, "readyState", {
    get: function () { return keadaanSiap; },
    enumerable: true,
    configurable: true,
  });
  // Pintu satu kali dari Rust: menyelesaikan pemuatan berarti mengirim
  // `DOMContentLoaded` lalu `load`. Urutannya tetap, dan `__siap` dihapus
  // sesudah dipakai supaya tidak jadi global yang bisa dilihat skrip halaman.
  Object.defineProperty(globalThis, "__siap", {
    value: {
      selesai: function () {
        if (keadaanSiap === "complete") return 0;
        keadaanSiap = "interactive";
        sebar(new Peristiwa("DOMContentLoaded", { bubbles: false }), docId);
        keadaanSiap = "complete";
        sebar(new Peristiwa("load", { bubbles: false }), docId);
        return 1;
      },
    },
    enumerable: false,
    configurable: true,
    writable: false,
  });

  // --- konteks halaman: location, viewport, timer --------------------------
  //
  // Nilainya datang dari Rust lewat operasi `halaman`. Isolat tes tidak punya
  // halaman, jadi `panggil` akan melempar; di situ `location.href` diisi teks
  // kosong alih-alih `about:blank` yang bisa menyesatkan skrip yang
  // memeriksanya.
  var info;
  try {
    info = panggil("halaman");
  } catch (e) {
    info = { url: "", lebar: 0, tinggi: 0 };
  }
  globalThis.innerWidth = info.lebar;
  globalThis.innerHeight = info.tinggi;
  globalThis.outerWidth = info.lebar;
  globalThis.outerHeight = info.tinggi;
  globalThis.devicePixelRatio = 1;
  (function () {
    var u = String(info.url || "");
    var m = /^([a-zA-Z][a-zA-Z0-9+.-]*:)\/\/([^\/?#]*)([^?#]*)(\?[^#]*)?(#.*)?$/.exec(u);
    var protokol = m ? m[1] : "";
    var host = m ? m[2] : "";
    var port = (host.match(/:([0-9]+)$/) || ["", ""])[1];
    var lokasi = {
      href: u,
      protocol: protokol,
      host: host,
      hostname: host.replace(/:[0-9]+$/, ""),
      port: port,
      pathname: m ? (m[3] || "/") : "",
      search: m ? (m[4] || "") : "",
      hash: m ? (m[5] || "") : "",
      origin: protokol && host ? protokol + "//" + host : "",
    };
    lokasi.toString = function () { return u; };
    globalThis.location = lokasi;
  })();

  // Antrean timer. rquickjs tidak punya event loop, dan menjalankan satu di
  // dalam isolat berarti satu state mesin lagi yang harus diuji terhadap pagu
  // waktu. Jadi antreannya dikuras Rust lewat `__timer.kuras()`, dan batasnya
  // jujur: timer hanya berjalan saat mesin memompa, bukan selama halaman
  // menganggur.
  var antrean = [];
  var nomorTimer = 1;
  function jadwalkan(fn, jeda, berulang, args) {
    if (typeof fn !== "function") return 0;
    var id = nomorTimer++;
    antrean.push({
      id: id,
      pada: Date.now() + (Number(jeda) || 0),
      jeda: Number(jeda) || 0,
      fn: fn,
      args: args || [],
      berulang: !!berulang,
    });
    return id;
  }
  function batalkan(id) {
    for (var i = 0; i < antrean.length; i++) {
      if (antrean[i].id === id) {
        antrean.splice(i, 1);
        return;
      }
    }
  }
  globalThis.setTimeout = function (fn, jeda) {
    return jadwalkan(fn, jeda, false, Array.prototype.slice.call(arguments, 2));
  };
  globalThis.clearTimeout = batalkan;
  globalThis.setInterval = function (fn, jeda) {
    return jadwalkan(fn, jeda, true, Array.prototype.slice.call(arguments, 2));
  };
  globalThis.clearInterval = batalkan;
  globalThis.requestAnimationFrame = function (fn) {
    return jadwalkan(fn, 0, false, [Date.now()]);
  };
  globalThis.cancelAnimationFrame = batalkan;
  Object.defineProperty(globalThis, "__timer", {
    value: {
      kuras: function () {
        var sekarang = Date.now();
        var siap = [];
        for (var i = 0; i < antrean.length; i++) {
          if (antrean[i].pada <= sekarang) siap.push(antrean[i]);
        }
        siap.sort(function (a, b) { return a.pada - b.pada || a.id - b.id; });
        var dijalankan = 0;
        for (var j = 0; j < siap.length; j++) {
          var t = siap[j];
          if (!t.berulang) batalkan(t.id);
          t.fn.apply(null, t.args);
          dijalankan++;
          if (t.berulang) t.pada = Date.now() + Math.max(t.jeda, 1);
        }
        return dijalankan;
      },
      jumlah: function () { return antrean.length; },
    },
    enumerable: false,
    configurable: false,
    writable: false,
  });

  // `getComputedStyle` membaca hasil layout, bukan atribut `style`: nilai yang
  // dikembalikan sudah lewat kaskade dan pewarisan, sama seperti browser.
  // Properti yang tidak dihitung mesin ini menjawab teks kosong, bukan angka
  // karangan.
  function Gaya(el) {
    this.__peta = panggil("style", idDari(el));
  }
  Gaya.prototype.getPropertyValue = function (nama) {
    var kunci = String(nama);
    if (kunci in this.__peta) return this.__peta[kunci];
    var kebab = kunci.replace(/[A-Z]/g, function (c) { return "-" + c.toLowerCase(); });
    return kebab in this.__peta ? this.__peta[kebab] : "";
  };
  var PROPERTI_GAYA = [
    "color", "fontSize", "fontWeight", "lineHeight", "visibility",
    "textAlign", "fontFamily", "display", "position", "width", "height",
  ];
  for (var gi = 0; gi < PROPERTI_GAYA.length; gi++) {
    (function (nama) {
      Object.defineProperty(Gaya.prototype, nama, {
        get: function () { return this.getPropertyValue(nama); },
      });
    })(PROPERTI_GAYA[gi]);
  }
  globalThis.getComputedStyle = function (el) {
    if (!el) throw new TypeError("getComputedStyle butuh elemen");
    return new Gaya(el);
  };

  // Dispatcher dipasang **non-enumerable**, sama seperti `__log`. Rust harus
  // bisa mengambilnya lagi pada setiap peristiwa, dan satu-satunya cara
  // memegang fungsi JavaScript lintas panggilan adalah `Persistent`, yang
  // `!Send`; menyimpannya di `Isolat` akan membatalkan `Engine: Send + Sync`.
  //
  // Batas yang jujur: skrip halaman yang menebak nama ini bisa memanggilnya
  // untuk mengirim peristiwa ke node mana pun. Itu tidak menambah kuasa yang
  // belum dimilikinya, karena `dispatchEvent` dan `element.click()` memang
  // sudah sengaja terbuka, dan `Object.keys(globalThis)` tidak memuatnya
  // sehingga sidik jari `AC-018` tetap bersih.
  Object.defineProperty(globalThis, "__kirim", {
    value: kirim,
    enumerable: false,
    configurable: false,
    writable: false,
  });
})();
"##;

/// Lapisan `fetch()` di atas satu fungsi jembatan Rust.
///
/// Mesin ini tidak membuka soket sendiri: `__ambil` memanggil
/// [`Pengambil`] milik pemanggil, yang memegang kebijakan jaringan daemon.
/// Tanpa hook, panggilan berakhir dengan galat yang menyebut sebabnya, bukan
/// jawaban kosong yang terlihat seperti halaman tanpa isi.
///
/// Yang dikembalikan **Promise asli**, dan antrean microtask-nya dikuras
/// [`pompa_mikrotugas`] sesudah setiap evaluasi, jadi `await fetch(...)`
/// maupun `fetch(...).then(...)` sama-sama selesai dalam satu panggilan.
/// Executor promise berjalan sinkron, sehingga gerbang jaringan tetap
/// diputuskan sebelum `fetch` kembali.
const JEMBATAN_AMBIL: &str = r##"
(function () {
  var ambil = globalThis.__ambil;
  delete globalThis.__ambil;
  if (typeof ambil !== "function") return;

  function jawaban(data) {
    var teks = typeof data.badan === "string" ? data.badan : "";
    var isi = {
      ok: data.ok === true,
      status: data.status,
      statusText: "",
      url: data.url,
      redirected: false,
      type: "basic",
      bodyUsed: false,
      headers: {
        get: function (nama) {
          var n = String(nama).toLowerCase();
          if (n === "content-type") return data.contentType || null;
          return null;
        },
        has: function (nama) {
          return this.get(nama) !== null;
        },
      },
      // `Response.text()` dan `Response.json()` di browser mengembalikan
      // Promise, bukan nilai. Halaman memakai keduanya dengan `await` maupun
      // `.then()`, jadi bentuk sinkron akan lolos pada `await` tapi gagal
      // senyap pada `.then()`. Microtask-nya dikuras pemompa, jadi promise di
      // sini selesai sebelum `eval` kembali.
      text: function () {
        return Promise.resolve(teks);
      },
      json: function () {
        try {
          return Promise.resolve(JSON.parse(teks));
        } catch (e) {
          return Promise.reject(new SyntaxError(String((e && e.message) || e)));
        }
      },
    };
    return isi;
  }

  // Galat dilempar sinkron, bukan lewat promise yang ditolak. Bagi skrip yang
  // memakai `await` di dalam `try`, keduanya sama; bagi skrip yang memakai
  // `.catch()`, keduanya sama-sama tidak bekerja, dan itu tercatat di CELAH
  // alih-alih disamarkan.
  function fetch(url, init) {
    // Promise asli, bukan objek mirip. Executor-nya tetap berjalan sinkron,
    // jadi gerbang jaringan sudah diputuskan saat `fetch` dipanggil; yang
    // berubah hanya bentuk kegagalannya: penolakan datang sebagai promise
    // yang ditolak, sehingga `.catch()` dan `try { await }` sama-sama bekerja.
    return new Promise(function (resolve, reject) {
      var metode = init && init.method ? String(init.method) : "GET";
      var badan = "";
      if (init && init.body !== undefined && init.body !== null) {
        badan = String(init.body);
      }
      var data;
      try {
        data = JSON.parse(ambil(metode, String(url), badan));
      } catch (e) {
        reject(new TypeError(String((e && e.message) || e)));
        return;
      }
      if (data && typeof data.__galat === "string") {
        reject(new TypeError(data.__galat));
        return;
      }
      resolve(jawaban(data));
    });
  }

  // `fetch` di browser sungguhan adalah properti milik `window`, dan
  // enumerable. Dibuat sama di sini supaya pemeriksa sidik jari tidak melihat
  // perbedaan yang tidak perlu.
  Object.defineProperty(globalThis, "fetch", {
    value: fetch,
    enumerable: true,
    configurable: true,
    writable: true,
  });
})();
"##;

/// Satu pintu untuk semua operasi pohon yang datang dari skrip halaman.
///
/// Protokolnya sengaja teks: `op`, tiga argumen string, balasan JSON. Satu
/// fungsi berarti satu tempat untuk menjaga aturan (node yang tidak ada
/// dilaporkan, bukan ditelan) dan satu tempat untuk menambah operasi tanpa
/// menambah global di dalam isolat.
///
/// Nomor node dikirim sebagai teks karena JavaScript tidak punya bilangan
/// bulat bertanda yang bisa dipercaya setelah `String()`; `-1` dipakai sebagai
/// "tidak ada" dan tidak pernah menjadi nomor node yang sah.
/// Mengunci pohon untuk satu operasi jembatan.
///
/// Poison dipulihkan, bukan diteruskan: satu panik di tengah operasi DOM tidak
/// boleh membuat tab itu tidak bisa dibaca selamanya.
fn kunci(dokumen: &Arc<Mutex<Document>>) -> MutexGuard<'_, Document> {
    dokumen.lock().unwrap_or_else(|e| e.into_inner())
}

fn op_dom(
    dokumen: &Arc<Mutex<Document>>,
    halaman: Option<&Halaman>,
    op: &str,
    a: &str,
    b: &str,
    c: &str,
) -> Result<serde_json::Value, String> {
    use serde_json::json;

    // Operasi yang mengubah pohon menaikkan versi halaman. Tanpa ini,
    // `getBoundingClientRect` sesudah `appendChild` menjawab kotak versi lama,
    // dan nilai basi yang tidak memberi galat adalah cara paling halus untuk
    // membuat skrip halaman salah tanpa ada yang tahu.
    if matches!(
        op,
        "setText"
            | "setAttr"
            | "delAttr"
            | "create"
            | "createText"
            | "append"
            | "insertBefore"
            | "remove"
            | "setTitle"
            | "attachShadow"
            | "setHtml"
            | "insertHtml"
            | "clone"
    ) && let Some(halaman) = halaman
    {
        halaman.naikkan_versi();
    }

    fn nomor(teks: &str) -> Result<NodeId, String> {
        teks.parse::<NodeId>()
            .map_err(|_| format!("nomor node tidak sah: {teks:?}"))
    }

    match op {
        "nodeType" => {
            let id = nomor(a)?;
            let dok = kunci(dokumen);
            let node = dok.node(id).ok_or_else(|| format!("node {id} tidak ada"))?;
            let jenis = match &node.kind {
                NodeKind::Document => 9,
                NodeKind::Doctype => 10,
                NodeKind::Element(_) => 1,
                NodeKind::Text(_) => 3,
                // `DOCUMENT_FRAGMENT_NODE`: shadow root adalah fragment.
                NodeKind::ShadowRoot { .. } => 11,
            };
            Ok(json!(jenis))
        }
        "docid" => {
            let dok = kunci(dokumen);
            Ok(json!(dok.root()))
        }
        "root" => {
            let dok = kunci(dokumen);
            let akar = dok.root();
            let anak = dok
                .node(akar)
                .map(|n| n.children.clone())
                .unwrap_or_default();
            let html = anak
                .into_iter()
                .find(|&id| dok.node(id).and_then(|n| n.tag()).is_some())
                .unwrap_or(akar);
            Ok(json!(html))
        }
        "byid" => {
            let dok = kunci(dokumen);
            Ok(json!(dok.cari_id(a).map_or(-1i64, |id| id as i64)))
        }
        "byidTerang" => {
            let dok = kunci(dokumen);
            let ketemu = dok.cari_id(a).filter(|&id| dok.terlihat_skrip(id));
            Ok(json!(ketemu.map_or(-1i64, |id| id as i64)))
        }
        "byidDalam" => {
            let akar = nomor(a)?;
            let dok = kunci(dokumen);
            let pohon = dok.akar_pohon(akar);
            let ketemu = dok.nodes().iter().enumerate().find_map(|(i, n)| {
                (n.tag().is_some() && n.attr("id") == Some(b) && dok.akar_pohon(i) == pohon)
                    .then_some(i as i64)
            });
            Ok(json!(ketemu.unwrap_or(-1)))
        }
        // Jalur skrip halaman. Empat operasi ini sengaja terpisah dari
        // `select*`: yang satu menembus batas pohon, yang lain tidak, dan
        // menyatukannya berarti `mode: "closed"` kehilangan artinya.
        "selectTerang" => {
            let dok = kunci(dokumen);
            let ketemu = dok.pilih_satu_terang(a).map_err(|e| e.to_string())?;
            Ok(json!(ketemu.map_or(-1i64, |id| id as i64)))
        }
        "selectAllTerang" => {
            let dok = kunci(dokumen);
            let daftar = dok.pilih_terang(a).map_err(|e| e.to_string())?;
            Ok(json!(daftar))
        }
        "selectDalamTerang" => {
            let akar = nomor(a)?;
            let dok = kunci(dokumen);
            let ketemu = dok.pilih_dalam_terang(akar, b).map_err(|e| e.to_string())?;
            Ok(json!(ketemu.first().map_or(-1i64, |&id| id as i64)))
        }
        "selectAllDalamTerang" => {
            let akar = nomor(a)?;
            let dok = kunci(dokumen);
            let daftar = dok.pilih_dalam_terang(akar, b).map_err(|e| e.to_string())?;
            Ok(json!(daftar))
        }
        "attachShadow" => {
            let host = nomor(a)?;
            let mode = match b {
                "open" => ModeBayang::Terbuka,
                "closed" => ModeBayang::Tertutup,
                lain => return Err(format!("mode shadow tidak dikenal: {lain:?}")),
            };
            let id = kunci(dokumen)
                .pasang_bayang(host, mode)
                .map_err(|e| e.to_string())?;
            Ok(json!(id))
        }
        // `element.shadowRoot`: hanya mode terbuka. Root tertutup tetap ada di
        // pohon dan tetap bisa ditembus otomasi, tapi jalur ini yang dipakai
        // skrip halaman, jadi ia harus menghormati mode.
        "shadowRoot" => {
            let host = nomor(a)?;
            let dok = kunci(dokumen);
            Ok(json!(
                dok.bayang_terbuka(host).map_or(-1i64, |id| id as i64)
            ))
        }
        "bayangHost" => {
            let id = nomor(a)?;
            let dok = kunci(dokumen);
            let host = dok.node(id).and_then(|n| n.parent);
            Ok(json!(host.map_or(-1i64, |p| p as i64)))
        }
        "bayangMode" => {
            let id = nomor(a)?;
            let dok = kunci(dokumen);
            let mode = dok.mode_bayang(id).map_or("", |m| m.nama());
            Ok(json!(mode))
        }
        "select" => {
            let dok = kunci(dokumen);
            let ketemu = dok.pilih_satu(a).map_err(|e| e.to_string())?;
            Ok(json!(ketemu.map_or(-1i64, |id| id as i64)))
        }
        "selectAll" => {
            let dok = kunci(dokumen);
            let daftar = dok.pilih(a).map_err(|e| e.to_string())?;
            Ok(json!(daftar))
        }
        "selectDalam" => {
            let akar = nomor(a)?;
            let dok = kunci(dokumen);
            let ketemu = dok.pilih_dalam(akar, b).map_err(|e| e.to_string())?;
            Ok(json!(ketemu.first().map_or(-1i64, |&id| id as i64)))
        }
        "selectAllDalam" => {
            let akar = nomor(a)?;
            let dok = kunci(dokumen);
            let daftar = dok.pilih_dalam(akar, b).map_err(|e| e.to_string())?;
            Ok(json!(daftar))
        }
        "tag" => {
            let id = nomor(a)?;
            let dok = kunci(dokumen);
            let node = dok.node(id).ok_or_else(|| format!("node {id} tidak ada"))?;
            Ok(json!(node.tag().unwrap_or("")))
        }
        "text" => {
            let id = nomor(a)?;
            let dok = kunci(dokumen);
            if dok.node(id).is_none() {
                return Err(format!("node {id} tidak ada"));
            }
            Ok(json!(dok.teks(id)))
        }
        "setText" => {
            let id = nomor(a)?;
            kunci(dokumen).set_teks(id, b).map_err(|e| e.to_string())?;
            Ok(json!(null))
        }
        "attr" => {
            let id = nomor(a)?;
            let dok = kunci(dokumen);
            let node = dok.node(id).ok_or_else(|| format!("node {id} tidak ada"))?;
            Ok(match node.attr(b) {
                Some(nilai) => json!(nilai),
                None => json!(null),
            })
        }
        "setAttr" => {
            let id = nomor(a)?;
            kunci(dokumen)
                .set_atribut(id, b, c)
                .map_err(|e| e.to_string())?;
            Ok(json!(null))
        }
        "delAttr" => {
            let id = nomor(a)?;
            let dihapus = kunci(dokumen).hapus_atribut(id, b);
            Ok(json!(dihapus))
        }
        "children" => {
            let id = nomor(a)?;
            let dok = kunci(dokumen);
            let anak = dok
                .node(id)
                .map(|n| n.children.clone())
                .ok_or_else(|| format!("node {id} tidak ada"))?;
            Ok(json!(anak))
        }
        "parent" => {
            let id = nomor(a)?;
            let dok = kunci(dokumen);
            let induk = dok.node(id).and_then(|n| n.parent);
            Ok(json!(induk.map_or(-1i64, |p| p as i64)))
        }
        "create" => {
            let id = kunci(dokumen).buat_elemen(a);
            Ok(json!(id))
        }
        "createText" => {
            let id = kunci(dokumen).buat_teks(a);
            Ok(json!(id))
        }
        "append" => {
            let induk = nomor(a)?;
            let anak = nomor(b)?;
            kunci(dokumen)
                .tambah_anak(induk, anak)
                .map_err(|e| e.to_string())?;
            Ok(json!(null))
        }
        "insertBefore" => {
            let induk = nomor(a)?;
            let anak = nomor(b)?;
            let acuan = nomor(c)?;
            kunci(dokumen)
                .sisip_sebelum(induk, anak, acuan)
                .map_err(|e| e.to_string())?;
            Ok(json!(null))
        }
        "remove" => {
            let id = nomor(a)?;
            let dilepas = kunci(dokumen).lepas(id);
            Ok(json!(dilepas))
        }
        "title" => {
            let dok = kunci(dokumen);
            Ok(json!(dok.judul().unwrap_or_default()))
        }
        "setTitle" => {
            let mut dok = kunci(dokumen);
            let ada = dok.nodes().iter().position(|n| n.tag() == Some("title"));
            match ada {
                Some(id) => dok.set_teks(id, a).map_err(|e| e.to_string())?,
                None => {
                    let kepala = dok.nodes().iter().position(|n| n.tag() == Some("head"));
                    let judul = dok.buat_elemen("title");
                    let teks = dok.buat_teks(a);
                    dok.tambah_anak(judul, teks).map_err(|e| e.to_string())?;
                    if let Some(kepala) = kepala {
                        dok.tambah_anak(kepala, judul).map_err(|e| e.to_string())?;
                    }
                }
            }
            Ok(json!(null))
        }
        "html" => {
            let id = nomor(a)?;
            let dok = kunci(dokumen);
            dok.dalam_html(id)
                .map(|isi| json!(isi))
                .ok_or_else(|| format!("node {id} tidak ada"))
        }
        "outer" => {
            let id = nomor(a)?;
            let dok = kunci(dokumen);
            dok.luar_html(id)
                .map(|isi| json!(isi))
                .ok_or_else(|| format!("node {id} bukan elemen"))
        }
        "setHtml" => {
            let id = nomor(a)?;
            kunci(dokumen)
                .ganti_dalam_html(id, b)
                .map_err(|e| e.to_string())?;
            Ok(json!(null))
        }
        "insertHtml" => {
            let id = nomor(a)?;
            let posisi = match b {
                "beforebegin" => super::dom::PosisiSisip::Sebelum,
                "afterbegin" => super::dom::PosisiSisip::Awal,
                "beforeend" => super::dom::PosisiSisip::Akhir,
                "afterend" => super::dom::PosisiSisip::Sesudah,
                lain => return Err(format!("posisi sisip tidak dikenal: {lain:?}")),
            };
            kunci(dokumen)
                .sisip_html(id, posisi, c)
                .map_err(|e| e.to_string())?;
            Ok(json!(null))
        }
        "clone" => {
            let id = nomor(a)?;
            let dalam = b == "1";
            let baru = kunci(dokumen).klon(id, dalam).map_err(|e| e.to_string())?;
            Ok(json!(baru))
        }
        "matches" => {
            let id = nomor(a)?;
            let dok = kunci(dokumen);
            dok.cocok(id, b)
                .map(|ya| json!(ya))
                .map_err(|e| e.to_string())
        }
        "connected" => {
            let id = nomor(a)?;
            let dok = kunci(dokumen);
            Ok(json!(dok.terpasang(id)))
        }
        "tagAll" => {
            let id = nomor(a)?;
            let tag = b.to_ascii_lowercase();
            let dok = kunci(dokumen);
            let mut hasil = Vec::new();
            let mut tumpuk = vec![id];
            while let Some(kini) = tumpuk.pop() {
                let Some(node) = dok.node(kini) else {
                    continue;
                };
                if let NodeKind::Element(t) = &node.kind
                    && (tag == "*" || t.eq_ignore_ascii_case(&tag))
                {
                    hasil.push(kini);
                }
                // Urutan dokumen: anak dibalik sebelum masuk tumpukan, karena
                // tumpukan mengeluarkan yang terakhir dimasukkan.
                for &anak in node.children.iter().rev() {
                    tumpuk.push(anak);
                }
            }
            Ok(json!(hasil))
        }
        "rect" => {
            let id = nomor(a)?;
            let Some(halaman) = halaman else {
                return Err("getBoundingClientRect butuh konteks halaman".to_owned());
            };
            let dok = kunci(dokumen);
            let laporan = halaman
                .layout(&dok)
                .ok_or_else(|| "layout tidak bisa dihitung".to_owned())?;
            let kotak = laporan
                .kotak_untuk(id)
                .ok_or_else(|| format!("node {id} tidak punya kotak layout"))?;
            Ok(json!({
                "x": kotak.x,
                "y": kotak.y,
                "width": kotak.width,
                "height": kotak.height,
                "top": kotak.y,
                "left": kotak.x,
                "right": kotak.x + kotak.width,
                "bottom": kotak.y + kotak.height,
            }))
        }
        "style" => {
            let id = nomor(a)?;
            let Some(halaman) = halaman else {
                return Err("getComputedStyle butuh konteks halaman".to_owned());
            };
            let dok = kunci(dokumen);
            let laporan = halaman
                .layout(&dok)
                .ok_or_else(|| "layout tidak bisa dihitung".to_owned())?;
            let mut peta = serde_json::Map::new();
            // Deklarasi pemenang dulu, lalu nilai terhitung menimpanya.
            // Urutan itu penting: `color: currentColor` yang menang di kaskade
            // bukan jawaban yang berguna bagi skrip, sedangkan warna hasil
            // warisan adalah.
            if let Some(daftar) = laporan.gaya.get(&id) {
                for (nama, nilai) in daftar {
                    peta.insert(nama.clone(), json!(nilai));
                }
            }
            if let Some(gaya) = laporan.gaya_terhitung(id) {
                if let Some(warna) = &gaya.warna {
                    peta.insert("color".to_owned(), json!(warna));
                }
                peta.insert(
                    "font-size".to_owned(),
                    json!(format!("{:.1}px", gaya.ukuran_font)),
                );
                peta.insert("font-weight".to_owned(), json!(gaya.tebal_font.to_string()));
                peta.insert(
                    "line-height".to_owned(),
                    json!(format!("{:.1}px", gaya.tinggi_baris)),
                );
                peta.insert(
                    "visibility".to_owned(),
                    json!(if gaya.terlihat { "visible" } else { "hidden" }),
                );
                if let Some(perataan) = &gaya.perataan {
                    peta.insert("text-align".to_owned(), json!(perataan));
                }
                if !gaya.keluarga_font.is_empty() {
                    peta.insert(
                        "font-family".to_owned(),
                        json!(gaya.keluarga_font.join(", ")),
                    );
                }
            }
            Ok(serde_json::Value::Object(peta))
        }
        "halaman" => {
            let Some(halaman) = halaman else {
                return Err("isolat ini tidak punya konteks halaman".to_owned());
            };
            let viewport = halaman.viewport();
            Ok(json!({
                "url": halaman.url(),
                "lebar": viewport.width,
                "tinggi": viewport.height,
            }))
        }
        lain => Err(format!("operasi jembatan tidak dikenal: {lain:?}")),
    }
}

/// Batas jumlah microtask yang dijalankan sesudah satu evaluasi.
///
/// Rantai promise yang saling menjadwalkan tanpa akhir akan mengunci tab ini
/// kalau antreannya dikuras tanpa batas. Batasnya jauh di atas kebutuhan
/// halaman sungguhan (ratusan), dan melewatinya berarti skripnya sendiri yang
/// salah, bukan mesin yang lambat.
const BATAS_MIKROTUGAS: usize = 10_000;

/// Mengosongkan antrean microtask sesudah satu evaluasi.
///
/// `rquickjs` tidak menjalankan antrean itu sendiri: tanpa pemompaan di sini,
/// `await` pada Promise yang sudah selesai pun tidak pernah dilanjutkan, dan
/// `fetch()` yang mengembalikan Promise asli akan terlihat menggantung.
fn pompa_mikrotugas(ctx: &rquickjs::Ctx<'_>) {
    let mut dijalankan = 0usize;
    while ctx.execute_pending_job() {
        dijalankan += 1;
        if dijalankan >= BATAS_MIKROTUGAS {
            break;
        }
    }
}

/// Nilai balik skrip dengan tipe JavaScript yang **dipertahankan**.
///
/// Sebelum ini setiap hasil dijadikan string, jadi `navigator.webdriver`
/// keluar sebagai `"false"` dan tidak ada klien yang bisa membedakan boolean
/// dari teks. Bilangan dipisah bulat dan pecahan karena V8 juga begitu: `2`
/// dan `2.0` sama nilainya di JavaScript, tapi `2.0` tercetak `2.0` di JSON,
/// dan pembaca yang menuntut bilangan bulat akan menolaknya.
///
/// Objek dibawa sebagai teks JSON hasil `JSON.stringify`, bukan sebagai
/// `objectId`. Mesin ini belum punya penyimpanan objek, jadi satu-satunya
/// bentuk yang jujur adalah *by value*; batas itu tercatat di [`super::CELAH`].
#[derive(Debug, Clone, PartialEq)]
pub enum NilaiJs {
    /// `undefined`
    TanpaNilai,
    /// `null`
    Null,
    /// `true` / `false`
    Boolean(bool),
    /// Bilangan bulat (tag `int` di V8).
    Bulat(i32),
    /// Bilangan pecahan (tag `float64` di V8), termasuk `NaN` dan `Infinity`.
    Pecahan(f64),
    /// Teks.
    Teks(String),
    /// Objek, larik, promise, galat, atau proxy. `json` kosong kalau nilai
    /// tidak bisa diserialkan (mis. struktur melingkar); `deskripsi` selalu
    /// terisi supaya klien tetap punya sesuatu untuk dibaca.
    Objek {
        json: Option<String>,
        deskripsi: String,
    },
    /// Fungsi atau konstruktor, berisi sumbernya.
    Fungsi(String),
    /// Simbol, berisi deskripsinya.
    Simbol(String),
    /// BigInt, berisi nilainya sebagai teks.
    BigInt(String),
}

pub struct Isolat {
    // Urutan deklarasi penting: konteks harus dijatuhkan sebelum runtime.
    // Runtime disimpan hanya untuk menjaga umurnya, karena `Context` bergantung
    // padanya; prefiks garis bawah menandai itu ke pembaca dan ke clippy.
    context: Context,
    _runtime: Runtime,
    /// Pohon yang dibagi dengan tab, kalau jembatan DOM dipasang.
    ///
    /// Dibagi, bukan disalin: skrip halaman yang memutasi `document` harus
    /// mengubah pohon yang sama yang dibaca `DOM.getDocument` dan dilukis
    /// raster. Dua salinan berarti dua kebenaran yang menyimpang.
    ///
    /// `Arc<Mutex<..>>`, bukan `Rc<RefCell<..>>`. `RefCell` lebih murah, tapi
    /// `Isolat` hidup di dalam state axum lewat `Arc<Mutex<Dalam>>`, dan state
    /// itu wajib `Send`: `Rc` tidak. Harga `Mutex` di sini satu kunci tanpa
    /// kontensi per operasi DOM, karena satu tab hanya disentuh satu thread.
    ///
    /// **Jangan pernah memegang kunci ini sambil menjalankan [`Isolat::eval`].**
    /// Jembatan mengunci pohon saat skrip berjalan, jadi pinjaman yang
    /// ditinggal di sisi Rust akan mengunci diri sendiri (deadlock, bukan
    /// panik). Pemanggil membaca pohon setelah `eval` selesai.
    dokumen: Option<Arc<Mutex<Document>>>,
    /// Tenggat evaluasi yang sedang berjalan, dalam milidetik sejak
    /// [`sekarang_ms`] titik nol. Nol berarti tidak ada tenggat.
    tenggat: Arc<AtomicU64>,
    /// Diangkat oleh handler interrupt saat ia membatalkan sebuah evaluasi.
    ///
    /// Dipisah dari `tenggat` supaya galat yang keluar bisa menyebut penyebab
    /// yang benar: skrip yang dibatalkan karena waktu habis berbeda dari skrip
    /// yang melempar galat sendiri, dan pemanggil berhak membedakannya.
    lewat_batas: Arc<AtomicBool>,
    /// Batas waktu yang dipakai [`Self::eval`] dan [`Self::eval_bertipe`].
    batas_waktu: Duration,
    /// URL dan viewport halaman ini, kalau pemanggil menyerahkannya.
    ///
    /// `None` berarti isolat tanpa halaman (tes murni): `location` dan
    /// `innerWidth` tetap ada tapi berisi nilai kosong, dan
    /// `getComputedStyle`/`getBoundingClientRect` menjawab kosong dengan
    /// alasan, bukan mengarang angka.
    halaman: Option<Arc<Halaman>>,
    /// Jalur jaringan yang sama dengan yang dipakai `fetch()`.
    ///
    /// Disimpan terpisah karena `<script src>` butuh pengambil yang sama
    /// tanpa harus lewat JavaScript: byte-nya diambil Rust lalu dievaluasi.
    pengambil: Option<Arc<dyn Pengambil>>,
}

impl Isolat {
    /// Isolat tanpa jalur jaringan: `fetch()` menolak semua.
    ///
    /// Yang dipakai tes mesin dan isolat yang tidak butuh jaringan. Menolak,
    /// bukan mengizinkan: isolat yang lupa dipasangi kebijakan tidak boleh
    /// berubah jadi jalan keluar bebas ke jaringan.
    pub fn new(pagu: PaguIsolat) -> Result<Self, NativeError> {
        Self::dengan_ambil(pagu, Arc::new(PengambilTolak))
    }

    /// Isolat yang `fetch()`-nya lewat `pengambil`.
    ///
    /// `pengambil` adalah satu-satunya jalan skrip halaman menyentuh jaringan,
    /// dan implementasinya memegang kebijakan. Crate ini tidak tahu apa itu
    /// daftar izin; kalau ia tahu, aturannya akan bercabang dua.
    pub fn dengan_ambil(
        pagu: PaguIsolat,
        pengambil: Arc<dyn Pengambil>,
    ) -> Result<Self, NativeError> {
        let runtime = Runtime::new().map_err(NativeError::runtime)?;
        runtime.set_memory_limit(pagu.memori_bytes);
        runtime.set_max_stack_size(pagu.stack_bytes);
        let context = Context::full(&runtime).map_err(NativeError::runtime)?;

        let tenggat = Arc::new(AtomicU64::new(0));
        let lewat_batas = Arc::new(AtomicBool::new(false));
        {
            let tenggat = Arc::clone(&tenggat);
            let lewat_batas = Arc::clone(&lewat_batas);
            runtime.set_interrupt_handler(Some(Box::new(move || {
                let batas = tenggat.load(Ordering::Relaxed);
                if batas == 0 {
                    return false;
                }
                if sekarang_ms() < batas {
                    return false;
                }
                lewat_batas.store(true, Ordering::Relaxed);
                true
            })));
        }

        let isolat = Self {
            context,
            _runtime: runtime,
            dokumen: None,
            tenggat,
            lewat_batas,
            batas_waktu: pagu.batas_waktu,
            halaman: None,
            pengambil: None,
        };
        isolat.eval(PEMBUKA)?;
        isolat.pasang_ambil(Arc::clone(&pengambil))?;
        // Disimpan sesudah `pasang_ambil` berhasil: kalau jembatan gagal
        // dipasang, isolatnya dibuang dan tidak ada yang perlu jalur ini.
        Ok(Self {
            pengambil: Some(pengambil),
            ..isolat
        })
    }

    /// Isolat yang `document`-nya adalah pohon sungguhan, bukan objek kosong.
    ///
    /// G10. Pohon dibagi lewat [`Rc`], jadi mutasi dari skrip halaman terlihat
    /// oleh Rust tanpa penyalinan dan tanpa sinkronisasi manual. Satu isolat
    /// tetap satu tab: [`Rc`] tidak membuat isolat bisa dipakai lintas thread,
    /// dan `Isolat` memang tidak `Send`.
    pub fn dengan_dokumen(
        pagu: PaguIsolat,
        dokumen: Arc<Mutex<Document>>,
    ) -> Result<Self, NativeError> {
        Self::dengan_dokumen_dan_ambil(pagu, dokumen, Arc::new(PengambilTolak))
    }

    /// Sama dengan [`Self::dengan_dokumen`], tapi `fetch()` lewat `pengambil`.
    ///
    /// Inilah bentuk yang dipakai [`super::tab_pool::TabPool`]: satu tab butuh
    /// pohon sekaligus jalur jaringan, dan keduanya harus dipasang sebelum
    /// skrip halaman pertama berjalan.
    pub fn dengan_dokumen_dan_ambil(
        pagu: PaguIsolat,
        dokumen: Arc<Mutex<Document>>,
        pengambil: Arc<dyn Pengambil>,
    ) -> Result<Self, NativeError> {
        Self::dengan_dokumen_ambil_dan_halaman(pagu, dokumen, pengambil, None)
    }

    /// Bentuk terlengkap: pohon, jalur jaringan, dan konteks halaman.
    ///
    /// `halaman` yang `None` bukan kesalahan: isolat tes memang tidak punya
    /// URL maupun viewport, dan `location`/`innerWidth` harus menjawab kosong
    /// dengan jujur alih-alih menebak `about:blank` lalu menyesatkan skrip
    /// yang memeriksanya.
    pub fn dengan_dokumen_ambil_dan_halaman(
        pagu: PaguIsolat,
        dokumen: Arc<Mutex<Document>>,
        pengambil: Arc<dyn Pengambil>,
        halaman: Option<Arc<Halaman>>,
    ) -> Result<Self, NativeError> {
        let mut isolat = Self::dengan_ambil(pagu, pengambil)?;
        isolat.dokumen = Some(dokumen);
        isolat.halaman = halaman;
        isolat.pasang_jembatan()?;
        Ok(isolat)
    }

    /// Konteks halaman yang sedang dipakai isolat, kalau ada.
    #[must_use]
    pub fn halaman(&self) -> Option<&Arc<Halaman>> {
        self.halaman.as_ref()
    }

    /// Menjalankan semua `<script>` milik dokumen, urut kemunculan.
    ///
    /// Ini yang membedakan mesin yang "bisa memparse HTML" dari mesin yang
    /// bisa membuka situs hari ini: hampir semua aplikasi web mengirim kerangka
    /// kosong lalu mengisinya dari JavaScript. Tanpa langkah ini, halaman
    /// seperti itu tampil kosong tanpa satu pun galat.
    ///
    /// Yang dijalankan: `<script>` tanpa `src` (teksnya dievaluasi langsung)
    /// dan `<script src>` yang byte-nya didapat lewat [`Pengambil`] yang sama
    /// dengan `fetch()`. Yang dilewati beserta alasannya: `type` yang bukan
    /// MIME JavaScript (`application/json`, `text/template`, ...) dan
    /// `type="module"`, karena modul butuh pemuat impor yang belum ada.
    ///
    /// Satu skrip yang gagal **tidak** menghentikan yang lain: browser juga
    /// begitu, dan halaman yang satu skripnya error tetap harus bisa dibaca.
    /// Galatnya dikumpulkan, bukan ditelan.
    pub fn jalankan_skrip_halaman(&self) -> Result<LaporanSkrip, NativeError> {
        let mut laporan = LaporanSkrip::default();
        let (daftar, dasar) = {
            let Some(dokumen) = self.dokumen.as_ref() else {
                return Err(NativeError::dom("eksekusi skrip butuh pohon dokumen"));
            };
            let dok = kunci(dokumen);
            let dasar = dok
                .nodes()
                .iter()
                .find(|n| n.tag() == Some("base"))
                .and_then(|n| n.attr("href"))
                .map(str::to_owned)
                .or_else(|| self.halaman.as_ref().map(|h| h.url().to_owned()));
            (kumpulkan_skrip(&dok, &mut laporan), dasar)
        };

        for skrip in daftar {
            let sumber = match skrip.sumber {
                SumberSkrip::Sebaris(teks) => teks,
                SumberSkrip::Luar(src) => {
                    let Some(pengambil) = self.pengambil.as_ref() else {
                        laporan
                            .dilewati
                            .push(format!("{src}: isolat ini tidak punya jalur jaringan"));
                        continue;
                    };
                    let Some(url) = super::bingkai::selesaikan(&src, dasar.as_deref()) else {
                        laporan
                            .dilewati
                            .push(format!("{src}: URL tidak bisa diselesaikan"));
                        continue;
                    };
                    let permintaan = PermintaanAmbil {
                        metode: "GET".to_owned(),
                        url: url.clone(),
                        badan: None,
                    };
                    match pengambil.ambil(&permintaan) {
                        Ok(jawaban) if (200..300).contains(&jawaban.status) => jawaban.badan,
                        Ok(jawaban) => {
                            laporan.dilewati.push(format!(
                                "{url}: HTTP {} dari jalur jaringan",
                                jawaban.status
                            ));
                            continue;
                        }
                        Err(pesan) => {
                            laporan.dilewati.push(format!("{url}: {pesan}"));
                            continue;
                        }
                    }
                }
            };
            if sumber.trim().is_empty() {
                continue;
            }
            match self.eval(&sumber) {
                Ok(_) => laporan.dijalankan += 1,
                Err(NativeError::BatasWaktu(pesan)) => {
                    // Skrip yang melewati pagu waktu tetap dihitung sudah
                    // dijalankan: yang gagal adalah penyelesaiannya, bukan
                    // pemanggilannya, dan menyembunyikan itu membuat halaman
                    // yang lambat terlihat seperti halaman tanpa skrip.
                    laporan.dijalankan += 1;
                    laporan.galat.push(pesan);
                }
                Err(err) => {
                    laporan.dijalankan += 1;
                    laporan.galat.push(err.to_string());
                }
            }
        }
        Ok(laporan)
    }

    /// Menjalankan timer yang sudah jatuh tempo.
    ///
    /// Mengembalikan jumlah callback yang dipanggil. Dipanggil pemanggil
    /// sesudah skrip halaman dan sesudah setiap evaluasi dari klien: mesin ini
    /// tidak punya event loop, jadi tidak ada yang menguras antrean kalau
    /// tidak diminta.
    pub fn kuras_timer(&self) -> Result<usize, NativeError> {
        let hasil = self.eval("globalThis.__timer ? globalThis.__timer.kuras() : 0")?;
        Ok(hasil.trim().parse::<usize>().unwrap_or(0))
    }

    /// Menyelesaikan pemuatan: `readyState` menjadi `complete` dan peristiwa
    /// `DOMContentLoaded` lalu `load` dikirim berurutan.
    ///
    /// Mengembalikan `true` kalau peristiwa benar-benar dikirim. Pintu
    /// `__siap` dihapus sesudah dipakai supaya skrip halaman tidak bisa
    /// memicunya dua kali, dan supaya `Object.keys(globalThis)` tetap bersih
    /// seperti di browser sungguhan.
    pub fn selesaikan_pemuatan(&self) -> Result<bool, NativeError> {
        let hasil = self.eval("globalThis.__siap ? globalThis.__siap.selesai() : 0")?;
        let _ = self.eval("delete globalThis.__siap");
        Ok(hasil.trim() != "0")
    }

    /// Pohon yang sedang dipakai jembatan DOM, kalau ada.
    #[must_use]
    pub fn dokumen(&self) -> Option<&Arc<Mutex<Document>>> {
        self.dokumen.as_ref()
    }

    /// Memasang jembatan: satu fungsi Rust `__dom(op, a, b, c)`, lalu lapisan
    /// JavaScript yang membangun `document` di atasnya, lalu **nama itu
    /// dihapus**.
    ///
    /// Penghapusan itu bukan kerapian. Global yang bisa ditemukan skrip
    /// halaman adalah sidik jari otomasi: `Object.keys(globalThis)` di browser
    /// sungguhan tidak memuat apa pun selain `document`, `navigator`, dan
    /// kawan-kawannya. Setelah `delete globalThis.__dom`, satu-satunya
    /// pemegang fungsi itu adalah closure lapisan JavaScript di bawah, jadi
    /// skrip halaman tidak bisa memanggilnya langsung maupun menemukannya
    /// lewat penyisiran properti.
    fn pasang_jembatan(&self) -> Result<(), NativeError> {
        let Some(dokumen) = self.dokumen.clone() else {
            return Err(NativeError::dom("jembatan DOM dipasang tanpa pohon"));
        };
        let halaman = self.halaman.clone();
        self.context.with(|ctx| {
            let fungsi = Function::new(
                ctx.clone(),
                move |op: String, a: String, b: String, c: String| -> String {
                    match op_dom(&dokumen, halaman.as_deref(), &op, &a, &b, &c) {
                        Ok(nilai) => nilai.to_string(),
                        Err(pesan) => serde_json::json!({ "__galat": pesan }).to_string(),
                    }
                },
            )
            .map_err(NativeError::script)?;
            ctx.globals()
                .set("__dom", fungsi)
                .map_err(NativeError::script)
        })?;
        self.eval(JEMBATAN_DOM)?;
        Ok(())
    }

    /// Memasang jalur jaringan: satu fungsi Rust `__ambil(metode, url, badan)`,
    /// lalu lapisan `fetch()` di atasnya, lalu **nama itu dihapus**.
    ///
    /// Penghapusan global sama alasannya dengan [`Self::pasang_jembatan`]:
    /// global yang bisa ditemukan skrip halaman adalah sidik jari otomasi.
    fn pasang_ambil(&self, pengambil: Arc<dyn Pengambil>) -> Result<(), NativeError> {
        self.context.with(|ctx| {
            let fungsi = Function::new(
                ctx.clone(),
                move |metode: String, url: String, badan: String| -> String {
                    let permintaan = PermintaanAmbil {
                        metode: metode.to_ascii_uppercase(),
                        url,
                        badan: (!badan.is_empty()).then_some(badan),
                    };
                    match pengambil.ambil(&permintaan) {
                        Ok(jawaban) => jawaban_ke_json(&jawaban),
                        Err(pesan) => serde_json::json!({ "__galat": pesan }).to_string(),
                    }
                },
            )
            .map_err(NativeError::script)?;
            ctx.globals()
                .set("__ambil", fungsi)
                .map_err(NativeError::script)
        })?;
        self.eval(JEMBATAN_AMBIL)?;
        Ok(())
    }

    /// Mengirim satu peristiwa ke node, memanggil pendengar yang didaftarkan
    /// skrip halaman. Mengembalikan jumlah pendengar yang benar-benar
    /// dijalankan, sehingga pemanggil bisa membedakan "tidak ada pendengar"
    /// dari "pendengar ada tapi tidak terpanggil".
    ///
    /// **Jangan memegang kunci pohon saat memanggil ini.** Pendengar biasanya
    /// membaca atau memutasi DOM lewat jembatan, dan jembatan mengunci pohon
    /// yang sama.
    pub fn kirim_peristiwa(
        &self,
        tipe: &str,
        node: NodeId,
        detail: &str,
    ) -> Result<usize, NativeError> {
        if self.dokumen.is_none() {
            return Err(NativeError::dom("isolat ini tidak punya jembatan DOM"));
        }
        self.pasang_tenggat();
        let hasil = self.context.with(|ctx| {
            // Dispatcher diambil dari global pada setiap panggilan, bukan
            // disimpan di sisi Rust. `Persistent` adalah satu-satunya cara
            // memegang nilai JavaScript lintas panggilan, dan ia `!Send`
            // karena memuat pointer runtime; menyimpannya di `Isolat` akan
            // membatalkan `Engine: Send + Sync`, sedangkan `unsafe impl Send`
            // dilarang `#![forbid(unsafe_code)]` di crate ini. Satu lookup
            // properti per peristiwa jauh lebih murah daripada salah satu
            // dari dua kerusakan itu.
            let fungsi: Function = ctx
                .globals()
                .get(NAMA_KIRIM)
                .map_err(|e| self.galat_skrip(&ctx, e, None))?;
            let dijalankan: i32 = fungsi
                .call((tipe, i64::try_from(node).unwrap_or(i64::MAX), detail))
                .map_err(|e| self.galat_skrip(&ctx, e, None))?;
            Ok::<_, NativeError>(usize::try_from(dijalankan).unwrap_or(0))
        });
        self.lepas_tenggat();
        hasil
    }

    /// Memasang tenggat untuk satu evaluasi.
    fn pasang_tenggat(&self) {
        self.lewat_batas.store(false, Ordering::Relaxed);
        let nilai = if self.batas_waktu.is_zero() {
            0
        } else {
            let jendela = u64::try_from(self.batas_waktu.as_millis()).unwrap_or(u64::MAX);
            sekarang_ms().saturating_add(jendela)
        };
        self.tenggat.store(nilai, Ordering::Relaxed);
    }

    /// Melepas tenggat setelah evaluasi selesai, supaya pekerjaan Rust di luar
    /// skrip (mis. memanggil jembatan dari sisi host) tidak ikut dibatalkan.
    fn lepas_tenggat(&self) {
        self.tenggat.store(0, Ordering::Relaxed);
    }

    /// Menerjemahkan galat evaluasi. Kalau handler interrupt yang membatalkan
    /// skripnya, galatnya dilaporkan sebagai batas waktu, bukan sebagai
    /// kesalahan skrip.
    ///
    /// `ctx` wajib ikut karena `Debug` untuk `rquickjs::Error::Exception` cuma
    /// mencetak kata "Exception": nama simbol yang hilang dan pesannya hidup di
    /// nilai yang tertunda di konteks, bukan di galatnya. Tanpa `ctx.catch()`
    /// klien menerima `-32000 skrip gagal: Exception`, yang tidak memberi tahu
    /// apa pun tentang apa yang salah di halamannya.
    fn galat_skrip(
        &self,
        ctx: &rquickjs::Ctx<'_>,
        err: rquickjs::Error,
        sumber: Option<&str>,
    ) -> NativeError {
        if self.lewat_batas.swap(false, Ordering::Relaxed) {
            return NativeError::batas_waktu(self.batas_waktu);
        }
        if matches!(err, rquickjs::Error::Exception) && ctx.has_exception() {
            // `catch()` mengosongkan pengecualian yang tertunda sekaligus
            // mengembalikan nilainya, jadi panggilan berikutnya di konteks yang
            // sama tidak mewarisi galat yang sudah dilaporkan ini.
            let nilai = ctx.catch();
            let pesan = teks_dari(ctx, &nilai);
            if !pesan.is_empty() {
                // QuickJS sendiri tidak selalu menyebut simbolnya:
                // `window.matchMedia('x')` saat `matchMedia` tidak ada cuma
                // menghasilkan "TypeError: not a function". Potongan ekspresi
                // yang gagal ditempelkan supaya klien tetap tahu API mana yang
                // dimaksud, tanpa harus menebak dari halaman.
                return NativeError::Script(match sumber {
                    Some(teks) => format!("{pesan} (pada: {})", ringkas_sumber(teks)),
                    None => pesan,
                });
            }
        }
        NativeError::script(err)
    }

    /// Menjalankan sumber sebagai *program* di scope global, lalu mengembalikan
    /// nilai akhirnya sebagai string.
    ///
    /// Sumber dijalankan apa adanya, bukan dibungkus `eval()`. Ini penting:
    /// `var` di dalam `eval()` bersarang tidak pernah menempel ke objek global
    /// (diukur dengan `rquickjs` 0.14: `eval('var x = 1')` lalu `typeof x`
    /// menghasilkan `undefined`), sehingga state tab hilang di antara dua
    /// panggilan. Dijalankan sebagai program, deklarasi `var` dan `function`
    /// tetap terlihat oleh skrip berikutnya, sama seperti di browser.
    pub fn eval(&self, sumber: &str) -> Result<String, NativeError> {
        self.pasang_tenggat();
        let hasil = self.context.with(|ctx| {
            let nilai = match ctx.eval::<rquickjs::Coerced<String>, _>(sumber.as_bytes().to_vec()) {
                Ok(nilai) => nilai,
                // Diterjemahkan DI DALAM closure: pesan pengecualian hanya
                // bisa diambil selama konteksnya masih dipegang.
                Err(err) => return Err(self.galat_skrip(&ctx, err, Some(sumber))),
            };
            // Microtask yang lahir dari evaluasi ini dijalankan sebelum
            // nilainya dikembalikan, sama seperti event loop browser yang
            // mengosongkan antrean microtask di antara dua tugas. Tanpa
            // ini, `fetch()` yang mengembalikan Promise asli tidak akan
            // pernah menyelesaikan `await`-nya.
            pompa_mikrotugas(&ctx);
            Ok::<_, NativeError>(nilai.0)
        });
        self.lepas_tenggat();
        hasil
    }

    /// Menjalankan sumber sebagai program dan mengembalikan nilai akhirnya
    /// **dengan tipe aslinya**, bukan string.
    ///
    /// Sumber dijalankan apa adanya, sama seperti [`Self::eval`], jadi `var`
    /// dan `function` tetap menempel ke objek global untuk panggilan
    /// berikutnya. Yang berbeda hanya cara nilai balik dibaca.
    ///
    /// Objek diambil *by value* lewat `JSON.stringify` di sisi Rust
    /// (`Ctx::json_stringify`), bukan lewat global baru: menambah
    /// `globalThis.__oc_*` akan melanggar AC-018 (`test_no_automation_globals`)
    /// karena global semacam itu terlihat oleh skrip halaman.
    pub fn eval_bertipe(&self, sumber: &str) -> Result<NilaiJs, NativeError> {
        self.pasang_tenggat();
        let hasil = self.eval_bertipe_dalam(sumber);
        self.lepas_tenggat();
        hasil
    }

    fn eval_bertipe_dalam(&self, sumber: &str) -> Result<NilaiJs, NativeError> {
        self.context.with(|ctx| {
            let nilai: rquickjs::Value = ctx
                .eval(sumber.as_bytes().to_vec())
                .map_err(|e| self.galat_skrip(&ctx, e, Some(sumber)))?;
            // Sama seperti [`Isolat::eval`]: microtask yang dijadwalkan oleh
            // ekspresi ini dijalankan sebelum nilainya dibaca, sehingga
            // `await` di dalam fungsi async yang dipanggil klien selesai
            // dalam satu panggilan.
            pompa_mikrotugas(&ctx);

            let hasil = match nilai.type_of() {
                rquickjs::Type::Uninitialized | rquickjs::Type::Undefined => NilaiJs::TanpaNilai,
                rquickjs::Type::Null => NilaiJs::Null,
                rquickjs::Type::Bool => NilaiJs::Boolean(nilai.as_bool().unwrap_or(false)),
                rquickjs::Type::Int => NilaiJs::Bulat(nilai.as_int().unwrap_or_default()),
                rquickjs::Type::Float => NilaiJs::Pecahan(nilai.as_float().unwrap_or_default()),
                rquickjs::Type::String => NilaiJs::Teks(
                    nilai
                        .as_string()
                        .and_then(|teks| teks.to_string().ok())
                        .unwrap_or_default(),
                ),
                rquickjs::Type::Symbol => NilaiJs::Simbol(teks_dari(&ctx, &nilai)),
                rquickjs::Type::Unknown => NilaiJs::Objek {
                    // Tag internal yang tidak dikenali rquickjs. Tidak
                    // ditebak tipenya; dilaporkan apa adanya supaya klien
                    // tidak menerima nilai palsu.
                    json: None,
                    deskripsi: "[nilai dengan tag tak dikenal]".to_owned(),
                },
                rquickjs::Type::BigInt => NilaiJs::BigInt(teks_dari(&ctx, &nilai)),
                rquickjs::Type::Function | rquickjs::Type::Constructor | rquickjs::Type::Module => {
                    NilaiJs::Fungsi(teks_dari(&ctx, &nilai))
                }
                rquickjs::Type::Array
                | rquickjs::Type::Object
                | rquickjs::Type::Promise
                | rquickjs::Type::Exception
                | rquickjs::Type::Proxy => {
                    // Struktur melingkar membuat `JSON.stringify` melempar
                    // galat; itu bukan alasan menggagalkan seluruh evaluasi,
                    // jadi hasilnya jatuh ke deskripsi teks.
                    let json = ctx
                        .json_stringify(nilai.clone())
                        .ok()
                        .flatten()
                        .and_then(|teks| teks.to_string().ok());
                    let deskripsi = json.clone().unwrap_or_else(|| teks_dari(&ctx, &nilai));
                    NilaiJs::Objek { json, deskripsi }
                }
            };
            Ok(hasil)
        })
    }

    /// Baris `console.log` yang belum dilaporkan, sekaligus mengosongkan
    /// buffer.
    ///
    /// Dipakai jalur CDP: `Runtime.consoleAPICalled` harus dikirim sekali per
    /// baris, dan buffer yang tidak dikosongkan akan mengirim ulang baris lama
    /// di setiap evaluasi berikutnya.
    #[must_use]
    pub fn ambil_log(&self) -> Vec<String> {
        let json = self
            .eval("(function(){var a=globalThis.__log||[];globalThis.__log=[];return JSON.stringify(a)})()")
            .unwrap_or_else(|_| "[]".to_string());
        serde_json::from_str(&json).unwrap_or_default()
    }

    /// Baris yang dicatat `console.log` sejak isolat dibuat.
    #[must_use]
    pub fn log(&self) -> Vec<String> {
        let json = self
            .eval("JSON.stringify(globalThis.__log || [])")
            .unwrap_or_else(|_| "[]".to_string());
        serde_json::from_str(&json).unwrap_or_default()
    }

    /// Memasang `document.title` yang bisa dibaca skrip halaman.
    ///
    /// Ini objek biasa, bukan DOM sungguhan. Skrip yang menulis
    /// `document.title = "x"` tidak mengubah apa pun di pohon DOM, dan itu
    /// disebut di [`super::CELAH`].
    pub fn pasang_judul(&self, judul: &str) -> Result<(), NativeError> {
        let literal = serde_json::to_string(judul).map_err(NativeError::script)?;
        self.eval(&format!("globalThis.document.title = {literal}"))?;
        Ok(())
    }
}

/// Mengubah nilai apa pun jadi teks lewat global `String` milik JavaScript.
///
/// Dipanggil dari Rust, bukan lewat global tambahan: nama seperti `__oc_*`
/// akan terlihat oleh skrip halaman dan melanggar AC-018
/// (`test_no_automation_globals`).
/// Merapikan potongan sumber untuk ditempel ke pesan galat.
///
/// Spasi dan baris baru diringkas jadi satu spasi supaya pesan tetap satu
/// baris, lalu dipotong pada batas karakter (bukan byte) supaya teks UTF-8
/// tidak terbelah.
fn ringkas_sumber(sumber: &str) -> String {
    let rapat = sumber.split_whitespace().collect::<Vec<_>>().join(" ");
    const BATAS: usize = 120;
    if rapat.chars().count() <= BATAS {
        return rapat;
    }
    let potong: String = rapat.chars().take(BATAS).collect();
    format!("{potong}…")
}

fn teks_dari<'js>(ctx: &rquickjs::Ctx<'js>, nilai: &rquickjs::Value<'js>) -> String {
    ctx.globals()
        .get::<_, rquickjs::Function<'js>>("String")
        .and_then(|fungsi| fungsi.call::<_, rquickjs::Coerced<String>>((nilai.clone(),)))
        .map(|teks| teks.0)
        .unwrap_or_else(|_| "[nilai tidak bisa diubah ke teks]".to_owned())
}

/// Bentuk JSON satu jawaban untuk lapisan `fetch()`.
///
/// `ok` mengikuti definisi `Response.ok` di browser: status 2xx. Dihitung di
/// sini, bukan di JavaScript, supaya satu definisi saja yang dipakai.
fn jawaban_ke_json(jawaban: &JawabanAmbil) -> String {
    serde_json::json!({
        "ok": (200..300).contains(&jawaban.status),
        "status": jawaban.status,
        "url": jawaban.url,
        "contentType": jawaban.content_type,
        "badan": jawaban.badan,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gerbang jaringan palsu: mencatat setiap permintaan, mengizinkan hanya
    /// host yang disebut, dan menolak sisanya dengan alasan yang menyebut
    /// host-nya.
    ///
    /// Pencatatan itu bagian dari kontrak, bukan kemudahan tes: keputusan
    /// "boleh atau tidak" harus bisa dibaca ulang, dan satu-satunya tempat yang
    /// pasti melihatnya adalah gerbangnya sendiri.
    struct GerbangUji {
        izin: Vec<String>,
        dicatat: Arc<Mutex<Vec<PermintaanAmbil>>>,
    }

    impl GerbangUji {
        fn baru(izin: &[&str]) -> (Arc<Self>, Arc<Mutex<Vec<PermintaanAmbil>>>) {
            let dicatat = Arc::new(Mutex::new(Vec::new()));
            let gerbang = Arc::new(Self {
                izin: izin.iter().map(|h| (*h).to_owned()).collect(),
                dicatat: Arc::clone(&dicatat),
            });
            (gerbang, dicatat)
        }
    }

    impl Pengambil for GerbangUji {
        fn ambil(&self, permintaan: &PermintaanAmbil) -> Result<JawabanAmbil, String> {
            self.dicatat
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(permintaan.clone());
            let host = permintaan
                .url
                .split("://")
                .nth(1)
                .and_then(|sisa| sisa.split('/').next())
                .unwrap_or("");
            if self.izin.iter().any(|izin| izin == host) {
                Ok(JawabanAmbil {
                    status: 200,
                    url: permintaan.url.clone(),
                    content_type: Some("text/plain".to_owned()),
                    badan: format!("badan dari {host}"),
                })
            } else {
                Err(format!(
                    "host '{host}' tidak ada di daftar izin gerbang jaringan"
                ))
            }
        }
    }

    #[test]
    fn fetch_menolak_tanpa_jalur_jaringan() {
        // Isolat tanpa hook tidak boleh berubah jadi jalan keluar bebas.
        let isolat = Isolat::new(PaguIsolat::default()).unwrap();
        isolat
            .eval(
                "(async function () { \
                   try { await fetch('https://contoh.test/'); globalThis.__pesan = 'tidak menolak'; } \
                   catch (e) { globalThis.__pesan = e.name + ': ' + e.message; } \
                 })()",
            )
            .unwrap();
        let pesan = isolat.eval("globalThis.__pesan").unwrap();
        assert!(
            pesan.starts_with("TypeError: "),
            "penolakan harus berupa galat yang bisa ditangkap: {pesan}"
        );
        assert!(
            pesan.contains("tidak ada jalur jaringan"),
            "alasan penolakan harus menyebut sebabnya: {pesan}"
        );
    }

    #[test]
    fn fetch_yang_diizinkan_mengembalikan_badan() {
        let (gerbang, dicatat) = GerbangUji::baru(&["contoh.test"]);
        let isolat =
            Isolat::dengan_ambil(PaguIsolat::default(), gerbang as Arc<dyn Pengambil>).unwrap();
        // Satu panggilan per sifat yang diperiksa, supaya jumlah panggilan
        // yang tercatat di gerbang tetap empat seperti sebelumnya.
        isolat
            .eval(
                "(async function () { \
                   var r = await fetch('https://contoh.test/a'); \
                   globalThis.__teks = await r.text(); \
                 })()",
            )
            .unwrap();
        assert_eq!(
            isolat.eval("globalThis.__teks").unwrap(),
            "badan dari contoh.test"
        );
        isolat
            .eval(
                "(async function () { \
                   globalThis.__status = (await fetch('https://contoh.test/a')).status; \
                 })()",
            )
            .unwrap();
        assert_eq!(isolat.eval("globalThis.__status").unwrap(), "200");
        isolat
            .eval(
                "(async function () { \
                   globalThis.__ok = (await fetch('https://contoh.test/a')).ok; \
                 })()",
            )
            .unwrap();
        assert_eq!(isolat.eval("globalThis.__ok").unwrap(), "true");
        isolat
            .eval(
                "(async function () { \
                   var r = await fetch('https://contoh.test/a'); \
                   globalThis.__tipe = r.headers.get('Content-Type'); \
                 })()",
            )
            .unwrap();
        assert_eq!(isolat.eval("globalThis.__tipe").unwrap(), "text/plain");
        let tercatat = dicatat.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(tercatat.len(), 4, "setiap panggilan harus lewat gerbang");
        assert_eq!(tercatat[0].url, "https://contoh.test/a");
        assert_eq!(tercatat[0].metode, "GET");
        assert_eq!(tercatat[0].badan, None);
    }

    #[test]
    fn fetch_yang_ditolak_menyebut_alasan_gerbang() {
        let (gerbang, dicatat) = GerbangUji::baru(&["contoh.test"]);
        let isolat =
            Isolat::dengan_ambil(PaguIsolat::default(), gerbang as Arc<dyn Pengambil>).unwrap();
        isolat
            .eval(
                "(async function () { \
                   try { await fetch('https://terlarang.test/'); globalThis.__pesan = 'tidak menolak'; } \
                   catch (e) { globalThis.__pesan = e.message; } \
                 })()",
            )
            .unwrap();
        let pesan = isolat.eval("globalThis.__pesan").unwrap();
        assert!(
            pesan.contains("terlarang.test"),
            "penolakan harus menyebut host yang ditolak: {pesan}"
        );
        let tercatat = dicatat.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(tercatat.len(), 1, "penolakan tetap tercatat di gerbang");
    }

    #[test]
    fn fetch_meneruskan_metode_dan_badan() {
        let (gerbang, dicatat) = GerbangUji::baru(&["contoh.test"]);
        let isolat =
            Isolat::dengan_ambil(PaguIsolat::default(), gerbang as Arc<dyn Pengambil>).unwrap();
        isolat
            .eval(
                "(async function () { \
                   await (await fetch('https://contoh.test/kirim', \
                     {method: 'post', body: 'a=1'})).text(); \
                 })()",
            )
            .unwrap();
        let tercatat = dicatat.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            tercatat[0].metode, "POST",
            "metode dinormalkan ke huruf besar"
        );
        assert_eq!(tercatat[0].badan.as_deref(), Some("a=1"));
    }

    #[test]
    fn fetch_tidak_meninggalkan_global_tambahan() {
        // Nama jembatan dihapus sebelum skrip halaman mana pun berjalan; yang
        // tersisa hanya `fetch` sendiri.
        let (gerbang, _) = GerbangUji::baru(&["contoh.test"]);
        let isolat =
            Isolat::dengan_ambil(PaguIsolat::default(), gerbang as Arc<dyn Pengambil>).unwrap();
        assert_eq!(
            isolat.eval("typeof globalThis.__ambil").unwrap(),
            "undefined"
        );
        assert_eq!(isolat.eval("typeof fetch").unwrap(), "function");
        assert_eq!(
            isolat
                .eval("Object.keys(globalThis).includes('fetch')")
                .unwrap(),
            "true"
        );
    }

    #[test]
    fn fetch_json_mengurai_badan() {
        struct Json;
        impl Pengambil for Json {
            fn ambil(&self, permintaan: &PermintaanAmbil) -> Result<JawabanAmbil, String> {
                Ok(JawabanAmbil {
                    status: 201,
                    url: permintaan.url.clone(),
                    content_type: Some("application/json".to_owned()),
                    badan: "{\"nilai\": 7}".to_owned(),
                })
            }
        }
        let isolat = Isolat::dengan_ambil(PaguIsolat::default(), Arc::new(Json)).unwrap();
        isolat
            .eval(
                "(async function () { \
                   var r = await fetch('https://contoh.test/j'); \
                   globalThis.__nilai = (await r.json()).nilai; \
                 })()",
            )
            .unwrap();
        assert_eq!(isolat.eval("globalThis.__nilai").unwrap(), "7");
    }

    #[test]
    fn mengevaluasi_ekspresi() {
        let isolat = Isolat::new(PaguIsolat::default()).unwrap();
        assert_eq!(isolat.eval("1 + 2").unwrap(), "3");
        assert_eq!(isolat.eval("'a' + \"b\"").unwrap(), "ab");
    }

    #[test]
    fn state_yang_sama_terlihat_antar_panggilan() {
        let isolat = Isolat::new(PaguIsolat::default()).unwrap();
        isolat.eval("var hitungan = 41").unwrap();
        assert_eq!(isolat.eval("hitungan + 1").unwrap(), "42");
    }

    #[test]
    fn console_log_tertangkap() {
        let isolat = Isolat::new(PaguIsolat::default()).unwrap();
        isolat.eval("console.log('halo', 7)").unwrap();
        isolat.eval("console.log('baris kedua')").unwrap();
        assert_eq!(
            isolat.log(),
            vec!["halo 7".to_string(), "baris kedua".to_string()]
        );
    }

    #[test]
    fn judul_bisa_dibaca_skrip() {
        let isolat = Isolat::new(PaguIsolat::default()).unwrap();
        isolat.pasang_judul("uji").unwrap();
        assert_eq!(isolat.eval("document.title").unwrap(), "uji");
    }

    #[test]
    fn skrip_tidak_punya_akses_berkas_atau_modul() {
        let isolat = Isolat::new(PaguIsolat::default()).unwrap();
        let galat = isolat.eval("typeof require").unwrap();
        assert_eq!(galat, "undefined");
    }

    #[test]
    fn galat_skrip_dilaporkan_bukan_ditelan() {
        let isolat = Isolat::new(PaguIsolat::default()).unwrap();
        assert!(isolat.eval("tidak_didefinisikan()").is_err());
    }

    #[test]
    fn galat_skrip_menyebut_simbol_yang_hilang() {
        // AC-023. Pesan `-32000 skrip gagal: Exception` tidak memberi tahu
        // klien apa yang hilang. Debug untuk `rquickjs::Error::Exception`
        // cuma mencetak kata "Exception", jadi pesan pengecualian harus
        // diambil dari nilai yang tertunda di konteks.
        let isolat = Isolat::new(PaguIsolat::default()).unwrap();
        let galat = isolat
            .eval_bertipe("window.matchMedia('(min-width: 1px)')")
            .expect_err("panggilan ke API yang tidak ada harus gagal");
        let pesan = galat.to_string();
        assert!(
            pesan.contains("matchMedia"),
            "pesan galat harus menyebut simbol yang hilang, dapat: {pesan}"
        );
    }

    #[test]
    fn test_navigator_webdriver_false() {
        // AC-017. Skrip anti-bot membandingkan `navigator.webdriver === false`,
        // jadi yang harus keluar boolean, bukan string "false" dan bukan
        // `undefined` dari objek yang tidak pernah dipasang.
        let isolat = Isolat::new(PaguIsolat::default()).unwrap();
        assert_eq!(
            isolat.eval_bertipe("navigator.webdriver").unwrap(),
            NilaiJs::Boolean(false)
        );
        assert_eq!(
            isolat.eval("navigator.webdriver === false").unwrap(),
            "true"
        );
        assert_eq!(
            isolat.eval("typeof navigator.webdriver").unwrap(),
            "boolean"
        );
    }

    #[test]
    fn tipe_nilai_dipertahankan() {
        let isolat = Isolat::new(PaguIsolat::default()).unwrap();
        assert_eq!(isolat.eval_bertipe("1 + 1").unwrap(), NilaiJs::Bulat(2));
        assert_eq!(isolat.eval_bertipe("1.5").unwrap(), NilaiJs::Pecahan(1.5));
        assert_eq!(
            isolat.eval_bertipe("'halo'").unwrap(),
            NilaiJs::Teks("halo".to_owned())
        );
        assert_eq!(isolat.eval_bertipe("true").unwrap(), NilaiJs::Boolean(true));
        assert_eq!(isolat.eval_bertipe("null").unwrap(), NilaiJs::Null);
        assert_eq!(
            isolat.eval_bertipe("undefined").unwrap(),
            NilaiJs::TanpaNilai
        );
        assert_eq!(
            isolat.eval_bertipe("({a: 1, b: 'dua'})").unwrap(),
            NilaiJs::Objek {
                json: Some(r#"{"a":1,"b":"dua"}"#.to_owned()),
                deskripsi: r#"{"a":1,"b":"dua"}"#.to_owned(),
            }
        );
        assert_eq!(
            isolat.eval_bertipe("[1, 2, 3]").unwrap(),
            NilaiJs::Objek {
                json: Some("[1,2,3]".to_owned()),
                deskripsi: "[1,2,3]".to_owned(),
            }
        );
    }

    #[test]
    fn test_no_automation_globals() {
        // AC-018. Penanda otomasi yang dikenal: `cdc_*` (ChromeDriver),
        // `__playwright`/`__puppeteer`/`__pw_*`, `__selenium`/`__webdriver*`,
        // `_phantom`, `__nightmare`, `callSelenium`.
        const PENANDA: &[&str] = &[
            "cdc_",
            "__playwright",
            "__puppeteer",
            "__pw_",
            "__selenium",
            "__webdriver",
            "_phantom",
            "__nightmare",
            "callSelenium",
            "__driver",
            "__oc_",
        ];
        let isolat = Isolat::new(PaguIsolat::default()).unwrap();
        let kunci = isolat
            .eval("JSON.stringify(Object.keys(globalThis))")
            .unwrap();
        let daftar: Vec<String> = serde_json::from_str(&kunci).unwrap();
        assert!(!daftar.is_empty(), "globalThis harus punya kunci");
        for nama in &daftar {
            for penanda in PENANDA {
                assert!(
                    !nama.contains(penanda),
                    "global '{nama}' mengandung penanda otomasi '{penanda}': {daftar:?}"
                );
            }
        }
        // `__log` ada, tapi non-enumerable, jadi tidak muncul di daftar di atas
        // dan tidak terlihat oleh pemeriksa yang menyisir `Object.keys`.
        assert!(
            !daftar.iter().any(|nama| nama == "__log"),
            "__log tidak boleh enumerable: {daftar:?}"
        );
        assert_eq!(isolat.eval("typeof globalThis.__log").unwrap(), "object");
    }

    #[test]
    fn objek_melingkar_tidak_menggagalkan_evaluasi() {
        let isolat = Isolat::new(PaguIsolat::default()).unwrap();
        match isolat.eval_bertipe("var a = {}; a.self = a; a").unwrap() {
            NilaiJs::Objek { json, deskripsi } => {
                assert!(
                    json.is_none(),
                    "struktur melingkar tidak punya JSON: {json:?}"
                );
                assert!(!deskripsi.is_empty(), "deskripsi tetap harus ada");
            }
            lain => panic!("harus Objek, dapat {lain:?}"),
        }
    }

    #[test]
    fn fungsi_dilaporkan_sebagai_fungsi() {
        let isolat = Isolat::new(PaguIsolat::default()).unwrap();
        match isolat
            .eval_bertipe("(function tambah(a, b) { return a + b; })")
            .unwrap()
        {
            NilaiJs::Fungsi(teks) => assert!(teks.contains("tambah"), "sumber fungsi: {teks}"),
            lain => panic!("harus Fungsi, dapat {lain:?}"),
        }
    }

    #[test]
    fn pagu_memori_menghentikan_alokasi_besar() {
        let isolat = Isolat::new(PaguIsolat::new(512 * 1024, 128 * 1024)).unwrap();
        let hasil = isolat.eval("new Uint8Array(4 * 1024 * 1024).length");
        assert!(
            hasil.is_err(),
            "alokasi di atas pagu harus gagal: {hasil:?}"
        );
    }

    // ── Jembatan DOM ──────────────────────────────────────────────────────
    // W4b. Sebelum ini `document` di dalam isolat adalah objek kosong: skrip
    // halaman bisa membaca `document.title`, tapi `getElementById` tidak ada
    // dan tidak ada mutasi yang sampai ke pohon Rust. Tes di bawah menuntut
    // dua arah sekaligus: JS melihat isi pohon, dan Rust melihat hasil
    // mutasinya. Satu arah saja tidak membuktikan jembatan, hanya membuktikan
    // bahwa ada sesuatu yang bisa dipanggil.

    fn isolat_dengan(html: &str) -> (Isolat, Arc<Mutex<Document>>) {
        let dok = Arc::new(Mutex::new(Document::parse_html(html).unwrap()));
        let isolat = Isolat::dengan_dokumen(PaguIsolat::default(), Arc::clone(&dok)).unwrap();
        (isolat, dok)
    }

    /// Membaca pohon dari sisi Rust. Dipakai tes, bukan jalur produksi, supaya
    /// satu pinjaman tidak pernah hidup melewati satu pernyataan.
    fn isi(dok: &Arc<Mutex<Document>>) -> MutexGuard<'_, Document> {
        dok.lock().unwrap()
    }

    // ── Eksekusi `<script>` halaman ───────────────────────────────────────
    // Sampai W5, satu-satunya jalan menjalankan JavaScript adalah
    // `Runtime.evaluate` dari klien: halaman yang mengisi dirinya sendiri
    // lewat `<script>` tampil kosong di mesin ini, tanpa satu pun galat. Tes
    // di bawah menuntut hal yang berbeda dari tes jembatan DOM: bukan "JS
    // bisa memutasi pohon kalau dipanggil", tapi "pohon berubah karena
    // halaman memintanya sendiri".

    fn isolat_halaman(
        html: &str,
        url: &str,
        pengambil: Option<Arc<dyn Pengambil>>,
    ) -> (Isolat, Arc<Mutex<Document>>) {
        let dok = Arc::new(Mutex::new(Document::parse_html(html).unwrap()));
        let halaman = Arc::new(Halaman::baru(url, Viewport::new(800.0, 600.0), Vec::new()));
        let isolat = Isolat::dengan_dokumen_ambil_dan_halaman(
            PaguIsolat::default(),
            Arc::clone(&dok),
            pengambil.unwrap_or_else(|| Arc::new(PengambilTolak)),
            Some(halaman),
        )
        .unwrap();
        (isolat, dok)
    }

    #[test]
    fn skrip_sebaris_berjalan_saat_halaman_dibuka() {
        let (isolat, dok) = isolat_halaman(
            "<html><head><title>awal</title></head><body>\
             <div id=\"x\">lama</div>\
             <script>document.title = 'dari skrip'; \
             document.getElementById('x').textContent = 'halo';</script>\
             </body></html>",
            "https://contoh.test/",
            None,
        );
        let laporan = isolat.jalankan_skrip_halaman().unwrap();
        assert_eq!(laporan.dijalankan, 1, "satu skrip sebaris harus berjalan");
        assert!(
            laporan.galat.is_empty(),
            "galat tak terduga: {:?}",
            laporan.galat
        );
        assert!(laporan.dilewati.is_empty(), "tidak ada yang dilewati");
        assert!(laporan.bersih());

        // Sisi Rust harus melihat hasilnya: judul di pohon, bukan di isolat.
        assert_eq!(isi(&dok).judul().as_deref(), Some("dari skrip"));
        assert_eq!(
            isolat
                .eval("document.getElementById('x').textContent")
                .unwrap(),
            "halo"
        );
    }

    #[test]
    fn skrip_bukan_javascript_dilewati_dengan_alasan() {
        // `application/json` bukan program. Menjalankannya akan melempar galat
        // sintaks yang menyamar sebagai halaman rusak, jadi yang benar adalah
        // melewatinya dan menyebut sebabnya.
        let (isolat, _dok) = isolat_halaman(
            "<html><body>\
             <script type=\"application/json\">{\"a\":1}</script>\
             <script type=\"module\">export const a = 1;</script>\
             </body></html>",
            "https://contoh.test/",
            None,
        );
        let laporan = isolat.jalankan_skrip_halaman().unwrap();
        assert_eq!(laporan.dijalankan, 0, "tidak ada yang boleh dijalankan");
        assert!(laporan.galat.is_empty());
        assert_eq!(laporan.dilewati.len(), 2, "{:?}", laporan.dilewati);
        assert!(
            laporan
                .dilewati
                .iter()
                .any(|a| a.contains("application/json")),
            "{:?}",
            laporan.dilewati
        );
        assert!(
            laporan.dilewati.iter().any(|a| a.contains("module")),
            "{:?}",
            laporan.dilewati
        );
        assert!(!laporan.bersih(), "nol skrip berjalan bukan putaran bersih");
    }

    /// Gerbang jaringan yang mengembalikan JavaScript, dan mencatat URL yang
    /// diminta. Dipakai untuk membuktikan `src` relatif diselesaikan terhadap
    /// URL halaman, bukan terhadap direktori kerja proses.
    struct GerbangSkrip {
        dicatat: Arc<Mutex<Vec<String>>>,
    }

    impl Pengambil for GerbangSkrip {
        fn ambil(&self, permintaan: &PermintaanAmbil) -> Result<JawabanAmbil, String> {
            self.dicatat
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(permintaan.url.clone());
            Ok(JawabanAmbil {
                status: 200,
                url: permintaan.url.clone(),
                content_type: Some("text/javascript".to_owned()),
                badan: "document.title = 'dari luar';".to_owned(),
            })
        }
    }

    #[test]
    fn skrip_luar_diambil_lewat_jalur_jaringan_lalu_berjalan() {
        let dicatat = Arc::new(Mutex::new(Vec::new()));
        let gerbang = Arc::new(GerbangSkrip {
            dicatat: Arc::clone(&dicatat),
        });
        let (isolat, dok) = isolat_halaman(
            "<html><head><title>awal</title></head><body>\
             <script src=\"app.js\"></script></body></html>",
            "https://contoh.test/folder/halaman",
            Some(gerbang as Arc<dyn Pengambil>),
        );
        let laporan = isolat.jalankan_skrip_halaman().unwrap();
        assert_eq!(laporan.dijalankan, 1, "galat: {:?}", laporan.galat);
        assert!(laporan.galat.is_empty(), "{:?}", laporan.galat);

        let diminta = dicatat.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(
            diminta,
            vec!["https://contoh.test/folder/app.js".to_owned()],
            "src relatif harus diselesaikan terhadap URL halaman"
        );
        assert_eq!(isi(&dok).judul().as_deref(), Some("dari luar"));
    }

    #[test]
    fn skrip_yang_melempar_galat_tidak_menghentikan_skrip_lain() {
        // Satu skrip rusak di kepala halaman tidak boleh mematikan skrip
        // berikutnya: browser juga begitu, dan halaman seperti itu tetap harus
        // bisa dibaca.
        let (isolat, dok) = isolat_halaman(
            "<html><body>\
             <script>lempar_galat_yang_tidak_ada()</script>\
             <script>document.title = 'skrip kedua tetap jalan'</script>\
             </body></html>",
            "https://contoh.test/",
            None,
        );
        let laporan = isolat.jalankan_skrip_halaman().unwrap();
        assert_eq!(laporan.dijalankan, 2, "dua skrip dipanggil");
        assert_eq!(laporan.galat.len(), 1, "satu galat dilaporkan: {laporan:?}");
        assert_eq!(
            isi(&dok).judul().as_deref(),
            Some("skrip kedua tetap jalan")
        );
    }

    // ── API konteks halaman ───────────────────────────────────────────────
    // Satu tes per API, dan nama tesnya menyebut API-nya. Halaman nyata
    // memanggil `location.href`, `window.innerWidth`, `setTimeout`, dan
    // `getComputedStyle` di baris pertama skripnya; tanpa ini skrip halaman
    // berjalan lalu langsung melempar `ReferenceError`, dan halaman yang
    // seharusnya terisi tampil kosong.

    #[test]
    fn location_membaca_url_halaman() {
        let (isolat, _dok) = isolat_halaman(
            "<html><body></body></html>",
            "https://contoh.test/folder/halaman?x=1#bagian",
            None,
        );
        assert_eq!(
            isolat.eval("location.href").unwrap(),
            "https://contoh.test/folder/halaman?x=1#bagian"
        );
        assert_eq!(
            isolat.eval("location.origin").unwrap(),
            "https://contoh.test"
        );
        assert_eq!(isolat.eval("location.protocol").unwrap(), "https:");
        assert_eq!(isolat.eval("location.hostname").unwrap(), "contoh.test");
        assert_eq!(isolat.eval("location.pathname").unwrap(), "/folder/halaman");
        assert_eq!(isolat.eval("location.search").unwrap(), "?x=1");
        assert_eq!(isolat.eval("location.hash").unwrap(), "#bagian");
        assert_eq!(
            isolat.eval("String(location)").unwrap(),
            "https://contoh.test/folder/halaman?x=1#bagian"
        );
    }

    #[test]
    fn inner_width_dan_inner_height_membaca_viewport() {
        let (isolat, _dok) =
            isolat_halaman("<html><body></body></html>", "https://contoh.test/", None);
        assert_eq!(isolat.eval("innerWidth").unwrap(), "800");
        assert_eq!(isolat.eval("innerHeight").unwrap(), "600");
        assert_eq!(isolat.eval("window.outerWidth").unwrap(), "800");
        assert_eq!(isolat.eval("window.outerHeight").unwrap(), "600");
        assert_eq!(isolat.eval("devicePixelRatio").unwrap(), "1");
    }

    #[test]
    fn settimeout_dijalankan_saat_timer_dikuras() {
        let (isolat, dok) = isolat_halaman(
            "<html><head><title>awal</title></head><body></body></html>",
            "https://contoh.test/",
            None,
        );
        isolat
            .eval("setTimeout(function () { document.title = 'dari timer'; }, 0)")
            .unwrap();
        // Belum dikuras: tidak ada yang berjalan sendiri. Ini batas yang
        // disebut di CELAH, dan tes ini yang membuatnya tidak diam-diam
        // berubah jadi janji yang lebih besar.
        assert_eq!(isi(&dok).judul().as_deref(), Some("awal"));
        assert_eq!(isolat.kuras_timer().unwrap(), 1);
        assert_eq!(isi(&dok).judul().as_deref(), Some("dari timer"));
    }

    #[test]
    fn clear_timeout_membatalkan_callback() {
        let (isolat, dok) = isolat_halaman(
            "<html><head><title>awal</title></head><body></body></html>",
            "https://contoh.test/",
            None,
        );
        isolat
            .eval("var id = setTimeout(function () { document.title = 'tidak boleh'; }, 0); clearTimeout(id)")
            .unwrap();
        assert_eq!(isolat.kuras_timer().unwrap(), 0);
        assert_eq!(isi(&dok).judul().as_deref(), Some("awal"));
    }

    #[test]
    fn setinterval_dan_clearinterval_mengatur_antrean() {
        // Yang diuji antreannya, bukan waktunya: tes yang menunggu 1 detik
        // untuk membuktikan interval berulang itu tes yang lambat dan rapuh.
        let (isolat, _dok) =
            isolat_halaman("<html><body></body></html>", "https://contoh.test/", None);
        isolat
            .eval("var id = setInterval(function () {}, 1000)")
            .unwrap();
        assert_eq!(isolat.eval("__timer.jumlah()").unwrap(), "1");
        // Belum jatuh tempo, jadi kuras tidak memanggil apa pun.
        assert_eq!(isolat.kuras_timer().unwrap(), 0);
        isolat.eval("clearInterval(id)").unwrap();
        assert_eq!(isolat.eval("__timer.jumlah()").unwrap(), "0");
    }

    #[test]
    fn get_computed_style_membaca_kaskade_bukan_atribut() {
        let (isolat, _dok) = isolat_halaman(
            "<html><head><style>#a { color: rgb(1, 2, 3); }</style></head>\
             <body><div id=\"a\" style=\"font-size: 20px\">x</div></body></html>",
            "https://contoh.test/",
            None,
        );
        // Ukuran font dari atribut `style`, warna dari `<style>`: dua sumber
        // berbeda yang keduanya harus lewat kaskade.
        assert_eq!(
            isolat
                .eval("getComputedStyle(document.getElementById('a')).fontSize")
                .unwrap(),
            "20.0px"
        );
        let warna = isolat
            .eval("getComputedStyle(document.getElementById('a')).getPropertyValue('color')")
            .unwrap();
        assert!(
            warna.contains("1, 2, 3") || warna.contains("1,2,3"),
            "warna dari <style> harus terbaca, dapat: {warna:?}"
        );
    }

    #[test]
    fn get_bounding_client_rect_memakai_layout() {
        let (isolat, _dok) = isolat_halaman(
            "<html><body><div id=\"a\" style=\"width:50px;height:20px\">x</div></body></html>",
            "https://contoh.test/",
            None,
        );
        assert_eq!(
            isolat
                .eval("String(document.getElementById('a').getBoundingClientRect().height)")
                .unwrap(),
            "20"
        );
        assert_eq!(
            isolat
                .eval("String(document.getElementById('a').getBoundingClientRect().width)")
                .unwrap(),
            "50"
        );
    }

    #[test]
    fn cache_layout_dibuang_saat_pohon_berubah() {
        // Cache layout dikunci versi pohon, dan versi itu dinaikkan oleh
        // jembatan saat operasi yang mengubah pohon lewat. Tes ini menuntut
        // kenaikan itu benar-benar terjadi: tanpa itu, `setAttribute` yang
        // mengubah lebar akan tetap menjawab angka lama, tanpa satu pun galat.
        let (isolat, _dok) = isolat_halaman(
            "<html><body><div id=\"a\" style=\"width:50px;height:20px\">x</div></body></html>",
            "https://contoh.test/",
            None,
        );
        let baca = |ekspresi: &str| isolat.eval(ekspresi).unwrap();
        let lebar = "String(document.getElementById('a').getBoundingClientRect().width)";
        assert_eq!(baca(lebar), "50", "lebar awal dari layout");
        // Panggilan kedua membaca cache: hasilnya harus sama, bukan nol.
        assert_eq!(baca(lebar), "50", "panggilan kedua memakai cache");
        isolat
            .eval("document.getElementById('a').setAttribute('style', 'width:120px;height:20px')")
            .unwrap();
        assert_eq!(
            baca(lebar),
            "120",
            "layout harus dihitung ulang sesudah atribut berubah"
        );
    }

    #[test]
    fn inner_html_getter_mengembalikan_isi_elemen() {
        let (isolat, _dok) = isolat_halaman(
            "<html><body><div id=\"p\">teks<span class=\"s\">satu</span></div></body></html>",
            "https://contoh.test/",
            None,
        );
        let isi_elemen = isolat
            .eval("document.getElementById('p').innerHTML")
            .unwrap();
        assert!(isi_elemen.contains("teks"), "{isi_elemen:?}");
        assert!(isi_elemen.contains("<span"), "{isi_elemen:?}");
        assert!(isi_elemen.contains("satu"), "{isi_elemen:?}");
        // Tag elemennya sendiri tidak ikut: itu `outerHTML`.
        assert!(!isi_elemen.contains("id=\"p\""), "{isi_elemen:?}");
    }

    #[test]
    fn get_elements_by_tag_name_mengembalikan_semua() {
        let (isolat, _dok) = isolat_halaman(
            "<html><body><div><span>a</span></div><span>b</span></body></html>",
            "https://contoh.test/",
            None,
        );
        assert_eq!(
            isolat
                .eval("document.getElementsByTagName('span').length")
                .unwrap(),
            "2"
        );
        assert_eq!(
            isolat
                .eval("document.getElementsByTagName('span')[1].textContent")
                .unwrap(),
            "b"
        );
        // Versi elemen hanya menelusuri anaknya sendiri.
        assert_eq!(
            isolat
                .eval("document.querySelector('div').getElementsByTagName('span').length")
                .unwrap(),
            "1"
        );
    }

    #[test]
    fn first_element_child_dan_last_element_child_melewati_teks() {
        let (isolat, _dok) = isolat_halaman(
            "<html><body><div id=\"p\">teks<span>satu</span><b>dua</b>teks</div></body></html>",
            "https://contoh.test/",
            None,
        );
        assert_eq!(
            isolat
                .eval("document.getElementById('p').firstElementChild.tagName")
                .unwrap(),
            "SPAN"
        );
        assert_eq!(
            isolat
                .eval("document.getElementById('p').lastElementChild.tagName")
                .unwrap(),
            "B"
        );
        assert_eq!(
            isolat
                .eval("document.getElementById('p').childElementCount")
                .unwrap(),
            "2"
        );
    }

    #[test]
    fn add_event_listener_di_document_menerima_click_yang_menggelembung() {
        let (isolat, _dok) = isolat_halaman(
            "<html><body><div id=\"tombol\">tekan</div></body></html>",
            "https://contoh.test/",
            None,
        );
        let kena = isolat
            .eval(
                "var kena = 0; \
                 document.addEventListener('click', function () { kena++; }); \
                 document.getElementById('tombol').click(); \
                 kena",
            )
            .unwrap();
        assert_eq!(kena, "1", "click harus menggelembung sampai document");
        // `removeEventListener` harus benar-benar melepas, bukan cuma
        // menandai: pendengar yang tertinggal akan menghitung klik berikutnya.
        let sesudah = isolat
            .eval(
                "var f = function () { kena++; }; \
                 document.addEventListener('click', f); \
                 document.removeEventListener('click', f); \
                 document.getElementById('tombol').click(); \
                 kena",
            )
            .unwrap();
        assert_eq!(sesudah, "2", "pendengar yang dilepas tidak boleh dipanggil");
    }

    #[test]
    fn test_js_runtime_dom_mutations() {
        // AC-003. JavaScript harus bisa mencari elemen, membuat elemen,
        // menyisipkannya, dan mengubah teks, lalu semuanya terlihat di pohon
        // Rust yang dipakai `DOM.getDocument` dan raster.
        //
        // **Tes ini memanggil `eval` langsung, jadi ia tidak membuktikan
        // bahwa `<script>` di halaman pernah dijalankan.** Yang membuktikan
        // itu `skrip_sebaris_berjalan_saat_halaman_dibuka` dan kawan-kawannya
        // di bawah. Nama tes ini menyebut "runtime" karena itu jalur
        // `Runtime.evaluate`, bukan pemuatan halaman; sempat dibaca sebagai
        // bukti eksekusi `<script>`, dan itu keliru.
        let (isolat, dok) = isolat_dengan(
            "<html><head></head><body><div id=\"kartu\"><p>lama</p></div></body></html>",
        );

        // Baca: pencarian dan pembacaan teks dari sisi JS.
        assert_eq!(
            isolat
                .eval("document.getElementById('kartu').tagName")
                .unwrap(),
            "DIV"
        );
        assert_eq!(
            isolat
                .eval("document.getElementById('kartu').textContent")
                .unwrap(),
            "lama"
        );
        assert_eq!(
            isolat
                .eval("document.querySelector('p').textContent")
                .unwrap(),
            "lama"
        );

        // Tulis: teks, atribut, dan elemen baru.
        isolat
            .eval("document.querySelector('p').textContent = 'baru'")
            .unwrap();
        isolat
            .eval("document.getElementById('kartu').setAttribute('data-uji', '1')")
            .unwrap();
        isolat
            .eval(
                "var li = document.createElement('span');
                 li.textContent = 'tambahan';
                 document.getElementById('kartu').appendChild(li);",
            )
            .unwrap();

        // Sisi JS melihat hasilnya sendiri.
        assert_eq!(
            isolat
                .eval("document.getElementById('kartu').textContent")
                .unwrap(),
            "barutambahan"
        );
        assert_eq!(
            isolat
                .eval("document.getElementById('kartu').children.length")
                .unwrap(),
            "2"
        );
        assert_eq!(
            isolat
                .eval("document.querySelector('span').getAttribute('data-uji')")
                .unwrap(),
            "null"
        );

        // Sisi Rust melihat pohon yang sama, bukan salinan.
        let dok = isi(&dok);
        let kartu = dok.cari_id("kartu").expect("kartu harus ada");
        assert_eq!(dok.teks(kartu), "barutambahan");
        assert_eq!(dok.node(kartu).unwrap().attr("data-uji"), Some("1"));
        assert!(
            dok.to_html().contains("<span>tambahan</span>"),
            "elemen baru harus ikut terserialisasi: {}",
            dok.to_html()
        );
    }

    #[test]
    fn mutasi_dari_js_bertahan_ke_panggilan_berikutnya() {
        let (isolat, dok) = isolat_dengan("<body><div id=\"a\"></div></body>");
        isolat
            .eval("document.getElementById('a').appendChild(document.createTextNode('satu'))")
            .unwrap();
        assert_eq!(
            isolat
                .eval("document.getElementById('a').textContent")
                .unwrap(),
            "satu"
        );
        // Nomor node stabil: objek yang sama bisa dipakai lagi.
        isolat
            .eval("var simpul = document.getElementById('a')")
            .unwrap();
        assert_eq!(isolat.eval("simpul.textContent").unwrap(), "satu");
        {
            let dok = isi(&dok);
            assert_eq!(dok.teks(dok.cari_id("a").unwrap()), "satu");
        }
    }

    #[test]
    fn hapus_elemen_dari_js_menghilangkannya_dari_pohon() {
        let (isolat, dok) = isolat_dengan("<body><div id=\"a\"><b>x</b></div></body>");
        isolat
            .eval("document.getElementById('a').remove()")
            .unwrap();

        assert_eq!(isolat.eval("document.getElementById('a')").unwrap(), "null");
        let dok = isi(&dok);
        assert!(dok.cari_id("a").is_none());
        assert!(!dok.to_html().contains("<div id=\"a\">"));
    }

    #[test]
    fn judul_lewat_document_menulis_ke_pohon() {
        let (isolat, dok) =
            isolat_dengan("<html><head><title>lama</title></head><body></body></html>");
        assert_eq!(isolat.eval("document.title").unwrap(), "lama");

        isolat.eval("document.title = 'baru'").unwrap();

        assert_eq!(isolat.eval("document.title").unwrap(), "baru");
        assert_eq!(isi(&dok).judul().as_deref(), Some("baru"));
    }

    #[test]
    fn judul_dibuat_kalau_belum_ada() {
        let (isolat, dok) = isolat_dengan("<html><head></head><body></body></html>");
        isolat.eval("document.title = 'dibuat'").unwrap();
        assert_eq!(isi(&dok).judul().as_deref(), Some("dibuat"));
    }

    #[test]
    fn simpul_yang_sama_dikembalikan_sebagai_objek_yang_sama() {
        // Identitas objek penting: skrip halaman menyimpan elemen di variabel
        // lalu membandingkannya. Dua pembungkus berbeda untuk satu node akan
        // membuat `===` salah dan cache skrip halaman berantakan.
        let (isolat, _) = isolat_dengan("<body><div id=\"a\"></div></body>");
        assert_eq!(
            isolat
                .eval("document.getElementById('a') === document.getElementById('a')")
                .unwrap(),
            "true"
        );
    }

    #[test]
    fn jembatan_tidak_tertinggal_sebagai_global() {
        // AC-018 tetap berlaku setelah jembatan dipasang: nama `__dom` dihapus
        // sebelum skrip halaman mana pun berjalan, jadi tidak bisa dipanggil
        // langsung maupun ditemukan lewat penyisiran properti.
        let (isolat, _) = isolat_dengan("<body></body>");
        assert_eq!(isolat.eval("typeof globalThis.__dom").unwrap(), "undefined");
        let kunci = isolat
            .eval("JSON.stringify(Object.getOwnPropertyNames(globalThis))")
            .unwrap();
        assert!(
            !kunci.contains("__dom"),
            "nama jembatan tidak boleh tersisa: {kunci}"
        );
        assert!(
            kunci.contains("\"document\""),
            "document tetap ada: {kunci}"
        );
    }

    #[test]
    fn selektor_tidak_sah_dilaporkan_sebagai_galat() {
        let (isolat, _) = isolat_dengan("<body></body>");
        let hasil = isolat.eval("document.querySelector('div[')");
        assert!(hasil.is_err(), "selektor rusak harus melempar: {hasil:?}");
    }

    #[test]
    fn skrip_tanpa_akhir_dibatalkan_bukan_membekukan_tab() {
        // Sebelum ini `while (true) {}` menggantung tab selamanya: pagu memori
        // tidak menyentuhnya karena skrip itu tidak mengalokasikan apa pun.
        // Mekanismenya sama dengan `interrupt` di Luau: interpreter memanggil
        // handler berkala, dan handler yang mengembalikan `true` membatalkan
        // skripnya.
        let isolat =
            Isolat::new(PaguIsolat::default().dengan_batas_waktu(Duration::from_millis(50)))
                .unwrap();
        let mulai = Instant::now();
        let hasil = isolat.eval("while (true) {}");
        assert!(
            matches!(hasil, Err(NativeError::BatasWaktu(_))),
            "harus BatasWaktu, dapat {hasil:?}"
        );
        assert!(
            mulai.elapsed() < Duration::from_secs(3),
            "pembatalan harus cepat, bukan menunggu isolat mati: {:?}",
            mulai.elapsed()
        );
    }

    #[test]
    fn isolat_tetap_bisa_dipakai_setelah_batas_waktu() {
        // Pembatalan tidak boleh merusak isolat. Kalau ia meninggalkan runtime
        // dalam keadaan tidak bisa dipakai, satu skrip jahat cukup untuk
        // mematikan tab itu sampai navigasi berikutnya.
        let isolat =
            Isolat::new(PaguIsolat::default().dengan_batas_waktu(Duration::from_millis(50)))
                .unwrap();
        assert!(isolat.eval("while (true) {}").is_err());
        assert_eq!(isolat.eval("1 + 1").unwrap(), "2");
    }

    #[test]
    fn skrip_cepat_tidak_kena_batas_waktu() {
        let isolat =
            Isolat::new(PaguIsolat::default().dengan_batas_waktu(Duration::from_secs(5))).unwrap();
        assert_eq!(
            isolat.eval("for (var i = 0; i < 1000; i++) {} i").unwrap(),
            "1000"
        );
    }

    #[test]
    fn tanpa_dokumen_tidak_ada_jembatan() {
        let isolat = Isolat::new(PaguIsolat::default()).unwrap();
        assert!(isolat.dokumen().is_none());
        assert_eq!(
            isolat.eval("typeof document.getElementById").unwrap(),
            "undefined"
        );
    }

    // ── G10 lanjutan: permukaan DOM yang dipakai halaman nyata ─────────────
    //
    // Setiap tes di bawah gagal sebelum kemampuannya ada; itu syarat satu
    // baris `CELAH` boleh hilang. Yang diuji bukan "ada fungsi bernama X",
    // tapi "efeknya sampai ke pohon Rust" atau "nilainya sama dengan yang
    // dijawab browser sungguhan untuk fixture yang sama".

    #[test]
    fn class_list_menambah_dan_menghapus_kelas() {
        let (isolat, dok) = isolat_dengan("<div id=\"a\" class=\"satu dua\"></div>");
        // `contains` harus benar untuk kelas yang ada di atribut, bukan hanya
        // untuk yang ditambahkan lewat `classList`.
        assert_eq!(
            isolat
                .eval("document.getElementById('a').classList.contains('dua')")
                .unwrap(),
            "true"
        );
        isolat
            .eval(
                "var el = document.getElementById('a'); \
                   el.classList.add('tiga'); el.classList.remove('satu');",
            )
            .unwrap();
        // Kunci dokumen sekali: `isi(&dok).cari_id(..).map(|id| isi(&dok)..)`
        // mengunci ulang mutex yang masih dipegang temporer statement yang sama,
        // dan itu deadlock, bukan tes yang lambat.
        let id = isi(&dok).cari_id("a").unwrap();
        assert_eq!(
            isi(&dok).node(id).unwrap().attr("class"),
            Some("dua tiga"),
            "classList harus menulis atribut class di pohon Rust"
        );
        // `toggle` mengembalikan keadaan sesudahnya, dan `length` menghitung.
        assert_eq!(isolat.eval("el.classList.toggle('dua')").unwrap(), "false");
        assert_eq!(isolat.eval("el.classList.length").unwrap(), "1");
        assert_eq!(isolat.eval("el.className").unwrap(), "tiga");
    }

    #[test]
    fn dataset_membaca_dan_menulis_atribut_data() {
        let (isolat, dok) = isolat_dengan("<div id=\"a\" data-ubah-nama=\"lama\"></div>");
        // Nama ber-tanda hubung jadi camelCase, seperti di browser.
        assert_eq!(
            isolat
                .eval("document.getElementById('a').dataset.ubahNama")
                .unwrap(),
            "lama"
        );
        isolat
            .eval("document.getElementById('a').dataset.baruSekali = 'isi';")
            .unwrap();
        let id = isi(&dok).cari_id("a").unwrap();
        assert_eq!(
            isi(&dok).node(id).unwrap().attr("data-baru-sekali"),
            Some("isi"),
            "dataset harus menulis atribut data-* di pohon Rust"
        );
        // Menghapus lewat `delete` membuang atributnya, bukan menyisakan string
        // kosong yang akan terbaca sebagai `data-x=""` oleh CSS.
        isolat
            .eval("delete document.getElementById('a').dataset.baruSekali;")
            .unwrap();
        assert_eq!(isi(&dok).node(id).unwrap().attr("data-baru-sekali"), None);
    }

    #[test]
    fn setter_inner_html_mengganti_isi_elemen() {
        let (isolat, dok) = isolat_dengan("<div id=\"a\"><span>lama</span></div>");
        isolat
            .eval("document.getElementById('a').innerHTML = '<b>baru</b> teks';")
            .unwrap();
        let id = isi(&dok).cari_id("a").unwrap();
        // Isi lama hilang seluruhnya, dan yang baru ter-parse jadi simpul
        // sungguhan: satu elemen <b> plus satu simpul teks.
        assert_eq!(
            isi(&dok).dalam_html(id).as_deref(),
            Some("<b>baru</b> teks")
        );
        assert_eq!(
            isi(&dok).anak_tata(id).len(),
            2,
            "harus ada dua anak: <b> dan teks"
        );
        assert_eq!(
            isolat
                .eval("document.getElementById('a').textContent")
                .unwrap(),
            "baru teks"
        );
    }

    #[test]
    fn insert_adjacent_html_menyisipkan_di_posisi() {
        let (isolat, dok) = isolat_dengan("<div id=\"a\"><i>tengah</i></div>");
        isolat
            .eval(
                "var el = document.getElementById('a'); \
                 el.insertAdjacentHTML('afterbegin', '<b>awal</b>'); \
                 el.insertAdjacentHTML('beforeend', '<u>akhir</u>');",
            )
            .unwrap();
        let id = isi(&dok).cari_id("a").unwrap();
        assert_eq!(
            isi(&dok).dalam_html(id).as_deref(),
            Some("<b>awal</b><i>tengah</i><u>akhir</u>")
        );
        // `beforebegin` dan `afterend` menyentuh saudara, bukan anak: blok baru
        // mendarat di depan `#a` di induknya, bukan di dalamnya.
        isolat
            .eval("el.insertAdjacentHTML('beforebegin', '<p>sebelum</p>');")
            .unwrap();
        assert_eq!(
            isolat
                .eval("document.getElementById('a').previousElementSibling.outerHTML")
                .unwrap(),
            "<p>sebelum</p>"
        );
        assert_eq!(
            isi(&dok).dalam_html(id).as_deref(),
            Some("<b>awal</b><i>tengah</i><u>akhir</u>"),
            "beforebegin tidak boleh menambah anak"
        );
    }

    #[test]
    fn outer_html_mengembalikan_tag_sendiri() {
        let (isolat, _dok) = isolat_dengan("<div id=\"a\" class=\"x\"><b>isi</b></div>");
        assert_eq!(
            isolat
                .eval("document.getElementById('a').outerHTML")
                .unwrap(),
            "<div id=\"a\" class=\"x\"><b>isi</b></div>"
        );
    }

    #[test]
    fn matches_dan_closest_memakai_selektor() {
        let (isolat, _dok) = isolat_dengan(
            "<div id=\"luar\" class=\"kartu\"><section><span id=\"dalam\" class=\"label\">x</span></section></div>",
        );
        assert_eq!(
            isolat
                .eval("document.getElementById('dalam').matches('.label')")
                .unwrap(),
            "true"
        );
        assert_eq!(
            isolat
                .eval("document.getElementById('dalam').matches('div')")
                .unwrap(),
            "false"
        );
        // `closest` naik sampai ketemu, dan berhenti di akar tanpa melempar.
        assert_eq!(
            isolat
                .eval("document.getElementById('dalam').closest('.kartu').id")
                .unwrap(),
            "luar"
        );
        assert_eq!(
            isolat
                .eval("document.getElementById('dalam').closest('.tidak-ada')")
                .unwrap(),
            "null"
        );
    }

    #[test]
    fn next_dan_previous_element_sibling_melewati_teks() {
        let (isolat, _dok) = isolat_dengan(
            "<div><i id=\"a\">1</i> teks <i id=\"b\">2</i> teks <i id=\"c\">3</i></div>",
        );
        assert_eq!(
            isolat
                .eval("document.getElementById('a').nextElementSibling.id")
                .unwrap(),
            "b",
            "simpul teks di antaranya harus dilewati"
        );
        assert_eq!(
            isolat
                .eval("document.getElementById('c').nextElementSibling")
                .unwrap(),
            "null"
        );
        assert_eq!(
            isolat
                .eval("document.getElementById('c').previousElementSibling.id")
                .unwrap(),
            "b"
        );
    }

    #[test]
    fn owner_document_dan_is_connected() {
        let (isolat, _dok) = isolat_dengan("<div id=\"a\"></div>");
        assert_eq!(
            isolat
                .eval("document.getElementById('a').isConnected")
                .unwrap(),
            "true"
        );
        assert_eq!(
            isolat
                .eval("document.getElementById('a').ownerDocument === document")
                .unwrap(),
            "true"
        );
        // Simpul yang dibuat tapi belum ditempel bukan bagian dokumen.
        isolat
            .eval("var lepas = document.createElement('p');")
            .unwrap();
        assert_eq!(isolat.eval("lepas.isConnected").unwrap(), "false");
        isolat.eval("document.body.appendChild(lepas);").unwrap();
        assert_eq!(isolat.eval("lepas.isConnected").unwrap(), "true");
    }

    #[test]
    fn replace_children_dan_clone_node() {
        let (isolat, dok) = isolat_dengan("<div id=\"a\"><i>1</i><i>2</i></div>");
        isolat
            .eval("document.getElementById('a').replaceChildren(document.createElement('b'));")
            .unwrap();
        let id = isi(&dok).cari_id("a").unwrap();
        assert_eq!(isi(&dok).dalam_html(id).as_deref(), Some("<b></b>"));

        // `cloneNode(true)` menyalin subtree sebagai simpul baru, bukan
        // mengembalikan simpul yang sama: menempelkan hasil klon tidak boleh
        // memindahkan yang asli.
        isolat
            .eval("var asli = document.getElementById('a'); var salinan = asli.cloneNode(true);")
            .unwrap();
        assert_eq!(
            isolat.eval("salinan === asli").unwrap(),
            "false",
            "klon harus simpul lain"
        );
        assert_eq!(
            isolat.eval("salinan.outerHTML").unwrap(),
            "<div id=\"a\"><b></b></div>"
        );
        let sebelum = isi(&dok).jumlah_elemen();
        isolat.eval("document.body.appendChild(salinan);").unwrap();
        assert_eq!(
            isi(&dok).jumlah_elemen(),
            sebelum + 2,
            "menempelkan salinan menambah div dan anaknya, bukan memindahkan asli"
        );
    }

    // ── FR-013: readyState dan peristiwa pemuatan ─────────────────────────
    // Pustaka nyata menunggu `document.readyState` dan memasang pendengar
    // `DOMContentLoaded`/`load` sebelum menyentuh DOM. Tanpa keduanya,
    // halaman yang memakai jQuery atau Alpine berhenti di baris pertama.

    #[test]
    fn ready_state_loading_lalu_complete() {
        let (isolat, _dok) = isolat_halaman("<div id=\"a\"></div>", "https://contoh.test/", None);
        assert_eq!(
            isolat.eval("document.readyState").unwrap(),
            "loading",
            "selama skrip halaman berjalan, dokumen masih loading"
        );
        assert!(isolat.selesaikan_pemuatan().unwrap());
        assert_eq!(isolat.eval("document.readyState").unwrap(), "complete");
        assert!(
            !isolat.selesaikan_pemuatan().unwrap(),
            "pemuatan yang sama tidak boleh diselesaikan dua kali"
        );
    }

    #[test]
    fn dom_content_loaded_dan_load_dipanggil_berurutan() {
        let (isolat, dok) = isolat_halaman(
            "<html><head><title>awal</title></head><body>\
             <script>\
               window.addEventListener('DOMContentLoaded', function () { \
                 document.title = 'dcl:' + document.readyState; });\
               window.addEventListener('load', function () { \
                 document.title = document.title + '|load:' + document.readyState; });\
             </script></body></html>",
            "https://contoh.test/",
            None,
        );
        let laporan = isolat.jalankan_skrip_halaman().unwrap();
        assert!(laporan.galat.is_empty(), "galat: {:?}", laporan.galat);
        assert_eq!(
            isi(&dok).judul().as_deref(),
            Some("awal"),
            "pendengar belum dipanggil sebelum pemuatan selesai"
        );
        assert!(isolat.selesaikan_pemuatan().unwrap());
        // `DOMContentLoaded` melihat `interactive`, `load` melihat `complete`.
        // Itu urutan yang dijamin spesifikasi, dan pustaka memakainya untuk
        // membedakan "DOM siap" dari "semua sumber daya siap".
        assert_eq!(
            isi(&dok).judul().as_deref(),
            Some("dcl:interactive|load:complete")
        );
    }

    #[test]
    fn window_add_event_listener_tidak_lagi_galat() {
        let (isolat, _dok) = isolat_dengan("<div id=\"a\"></div>");
        assert_eq!(
            isolat
                .eval(
                    "typeof window.addEventListener + ':' + \
                     typeof window.removeEventListener + ':' + \
                     typeof document.addEventListener"
                )
                .unwrap(),
            "function:function:function"
        );
    }

    // ── FR-009: fetch mengembalikan Promise asli ──────────────────────────

    #[test]
    fn fetch_mengembalikan_promise_dengan_then() {
        let (gerbang, _dicatat) = GerbangUji::baru(&["contoh.test"]);
        let isolat =
            Isolat::dengan_ambil(PaguIsolat::default(), gerbang as Arc<dyn Pengambil>).unwrap();
        assert_eq!(
            isolat
                .eval("typeof fetch('https://contoh.test/a').then")
                .unwrap(),
            "function",
            "hasil fetch harus thenable, bukan objek biasa"
        );
        assert_eq!(
            isolat
                .eval(
                    "fetch('https://contoh.test/a').then(function (r) { return r.status; }) \
                     instanceof Promise"
                )
                .unwrap(),
            "true"
        );
    }

    #[test]
    fn await_fetch_selesai_dalam_satu_evaluasi() {
        let (gerbang, _dicatat) = GerbangUji::baru(&["contoh.test"]);
        let isolat =
            Isolat::dengan_ambil(PaguIsolat::default(), gerbang as Arc<dyn Pengambil>).unwrap();
        // Microtask dikuras sesudah eval, jadi fungsi async yang dijalankan
        // di sini sudah selesai saat `eval` kembali. Kalau antreannya tidak
        // dikuras, judulnya tetap kosong.
        isolat
            .eval(
                "(async function () { \
                   var r = await fetch('https://contoh.test/a'); \
                   globalThis.__hasil = await r.text(); \
                 })()",
            )
            .unwrap();
        let hasil = isolat.eval("globalThis.__hasil").unwrap();
        assert!(
            !hasil.is_empty() && hasil != "undefined",
            "await fetch harus menghasilkan badan, bukan {hasil:?}"
        );
    }

    #[test]
    fn fetch_yang_ditolak_menjadi_promise_yang_ditolak() {
        let (gerbang, _dicatat) = GerbangUji::baru(&["contoh.test"]);
        let isolat =
            Isolat::dengan_ambil(PaguIsolat::default(), gerbang as Arc<dyn Pengambil>).unwrap();
        let pesan = isolat
            .eval(
                "var hasil = 'belum'; \
                 fetch('https://terlarang.test/').catch(function (e) { \
                   hasil = e.name + ': ' + e.message; }); \
                 hasil",
            )
            .unwrap();
        // `.catch` adalah microtask: nilainya belum terlihat di baris yang
        // sama, dan itu memang perilaku browser. Yang diuji adalah hasil
        // sesudah antreannya dikuras, bukan ketergesa-gesaan.
        assert_eq!(pesan, "belum");
        let sesudah = isolat.eval("hasil").unwrap();
        assert!(
            sesudah.starts_with("TypeError: "),
            "penolakan harus berupa TypeError yang bisa ditangkap: {sesudah}"
        );
        assert!(
            sesudah.contains("tidak diizinkan") || sesudah.contains("gerbang"),
            "alasan penolakan harus menyebut gerbangnya: {sesudah}"
        );
    }
}
