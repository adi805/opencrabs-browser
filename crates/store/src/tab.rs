//! Tab dingin yang bertahan antar restart.
//!
//! Pool tab di `engine` menyingkirkan tab dari RAM saat anggaran penuh. Yang
//! disingkirkan bukan dibuang: HTML hasil serialisasi disimpan di sini, dan
//! disegel dengan kunci store **sebelum** menyentuh disk.
//!
//! Kenapa disegel: DOM sebuah tab bisa memuat token sesi, alamat email, atau
//! isi halaman yang baru dibuka setelah login. Menyimpannya sebagai plaintext
//! berarti melindungi cookie dengan enkripsi sambil membocorkan hal yang sama
//! lewat pintu belakang.
//!
//! Yang TIDAK disimpan: cookie (sudah punya tabelnya sendiri), riwayat
//! navigasi, maupun isi `localStorage`. Tab yang dihidupkan kembali membawa
//! pohon DOM-nya, bukan sesi JavaScript-nya.
//!
//! Modul ini sengaja tidak bergantung pada crate `engine`: yang lintas batas
//! hanya bentuk data, dan adapter di `browserd` yang menjembatani keduanya.

use rusqlite::{OptionalExtension, params};

use crate::Store;
use crate::crypto::KeyError;

/// Satu lembar gaya eksternal milik tab: `href` apa adanya dan teks CSS-nya.
///
/// `href` disimpan bersama teksnya karena kaskade mencocokkan lembar ke node
/// `<link>` lewat atribut itu, bukan lewat URL hasil penyelesaian.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TabSheet {
    pub href: String,
    pub css: String,
}

/// Satu tab yang disingkirkan dari RAM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabSnapshot {
    /// Nomor tab di dalam sesi. Bukan nomor global.
    pub tab_id: i64,
    pub url: String,
    pub title: Option<String>,
    /// HTML hasil serialisasi pohon DOM, plaintext hanya di memori.
    pub html: String,
    pub node_count: usize,
    /// Unix epoch milidetik saat tab disingkirkan.
    pub evicted_at: i64,
    /// Lembar gaya eksternal yang sudah diambil untuk dokumen ini.
    ///
    /// Kosong untuk tab yang dokumennya tidak punya `<link rel=stylesheet>`.
    pub sheets: Vec<TabSheet>,
}

/// Kegagalan menyimpan atau memuat tab dingin.
#[derive(Debug, thiserror::Error)]
pub enum TabError {
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("kunci store gagal dipakai: {0}")]
    Key(#[from] KeyError),

    #[error("lembar gaya tab tidak bisa disandikan: {0}")]
    Sheets(String),
}

