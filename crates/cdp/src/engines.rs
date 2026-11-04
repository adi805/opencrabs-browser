//! Kumpulan mesin yang dilayani satu daemon.
//!
//! Selama v0.1 daemon hanya punya satu mesin, jadi "mesin" dan "daemon" bisa
//! dipakai bergantian tanpa salah. Multiplex mematahkan itu: satu koneksi klien
//! kini boleh memakai lebih dari satu mesin sekaligus, dan setiap pesan harus
//! tahu mesin mana pemiliknya.
//!
//! Karena itu id mesin di sini **bukan** [`engine::Engine::kind`]. `kind` cuma
//! menyebut jenisnya (`"shell"`), sedangkan dua mesin jenis sama tetap harus
//! dibedakan: dua chrome-headless-shell punya dua port dan dua proses, dan
//! mencampurnya berarti pesan satu tab mendarat di tab lain. Id diambil dari
//! nama bagian di config, mis. `chrome_shell` dan `chrome_shell2`.
//!
//! Id juga muncul di URL dan di id target yang dilihat klien
//! (`<mesin>~<id>`), jadi bentuknya dibatasi: huruf, angka, `-`, `_`.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use engine::{Engine, Slot};

/// Pemisah antara id mesin dan id milik mesin di `/json/list`.
///
/// `~` tidak termasuk karakter yang lolos di id Chrome (hex) maupun yang
/// dipakai penulis, jadi tidak bisa bertabrakan. Id mesin sendiri dilarang
/// memuat karakter ini.
pub const PEMISAH: char = '~';

/// Batas waktu boot yang dipakai jalur protokol kalau pemanggil tidak
/// menyebutkan angka lain.
pub const BOOT_TIMEOUT_MS: u64 = 20_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PoolError {
    #[error("tidak ada mesin yang dikonfigurasi")]
    Kosong,

    #[error("id mesin '{0}' tidak sah: hanya huruf, angka, '-' dan '_'")]
    IdTidakSah(String),

    #[error("mesin bawaan '{0}' tidak ada di daftar mesin")]
    BawaanTidakAda(String),
}

impl PoolError {
    /// Layak dijawab sebagai salah konfigurasi (400), bukan kegagalan server.
    #[must_use]
    pub const fn salah_konfigurasi(&self) -> bool {
        true
    }
}

/// Mesin-mesin yang bisa dipakai klien, plus satu yang jadi bawaan.
///
/// Yang disimpan adalah [`Slot`], bukan mesin yang sedang hidup: mesin boleh
/// tidur (di-reap karena menganggur) lalu dibangunkan lagi saat ada yang
/// memakainya, dan pool tetap menyebut id yang sama sepanjang daemon hidup.
/// Menyimpan `Arc<dyn Engine>` akan membekukan "mesin ini hidup selamanya"
/// dan itu yang membuat reap idle mustahil.
///
/// Urutannya `BTreeMap` supaya `/json/list` dan metrik keluar dengan urutan
/// yang sama setiap kali. Urutan yang berubah-ubah membuat dua tangkapan
/// metrik tidak bisa dibandingkan tanpa berpikir.
#[derive(Clone, Default)]
pub struct EnginePool {
    default: String,
    slots: BTreeMap<String, Arc<Slot>>,
    /// Batas tab per mesin, dari `[engines.<id>] max_tabs`.
    ///
    /// Disimpan di pool, bukan di [`Slot`], karena batas ini milik lapisan
    /// protokol (berapa tab yang boleh dibuka klien), bukan milik proses.
    /// Mesin tanpa entri di sini berarti **tanpa batas**, dan pemanggil yang
    /// benar-benar akan membuka tab tidak perlu menyentuh mesin sama sekali.
    /// Dibungkus `RwLock` **berbagi** antar klon pool: SIGHUP mengganti batas
    /// saat daemon melayani, dan semua jalur (proxy CDP, pemutus reap, kontrol)
    /// memegang klon pool yang berbeda. Nilai yang disalin per klon akan membuat
    /// batas baru hanya berlaku di jalur yang kebetulan memegang klon itu.
    batas: std::sync::Arc<std::sync::RwLock<BTreeMap<String, u32>>>,
}

