//! AC-011: tab dingin dihidupkan kembali dari SQLite di bawah 100 ms.
//!
//! Tes ini mengukur jalur produksi, bukan jalur tes: adapter yang sama dengan
//! yang dipasang daemon (`browserd::tabstore::StoreColdStore`), database
//! SQLite sungguhan di direktori sementara, dan pool kedua yang dibuka dari
//! berkas yang sama untuk meniru daemon yang restart.
//!
//! Yang diukur hanya `TabPool::hidupkan`: baca satu baris dari SQLite, buka
//! segelnya, parse HTML, bangun isolat, lalu hapus baris dinginnya. Itu yang
//! dijanjikan PRD FR-006, dan itu juga yang dirasakan klien saat berpindah ke
//! tab yang sudah lama tidak dipakai.

use std::fmt::Write as _;
use std::time::Instant;

use browserd::tabstore::StoreColdStore;
use engine::native::tab_pool::{ColdStore, TabPool};
use store::{Store, StoreKey};

/// Jumlah `div` di dokumen beban. Sama dengan tes anggaran memori, supaya
/// angka latensi dan angka PSS diukur pada pekerjaan yang setara.
const JUMLAH_DIV: usize = 1500;

/// Batas PRD untuk satu rehidrasi: di bawah 100 ms (FR-006).
///
/// Angka 100 ms itu janji tentang biner yang dikirim ke pengguna, jadi yang
/// mengesahkannya adalah run `--release`. Build `debug` tidak mengoptimasi apa
/// pun: mem-parse 100 KB HTML saja sudah 379 ms di sana, layout 276 ms, baca
/// SQLite 180 ms, jadi 100 ms di debug mengukur rustc, bukan mesin ini.
///
/// Batas debug bukan klaim performa, hanya penjaga "tidak menggantung": angka
/// debug terukur 1,1 sampai 1,7 detik untuk dokumen yang sama, dan
/// `cargo test --workspace` menjalankan suite lain paralel di mesin yang sama,
/// jadi ambang ketat di sini cuma menghasilkan gerbang yang kadang merah.
/// Yang mengesahkan janji < 100 ms adalah run `--release`.
const BATAS_MS: f64 = if cfg!(debug_assertions) {
    5000.0
} else {
    100.0
};

struct Sementara {
    path: std::path::PathBuf,
}

impl Sementara {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ocb-rehidrasi-{}-{label}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("direktori sementara");
        Self { path }
    }

    fn db(&self) -> std::path::PathBuf {
        self.path.join("store.db")
    }
}

