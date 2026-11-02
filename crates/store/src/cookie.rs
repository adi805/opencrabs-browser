//! Cookie jar terenkripsi, satu jar per profil.
//!
//! Aturan yang dipegang (dokumen 02 §7-§8):
//!
//! - **Nilai disimpan terenkripsi**, nama boleh dibaca, nilai tidak pernah
//!   masuk pesan galat. Galat yang menyebut nilai cookie adalah kebocoran yang
//!   tercatat permanen di log.
//! - **Engine menang saat konflik.** Nilai yang baru diterima dari server
//!   adalah yang paling baru; menimpanya dengan salinan lama membuat sesi
//!   login gagal tanpa sebab yang jelas.
//! - **Cookie > 4096 byte dilewati, bukan dipotong.** Cookie yang dipotong
//!   nilainya bukan cookie yang sah, dan menyimpannya berarti menyimpan bug.
//! - **Profil tidak pernah bercampur.** Query selalu difilter `profile_id`;
//!   ini diuji silang, bukan diasumsikan.

use rusqlite::{OptionalExtension, params};

use crate::Store;
use crate::crypto::KeyError;
use crate::profile::ProfileError;

/// Batas nilai cookie yang disimpan. Di atas ini, cookie dilewati.
pub const MAX_COOKIE_BYTES: usize = 4096;

/// Satu cookie dalam bentuk yang siap dikirim ke CDP.
#[derive(Clone, PartialEq, Eq)]
pub struct Cookie {
    pub host: String,
    pub name: String,
    pub path: String,
    /// Nilai plaintext. Hanya hidup di memori; tidak pernah ditulis apa adanya.
    pub value: String,
    pub expires: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
    /// `-1` tidak ditentukan, `0` none, `1` lax, `2` strict. Disimpan sebagai
    /// angka karena itulah yang dipakai `Network.setCookies`.
    pub samesite: Option<i64>,
}

/// Debug manual: nama dan host boleh muncul di log, nilai tidak pernah.
/// `derive(Debug)` akan mencetak `value` utuh ke setiap `{:?}`, termasuk yang
/// kebetulan nongol di pesan panic dan log `tracing`, jadi impl ini bukan
/// kehati-hatian berlebih, ia satu-satunya penghalang antara cookie sesi
/// (token login) dan berkas log yang dibaca orang lain.
impl std::fmt::Debug for Cookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cookie")
            .field("host", &self.host)
            .field("name", &self.name)
            .field("path", &self.path)
            .field(
                "value",
                &format_args!("<disembunyikan: {} byte>", self.value.len()),
            )
            .field("expires", &self.expires)
            .field("secure", &self.secure)
            .field("http_only", &self.http_only)
            .field("samesite", &self.samesite)
            .finish()
    }
}

/// Hasil satu putaran penyimpanan.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SaveReport {
    pub saved: usize,
    pub skipped_too_big: usize,
    pub skipped_invalid: usize,
}

/// Kegagalan operasi cookie.
#[derive(Debug, thiserror::Error)]
pub enum CookieError {
    #[error("profil {0:?} belum ada; buat profil dulu sebelum menyimpan cookie")]
    NoProfile(String),

    #[error("nilai cookie {profile}/{host}/{name} tidak bisa dibuka dengan kunci sekarang")]
    Decrypt {
        profile: String,
        host: String,
        name: String,
    },