impl Store {
    /// Menyimpan tab dingin. Baris lama dengan `(session_id, tab_id)` yang sama
    /// ditimpa: tab yang disingkirkan dua kali hanya perlu salinan terbaru.
    pub fn save_tab_snapshot(
        &self,
        session_id: &str,
        snapshot: &TabSnapshot,
    ) -> Result<(), TabError> {
        let html_enc = self.key().seal(snapshot.html.as_bytes())?;
        // Lembar disegel juga: teks CSS bisa memuat token yang ditempel server
        // ke dalam stylesheet. Yang disegel adalah JSON utuh, bukan per lembar,
        // supaya jumlah lembar tidak perlu kolom sendiri.
        let sheets_enc = if snapshot.sheets.is_empty() {
            None
        } else {
            let json = serde_json::to_vec(&snapshot.sheets)
                .map_err(|e| TabError::Sheets(e.to_string()))?;
            Some(self.key().seal(&json)?)
        };
        self.conn().execute(
            "INSERT INTO tab_snapshot
               (session_id, tab_id, url, title, html_enc, node_count, evicted_at, sheets_enc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(session_id, tab_id) DO UPDATE SET
               url = excluded.url,
               title = excluded.title,
               html_enc = excluded.html_enc,
               node_count = excluded.node_count,
               evicted_at = excluded.evicted_at,
               sheets_enc = excluded.sheets_enc",
            params![
                session_id,
                snapshot.tab_id,
                snapshot.url,
                snapshot.title,
                html_enc,
                snapshot.node_count as i64,
                snapshot.evicted_at,
                sheets_enc,
            ],
        )?;
        Ok(())
    }

    /// Memuat tab dingin. `None` berarti tab itu tidak pernah disingkirkan,
    /// atau sudah dibuang pembersihan retensi.
    pub fn load_tab_snapshot(
        &self,
        session_id: &str,
        tab_id: i64,
    ) -> Result<Option<TabSnapshot>, TabError> {
        let baris = self
            .conn()
            .query_row(
                "SELECT url, title, html_enc, node_count, evicted_at, sheets_enc
                   FROM tab_snapshot WHERE session_id = ?1 AND tab_id = ?2",
                params![session_id, tab_id],
                |b| {
                    Ok((
                        b.get::<_, String>(0)?,
                        b.get::<_, Option<String>>(1)?,
                        b.get::<_, Vec<u8>>(2)?,
                        b.get::<_, i64>(3)?,
                        b.get::<_, i64>(4)?,
                        b.get::<_, Option<Vec<u8>>>(5)?,
                    ))
                },
            )
            .optional()?;

        let Some((url, title, html_enc, node_count, evicted_at, sheets_enc)) = baris else {
            return Ok(None);
        };

        let html = self.key().open(&html_enc)?;
        let sheets = match sheets_enc {
            None => Vec::new(),
            Some(enc) => {
                let json = self.key().open(&enc)?;
                serde_json::from_slice(&json).map_err(|e| TabError::Sheets(e.to_string()))?
            }
        };
        Ok(Some(TabSnapshot {
            tab_id,
            url,
            title,
            html: String::from_utf8_lossy(&html).into_owned(),
            node_count: node_count.max(0) as usize,
            evicted_at,
            sheets,
        }))
    }

    pub fn delete_tab_snapshot(&self, session_id: &str, tab_id: i64) -> Result<(), TabError> {
        self.conn().execute(
            "DELETE FROM tab_snapshot WHERE session_id = ?1 AND tab_id = ?2",
            params![session_id, tab_id],
        )?;
        Ok(())
    }

    /// Jumlah tab dingin milik satu sesi. Dipakai untuk memastikan tab yang
    /// disingkirkan benar-benar tersimpan, bukan cuma hilang dari RAM.
    pub fn count_tab_snapshots(&self, session_id: &str) -> Result<usize, TabError> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM tab_snapshot WHERE session_id = ?1",
            params![session_id],
            |b| b.get(0),
        )?;
        Ok(n.max(0) as usize)
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

    fn sesi(store: &Store) {
        store
            .conn()
            .execute(
                "INSERT INTO session (id, engine, state, opened_at, last_used_at)
                 VALUES ('s1', 'native', 'open', 1, 1)",
                [],
            )
            .expect("sesi");
    }

    fn snapshot() -> TabSnapshot {
        TabSnapshot {
            tab_id: 7,
            url: "https://contoh.test/halaman".to_owned(),
            title: Some("Halaman".to_owned()),
            html: "<html><body><p>rahasia-token-abc</p></body></html>".to_owned(),
            node_count: 4,
            evicted_at: 1_700_000_000_000,
            sheets: vec![TabSheet {
                href: "gaya.css".to_owned(),
                css: "#a{width:10px}".to_owned(),
            }],
        }
    }

    #[test]
    fn simpan_dan_muat_kembali_utuh() {
        let dir = TempDir::new("tab-bulat");
        let store = store(&dir);
        sesi(&store);
        store.save_tab_snapshot("s1", &snapshot()).expect("simpan");

        let kembali = store
            .load_tab_snapshot("s1", 7)
            .expect("muat")
            .expect("ada");
        assert_eq!(kembali, snapshot(), "isi berubah setelah pulang-pergi");
    }

    /// Inti modul ini: HTML tidak boleh ada di disk sebagai plaintext.
    #[test]
    fn html_tidak_tersimpan_plaintext() {
        let dir = TempDir::new("tab-geser");
        let store = store(&dir);
        sesi(&store);
        store.save_tab_snapshot("s1", &snapshot()).expect("simpan");

        let mentah = std::fs::read(dir.path().join("store.db")).expect("baca db");
        let jarum = b"rahasia-token-abc";
        assert!(
            !mentah.windows(jarum.len()).any(|w| w == jarum),
            "isi DOM bocor sebagai plaintext di berkas database"
        );
    }

