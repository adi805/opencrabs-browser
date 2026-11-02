//! Menyimpan profil dan cookie terenkripsi yang harus bertahan antar restart.
//!
//! Rancangan lengkap ada di paket desain dokumen 02 §5-§8. Yang mengikat crate
//! ini:
//!
//! - **Nilai cookie tidak pernah disimpan sebagai plaintext.** Kunci dimuat
//!   lewat [`crypto::StoreKey::load`], dan tanpa kunci daemon menolak start.
//! - **Satu profil = satu identitas beku.** UA yang berubah membuat
//!   `cf_clearance` hangus, jadi rotasi identitas harus lewat profil baru.
//! - **Nama cookie boleh masuk log, nilainya tidak pernah.**
//!
//! Crate ini tidak tahu apa-apa soal CDP atau mesin browser: dia hanya
//! menyimpan dan mengambil. Sinkronisasi ke engine ada di `browserd`.

#![forbid(unsafe_code)]

use std::path::Path;

use rusqlite::Connection;

pub mod cookie;
pub mod crypto;
pub mod profile;
pub mod schema;

#[cfg(test)]
mod testutil;

pub use cookie::{Cookie, CookieError, MAX_COOKIE_BYTES, SaveReport};
pub use crypto::{KeyError, StoreKey};
pub use profile::{Profile, ProfileError, ProfileMeta};
pub use schema::{SchemaError, SweepReport};

/// Kegagalan membuka store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Key(#[from] KeyError),

    #[error(transparent)]
    Schema(#[from] SchemaError),
}

/// Pegangan database plus kunci enkripsinya.
///
/// Kunci disimpan di sini, bukan diambil tiap operasi, supaya tidak ada jalur
/// kode yang bisa "lupa" kunci lalu menulis plaintext sebagai kompromi.
pub struct Store {
    conn: Connection,
    key: StoreKey,
}

impl Store {
    /// Membuka database. Kunci yang salah tidak bisa dideteksi di sini (tidak
    /// ada checksum kunci), jadi kegagalannya muncul saat cookie pertama
    /// dibaca, sebagai [`CookieError::Decrypt`].
    pub fn open(db_path: impl AsRef<Path>, key: StoreKey) -> Result<Self, StoreError> {
        let conn = schema::open(db_path.as_ref())?;
        Ok(Self { conn, key })
    }

    /// Membuka database dengan kunci dari berkas. Berkas yang tidak ada
    /// menghasilkan [`KeyError::Missing`], bukan store baru tanpa kunci.
    pub fn open_with_key_file(
        db_path: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
    ) -> Result<Self, StoreError> {
        let key = StoreKey::load(key_path.as_ref())?;
        Self::open(db_path, key)
    }

    /// Menjalankan pembersihan retensi. Dipanggil saat boot.
    pub fn sweep(&self, now: i64) -> Result<SweepReport, StoreError> {
        Ok(schema::sweep(&self.conn, now)?)
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn key(&self) -> &StoreKey {
        &self.key
    }
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn cookie_simple(nama: &str, nilai: &str) -> Cookie {
        Cookie {
            host: "shop.example".to_owned(),
            name: nama.to_owned(),
            path: "/".to_owned(),
            value: nilai.to_owned(),
            expires: None,
            secure: true,
            http_only: true,
            samesite: Some(1),
        }
    }

    #[test]
    fn cookie_bertahan_setelah_store_dibuka_ulang() {
        let dir = TempDir::new("restart");
        let db = dir.path().join("store.db");
        let kunci_path = dir.path().join("store.key");
        let kunci = StoreKey::create(&kunci_path).expect("kunci");

        {
            let store = Store::open(&db, kunci).expect("buka pertama");
            store
                .upsert_profile("shop", &ProfileMeta::default(), 1000)
                .expect("profil");
            store
                .save_cookies(
                    "shop",
                    &[cookie_simple("cf_clearance", "tahan-restart")],
                    2000,
                )
                .expect("simpan");
        }

        let store = Store::open_with_key_file(&db, &kunci_path).expect("buka kedua");
        let cookie = store.cookies("shop", None, 2000).expect("baca");
        assert_eq!(cookie.len(), 1);
        assert_eq!(cookie[0].value, "tahan-restart");
    }

    #[test]
    fn kunci_berbeda_gagal_membuka_cookie_dengan_pesan_jelas() {
        let dir = TempDir::new("kunci-salah");
        let db = dir.path().join("store.db");

        {
            let store = Store::open(&db, StoreKey::generate()).expect("buka");
            store
                .upsert_profile("shop", &ProfileMeta::default(), 1000)
                .expect("profil");
            store
                .save_cookies("shop", &[cookie_simple("sesi", "rahasia")], 2000)
                .expect("simpan");
        }

        let store = Store::open(&db, StoreKey::generate()).expect("buka kunci lain");
        let err = store
            .cookies("shop", None, 2000)
            .expect_err("kunci lain tidak boleh bisa membaca");
        assert!(
            matches!(err, CookieError::Decrypt { .. }),
            "galat salah: {err}"
        );
        assert!(
            !err.to_string().contains("rahasia"),
            "pesan galat tidak boleh memuat nilai cookie"
        );
    }

    #[test]
    fn store_tanpa_berkas_kunci_ditolak() {
        let dir = TempDir::new("tanpa-kunci");
        let err =
            Store::open_with_key_file(dir.path().join("store.db"), dir.path().join("tidak.key"))
                .expect_err("harus menolak");
        assert!(
            matches!(err, StoreError::Key(KeyError::Missing(_))),
            "galat salah: {err}"
        );
    }
}