    #[error("nilai cookie {profile}/{host}/{name} rusak: {source}")]
    Corrupt {
        profile: String,
        host: String,
        name: String,
        #[source]
        source: KeyError,
    },

    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

impl From<ProfileError> for CookieError {
    fn from(err: ProfileError) -> Self {
        match err {
            ProfileError::Sqlite(inner) => Self::Sqlite(inner),
            lain => Self::Sqlite(rusqlite::Error::InvalidParameterName(lain.to_string())),
        }
    }
}

impl Cookie {
    /// Cookie sesi (tanpa `expires`) selalu dikembalikan; cookie dengan
    /// `expires` di masa lalu tidak.
    fn belum_kedaluwarsa(&self, now: i64) -> bool {
        self.expires.is_none_or(|expires| expires > now)
    }
}

impl Store {
    /// Menyimpan cookie ke jar profil. Nilai dienkripsi sebelum menyentuh
    /// database, dan konflik di-update dengan nilai baru.
    pub fn save_cookies(
        &self,
        profil: &str,
        cookies: &[Cookie],
        now: i64,
    ) -> Result<SaveReport, CookieError> {
        if self.profile(profil)?.is_none() {
            return Err(CookieError::NoProfile(profil.to_owned()));
        }

        let mut laporan = SaveReport::default();
        let mut stmt = self.conn().prepare(
            "INSERT INTO cookie \
               (profile_id, host, name, path, value_enc, expires, secure, http_only, samesite, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT(profile_id, host, name, path) DO UPDATE SET \
               value_enc = excluded.value_enc, \
               expires = excluded.expires, \
               secure = excluded.secure, \
               http_only = excluded.http_only, \
               samesite = excluded.samesite, \
               updated_at = excluded.updated_at",
        )?;

        for cookie in cookies {
            if cookie.host.is_empty() || cookie.name.is_empty() || cookie.path.is_empty() {
                laporan.skipped_invalid += 1;
                continue;
            }
            if cookie.value.len() > MAX_COOKIE_BYTES {
                laporan.skipped_too_big += 1;
                continue;
            }

            let terenkripsi = self.key().seal(cookie.value.as_bytes()).map_err(|source| {
                CookieError::Corrupt {
                    profile: profil.to_owned(),
                    host: cookie.host.clone(),
                    name: cookie.name.clone(),
                    source,
                }
            })?;

            stmt.execute(params![
                profil,
                cookie.host,
                cookie.name,
                cookie.path,
                terenkripsi,
                cookie.expires,
                i64::from(cookie.secure),
                i64::from(cookie.http_only),
                cookie.samesite,
                now
            ])?;
            laporan.saved += 1;
        }

        Ok(laporan)
    }

    /// Cookie profil yang belum kedaluwarsa. `host` menyaring satu host saja;
    /// `None` berarti semua host milik profil itu.
    pub fn cookies(
        &self,
        profil: &str,
        host: Option<&str>,
        now: i64,
    ) -> Result<Vec<Cookie>, CookieError> {
        let mut hasil = Vec::new();

        {
            let mut stmt = self.conn().prepare(
                "SELECT host, name, path, value_enc, expires, secure, http_only, samesite \
                 FROM cookie WHERE profile_id = ?1 AND (expires IS NULL OR expires > ?2) \
                 ORDER BY host, name, path",
            )?;
            let baris = stmt.query_map(params![profil, now], |baris| {
                Ok((
                    baris.get::<_, String>("host")?,
                    baris.get::<_, String>("name")?,
                    baris.get::<_, String>("path")?,
                    baris.get::<_, Vec<u8>>("value_enc")?,
                    baris.get::<_, Option<i64>>("expires")?,
                    baris.get::<_, i64>("secure")? != 0,
                    baris.get::<_, i64>("http_only")? != 0,
                    baris.get::<_, Option<i64>>("samesite")?,
                ))
            })?;

            for satu in baris {
                let (host_cookie, name, path, terenkripsi, expires, secure, http_only, samesite) =
                    satu?;
                if host.is_some_and(|saring| saring != host_cookie) {
                    continue;
                }
                let nilai = self
                    .key()
                    .open(&terenkripsi)
                    .map_err(|source| match source {
                        KeyError::Aead => CookieError::Decrypt {
                            profile: profil.to_owned(),
                            host: host_cookie.clone(),
                            name: name.clone(),
                        },
                        lain => CookieError::Corrupt {
                            profile: profil.to_owned(),
                            host: host_cookie.clone(),
                            name: name.clone(),
                            source: lain,
                        },
                    })?;
                let value = String::from_utf8_lossy(&nilai).into_owned();
                let cookie = Cookie {
                    host: host_cookie,
                    name,
                    path,
                    value,
                    expires,
                    secure,
                    http_only,
                    samesite,
                };
                if cookie.belum_kedaluwarsa(now) {
                    hasil.push(cookie);
                }
            }
        }

        Ok(hasil)
    }

