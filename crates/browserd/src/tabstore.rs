//! Jembatan antara pool tab di `engine` dan penyimpanan dingin di `store`.
//!
//! Dua crate itu sengaja tidak saling mengenal: `engine` mendefinisikan trait
//! [`ColdStore`], `store` menyediakan tabel `tab_snapshot`. Perkawinannya
//! terjadi di sini, di lapisan yang memang melihat keduanya.
//!
//! Yang dijaga adapter ini:
//!
//! - **Satu sesi, satu ruang tab.** Setiap operasi membawa `session_id`, jadi
//!   tab milik sesi lain tidak bisa termuat ke sesi ini walaupun nomornya sama.
//! - **HTML disegel sebelum menyentuh disk.** Penyegelan dilakukan di `store`,
//!   bukan di sini, supaya tidak ada jalur tulis yang bisa melewatinya.
//! - **Satu koneksi per pool, bukan satu koneksi bersama.** `Store` memegang
//!   koneksi SQLite yang `Send` tapi tidak `Sync`, jadi `Arc<Store>` tidak bisa
//!   dipakai sebagai state axum. Adapter ini karena itu membuka koneksinya
//!   sendiri dari berkas database dan kunci, satu per pool; SQLite memang
//!   dirancang untuk beberapa koneksi ke berkas yang sama.
//! - **Galat penyimpanan tidak pernah ditelan.** Pool yang mengira tab aman
//!   padahal gagal disimpan akan kehilangan tab tanpa jejak, dan itu lebih
//!   buruk daripada gagal terang-terangan.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine::native::tab_pool::{ColdStore, Snapshot, TabId};
use store::{Store, StoreKey, TabSnapshot};

/// Penyimpanan dingin yang ditopang SQLite.
///
/// `Store` dipegang sendiri, bukan lewat `Rc` bersama: pool tab hidup di dalam
/// state axum yang menuntut `Send`, sedangkan `Store` hanya `Send` dan bukan
/// `Sync`, jadi `Arc<Store>` tidak bisa dipakai. Membuka koneksi per pool
/// menghindari keduanya, dan SQLite memang dirancang untuk beberapa koneksi ke
/// berkas yang sama.
pub struct StoreColdStore {
    /// Hasil pembukaan disimpan, bukan di-`unwrap`: kegagalan membuka database
    /// harus muncul sebagai galat simpan/muat yang menyebut sebabnya, bukan
    /// sebagai panic di jalur penyingkiran tab.
    store: Result<Store, String>,
    session_id: String,
}

impl StoreColdStore {
    #[must_use]
    pub fn new(store: Store, session_id: impl Into<String>) -> Self {
        Self {
            store: Ok(store),
            session_id: session_id.into(),
        }
    }

    /// Membuka sendiri dari berkas database dan kunci.
    ///
    /// Dipakai pabrik daemon: tiap mesin native mendapat penyimpanan sendiri
    /// tanpa harus berbagi satu koneksi yang tidak `Sync`.
    #[must_use]
    pub fn dari_berkas(db: &Path, key: StoreKey, session_id: impl Into<String>) -> Self {
        Self {
            store: Store::open(db, key).map_err(|err| format!("buka {}: {err}", db.display())),
            session_id: session_id.into(),
        }
    }

    fn koneksi(&self) -> Result<&Store, String> {
        self.store.as_ref().map_err(Clone::clone)
    }
}

/// Berkas tempat penyimpanan dingin hidup: satu database dan satu kunci.
///
/// Dipisah dari pabriknya karena satu daemon bisa menyalakan lebih dari satu
/// mesin native, dan tiap mesin butuh `session_id` sendiri: nomor tab mulai
/// dari 1 di tiap pool, jadi dua mesin yang berbagi sesi akan saling menimpa
/// baris `tab_snapshot` dan memulihkan dokumen milik mesin lain.
#[derive(Clone, Debug)]
pub struct SumberDingin {
    db: PathBuf,
    kunci: PathBuf,
}

impl SumberDingin {
    #[must_use]
    pub fn new(db: impl Into<PathBuf>, kunci: impl Into<PathBuf>) -> Self {
        Self {
            db: db.into(),
            kunci: kunci.into(),
        }
    }

