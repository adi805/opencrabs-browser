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
/// dalam isolat. `fetch` masuk daftar [`super::CELAH`] sampai ada jalur
/// jaringan yang lewat daemon, bukan langsung dari skrip.
const PEMBUKA: &str = r#"
globalThis.__log = [];
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
"#;

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
    fn pagu_memori_menghentikan_alokasi_besar() {
        let isolat = Isolat::new(PaguIsolat::new(512 * 1024, 128 * 1024)).unwrap();
        let hasil = isolat.eval("new Uint8Array(4 * 1024 * 1024).length");
        assert!(
            hasil.is_err(),
            "alokasi di atas pagu harus gagal: {hasil:?}"
        );
    }
}
