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

use rquickjs::{Context, Runtime};

use super::NativeError;

/// Pagu keras untuk satu isolat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaguIsolat {
    pub memori_bytes: usize,
    pub stack_bytes: usize,
}

impl Default for PaguIsolat {
    /// 8 MB heap per tab. Di atas biaya terukur 3,10 MB per tab, cukup untuk
    /// halaman biasa, dan tetap di bawah anggaran 25 tab panas.
    fn default() -> Self {
        Self {
            memori_bytes: 8 * 1024 * 1024,
            stack_bytes: 512 * 1024,
        }
    }
}

impl PaguIsolat {
    #[must_use]
    pub fn new(memori_bytes: usize, stack_bytes: usize) -> Self {
        Self {
            memori_bytes,
            stack_bytes,
        }
    }
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
}

impl Isolat {
    pub fn new(pagu: PaguIsolat) -> Result<Self, NativeError> {
        let runtime = Runtime::new().map_err(NativeError::runtime)?;
        runtime.set_memory_limit(pagu.memori_bytes);
        runtime.set_max_stack_size(pagu.stack_bytes);
        let context = Context::full(&runtime).map_err(NativeError::runtime)?;

        let isolat = Self {
            context,
            _runtime: runtime,
        };
        isolat.eval(PEMBUKA)?;
        Ok(isolat)
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
        self.context
            .with(|ctx| ctx.eval::<rquickjs::Coerced<String>, _>(sumber.as_bytes().to_vec()))
            .map(|v| v.0)
            .map_err(NativeError::script)
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
        self.context.with(|ctx| {
            let nilai: rquickjs::Value = ctx
                .eval(sumber.as_bytes().to_vec())
                .map_err(NativeError::script)?;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
