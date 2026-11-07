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
//! - **Kepemilikan memakai `Rc`, bukan `Arc`.** `Store` memegang koneksi
//!   SQLite yang tidak `Sync`, jadi `Arc<Store>` hanya akan lolos kalau
//!   ke-Send-annya dibohongi. Pool tab juga tidak `Send` sejak awal (setiap tab
//!   punya `rquickjs::Runtime` milik satu thread), jadi satu thread memang
//!   batasnya, bukan kekurangan.
//! - **Galat penyimpanan tidak pernah ditelan.** Pool yang mengira tab aman
//!   padahal gagal disimpan akan kehilangan tab tanpa jejak, dan itu lebih
//!   buruk daripada gagal terang-terangan.

use std::rc::Rc;

use engine::native::tab_pool::{ColdStore, Snapshot, TabId};
use store::{Store, TabSnapshot};

/// Penyimpanan dingin yang ditopang SQLite.
pub struct StoreColdStore {
    store: Rc<Store>,
    session_id: String,
}

impl StoreColdStore {
    #[must_use]
    pub fn new(store: Rc<Store>, session_id: impl Into<String>) -> Self {
        Self {
            store,
            session_id: session_id.into(),
        }
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
        self.store
            .save_tab_snapshot(&self.session_id, &baris)
            .map_err(|e| format!("menyimpan tab {tab_id}: {e}"))
    }

    fn muat(&self, id: TabId) -> Result<Option<Snapshot>, Self::Error> {
        let tab_id = ke_i64(id)?;
        let baris = self
            .store
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
        self.store
            .delete_tab_snapshot(&self.session_id, tab_id)
            .map_err(|e| format!("menghapus tab {tab_id}: {e}"))
    }

    fn jumlah(&self) -> Result<usize, Self::Error> {
        self.store
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
            Self { path }
        }
    }

    impl Drop for Sementara {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn store(dir: &Sementara, sesi: &str) -> Rc<Store> {
        let store = Rc::new(
            Store::open(dir.path.join("store.db"), StoreKey::generate()).expect("buka store"),
        );
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
        let store = store(&dir, "s1");

        let id = {
            let dingin = StoreColdStore::new(Rc::clone(&store), "s1");
            let mut pool = TabPool::new(dingin, 99).dengan_maks_panas(1);
            let a = pool.buka("https://a.test", &halaman("isi-a")).unwrap();
            let _b = pool.buka("https://b.test", &halaman("isi-b")).unwrap();
            assert!(pool.tab(a).is_none(), "tab a harus sudah dingin");
            assert_eq!(pool.jumlah_dingin().unwrap(), 1);
            a
        };

        // Pool baru dari database yang sama, seperti daemon yang restart.
        let dingin = StoreColdStore::new(Rc::clone(&store), "s1");
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
        let store = store(&dir, "s1");
        store
            .open_session("s2", "native", None, 1)
            .expect("sesi kedua");

        let mut a = StoreColdStore::new(Rc::clone(&store), "s1");
        a.simpan(&Snapshot {
            id: TabId(3),
            url: "https://a.test".into(),
            judul: None,
            html: halaman("x"),
            jumlah_node: 5,
            disingkirkan_pada: 1,
        })
        .expect("simpan");

        let b = StoreColdStore::new(Rc::clone(&store), "s2");
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
        let store =
            Rc::new(Store::open(dir.path.join("store.db"), StoreKey::generate()).expect("buka"));
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