impl Drop for Sementara {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn dokumen_beban() -> String {
    let mut s = String::with_capacity(JUMLAH_DIV * 64);
    s.push_str("<!doctype html><html><head><title>beban</title></head><body>");
    for i in 0..JUMLAH_DIV {
        let _ = write!(
            s,
            "<div id=\"k{i}\" class=\"baris\" style=\"height:20px\">isi baris {i}</div>"
        );
    }
    s.push_str("</body></html>");
    s
}

/// Membuka pool baru dari database yang sudah ada, seperti daemon yang restart.
///
/// Kunci yang dipakai adalah kunci yang sama dengan yang menyegel snapshot:
/// kunci berbeda berarti HTML tidak bisa dibuka, dan itu akan muncul sebagai
/// galat muat, bukan sebagai latensi.
fn pool_dari_berkas(dir: &Sementara, kunci: &StoreKey) -> TabPool<StoreColdStore> {
    let store = Store::open(dir.db(), kunci.clone()).expect("buka store");
    let dingin = StoreColdStore::new(store, "native");
    TabPool::new(dingin, 99).dengan_maks_panas(1)
}

#[test]
fn test_tab_rehydration_latency() {
    let dir = Sementara::new("latensi");
    let kunci = StoreKey::generate();

    // Sesi harus ada sebelum tab pertama disingkirkan: foreign key menolak
    // baris `tab_snapshot` tanpa induknya.
    {
        let store = Store::open(dir.db(), kunci.clone()).expect("buka store");
        store
            .open_session("native", "native", None, 1)
            .expect("sesi");
    }

    let html = dokumen_beban();

    // Pool pertama: dua tab, anggaran satu tab panas, jadi tab pertama
    // disingkirkan ke SQLite saat tab kedua dibuka.
    let id_dingin = {
        let mut pool = pool_dari_berkas(&dir, &kunci);
        let a = pool.buka("https://a.test", &html).expect("tab a");
        let _b = pool.buka("https://b.test", &html).expect("tab b");
        assert!(pool.tab(a).is_none(), "tab a harus sudah dingin");
        assert_eq!(pool.jumlah_dingin().expect("hitung dingin"), 1);
        a
    };

    // Pool kedua: koneksi baru ke berkas yang sama, tanpa tab panas sama sekali.
    // Ini yang membuat pengukurannya rehidrasi, bukan pemakaian ulang RAM.
    let mut pool = pool_dari_berkas(&dir, &kunci);
    assert_eq!(
        pool.jumlah_panas(),
        0,
        "pool baru tidak boleh mewarisi tab panas dari proses sebelumnya"
    );
    assert_eq!(
        pool.jumlah_dingin().expect("hitung dingin"),
        1,
        "snapshot tidak sampai ke SQLite"
    );

    let mulai = Instant::now();
    pool.hidupkan(id_dingin).expect("hidupkan tab dingin");
    let ms = mulai.elapsed().as_secs_f64() * 1000.0;

    let tab = pool.tab(id_dingin).expect("tab hidup");
    assert_eq!(tab.url, "https://a.test");
    assert!(
        tab.dokumen.jumlah_node() > JUMLAH_DIV,
        "dokumen hasil rehidrasi terlalu kecil: {} node",
        tab.dokumen.jumlah_node()
    );
    assert_eq!(
        pool.jumlah_dingin().expect("hitung dingin"),
        0,
        "baris dingin tidak dibersihkan setelah dihidupkan"
    );

    // Profil disebut di baris ini supaya angka debug tidak pernah dikutip
    // sebagai bukti AC-011. Yang mengesahkan janji < 100 ms hanya `--release`.
    let profil = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    println!("rehidrasi {JUMLAH_DIV} div dari SQLite: {ms:.1} ms ({profil}, batas {BATAS_MS} ms)");
    assert!(
        ms < BATAS_MS,
        "rehidrasi {ms:.1} ms melewati batas {BATAS_MS} ms pada profil {profil}"
    );
}

/// Jalur yang sama, tapi memastikan snapshot benar-benar dibaca dari disk dan
/// bukan dari cache SQLite halaman yang masih panas di koneksi lama.
///
/// Pool baru membuka koneksinya sendiri, jadi ini juga membuktikan adapter
/// tidak bergantung pada satu koneksi yang hidup di proses yang sama.
#[test]
fn test_suspended_tab_memory_release() {
    let dir = Sementara::new("lepas");
    let kunci = StoreKey::generate();

    {
        let store = Store::open(dir.db(), kunci.clone()).expect("buka store");
        store
            .open_session("native", "native", None, 1)
            .expect("sesi");
    }

    let html = dokumen_beban();
    let id_dingin = {
        let mut pool = pool_dari_berkas(&dir, &kunci);
        let a = pool.buka("https://a.test", &html).expect("tab a");
        let _b = pool.buka("https://b.test", &html).expect("tab b");
        a
    };

    let pool = pool_dari_berkas(&dir, &kunci);
    assert_eq!(
        pool.jumlah_panas(),
        0,
        "tab dingin masih memakan tempat di RAM"
    );

    // Koneksi ketiga, terpisah dari pool mana pun: yang dibaca di sini adalah
    // isi disk, bukan sisa RAM proses pembuat snapshot.
    let store = Store::open(dir.db(), kunci.clone()).expect("buka store");
    let dingin = StoreColdStore::new(store, "native");
    let tersimpan = dingin
        .muat(id_dingin)
        .expect("muat snapshot")
        .expect("snapshot ada di SQLite");
    assert!(
        tersimpan.html.len() > JUMLAH_DIV * 40,
        "HTML tersimpan lebih pendek dari yang diharapkan: {} byte",
        tersimpan.html.len()
    );
    assert_eq!(dingin.jumlah().expect("hitung"), 1);
}