    /// Pabrik penyimpanan dingin untuk satu mesin.
    ///
    /// Kunci dibaca di sini, sekali per mesin; koneksinya dibuka per
    /// pemanggilan pabrik, yaitu sekali per pool. Kegagalan membaca kunci tidak
    /// bisa dilaporkan dari sini karena pabrik tidak mengembalikan `Result`,
    /// jadi galatnya ikut tersimpan di adapter dan muncul saat tab pertama
    /// disingkirkan.
    ///
    /// `session_id` sebaiknya id mesin: itulah yang membuat ruang tab satu
    /// mesin tidak tercampur dengan mesin lain.
    #[must_use]
    pub fn pabrik(&self, session_id: impl Into<String>) -> cdp::native_wire::PabrikDingin {
        let sesi = session_id.into();
        let db = self.db.clone();
        let kunci = self.kunci.clone();
        let kunci_terbaca = StoreKey::load(&kunci)
            .map_err(|err| format!("kunci store tidak terbaca ({}): {err}", kunci.display()));

        Arc::new(move || {
            let adapter = match &kunci_terbaca {
                Ok(key) => StoreColdStore::dari_berkas(&db, key.clone(), sesi.clone()),
                Err(pesan) => StoreColdStore {
                    store: Err(pesan.clone()),
                    session_id: sesi.clone(),
                },
            };
            Box::new(adapter) as Box<dyn cdp::native_wire::ColdStoreObj>
        })
    }
}

/// Tab id di pool adalah `u64`; kolom di SQLite `INTEGER` bertanda. Nilai di
/// atas `i64::MAX` tidak mungkin berasal dari pool sungguhan, tapi kalau
/// sampai terjadi, menyimpannya sebagai negatif akan menabrak baris lain.
fn ke_i64(id: TabId) -> Result<i64, String> {
    i64::try_from(id.0).map_err(|_| format!("tab id {} di luar jangkauan i64", id.0))
}

impl ColdStore for StoreColdStore {
    type Error = String;

    fn simpan(&mut self, snapshot: &Snapshot) -> Result<(), Self::Error> {
        let tab_id = ke_i64(snapshot.id)?;
        let baris = TabSnapshot {
            tab_id,
            url: snapshot.url.clone(),
            title: snapshot.judul.clone(),
            html: snapshot.html.clone(),
            node_count: snapshot.jumlah_node,
            evicted_at: i64::try_from(snapshot.disingkirkan_pada).unwrap_or(i64::MAX),
        };
        self.koneksi()?
            .save_tab_snapshot(&self.session_id, &baris)
            .map_err(|e| format!("menyimpan tab {tab_id}: {e}"))
    }

    fn muat(&self, id: TabId) -> Result<Option<Snapshot>, Self::Error> {
        let tab_id = ke_i64(id)?;
        let baris = self
            .koneksi()?
            .load_tab_snapshot(&self.session_id, tab_id)
            .map_err(|e| format!("memuat tab {tab_id}: {e}"))?;

        Ok(baris.map(|b| Snapshot {
            id,
            url: b.url,
            judul: b.title,
            html: b.html,
            jumlah_node: b.node_count,
            disingkirkan_pada: u64::try_from(b.evicted_at).unwrap_or(0),
        }))
    }

    fn hapus(&mut self, id: TabId) -> Result<(), Self::Error> {
        let tab_id = ke_i64(id)?;
        self.koneksi()?
            .delete_tab_snapshot(&self.session_id, tab_id)
            .map_err(|e| format!("menghapus tab {tab_id}: {e}"))
    }

