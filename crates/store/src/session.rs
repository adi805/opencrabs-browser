//! Siklus hidup sesi browser di dalam store.
//!
//! Sesi adalah induk dari cookie per profil dan dari tab dingin. Baris ini
//! harus ada **sebelum** tab pertama disingkirkan, karena `tab_snapshot`
//! mereferensikannya dan foreign key menolak snapshot yatim.
//!
//! Yang sengaja tidak dilakukan di sini: menebak `state` dari kejadian luar.
//! Sesi hanya berpindah state lewat pemanggilan eksplisit, supaya tidak ada
//! baris yang tampak "open" hanya karena tidak ada yang menutupnya.

use rusqlite::params;

use crate::Store;

/// Kegagalan membuka atau menutup sesi.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("sesi {0} sudah ada")]
    SudahAda(String),
}

impl Store {
    /// Membuka sesi baru. `engine` adalah nama mesin yang menjalankannya
    /// (`native`, `chrome`, ...), bukan nama biner.
    pub fn open_session(
        &self,
        id: &str,
        engine: &str,
        url: Option<&str>,
        now: i64,
    ) -> Result<(), SessionError> {
        let hasil = self.conn().execute(
            "INSERT INTO session (id, engine, url, state, opened_at, last_used_at)
             VALUES (?1, ?2, ?3, 'open', ?4, ?4)",
            params![id, engine, url, now],
        );
        match hasil {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(SessionError::SudahAda(id.to_owned()))
            }
            Err(e) => Err(SessionError::Sqlite(e)),
        }
    }

    /// Menutup sesi. Idempoten: menutup sesi yang sudah tertutup tidak
    /// menghasilkan galat, hanya tidak mengubah apa pun.
    pub fn close_session(&self, id: &str, reason: &str, now: i64) -> Result<(), SessionError> {
        self.conn().execute(
            "UPDATE session SET state = 'closed', closed_reason = ?2, last_used_at = ?3
             WHERE id = ?1 AND state != 'closed'",
            params![id, reason, now],
        )?;
        Ok(())
    }

    /// Mencatat bahwa sesi masih dipakai, supaya pembersihan retensi tidak
    /// membuang sesi yang aktif.
    pub fn touch_session(&self, id: &str, now: i64) -> Result<(), SessionError> {
        self.conn().execute(
            "UPDATE session SET last_used_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        Ok(())
    }

    pub fn session_state(&self, id: &str) -> Result<Option<String>, SessionError> {
        let state = self
            .conn()
            .query_row(
                "SELECT state FROM session WHERE id = ?1",
                params![id],
                |b| b.get::<_, String>(0),
            )
            .ok();
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoreKey;
    use crate::testutil::TempDir;

    fn store(dir: &TempDir) -> Store {
        Store::open(dir.path().join("store.db"), StoreKey::generate()).expect("buka")
    }

    #[test]
    fn buka_lalu_tutup_mengubah_state() {
        let dir = TempDir::new("sesi-buka");
        let store = store(&dir);
        store
            .open_session("s1", "native", Some("https://a.test"), 10)
            .expect("buka");
        assert_eq!(
            store.session_state("s1").expect("state").as_deref(),
            Some("open")
        );

        store.close_session("s1", "selesai", 20).expect("tutup");
        assert_eq!(
            store.session_state("s1").expect("state").as_deref(),
            Some("closed")
        );
    }

    #[test]
    fn buka_dua_kali_ditolak_dengan_nama_sesi() {
        let dir = TempDir::new("sesi-dobel");
        let store = store(&dir);
        store.open_session("s1", "native", None, 1).expect("buka");
        let err = store
            .open_session("s1", "native", None, 2)
            .expect_err("harus menolak");
        assert!(
            matches!(err, SessionError::SudahAda(ref id) if id == "s1"),
            "{err}"
        );
    }

    #[test]
    fn tutup_idempoten() {
        let dir = TempDir::new("sesi-idem");
        let store = store(&dir);
        store.open_session("s1", "native", None, 1).expect("buka");
        store.close_session("s1", "a", 2).expect("tutup pertama");
        store.close_session("s1", "b", 3).expect("tutup kedua");
        assert_eq!(
            store.session_state("s1").expect("state").as_deref(),
            Some("closed")
        );
    }

    #[test]
    fn sesi_tidak_ada_mengembalikan_none() {
        let dir = TempDir::new("sesi-kosong");
        let store = store(&dir);
        assert!(store.session_state("hantu").expect("state").is_none());
    }
}
