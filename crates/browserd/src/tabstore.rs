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
use std::sync::atomic::{AtomicBool, Ordering};

use engine::native::LembarLuar;
use engine::native::tab_pool::{ColdStore, Snapshot, TabId};
use store::{Store, StoreKey, TabSheet, TabSnapshot};

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
    /// Sesi dibuka sekali, bukan tiap penyingkiran: `simpan` dipanggil setiap
    /// kali tab panas penuh, dan mencoba INSERT yang pasti ditolak di setiap
    /// penyingkiran hanya menambah kerja tanpa mengubah apa pun.
    sesi_siap: AtomicBool,
}

impl StoreColdStore {
    #[must_use]
    pub fn new(store: Store, session_id: impl Into<String>) -> Self {
        Self {
            store: Ok(store),
            session_id: session_id.into(),
            sesi_siap: AtomicBool::new(false),
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
            sesi_siap: AtomicBool::new(false),
        }
    }

    fn koneksi(&self) -> Result<&Store, String> {
        self.store.as_ref().map_err(Clone::clone)
    }

    /// Memastikan baris `session` ada sebelum snapshot pertama ditulis.
    ///
    /// `tab_snapshot` mereferensikan `session`, dan foreign key menolak snapshot
    /// yatim. Adapter inilah satu-satunya pihak yang butuh baris itu, jadi
    /// adapter yang membukanya: kalau kewajiban ini ditaruh di pemanggil, daemon
    /// baru bisa lupa, dan memang lupa. Tab ke-26 di daemon 9236 ditolak
    /// `FOREIGN KEY constraint failed` pada 2026-11-13, dan klien menerimanya
    /// sebagai `-32000` di `Target.createTarget`, bukan sebagai galat store.
    ///
    /// Sesi yang sudah ada bukan kegagalan: daemon bisa memakai sesi yang sama
    /// setelah restart, dan tes yang sudah mendaftarkan sesinya sendiri tetap
    /// sah.
    fn pastikan_sesi(&self) -> Result<(), String> {
        if self.sesi_siap.load(Ordering::Relaxed) {
            return Ok(());
        }
        let sekarang = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        )
        .unwrap_or(i64::MAX);
        match self
            .koneksi()?
            .open_session(&self.session_id, "native", None, sekarang)
        {
            Ok(()) | Err(store::SessionError::SudahAda(_)) => {
                self.sesi_siap.store(true, Ordering::Relaxed);
                Ok(())
            }
            Err(err) => Err(format!("membuka sesi {}: {err}", self.session_id)),
        }
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
                    sesi_siap: AtomicBool::new(false),
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
        self.pastikan_sesi()?;
        let tab_id = ke_i64(snapshot.id)?;
        let baris = TabSnapshot {
            tab_id,
            url: snapshot.url.clone(),
            title: snapshot.judul.clone(),
            html: snapshot.html.clone(),
            node_count: snapshot.jumlah_node,
            evicted_at: i64::try_from(snapshot.disingkirkan_pada).unwrap_or(i64::MAX),
            // Lembar gaya ikut disimpan: tab yang dihidupkan kembali tanpa
            // CSS-nya akan terukur dan terlukis berbeda padahal HTML-nya sama.
            sheets: snapshot
                .lembar
                .iter()
                .map(|l| TabSheet {
                    href: l.href.clone(),
                    css: l.teks.clone(),
                })
                .collect(),
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
            lembar: b
                .sheets
                .into_iter()
                .map(|s| LembarLuar::baru(s.href, s.css))
                .collect(),
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
    fn tab_dingin_tersimpan_walau_tidak_ada_yang_membuka_sesi_dulu() {
        // Jalur daemon: `launch_all` menyalakan mesin native dan menyerahkan
        // adapter ini tanpa pernah membuka baris `session`. Sebelum perbaikan,
        // penyingkiran pertama mengembalikan `menyimpan tab 1: database:
        // FOREIGN KEY constraint failed`, dan daemon melaporkannya ke klien
        // sebagai `-32000` di `Target.createTarget` saat tab ke-26 dibuka.
        let dir = Sementara::new("tanpa-sesi");
        let dingin = StoreColdStore::new(buka(&dir), "mesin-uji");
        let mut pool = TabPool::new(dingin, 99).dengan_maks_panas(1);

        let a = pool.buka("https://a.test", &halaman("isi-a")).unwrap();
        let _b = pool.buka("https://b.test", &halaman("isi-b")).unwrap();

        assert!(pool.tab(a).is_none(), "tab a harus sudah dingin");
        assert_eq!(
            pool.jumlah_dingin().expect("hitung dingin"),
            1,
            "snapshot tidak mendarat di SQLite"
        );
    }

    /// Lembar gaya harus ikut pulang dari tab dingin. Kalau kolom `sheets_enc`
    /// lupa dibaca, tab yang dihidupkan kembali punya HTML yang sama persis
    /// tapi terukur dan terlukis berbeda, dan itu tidak kelihatan sampai ada
    /// tes yang membandingkan isinya.
    #[test]
    fn lembar_gaya_ikut_pulang_dari_tab_dingin() {
        let dir = Sementara::new("lembar");
        let lembar = vec![
            LembarLuar::baru("/a.css", "div { height: 11px }"),
            LembarLuar::baru("/b.css", "p { color: #00ff00 }"),
        ];
        {
            let mut a = StoreColdStore::new(buka(&dir), "s1");
            a.simpan(&Snapshot {
                id: TabId(7),
                url: "https://a.test".into(),
                judul: Some("a".into()),
                html: halaman("x"),
                jumlah_node: 5,
                disingkirkan_pada: 9,
                lembar: lembar.clone(),
            })
            .expect("simpan");
        }

        // Koneksi baru: lembar harus datang dari SQLite, bukan dari objek
        // adapter yang kebetulan masih hidup di memori.
        let b = StoreColdStore::new(buka(&dir), "s1");
        let snap = b.muat(TabId(7)).expect("muat").expect("baris ada");
        assert_eq!(snap.lembar, lembar, "lembar tidak utuh setelah bolak-balik");
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
            lembar: Vec::new(),
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
        // Galat tetap harus terang, tapi sebabnya sekarang harus galat store
        // yang sungguhan, bukan sesi yang belum dibuka: sesi dibuka adapter
        // sendiri (lihat `tab_dingin_tersimpan_walau_tidak_ada_yang_membuka_sesi_dulu`).
        // Kunci yang tidak ada adalah kegagalan produksi yang nyata: pabrik
        // daemon menyimpannya sebagai galat dan baru melaporkannya saat tab
        // pertama disingkirkan.
        let dir = Sementara::new("kunci-hilang");
        let sumber = SumberDingin::new(dir.path.join("store.db"), dir.path.join("tidak-ada.key"));
        let pabrik = sumber.pabrik("mesin-uji");
        let mut dingin = pabrik();

        let err = dingin
            .simpan(&Snapshot {
                id: TabId(1),
                url: "https://a.test".into(),
                judul: None,
                html: halaman("x"),
                jumlah_node: 5,
                lembar: Vec::new(),
                disingkirkan_pada: 1,
            })
            .expect_err("kunci hilang harus jadi galat, bukan tab yang dikira aman");
        assert!(
            err.contains("kunci store tidak terbaca"),
            "pesan galat harus menyebut sebabnya: {err}"
        );
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