    /// Jumlah cookie tersimpan untuk satu profil, termasuk yang kedaluwarsa.
    /// Dipakai metrik dan test; tidak pernah mengembalikan nilai.
    pub fn count_cookies(&self, profil: &str) -> Result<u64, CookieError> {
        let jumlah: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM cookie WHERE profile_id = ?1",
            [profil],
            |baris| baris.get(0),
        )?;
        Ok(u64::try_from(jumlah).unwrap_or(0))
    }

    /// Menghapus seluruh cookie satu host pada satu profil. Dipakai saat
    /// clearance terbukti basi, bukan saat logout biasa.
    pub fn delete_cookies(&self, profil: &str, host: &str) -> Result<usize, CookieError> {
        let jumlah = self.conn().execute(
            "DELETE FROM cookie WHERE profile_id = ?1 AND host = ?2",
            params![profil, host],
        )?;
        Ok(jumlah)
    }

    /// Apakah profil ada. Dipakai jalur CLI supaya pesan galatnya jelas.
    pub fn profile_ada(&self, profil: &str) -> Result<bool, CookieError> {
        let ada: Option<i64> = self
            .conn()
            .query_row("SELECT 1 FROM profile WHERE name = ?1", [profil], |b| {
                b.get(0)
            })
            .optional()?;
        Ok(ada.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use crate::profile::ProfileMeta;
    use crate::testutil::TempDir;

    fn toko(dir: &TempDir) -> Store {
        let store =
            Store::open(dir.path().join("store.db"), crate::StoreKey::generate()).expect("buka");
        store
            .upsert_profile("shop", &ProfileMeta::default(), 1000)
            .expect("profil");
        store
    }

    fn cookie(nama: &str, nilai: &str) -> Cookie {
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
    fn cookie_tersimpan_lalu_terbaca_sama() {
        let dir = TempDir::new("cookie");
        let store = toko(&dir);
        let laporan = store
            .save_cookies("shop", &[cookie("cf_clearance", "nilai-rahasia")], 2000)
            .expect("simpan");
        assert_eq!(laporan.saved, 1);

        let dibaca = store.cookies("shop", None, 2000).expect("baca");
        assert_eq!(dibaca.len(), 1);
        assert_eq!(dibaca[0].value, "nilai-rahasia");
        assert_eq!(dibaca[0].name, "cf_clearance");
        assert!(dibaca[0].secure);
        assert!(dibaca[0].http_only);
        assert_eq!(dibaca[0].samesite, Some(1));
    }

    #[test]
    fn nilai_tidak_tersimpan_sebagai_plaintext() {
        let dir = TempDir::new("plaintext");
        let store = toko(&dir);
        let rahasia = "JANGAN-MUNCUL-DI-DATABASE";
        store
            .save_cookies("shop", &[cookie("sesi", rahasia)], 2000)
            .expect("simpan");

        let blob: Vec<u8> = store
            .conn()
            .query_row(
                "SELECT value_enc FROM cookie WHERE name = 'sesi'",
                [],
                |b| b.get(0),
            )
            .expect("blob");
        assert!(
            !blob
                .windows(rahasia.len())
                .any(|jendela| jendela == rahasia.as_bytes()),
            "nilai cookie tersimpan sebagai plaintext"
        );
        assert!(
            blob.len() > rahasia.len(),
            "amplop harus lebih panjang dari tag"
        );
    }

    #[test]
    fn debug_cookie_tidak_memuat_nilai() {
        let rahasia = "TOKEN-SESI-YANG-TIDAK-BOLEH-MUNCUL-DI-LOG";
        let teks = format!("{:?}", cookie("sesi", rahasia));
        assert!(
            !teks.contains(rahasia),
            "nilai cookie bocor lewat Debug: {teks}"
        );
        assert!(teks.contains("sesi"), "nama cookie tetap harus terbaca");
        assert!(teks.contains("<disembunyikan"), "penanda redaksi hilang");
    }

    #[test]
    fn konflik_nilai_engine_menang() {
        let dir = TempDir::new("konflik");
        let store = toko(&dir);
        store
            .save_cookies("shop", &[cookie("sesi", "lama")], 2000)
            .expect("simpan lama");
        store
            .save_cookies("shop", &[cookie("sesi", "baru")], 3000)
            .expect("simpan baru");

        let dibaca = store.cookies("shop", None, 3000).expect("baca");
        assert_eq!(dibaca.len(), 1, "upsert tidak boleh membuat baris kedua");
        assert_eq!(dibaca[0].value, "baru");
    }

    #[test]
    fn cookie_kedaluwarsa_tidak_dikembalikan() {
        let dir = TempDir::new("kedaluwarsa");
        let store = toko(&dir);
        let mut basi = cookie("basi", "x");
        basi.expires = Some(1500);
        let mut segar = cookie("segar", "y");
        segar.expires = Some(9000);
        store
            .save_cookies("shop", &[basi, segar], 2000)
            .expect("simpan");

        let dibaca = store.cookies("shop", None, 5000).expect("baca");
        let nama: Vec<&str> = dibaca.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(nama, ["segar"]);
    }

    #[test]
    fn cookie_lebih_4096_byte_dilewati_bukan_dipotong() {
        let dir = TempDir::new("besar");
        let store = toko(&dir);
        let besar = cookie("besar", &"x".repeat(MAX_COOKIE_BYTES + 1));
        let laporan = store.save_cookies("shop", &[besar], 2000).expect("simpan");
        assert_eq!(laporan.skipped_too_big, 1);
        assert_eq!(laporan.saved, 0);
        assert!(store.cookies("shop", None, 2000).expect("baca").is_empty());
    }

    #[test]
    fn profil_tidak_saling_melihat_cookie() {
        let dir = TempDir::new("isolasi");
        let store = toko(&dir);
        store
            .upsert_profile("kerja", &ProfileMeta::default(), 1000)
            .expect("profil kedua");

        store
            .save_cookies("shop", &[cookie("sesi", "milik-shop")], 2000)
            .expect("simpan shop");
        store
            .save_cookies("kerja", &[cookie("sesi", "milik-kerja")], 2000)
            .expect("simpan kerja");

        let shop = store.cookies("shop", None, 2000).expect("baca shop");
        let kerja = store.cookies("kerja", None, 2000).expect("baca kerja");
        assert_eq!(shop.len(), 1);
        assert_eq!(kerja.len(), 1);
        assert_eq!(shop[0].value, "milik-shop");
        assert_eq!(kerja[0].value, "milik-kerja");
    }

    #[test]
    fn filter_host_hanya_mengembalikan_host_itu() {
        let dir = TempDir::new("host");
        let store = toko(&dir);
        let mut lain = cookie("a", "satu");
        lain.host = "lain.example".to_owned();
        store
            .save_cookies("shop", &[cookie("a", "dua"), lain], 2000)
            .expect("simpan");

        let hanya = store
            .cookies("shop", Some("shop.example"), 2000)
            .expect("baca");
        assert_eq!(hanya.len(), 1);
        assert_eq!(hanya[0].value, "dua");
    }

    #[test]
    fn cookie_untuk_profil_hilang_ditolak() {
        let dir = TempDir::new("hantu");
        let store = toko(&dir);
        let err = store
            .save_cookies("tidak-ada", &[cookie("a", "b")], 2000)
            .expect_err("harus menolak");
        assert!(
            matches!(err, CookieError::NoProfile(_)),
            "galat salah: {err}"
        );
    }

    #[test]
    fn hapus_cookie_satu_host_tidak_menyentuh_host_lain() {
        let dir = TempDir::new("hapus");
        let store = toko(&dir);
        let mut lain = cookie("b", "dua");
        lain.host = "lain.example".to_owned();
        store
            .save_cookies("shop", &[cookie("a", "satu"), lain], 2000)
            .expect("simpan");

        let terhapus = store.delete_cookies("shop", "shop.example").expect("hapus");
        assert_eq!(terhapus, 1);
        assert_eq!(store.count_cookies("shop").expect("hitung"), 1);
        assert!(store.profile_ada("shop").expect("ada"));
    }
}
