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

use engine::Engine;

/// Pemisah antara id mesin dan id milik mesin di `/json/list`.
///
/// `~` tidak termasuk karakter yang lolos di id Chrome (hex) maupun yang
/// dipakai penulis, jadi tidak bisa bertabrakan. Id mesin sendiri dilarang
/// memuat karakter ini.
pub const PEMISAH: char = '~';

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
/// Urutannya `BTreeMap` supaya `/json/list` dan metrik keluar dengan urutan
/// yang sama setiap kali. Urutan yang berubah-ubah membuat dua tangkapan
/// metrik tidak bisa dibandingkan tanpa berpikir.
#[derive(Clone, Default)]
pub struct EnginePool {
    default: String,
    engines: BTreeMap<String, Arc<dyn Engine>>,
}

impl std::fmt::Debug for EnginePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnginePool")
            .field("default", &self.default)
            .field("engines", &self.engines.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl EnginePool {
    /// Pool satu mesin. Id-nya diambil dari `kind`, dan untuk satu mesin itu
    /// sudah cukup: tidak ada mesin lain yang bisa tertukar dengannya.
    #[must_use]
    pub fn single(engine: Arc<dyn Engine>) -> Self {
        let id = engine.kind().to_owned();
        let mut engines = BTreeMap::new();
        engines.insert(id.clone(), engine);
        Self {
            default: id,
            engines,
        }
    }

    /// Pool banyak mesin dengan satu mesin bawaan.
    ///
    /// Semua id divalidasi di sini, bukan saat dipakai: id yang salah baru
    /// terlihat sebagai URL aneh di `/json/list` beberapa lapis kemudian, dan
    /// menelusurinya dari sana jauh lebih mahal daripada menolak di pintu.
    pub fn new(
        default: impl Into<String>,
        engines: BTreeMap<String, Arc<dyn Engine>>,
    ) -> Result<Self, PoolError> {
        if engines.is_empty() {
            return Err(PoolError::Kosong);
        }
        for id in engines.keys() {
            if !id_sah(id) {
                return Err(PoolError::IdTidakSah(id.clone()));
            }
        }
        let default = default.into();
        if !engines.contains_key(&default) {
            return Err(PoolError::BawaanTidakAda(default));
        }
        Ok(Self { default, engines })
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Arc<dyn Engine>> {
        self.engines.get(id)
    }

    /// Mesin bawaan.
    ///
    /// Dipakai endpoint discovery (`/json/version`) dan pesan yang tidak
    /// menyebut mesin sama sekali. Pool selalu punya minimal satu mesin, jadi
    /// ini tidak bisa `None`.
    #[must_use]
    pub fn default_engine(&self) -> &Arc<dyn Engine> {
        self.engines
            .get(&self.default)
            .expect("pool selalu punya mesin bawaan; dibangun lewat new/single")
    }

    #[must_use]
    pub fn default_id(&self) -> &str {
        &self.default
    }

    pub fn ids(&self) -> impl Iterator<Item = &String> {
        self.engines.keys()
    }

    pub fn engines(&self) -> impl Iterator<Item = (&String, &Arc<dyn Engine>)> {
        self.engines.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.engines.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.engines.is_empty()
    }

    /// Lebih dari satu mesin berarti penerusan harus lewat multiplex. Satu
    /// mesin tetap memakai jalur pass-through lama yang sudah terbukti, karena
    /// jalur itu yang dipakai produksi hari ini.
    #[must_use]
    pub fn is_multi(&self) -> bool {
        self.engines.len() > 1
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

    fn mesin(port: u16) -> Arc<dyn Engine> {
        Arc::new(FakeEngine::at_port(port))
    }

    fn peta(items: &[(&str, u16)]) -> BTreeMap<String, Arc<dyn Engine>> {
        items
            .iter()
            .map(|(id, port)| ((*id).to_owned(), mesin(*port)))
            .collect()
    }

    #[test]
    fn pool_satu_mesin_memakai_kind_sebagai_id() {
        let pool = EnginePool::single(mesin(41234));
        assert_eq!(pool.default_id(), "fake");
        assert_eq!(pool.len(), 1);
        assert!(!pool.is_multi());
        assert_eq!(pool.default_engine().info().devtools_port, 41234);
    }

    #[test]
    fn pool_dua_mesin_memilih_yang_benar() {
        let pool = EnginePool::new("a", peta(&[("a", 40001), ("b", 40002)])).expect("pool sah");
        assert!(pool.is_multi());
        assert_eq!(pool.len(), 2);
        assert_eq!(pool.get("a").expect("ada").info().devtools_port, 40001);
        assert_eq!(pool.get("b").expect("ada").info().devtools_port, 40002);
        assert!(pool.get("c").is_none());
        assert_eq!(
            pool.ids().cloned().collect::<Vec<_>>(),
            vec!["a".to_owned(), "b".to_owned()],
            "urutan harus stabil"
        );
    }

    #[test]
    fn bawaan_yang_tidak_ada_ditolak() {
        let err = EnginePool::new("z", peta(&[("a", 40001)])).expect_err("harus ditolak");
        assert_eq!(err, PoolError::BawaanTidakAda("z".to_owned()));
        assert!(err.to_string().contains('z'));
    }

    #[test]
    fn daftar_kosong_ditolak() {
        assert_eq!(
            EnginePool::new("a", BTreeMap::new()).expect_err("harus ditolak"),
            PoolError::Kosong
        );
    }

    #[test]
    fn id_dengan_pemisah_atau_spasi_ditolak() {
        for buruk in ["a~b", "a b", "a/b", ""] {
            let err = EnginePool::new(buruk, peta(&[(buruk, 40001)]))
                .expect_err("id buruk harus ditolak");
            assert_eq!(err, PoolError::IdTidakSah(buruk.to_owned()));
        }
        for baik in ["chrome_shell", "chrome-shell2", "A1"] {
            assert!(
                EnginePool::new(baik, peta(&[(baik, 40001)])).is_ok(),
                "id '{baik}' seharusnya sah"
            );
        }
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
