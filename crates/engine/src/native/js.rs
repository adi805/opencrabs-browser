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
use super::dom::{Document, ModeBayang, NodeId, NodeKind};

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
/// Batas yang masih ada dan **tidak** disembunyikan: tidak ada `innerHTML`
/// setter, tidak ada `style`/`classList`, tidak ada `dataset`, tidak ada
/// `insertAdjacentHTML`, dan `querySelectorAll` mengembalikan `Array` biasa
/// alih-alih `NodeList`. Semuanya tercatat di [`super::CELAH`].
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
/// Yang dikembalikan **objek biasa, bukan `Promise`**, dan itu disengaja:
/// `await fetch(...)` dan `await jawaban.text()` tetap bekerja karena `await`
/// pada nilai yang bukan thenable mengembalikan nilai itu sendiri, sementara
/// menjalankan mesin promise di dalam isolat berarti satu state mesin lagi
/// yang harus diuji terhadap batas waktu. Batasnya disebut di [`super::CELAH`].
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
      text: function () {
        return teks;
      },
      json: function () {
        return JSON.parse(teks);
      },
    };
    return isi;
  }

  // Galat dilempar sinkron, bukan lewat promise yang ditolak. Bagi skrip yang
  // memakai `await` di dalam `try`, keduanya sama; bagi skrip yang memakai
  // `.catch()`, keduanya sama-sama tidak bekerja, dan itu tercatat di CELAH
  // alih-alih disamarkan.
  function fetch(url, init) {
    var metode = init && init.method ? String(init.method) : "GET";
    var badan = "";
    if (init && init.body !== undefined && init.body !== null) {
      badan = String(init.body);
    }
    var data = JSON.parse(ambil(metode, String(url), badan));
    if (data && typeof data.__galat === "string") {
      throw new TypeError(data.__galat);
    }
    return jawaban(data);
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
    op: &str,
    a: &str,
    b: &str,
    c: &str,
) -> Result<serde_json::Value, String> {
    use serde_json::json;

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
        lain => Err(format!("operasi jembatan tidak dikenal: {lain:?}")),
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
        };
        isolat.eval(PEMBUKA)?;
        isolat.pasang_ambil(pengambil)?;
        Ok(isolat)
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
        let mut isolat = Self::dengan_ambil(pagu, pengambil)?;
        isolat.dokumen = Some(dokumen);
        isolat.pasang_jembatan()?;
        Ok(isolat)
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
        self.context.with(|ctx| {
            let fungsi = Function::new(
                ctx.clone(),
                move |op: String, a: String, b: String, c: String| -> String {
                    match op_dom(&dokumen, &op, &a, &b, &c) {
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
                .map_err(|e| self.galat_skrip(e))?;
            let dijalankan: i32 = fungsi
                .call((tipe, i64::try_from(node).unwrap_or(i64::MAX), detail))
                .map_err(|e| self.galat_skrip(e))?;
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
    fn galat_skrip(&self, err: rquickjs::Error) -> NativeError {
        if self.lewat_batas.swap(false, Ordering::Relaxed) {
            NativeError::batas_waktu(self.batas_waktu)
        } else {
            NativeError::script(err)
        }
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
        let hasil = self
            .context
            .with(|ctx| ctx.eval::<rquickjs::Coerced<String>, _>(sumber.as_bytes().to_vec()))
            .map(|v| v.0)
            .map_err(|e| self.galat_skrip(e));
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
                .map_err(|e| self.galat_skrip(e))?;

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
        let pesan = isolat
            .eval(
                "try { fetch('https://contoh.test/'); 'tidak menolak' } \
                 catch (e) { e.name + ': ' + e.message }",
            )
            .unwrap();
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
        assert_eq!(
            isolat
                .eval("fetch('https://contoh.test/a').text()")
                .unwrap(),
            "badan dari contoh.test"
        );
        assert_eq!(
            isolat
                .eval("fetch('https://contoh.test/a').status")
                .unwrap(),
            "200"
        );
        assert_eq!(
            isolat.eval("fetch('https://contoh.test/a').ok").unwrap(),
            "true"
        );
        assert_eq!(
            isolat
                .eval("fetch('https://contoh.test/a').headers.get('Content-Type')")
                .unwrap(),
            "text/plain"
        );
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
        let pesan = isolat
            .eval(
                "try { fetch('https://terlarang.test/'); 'tidak menolak' } \
                 catch (e) { e.message }",
            )
            .unwrap();
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
            .eval("fetch('https://contoh.test/kirim', {method: 'post', body: 'a=1'}).text()")
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
        assert_eq!(
            isolat
                .eval("fetch('https://contoh.test/j').json().nilai")
                .unwrap(),
            "7"
        );
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

    #[test]
    fn test_js_runtime_dom_mutations() {
        // AC-003. Skrip halaman harus bisa mencari elemen, membuat elemen,
        // menyisipkannya, dan mengubah teks, lalu semuanya terlihat di pohon
        // Rust yang dipakai `DOM.getDocument` dan raster.
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
}
