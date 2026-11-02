//! Skema SQLite, migrasi, dan retensi.
//!
//! Tiga keputusan yang perlu dijelaskan, karena gampang dikira iseng:
//!
//! - **Direktori 0700 adalah batas keamanannya, bukan mode berkas.** SQLite
//!   membuat `-wal` dan `-shm` sendiri dengan mode warisan umask; berkas itu
//!   tidak bisa kita chmod saat lahir. Direktori 0700 membuat berkas apa pun di
//!   dalamnya tidak bisa dibuka pengguna lain, dan mode 0600 pada `store.db`
//!   jadi lapisan kedua, bukan satu-satunya.
//! - **Migrasi memakai `PRAGMA user_version`, bukan tabel versi.** Satu angka
//!   yang tidak bisa lupa di-commit, dan tidak ada baris yang bisa hilang.
//! - **Skema yang lebih baru dari biner ini ditolak.** Membuka database dari
//!   versi daemon yang lebih baru lalu menulis ke dalamnya adalah cara
//!   termudah merusak data.

use std::fs::OpenOptions;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use rusqlite::Connection;

/// Versi skema yang dipahami biner ini.
pub const SCHEMA_VERSION: i64 = 1;

/// Retensi baris, dalam detik. Dokumen 02 §6 menetapkannya dalam hari.
pub const RETENSI_CAPTCHA_SECS: i64 = 90 * 24 * 60 * 60;
pub const RETENSI_OTP_SECS: i64 = 7 * 24 * 60 * 60;
pub const RETENSI_SESSION_SECS: i64 = 30 * 24 * 60 * 60;
pub const RETENSI_ENGINE_RUN_SECS: i64 = 30 * 24 * 60 * 60;

/// Kegagalan membuka atau memigrasi store.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("menyiapkan direktori store {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error(
        "database {path} memakai skema versi {found}, biner ini hanya paham {SCHEMA_VERSION}; pakai daemon yang lebih baru"
    )]
    NewerSchema {
        path: std::path::PathBuf,
        found: i64,
    },
}

/// Jumlah baris yang dibuang tiap tabel saat pembersihan.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepReport {
    pub captcha_event: usize,
    pub otp_event: usize,
    pub session: usize,
    pub engine_run: usize,
}

impl SweepReport {
    #[must_use]
    pub fn total(&self) -> usize {
        self.captcha_event + self.otp_event + self.session + self.engine_run
    }
}

/// Membuka database, menyiapkan direktori, lalu memigrasikan skema.
///
/// `now` tidak dipakai di sini; pembersihan retensi dipanggil terpisah supaya
/// waktu bisa dikendalikan test.
pub fn open(path: &Path) -> Result<Connection, SchemaError> {
    if let Some(induk) = path.parent()
        && !induk.as_os_str().is_empty()
    {
        std::fs::create_dir_all(induk).map_err(|source| SchemaError::Io {
            path: induk.to_path_buf(),
            source,
        })?;
        std::fs::set_permissions(induk, std::fs::Permissions::from_mode(0o700)).map_err(
            |source| SchemaError::Io {
                path: induk.to_path_buf(),
                source,
            },
        )?;
    }

    // Berkas dibuat sendiri dengan 0600 sebelum SQLite menyentuhnya, supaya
    // tidak ada jendela waktu dengan mode bawaan umask.
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|source| SchemaError::Io {
            path: path.to_path_buf(),
            source,
        })?;

    let conn = Connection::open(path)?;

    // WAL: pembacaan metrik tidak boleh memblokir penulisan cookie, dan
    // sebaliknya. `busy_timeout` menutup jendela "database is locked" saat
    // checkpoint berjalan bersamaan.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;

    migrate(&conn, path)?;
    Ok(conn)
}