impl std::fmt::Debug for EnginePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnginePool")
            .field("default", &self.default)
            .field("slots", &self.slots.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl EnginePool {
    /// Pool satu mesin dari mesin yang **sudah hidup**.
    ///
    /// Dipakai jalur lama dan test: resepnya diambil dari mesin itu sendiri,
    /// jadi kalau mesinnya nanti di-reap, slot ini tetap bisa
    /// menyalakannya kembali dengan argumen dan profil yang sama.
    #[must_use]
    pub fn single(engine: Arc<dyn Engine>) -> Self {
        let id = engine.kind().to_owned();
        let slot = Arc::new(Slot::siap(
            id.clone(),
            engine.kind().to_owned(),
            engine.spec().clone(),
            Arc::new(engine::PenggerakShell),
            engine,
            0,
        ));
        let mut slots = BTreeMap::new();
        slots.insert(id.clone(), slot);
        Self {
            default: id,
            slots,
            batas: std::sync::Arc::new(std::sync::RwLock::new(BTreeMap::new())),
        }
    }

    /// Pool dari mesin-mesin yang **sudah hidup**.
    ///
    /// Bentuk yang dipakai daemon saat boot: mesin sudah dinyalakan lebih dulu
    /// (lihat `browserd::daemon`), lalu diserahkan ke sini. Setiap mesin
    /// dibungkus [`Slot::siap`], jadi mulai sekarang pool boleh mematikannya
    /// dan menyalakannya lagi tanpa kehilangan id-nya.
    pub fn new(
        default: impl Into<String>,
        engines: BTreeMap<String, Arc<dyn Engine>>,
    ) -> Result<Self, PoolError> {
        let slots = engines
            .into_iter()
            .map(|(id, engine)| {
                let slot = Arc::new(Slot::siap(
                    id.clone(),
                    engine.kind().to_owned(),
                    engine.spec().clone(),
                    Arc::new(engine::PenggerakShell),
                    engine,
                    // Dicap "sekarang", bukan 0: nol berarti "terakhir dipakai
                    // 1970", sehingga mesin yang baru saja dinyalakan langsung
                    // dianggap menganggur berjam-jam dan di-reap pada putaran
                    // pertama. Mesin yang baru boot memang boleh di-reap, tapi
                    // setelah `idle_reap`, bukan seketika.
                    engine::slot::ms_sekarang(),
                ));
                (id, slot)
            })
            .collect::<BTreeMap<String, Arc<Slot>>>();
        Self::dari_slot(default, slots)
    }

    /// Pool dari slot, termasuk slot yang mesinnya sedang tidur.
    ///
    /// Ini bentuk yang dipakai kalau daemon tidak menyalakan mesin apa pun
    /// saat boot: mesin lahir saat klien pertama datang.
    pub fn dari_slot(
        default: impl Into<String>,
        slots: BTreeMap<String, Arc<Slot>>,
    ) -> Result<Self, PoolError> {
        if slots.is_empty() {
            return Err(PoolError::Kosong);
        }
        for id in slots.keys() {
            if !id_sah(id) {
                return Err(PoolError::IdTidakSah(id.clone()));
            }
        }
        let default = default.into();
        if !slots.contains_key(&default) {
            return Err(PoolError::BawaanTidakAda(default));
        }
        Ok(Self {
            default,
            slots,
            batas: std::sync::Arc::new(std::sync::RwLock::new(BTreeMap::new())),
        })
    }

    /// Memasang batas tab per mesin.
    ///
    /// Entri untuk id yang tidak ada diabaikan (bukan panic): pool sudah
    /// menolak konfigurasi yang menyebut mesin bawaan yang tidak ada, dan batas
    /// yang tidak punya mesin tidak bisa ditegakkan oleh siapa pun.
    #[must_use]
    pub fn dengan_batas_tab(self, batas: BTreeMap<String, u32>) -> Self {
        self.ganti_batas_tab(batas);
        self
    }

    /// Mengganti peta batas tab, dipakai saat boot dan saat config dimuat ulang.
    ///
    /// Entri diganti **seluruhnya**, bukan digabung: config baru yang menghapus
    /// `max_tabs` harus benar-benar berarti tanpa batas. Menggabung akan
    /// membuat batas lama hidup selamanya setelah dihapus dari berkas.
    ///
    /// Yang dikembalikan jumlah mesin yang batasnya benar-benar terpasang;
    /// entri untuk id yang tidak ada tetap diabaikan seperti di
    /// [`Self::dengan_batas_tab`].
    pub fn ganti_batas_tab(&self, batas: BTreeMap<String, u32>) -> usize {
        let baru: BTreeMap<String, u32> = batas
            .into_iter()
            .filter(|(id, _)| self.slots.contains_key(id))
            .collect();
        let jumlah = baru.len();
        let mut kunci = self.batas.write().unwrap_or_else(|err| err.into_inner());
        *kunci = baru;
        jumlah
    }

    /// Batas tab mesin ini, kalau operator memasangnya.
    #[must_use]
    pub fn batas_tab(&self, id: &str) -> Option<u32> {
        self.batas
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .get(id)
            .copied()
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Arc<Slot>> {
        self.slots.get(id)
    }

    /// Slot mesin bawaan.
    ///
    /// Dipakai endpoint discovery (`/json/version`) dan pesan yang tidak
    /// menyebut mesin sama sekali. Pool selalu punya minimal satu slot, jadi
    /// ini tidak bisa `None`.
    #[must_use]
    pub fn default_slot(&self) -> &Arc<Slot> {
        self.slots
            .get(&self.default)
            .expect("pool selalu punya slot bawaan; dibangun lewat baru/single")
    }

    #[must_use]
    pub fn default_id(&self) -> &str {
        &self.default
    }

    pub fn ids(&self) -> impl Iterator<Item = &String> {
        self.slots.keys()
    }

    pub fn slots(&self) -> impl Iterator<Item = (&String, &Arc<Slot>)> {
        self.slots.iter()
    }

    /// Semua slot dibangunkan bersamaan.
    ///
    /// `pastikan` mem-poll port dan berkas `DevToolsActivePort`, jadi setiap
    /// boot dijalankan di `spawn_blocking`: menjalankannya langsung di thread
    /// async akan menahan reactor selama belasan detik dan ikut menahan
    /// `/json/version` milik klien lain.
    ///
    /// Hasilnya memuat kegagalan per mesin, bukan `Result` tunggal: satu mesin
    /// yang tidak mau bangun tidak boleh menyembunyikan tab yang hidup di
    /// mesin lain.
    pub async fn bangun_semua(
        &self,
        timeout_ms: u64,
    ) -> Vec<(String, Result<Arc<dyn Engine>, String>)> {
        let sekarang = engine::slot::ms_sekarang();
        let mut tugas = Vec::with_capacity(self.slots.len());
        for (id, slot) in &self.slots {
            let id = id.clone();
            let slot = Arc::clone(slot);
            tugas.push(tokio::spawn(async move {
                let hasil =
                    tokio::task::spawn_blocking(move || slot.pastikan(timeout_ms, sekarang))
                        .await
                        .map_err(|err| format!("tugas boot gagal: {err}"))
                        .and_then(|hasil| hasil.map_err(|err| err.to_string()));
                (id, hasil)
            }));
        }
        let mut hasil = Vec::with_capacity(tugas.len());
        for tugas in tugas {
            match tugas.await {
                Ok(pair) => hasil.push(pair),
                Err(err) => tracing::warn!(%err, "tugas boot mesin dibatalkan"),
            }
        }
        hasil
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Lebih dari satu mesin berarti penerusan harus lewat multiplex. Satu
    /// mesin tetap memakai jalur pass-through lama yang sudah terbukti, karena
    /// jalur itu yang dipakai produksi hari ini.
    #[must_use]
    pub fn is_multi(&self) -> bool {
        self.slots.len() > 1
    }
}

/// Huruf, angka, `-`, `_`. Tidak ada spasi, garis miring, atau `~`.
#[must_use]
pub fn id_sah(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Memecah `<mesin>~<id>` jadi dua bagian. Id tanpa pemisah dianggap milik
/// mesin bawaan, sehingga URL lama (tanpa prefiks) tetap bekerja.
#[must_use]
pub fn pecah_id(teks: &str) -> (Option<&str>, &str) {
    match teks.split_once(PEMISAH) {
        // Sisa yang masih memuat `~` bukan id mesin: id Chrome selalu hex, jadi
        // bentuk seperti itu berarti pemanggil mengirim sesuatu yang bukan
        // bentuk kita, dan menebaknya sebagai prefiks lebih berbahaya daripada
        // menolaknya.
        Some((mesin, id)) if id_sah(mesin) && !id.is_empty() && !id.contains(PEMISAH) => {
            (Some(mesin), id)
        }
        _ => (None, teks),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testing::FakeEngine;

    /// Penggerak yang selalu gagal: uji pool tidak boleh menyalakan proses.
    struct TakBisaNyalakan;

    impl engine::slot::Penggerak for TakBisaNyalakan {
        fn nyalakan(
            &self,
            _spec: &engine::LaunchSpec,
            _timeout_ms: u64,
        ) -> Result<Arc<dyn Engine>, engine::EngineError> {
            Err(engine::EngineError::BinaryMissing(
                std::path::PathBuf::from("/tidak/ada"),
            ))
        }
    }

    fn mesin(port: u16) -> Arc<dyn Engine> {
        Arc::new(FakeEngine::at_port(port))
    }

    fn slot(id: &str, port: u16) -> Arc<Slot> {
        let engine = mesin(port);
        Arc::new(Slot::siap(
            id.to_owned(),
            engine.kind().to_owned(),
            engine.spec().clone(),
            Arc::new(TakBisaNyalakan),
            engine,
            1000,
        ))
    }

    fn peta(items: &[(&str, u16)]) -> BTreeMap<String, Arc<Slot>> {
        items
            .iter()
            .map(|(id, port)| ((*id).to_owned(), slot(id, *port)))
            .collect()
    }

    #[test]
    fn pool_satu_mesin_memakai_kind_sebagai_id() {
        let pool = EnginePool::single(mesin(41234));
        assert_eq!(pool.default_id(), "fake");
        assert_eq!(pool.len(), 1);
        assert!(!pool.is_multi());
        assert_eq!(
            pool.default_slot().info().expect("info").devtools_port,
            41234
        );
    }

    #[test]
    fn pool_dua_mesin_memilih_yang_benar() {
        let pool =
            EnginePool::dari_slot("a", peta(&[("a", 40001), ("b", 40002)])).expect("pool sah");
        assert!(pool.is_multi());
        assert_eq!(pool.len(), 2);
        assert_eq!(
            pool.get("a")
                .expect("ada")
                .info()
                .expect("info")
                .devtools_port,
            40001
        );
        assert_eq!(
            pool.get("b")
                .expect("ada")
                .info()
                .expect("info")
                .devtools_port,
            40002
        );
        assert!(pool.get("c").is_none());
        assert_eq!(
            pool.ids().cloned().collect::<Vec<_>>(),
            vec!["a".to_owned(), "b".to_owned()],
            "urutan harus stabil"
        );
    }

    // AC-010 pada lapisan pool: batas yang dipasang saat boot bisa diganti saat
    // daemon melayani, dan penggantian itu MENGHAPUS entri yang tidak ada di
    // peta baru. Menggabung akan membuat batas lama hidup selamanya setelah
    // operator menghapus `max_tabs` dari config.
    #[test]
    fn batas_tab_bisa_diganti_tanpa_membangun_ulang_pool() {
        let pool =
            EnginePool::dari_slot("a", peta(&[("a", 40001), ("b", 40002)])).expect("pool sah");
        pool.ganti_batas_tab(
            [("a".to_owned(), 6u32), ("b".to_owned(), 2u32)]
                .into_iter()
                .collect(),
        );
        assert_eq!(pool.batas_tab("a"), Some(6));
        assert_eq!(pool.batas_tab("b"), Some(2));

        // Peta baru tanpa entri `b`: `b` kembali tanpa batas, bukan tetap 2.
        let terpasang = pool.ganti_batas_tab([("a".to_owned(), 3u32)].into_iter().collect());
        assert_eq!(terpasang, 1);
        assert_eq!(pool.batas_tab("a"), Some(3));
        assert_eq!(pool.batas_tab("b"), None, "entri lama harus hilang");
    }

    // Klon pool adalah jalur yang berbeda (proxy CDP vs pemutus reap). Batas
    // harus ikut berubah di klon, kalau tidak batas baru hanya berlaku di jalur
    // yang kebetulan memegang klon aslinya.
    #[test]
    fn batas_tab_ikut_berubah_di_klon_pool() {
        let pool =
            EnginePool::dari_slot("a", peta(&[("a", 40001), ("b", 40002)])).expect("pool sah");
        let klon = pool.clone();
        pool.ganti_batas_tab([("b".to_owned(), 7u32)].into_iter().collect());
        assert_eq!(klon.batas_tab("b"), Some(7));
    }

    #[test]
    fn bawaan_yang_tidak_ada_ditolak() {
        let err = EnginePool::dari_slot("z", peta(&[("a", 40001)])).expect_err("harus ditolak");
        assert_eq!(err, PoolError::BawaanTidakAda("z".to_owned()));
        assert!(err.to_string().contains('z'));
    }

    #[test]
    fn daftar_kosong_ditolak() {
        assert_eq!(
            EnginePool::dari_slot("a", BTreeMap::new()).expect_err("harus ditolak"),
            PoolError::Kosong
        );
    }

    #[test]
    fn id_dengan_pemisah_atau_spasi_ditolak() {
        for buruk in ["a~b", "a b", "a/b", ""] {
            let err = EnginePool::dari_slot(buruk, peta(&[(buruk, 40001)]))
                .expect_err("id buruk harus ditolak");
            assert_eq!(err, PoolError::IdTidakSah(buruk.to_owned()));
        }
        for baik in ["chrome_shell", "chrome-shell2", "A1"] {
            assert!(
                EnginePool::dari_slot(baik, peta(&[(baik, 40001)])).is_ok(),
                "id '{baik}' seharusnya sah"
            );
        }
    }

    /// Penggerak yang mencatat berapa kali dipanggil dan menghasilkan mesin
    /// baru di port tetap. Dipakai membuktikan mesin yang tidur benar-benar
    /// dibangunkan, bukan cuma dilaporkan.
    struct PenggerakHitung {
        dipanggil: Arc<std::sync::atomic::AtomicU32>,
        port: u16,
    }

    impl engine::slot::Penggerak for PenggerakHitung {
        fn nyalakan(
            &self,
            _spec: &engine::LaunchSpec,
            _timeout_ms: u64,
        ) -> Result<Arc<dyn Engine>, engine::EngineError> {
            self.dipanggil
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Arc::new(FakeEngine::at_port(self.port)) as Arc<dyn Engine>)
        }
    }

    #[tokio::test]
    async fn slot_tidur_dibangunkan_sekali_saat_dibutuhkan() {
        let dipanggil = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mesin_awal: Arc<dyn Engine> = Arc::new(FakeEngine::at_port(40001));
        let slot = Arc::new(Slot::siap(
            "a".to_owned(),
            "fake".to_owned(),
            mesin_awal.spec().clone(),
            Arc::new(PenggerakHitung {
                dipanggil: Arc::clone(&dipanggil),
                port: 40001,
            }),
            Arc::clone(&mesin_awal),
            1000,
        ));
        let mut slots = BTreeMap::new();
        slots.insert("a".to_owned(), slot);
        let pool = EnginePool::dari_slot("a", slots).expect("pool sah");

        // Mesin ditidurkan seperti setelah reap: proses berhenti, resep tinggal.
        pool.default_slot().matikan().expect("berhenti");
        assert_eq!(
            pool.default_slot().keadaan(),
            engine::slot::Keadaan::Tidur,
            "setelah dimatikan slot harus berstatus tidur, bukan mati"
        );
        assert_eq!(dipanggil.load(std::sync::atomic::Ordering::SeqCst), 0);

        let hasil = pool.bangun_semua(1000).await;
        assert_eq!(hasil.len(), 1);
        assert!(hasil[0].1.is_ok(), "mesin tidur harus bangun");
        assert_eq!(
            dipanggil.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "boot kedua tidak boleh terjadi tanpa alasan"
        );

        // Panggilan kedua memakai mesin yang sudah hidup: tidak ada boot baru.
        let lagi = pool.bangun_semua(1000).await;
        assert!(lagi[0].1.is_ok());
        assert_eq!(
            dipanggil.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "mesin yang sudah hidup tidak boleh dinyalakan ulang"
        );
    }

    #[test]
    fn pecah_id_mengembalikan_mesin_dan_id() {
        assert_eq!(pecah_id("a~ABC123"), (Some("a"), "ABC123"));
        assert_eq!(
            pecah_id("a~b~c"),
            (None, "a~b~c"),
            "pemisah ganda bukan bentuk sah"
        );
        assert_eq!(pecah_id("ABC123"), (None, "ABC123"));
        assert_eq!(pecah_id("~kosong"), (None, "~kosong"));
    }
}