    fn jumlah(&self) -> Result<usize, Self::Error> {
        self.koneksi()?
            .count_tab_snapshots(&self.session_id)
            .map_err(|e| format!("menghitung tab dingin: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::native::tab_pool::{MemoryColdStore, TabPool};
    use store::StoreKey;

    struct Sementara {
        path: std::path::PathBuf,
        /// Satu kunci per direktori uji: adapter membuka koneksinya sendiri, dan
        /// kunci yang berbeda per koneksi berarti HTML yang disegel tidak bisa
        /// dibuka lagi.
        kunci: StoreKey,
    }

    impl Sementara {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ocb-adapter-{}-{label}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).expect("direktori sementara");
            Self {
                path,
                kunci: StoreKey::generate(),
            }
        }
    }

    impl Drop for Sementara {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Buka database tanpa menyentuh tabel `session`. Dipakai saat tes sudah
    /// mendaftarkan sesinya sendiri: `open_session` menolak id yang sudah ada,
    /// dan dua pool dari database yang sama memang harus berbagi satu sesi.
    fn buka(dir: &Sementara) -> Store {
        Store::open(dir.path.join("store.db"), dir.kunci.clone()).expect("buka store")
    }

    fn store(dir: &Sementara, sesi: &str) -> Store {
        let store = buka(dir);
        store.open_session(sesi, "native", None, 1).expect("sesi");
        store
    }

    fn halaman(isi: &str) -> String {
        format!(
            "<!doctype html><html><head><title>t</title></head><body><p>{isi}</p></body></html>"
        )
    }

    /// Inti dari adapter ini: tab yang disingkirkan pool benar-benar mendarat
    /// di SQLite, dan bisa dihidupkan kembali setelah pool dibuang.
    #[test]
    fn tab_dingin_bertahan_setelah_pool_dibuang() {
        let dir = Sementara::new("bertahan");
        // Baris `session` harus ada sebelum pool pertama menyingkirkan tab:
        // foreign key menolaknya kalau tidak, dan galatnya benar.
        store(&dir, "s1");

        let id = {
            let dingin = StoreColdStore::new(buka(&dir), "s1");
            let mut pool = TabPool::new(dingin, 99).dengan_maks_panas(1);
            let a = pool.buka("https://a.test", &halaman("isi-a")).unwrap();
            let _b = pool.buka("https://b.test", &halaman("isi-b")).unwrap();
            assert!(pool.tab(a).is_none(), "tab a harus sudah dingin");
            assert_eq!(pool.jumlah_dingin().unwrap(), 1);
            a
        };

        // Pool baru dari database yang sama, seperti daemon yang restart.
        let dingin = StoreColdStore::new(buka(&dir), "s1");
        let mut pool = TabPool::new(dingin, 99).dengan_maks_panas(2);
        assert_eq!(
            pool.jumlah_dingin().unwrap(),
            1,
            "tab dingin hilang antar restart"
        );
        pool.hidupkan(id).expect("hidupkan");
        let tab = pool.tab(id).expect("tab hidup");
        assert_eq!(tab.url, "https://a.test");
        assert_eq!(
            pool.jumlah_dingin().unwrap(),
            0,
            "baris dingin tidak dibersihkan"
        );
    }

    #[test]
    fn sesi_lain_tidak_bisa_memuat_tab_ini() {
        let dir = Sementara::new("sesi-lain");
        // Namanya bukan `store`: mengikat hasilnya ke `store` akan menutupi
        // fungsi pembantu bernama sama, dan panggilan berikutnya berubah jadi
        // "expected function, found `Store`".
        let koneksi = store(&dir, "s1");
        koneksi
            .open_session("s2", "native", None, 1)
            .expect("sesi kedua");

        let mut a = StoreColdStore::new(buka(&dir), "s1");
        a.simpan(&Snapshot {
            id: TabId(3),
            url: "https://a.test".into(),
            judul: None,
            html: halaman("x"),
            jumlah_node: 5,
            disingkirkan_pada: 1,
        })
        .expect("simpan");

        let b = StoreColdStore::new(buka(&dir), "s2");
        assert!(
            b.muat(TabId(3)).expect("muat").is_none(),
            "tab bocor antar sesi"
        );
        assert_eq!(b.jumlah().expect("hitung"), 0);
        assert_eq!(a.jumlah().expect("hitung"), 1);
    }

    #[test]
    fn galat_simpan_dilaporkan_bukan_ditelan() {
        let dir = Sementara::new("tanpa-sesi");
        // Tanpa baris `session`, foreign key menolak. Adapter harus
        // mengembalikan galat, bukan diam-diam menganggap tab tersimpan.
        let store = Store::open(dir.path.join("store.db"), dir.kunci.clone()).expect("buka");
        let mut dingin = StoreColdStore::new(store, "tidak-ada");
        let err = dingin
            .simpan(&Snapshot {
                id: TabId(1),
                url: "https://a.test".into(),
                judul: None,
                html: halaman("x"),
                jumlah_node: 5,
                disingkirkan_pada: 1,
            })
            .expect_err("harus galat");
        assert!(err.contains("menyimpan tab 1"), "pesan galat: {err}");
    }

    #[test]
    fn memory_store_tetap_bisa_dipakai_untuk_tes() {
        // Pool generik: implementasi memori tetap sah untuk tes yang tidak
        // butuh daya tahan.
        let mut pool = TabPool::new(MemoryColdStore::default(), 4).dengan_maks_panas(1);
        let a = pool.buka("https://a.test", &halaman("a")).unwrap();
        let _b = pool.buka("https://b.test", &halaman("b")).unwrap();
        pool.hidupkan(a).expect("hidupkan");
        assert_eq!(pool.jumlah_panas(), 1);
    }
}