/// Menjalankan migrasi ke [`SCHEMA_VERSION`]. Aman dipanggil berulang.
pub fn migrate(conn: &Connection, path: &Path) -> Result<(), SchemaError> {
    let versi: i64 = conn.pragma_query_value(None, "user_version", |baris| baris.get(0))?;

    if versi > SCHEMA_VERSION {
        return Err(SchemaError::NewerSchema {
            path: path.to_path_buf(),
            found: versi,
        });
    }
    if versi == SCHEMA_VERSION {
        return Ok(());
    }

    // Satu transaksi: skema setengah jadi lebih buruk daripada skema lama,
    // karena daemon berikutnya akan mengira migrasi sudah selesai.
    conn.execute_batch(
        r"
        BEGIN;
        CREATE TABLE IF NOT EXISTS profile (
          id            TEXT PRIMARY KEY,
          name          TEXT UNIQUE NOT NULL,
          meta_json     TEXT NOT NULL,
          engine_hint   TEXT,
          sticky        INTEGER NOT NULL DEFAULT 0,
          created_at    INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS session (
          id            TEXT PRIMARY KEY,
          profile_id    TEXT REFERENCES profile(id) ON DELETE SET NULL,
          engine        TEXT NOT NULL,
          target_id     TEXT,
          url           TEXT,
          state         TEXT NOT NULL,
          opened_at     INTEGER NOT NULL,
          last_used_at  INTEGER NOT NULL,
          sticky        INTEGER NOT NULL DEFAULT 0,
          closed_reason TEXT
        );
        CREATE INDEX IF NOT EXISTS session_state_idx ON session(state, last_used_at);

        CREATE TABLE IF NOT EXISTS cookie (
          profile_id    TEXT NOT NULL REFERENCES profile(id) ON DELETE CASCADE,
          host          TEXT NOT NULL,
          name          TEXT NOT NULL,
          path          TEXT NOT NULL,
          value_enc     BLOB NOT NULL,
          expires       INTEGER,
          secure        INTEGER NOT NULL DEFAULT 0,
          http_only     INTEGER NOT NULL DEFAULT 0,
          samesite      INTEGER,
          updated_at    INTEGER NOT NULL,
          PRIMARY KEY (profile_id, host, name, path)
        );

        CREATE TABLE IF NOT EXISTS captcha_event (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          session_id TEXT, host TEXT, kind TEXT,
          outcome TEXT,
          at INTEGER NOT NULL, detail TEXT
        );

        CREATE TABLE IF NOT EXISTS otp_event (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          session_id TEXT, source TEXT, host TEXT,
          code_hash TEXT NOT NULL,
          expires_at INTEGER NOT NULL,
          used INTEGER NOT NULL DEFAULT 0,
          at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS engine_run (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          engine TEXT NOT NULL, pid INTEGER,
          started_at INTEGER NOT NULL, stopped_at INTEGER,
          peak_pss INTEGER, reason TEXT
        );

        PRAGMA user_version = 1;
        COMMIT;
        ",
    )?;

    Ok(())
}

/// Membuang baris yang melewati masa retensi. Dijalankan saat boot, bukan cron
/// terpisah, supaya tidak ada proses yang harus diawasi.
pub fn sweep(conn: &Connection, now: i64) -> Result<SweepReport, SchemaError> {
    let captcha_event = conn.execute(
        "DELETE FROM captcha_event WHERE at < ?1",
        [now - RETENSI_CAPTCHA_SECS],
    )?;
    let otp_event = conn.execute(
        "DELETE FROM otp_event WHERE at < ?1",
        [now - RETENSI_OTP_SECS],
    )?;
    let session = conn.execute(
        "DELETE FROM session WHERE last_used_at < ?1 AND state != 'open'",
        [now - RETENSI_SESSION_SECS],
    )?;
    let engine_run = conn.execute(
        "DELETE FROM engine_run WHERE started_at < ?1",
        [now - RETENSI_ENGINE_RUN_SECS],
    )?;

    Ok(SweepReport {
        captcha_event,
        otp_event,
        session,
        engine_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn buka(dir: &TempDir) -> (Connection, std::path::PathBuf) {
        let path = dir.path().join("store.db");
        let conn = open(&path).expect("membuka store");
        (conn, path)
    }

    fn nama_tabel(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .expect("siap");
        stmt.query_map([], |baris| baris.get::<_, String>(0))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("kumpulkan")
    }

    #[test]
    fn store_baru_membuat_semua_tabel() {
        let dir = TempDir::new("skema");
        let (conn, _) = buka(&dir);
        let tabel = nama_tabel(&conn);
        for wajib in [
            "profile",
            "session",
            "cookie",
            "captcha_event",
            "otp_event",
            "engine_run",
        ] {
            assert!(
                tabel.iter().any(|t| t == wajib),
                "tabel {wajib} hilang: {tabel:?}"
            );
        }
    }

    #[test]
    fn membuka_ulang_tidak_menghapus_data() {
        let dir = TempDir::new("persist");
        let path = dir.path().join("store.db");
        {
            let conn = open(&path).expect("buka pertama");
            conn.execute(
                "INSERT INTO profile (id, name, meta_json, sticky, created_at) VALUES ('p1', 'shop', '{}', 0, 1762)",
                [],
            )
            .expect("sisip");
        }
        let conn = open(&path).expect("buka kedua");
        let jumlah: i64 = conn
            .query_row("SELECT COUNT(*) FROM profile", [], |b| b.get(0))
            .expect("hitung");
        assert_eq!(jumlah, 1, "data hilang setelah buka ulang");
    }

    #[test]
    fn migrasi_idempoten() {
        let dir = TempDir::new("migrasi");
        let (conn, path) = buka(&dir);
        migrate(&conn, &path).expect("migrasi kedua");
        migrate(&conn, &path).expect("migrasi ketiga");
        let versi: i64 = conn
            .pragma_query_value(None, "user_version", |b| b.get(0))
            .expect("versi");
        assert_eq!(versi, SCHEMA_VERSION);
    }

    #[test]
    fn skema_lebih_baru_ditolak() {
        let dir = TempDir::new("baru");
        let path = dir.path().join("store.db");
        {
            let conn = open(&path).expect("buka");
            conn.pragma_update(None, "user_version", 99).expect("set");
        }
        let err = open(&path).expect_err("harus menolak");
        assert!(
            matches!(err, SchemaError::NewerSchema { found: 99, .. }),
            "galat salah: {err}"
        );
    }

    #[test]
    fn direktori_0700_dan_berkas_0600() {
        let dir = TempDir::new("mode");
        let path = dir.path().join("sub").join("store.db");
        let _conn = open(&path).expect("buka");
        let mode_dir = std::fs::metadata(path.parent().expect("induk"))
            .expect("meta dir")
            .permissions()
            .mode()
            & 0o777;
        let mode_db = std::fs::metadata(&path)
            .expect("meta db")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode_dir, 0o700, "direktori store harus 0700");
        assert_eq!(mode_db, 0o600, "store.db harus 0600");
    }

    #[test]
    fn foreign_key_menolak_sesi_tanpa_profil() {
        let dir = TempDir::new("fk");
        let (conn, _) = buka(&dir);
        let err = conn
            .execute(
                "INSERT INTO session (id, profile_id, engine, state, opened_at, last_used_at) \
                 VALUES ('s1', 'profil-hantu', 'shell', 'open', 1, 1)",
                [],
            )
            .expect_err("FK harus menolak");
        assert!(
            err.to_string().contains("FOREIGN KEY"),
            "galat salah: {err}"
        );
    }

    #[test]
    fn sweep_membuang_hanya_yang_kedaluwarsa() {
        let dir = TempDir::new("sweep");
        let (conn, _) = buka(&dir);
        let now = 1_000_000_000_i64;

        conn.execute(
            "INSERT INTO profile (id, name, meta_json, created_at) VALUES ('p1', 'shop', '{}', ?1)",
            [now],
        )
        .expect("profil");

        conn.execute(
            "INSERT INTO captcha_event (host, kind, outcome, at) VALUES ('a', 'cf', 'solved_reuse', ?1), ('b', 'cf', 'failed', ?2)",
            [now - RETENSI_CAPTCHA_SECS - 1, now - 10],
        )
        .expect("captcha");
        conn.execute(
            "INSERT INTO otp_event (source, code_hash, expires_at, at) VALUES ('email', 'x', ?1, ?2), ('email', 'y', ?1, ?3)",
            [now, now - RETENSI_OTP_SECS - 1, now - 10],
        )
        .expect("otp");
        conn.execute(
            "INSERT INTO session (id, profile_id, engine, state, opened_at, last_used_at) VALUES \
             ('lama', 'p1', 'shell', 'closed', ?1, ?1), \
             ('baru', 'p1', 'shell', 'closed', ?2, ?2), \
             ('buka', 'p1', 'shell', 'open', ?1, ?1)",
            [now - RETENSI_SESSION_SECS - 1, now - 10],
        )
        .expect("sesi");
        conn.execute(
            "INSERT INTO engine_run (engine, started_at) VALUES ('shell', ?1), ('shell', ?2)",
            [now - RETENSI_ENGINE_RUN_SECS - 1, now - 10],
        )
        .expect("engine_run");

        let laporan = sweep(&conn, now).expect("sweep");
        assert_eq!(laporan.captcha_event, 1);
        assert_eq!(laporan.otp_event, 1);
        assert_eq!(laporan.session, 1, "sesi 'open' tidak boleh dibuang");
        assert_eq!(laporan.engine_run, 1);
        assert_eq!(laporan.total(), 4);

        let sisa_sesi: i64 = conn
            .query_row("SELECT COUNT(*) FROM session", [], |b| b.get(0))
            .expect("hitung sesi");
        assert_eq!(sisa_sesi, 2);
    }

    #[test]
    fn sweep_pada_data_baru_tidak_membuang_apa_pun() {
        let dir = TempDir::new("sweep-bersih");
        let (conn, _) = buka(&dir);
        let laporan = sweep(&conn, 1_000_000_000).expect("sweep");
        assert_eq!(laporan.total(), 0);
    }
}