    #[test]
    fn sesi_lain_tidak_melihat_tab_sesi_ini() {
        let dir = TempDir::new("tab-pisah");
        let store = store(&dir);
        sesi(&store);
        store
            .conn()
            .execute(
                "INSERT INTO session (id, engine, state, opened_at, last_used_at)
                 VALUES ('s2', 'native', 'open', 1, 1)",
                [],
            )
            .expect("sesi kedua");
        store.save_tab_snapshot("s1", &snapshot()).expect("simpan");

        assert!(store.load_tab_snapshot("s2", 7).expect("muat").is_none());
        assert_eq!(store.count_tab_snapshots("s1").expect("hitung"), 1);
        assert_eq!(store.count_tab_snapshots("s2").expect("hitung"), 0);
    }

    #[test]
    fn simpan_ulang_menimpa_bukan_menumpuk() {
        let dir = TempDir::new("tab-timpa");
        let store = store(&dir);
        sesi(&store);
        store.save_tab_snapshot("s1", &snapshot()).expect("pertama");

        let mut baru = snapshot();
        baru.html = "<html><body><p>versi-kedua</p></body></html>".to_owned();
        baru.node_count = 5;
        store.save_tab_snapshot("s1", &baru).expect("kedua");

        assert_eq!(store.count_tab_snapshots("s1").expect("hitung"), 1);
        let kembali = store
            .load_tab_snapshot("s1", 7)
            .expect("muat")
            .expect("ada");
        assert_eq!(kembali.html, baru.html);
        assert_eq!(kembali.node_count, 5);
    }

    #[test]
    fn hapus_menghilangkan_baris() {
        let dir = TempDir::new("tab-hapus");
        let store = store(&dir);
        sesi(&store);
        store.save_tab_snapshot("s1", &snapshot()).expect("simpan");
        store.delete_tab_snapshot("s1", 7).expect("hapus");

        assert!(store.load_tab_snapshot("s1", 7).expect("muat").is_none());
        assert_eq!(store.count_tab_snapshots("s1").expect("hitung"), 0);
    }

    /// CSS juga tidak boleh ada di disk sebagai plaintext: stylesheet bisa
    /// memuat token yang ditempel server.
    #[test]
    fn lembar_gaya_tidak_tersimpan_plaintext() {
        let dir = TempDir::new("tab-css-geser");
        let store = store(&dir);
        sesi(&store);
        let mut snap = snapshot();
        snap.sheets = vec![TabSheet {
            href: "rahasia.css".to_owned(),
            css: ".token{content:\"rahasia-css-xyz\"}".to_owned(),
        }];
        store.save_tab_snapshot("s1", &snap).expect("simpan");

        let mentah = std::fs::read(dir.path().join("store.db")).expect("baca db");
        for jarum in [b"rahasia-css-xyz".as_slice(), b"rahasia.css".as_slice()] {
            assert!(
                !mentah.windows(jarum.len()).any(|w| w == jarum),
                "lembar gaya bocor sebagai plaintext di berkas database"
            );
        }
    }

    #[test]
    fn snapshot_tanpa_lembar_tetap_utuh() {
        let dir = TempDir::new("tab-tanpa-css");
        let store = store(&dir);
        sesi(&store);
        let mut snap = snapshot();
        snap.sheets = Vec::new();
        store.save_tab_snapshot("s1", &snap).expect("simpan");
        let kembali = store
            .load_tab_snapshot("s1", 7)
            .expect("muat")
            .expect("ada");
        assert!(
            kembali.sheets.is_empty(),
            "lembar kosong harus tetap kosong"
        );
        assert_eq!(kembali.html, snap.html);
    }

    #[test]
    fn kunci_salah_gagal_membuka_dengan_galat_jelas() {
        let dir = TempDir::new("tab-kunci");
        let db = dir.path().join("store.db");
        {
            let store = Store::open(&db, StoreKey::generate()).expect("buka");
            sesi(&store);
            store.save_tab_snapshot("s1", &snapshot()).expect("simpan");
        }
        let store = Store::open(&db, StoreKey::generate()).expect("buka kunci lain");
        let err = store.load_tab_snapshot("s1", 7).expect_err("harus menolak");
        assert!(matches!(err, TabError::Key(_)), "galat salah: {err}");
    }
}
