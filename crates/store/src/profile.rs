//! Profil: satu identitas beku.
//!
//! Aturan yang mengikat dan alasannya (dokumen 02 §5): cookie `cf_clearance`
//! terikat pada IP, UA, dan sebagian fingerprint. Mengubah UA pada profil yang
//! sama membuat clearance yang sudah dipegang hangus, lalu captcha muncul lagi
//! tanpa ada yang mengubah konfigurasi. Karena itu `upsert_profile` **menolak**
//! perubahan UA; rotasi identitas harus lewat profil baru, dan penolakan itu
//! harus berisik supaya bukan kejutan besok.
//!
//! `id` disamakan dengan `name` di v0.1. Kolom `id` tetap ada karena ia yang
//! dirujuk `session.profile_id`: saat fitur rename datang, id tidak boleh ikut
//! berubah. Selama rename belum ada, dua nilai berbeda hanya menciptakan dua
//! sumber kebenaran.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::Store;

/// Batas panjang nama profil. Cukup panjang untuk nama host, cukup pendek
/// untuk tidak jadi jalur berkas yang aneh.
pub const MAX_NAME_LEN: usize = 64;

/// Ukuran viewport yang dipin ke profil.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Viewport {
    pub w: u32,
    pub h: u32,
    /// Device pixel ratio, dikali 100 supaya tetap bilangan bulat di JSON.
    pub dpr_x100: u32,
}

/// Identitas WebGL yang dilaporkan ke halaman.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebGl {
    pub vendor: String,
    pub renderer: String,
}

/// Isi `meta_json`: semua yang membuat satu profil bisa dikenali situs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileMeta {
    pub ua: String,
    pub viewport: Viewport,
    pub timezone: String,
    pub locale: String,
    pub languages: Vec<String>,
    pub platform: String,
    pub hardware_concurrency: u32,
    /// Seed noise canvas. Satu profil = satu seed, supaya gambar canvas tidak
    /// berubah antar sesi (perubahan mendadak itu sendiri sinyal bot).
    pub canvas_seed: u64,
    pub webgl: WebGl,
    #[serde(default)]
    pub engine_hint: Option<String>,
    #[serde(default)]
    pub sticky: bool,
}

impl Default for ProfileMeta {
    fn default() -> Self {
        Self {
            // UA beku: jangan diubah tanpa profil baru.
            ua: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/156.0.0.0 Safari/537.36"
                .to_owned(),
            viewport: Viewport {
                w: 1920,
                h: 1080,
                dpr_x100: 100,
            },
            timezone: "Asia/Jakarta".to_owned(),
            locale: "id-ID".to_owned(),
            languages: vec!["id-ID".to_owned(), "id".to_owned(), "en-US".to_owned()],
            platform: "Linux x86_64".to_owned(),
            hardware_concurrency: 4,
            canvas_seed: 1,
            webgl: WebGl {
                vendor: "Google Inc. (Intel)".to_owned(),
                renderer: "ANGLE (Intel, Mesa Intel(R) UHD Graphics, OpenGL 4.6)".to_owned(),
            },
            engine_hint: None,
            sticky: false,
        }
    }
}

impl ProfileMeta {
    /// Seed canvas yang jelas berbeda per profil, diturunkan dari nama.
    ///
    /// Ini bukan kriptografi: gunanya hanya memastikan dua profil tidak
    /// menghasilkan noise canvas identik, tanpa menambah dependensi acak.
    #[must_use]
    pub fn seed_dari_nama(nama: &str) -> u64 {
        // FNV-1a 64-bit: pendek, deterministik, dan tidak butuh dependensi.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in nama.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
}

/// Profil seperti yang dibaca dari database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub meta: ProfileMeta,
    pub engine_hint: Option<String>,
    pub sticky: bool,
    pub created_at: i64,
}

/// Kegagalan operasi profil.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error(
        "nama profil {0:?} tidak sah: pakai 1-{MAX_NAME_LEN} karakter [a-z0-9._-], diawali huruf atau angka"
    )]
    InvalidName(String),

    #[error(
        "profil {profile:?} sudah ada dengan UA berbeda; rotasi identitas wajib lewat profil baru, bukan mengubah profil ini"
    )]
    FrozenUa { profile: String },

    #[error("meta profil {profile:?} bukan JSON yang sah: {source}")]
    Json {
        profile: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Apakah nama profil sah: diawali huruf/angka, sisanya huruf kecil, angka,
/// titik, garis bawah, atau strip.
#[must_use]
pub fn nama_sah(nama: &str) -> bool {
    if nama.is_empty() || nama.len() > MAX_NAME_LEN {
        return false;
    }
    let mut karakter = nama.chars();
    let pertama_sah = karakter
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    pertama_sah
        && karakter
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

impl Store {
    /// Membuat profil, atau memperbarui profil yang sudah ada **tanpa** boleh
    /// mengubah UA. `created_at` profil lama dipertahankan: itu waktu identitas
    /// itu lahir, bukan waktu terakhir disentuh.
    pub fn upsert_profile(
        &self,
        nama: &str,
        meta: &ProfileMeta,
        now: i64,
    ) -> Result<Profile, ProfileError> {
        if !nama_sah(nama) {
            return Err(ProfileError::InvalidName(nama.to_owned()));
        }

        let lama = self.profile(nama)?;
        let created_at = match &lama {
            Some(ada) => {
                if ada.meta.ua != meta.ua {
                    return Err(ProfileError::FrozenUa {
                        profile: nama.to_owned(),
                    });
                }
                ada.created_at
            }
            None => now,
        };

        let meta_json = serde_json::to_string(meta).map_err(|source| ProfileError::Json {
            profile: nama.to_owned(),
            source,
        })?;

        self.conn().execute(
            "INSERT INTO profile (id, name, meta_json, engine_hint, sticky, created_at) \
             VALUES (?1, ?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(name) DO UPDATE SET \
               meta_json = excluded.meta_json, \
               engine_hint = excluded.engine_hint, \
               sticky = excluded.sticky",
            params![
                nama,
                meta_json,
                meta.engine_hint,
                i64::from(meta.sticky),
                created_at
            ],
        )?;

        self.profile(nama)?
            .ok_or_else(|| ProfileError::Sqlite(rusqlite::Error::QueryReturnedNoRows))
    }

    /// Profil berdasarkan nama. `None` berarti belum pernah dibuat.
    pub fn profile(&self, nama: &str) -> Result<Option<Profile>, ProfileError> {
        let mentah = self
            .conn()
            .query_row(
                "SELECT id, name, meta_json, engine_hint, sticky, created_at \
                 FROM profile WHERE name = ?1",
                [nama],
                baca_baris,
            )
            .optional()?;
        mentah.map(BarisMentah::jadi_profil).transpose()
    }

    /// Semua profil, urut nama. Dipakai endpoint diagnostik dan test isolasi.
    pub fn list_profiles(&self) -> Result<Vec<Profile>, ProfileError> {
        let mut stmt = self.conn().prepare(
            "SELECT id, name, meta_json, engine_hint, sticky, created_at \
             FROM profile ORDER BY name",
        )?;
        let baris = stmt.query_map([], baca_baris)?;
        let mut hasil = Vec::new();
        for satu in baris {
            hasil.push(satu?.jadi_profil()?);
        }
        Ok(hasil)
    }
}

/// Baris mentah: JSON belum di-parse, supaya galat parse bisa jadi
/// [`ProfileError::Json`] alih-alih galat SQLite yang menyesatkan.
struct BarisMentah {
    id: String,
    name: String,
    meta_json: String,
    engine_hint: Option<String>,
    sticky: bool,
    created_at: i64,
}

impl BarisMentah {
    fn jadi_profil(self) -> Result<Profile, ProfileError> {
        let meta = serde_json::from_str(&self.meta_json).map_err(|source| ProfileError::Json {
            profile: self.name.clone(),
            source,
        })?;
        Ok(Profile {
            id: self.id,
            name: self.name,
            meta,
            engine_hint: self.engine_hint,
            sticky: self.sticky,
            created_at: self.created_at,
        })
    }
}

fn baca_baris(baris: &rusqlite::Row<'_>) -> rusqlite::Result<BarisMentah> {
    Ok(BarisMentah {
        id: baris.get("id")?,
        name: baris.get("name")?,
        meta_json: baris.get("meta_json")?,
        engine_hint: baris.get("engine_hint")?,
        sticky: baris.get::<_, i64>("sticky")? != 0,
        created_at: baris.get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use crate::testutil::TempDir;

    fn toko(dir: &TempDir) -> Store {
        Store::open(dir.path().join("store.db"), crate::StoreKey::generate()).expect("buka store")
    }

    #[test]
    fn profil_baru_bisa_dibaca_kembali_utuh() {
        let dir = TempDir::new("profil");
        let store = toko(&dir);
        let meta = ProfileMeta::default();
        let dibuat = store.upsert_profile("shop", &meta, 1762).expect("buat");

        let dibaca = store.profile("shop").expect("baca").expect("ada");
        assert_eq!(dibaca, dibuat);
        assert_eq!(dibaca.meta, meta);
        assert_eq!(dibaca.created_at, 1762);
        assert_eq!(dibaca.id, "shop");
    }

    #[test]
    fn upsert_dengan_ua_sama_memperbarui_meta_dan_menjaga_created_at() {
        let dir = TempDir::new("upsert");
        let store = toko(&dir);
        let mut meta = ProfileMeta::default();
        store.upsert_profile("shop", &meta, 1000).expect("awal");

        meta.hardware_concurrency = 8;
        meta.sticky = true;
        meta.engine_hint = Some("chrome-full".to_owned());
        let diperbarui = store.upsert_profile("shop", &meta, 2000).expect("perbarui");

        assert_eq!(diperbarui.created_at, 1000, "created_at harus waktu lahir");
        assert_eq!(diperbarui.meta.hardware_concurrency, 8);
        assert!(diperbarui.sticky);
        assert_eq!(diperbarui.engine_hint.as_deref(), Some("chrome-full"));
    }

    #[test]
    fn ua_berubah_ditolak_dan_pesan_menyebut_profil() {
        let dir = TempDir::new("frozen");
        let store = toko(&dir);
        let meta = ProfileMeta::default();
        store.upsert_profile("shop", &meta, 1000).expect("awal");

        let mut berubah = meta.clone();
        berubah.ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/157.0.0.0".to_owned();
        let err = store
            .upsert_profile("shop", &berubah, 2000)
            .expect_err("harus menolak");
        assert!(
            matches!(err, ProfileError::FrozenUa { .. }),
            "galat salah: {err}"
        );
        assert!(err.to_string().contains("shop"));

        let tetap = store.profile("shop").expect("baca").expect("ada");
        assert_eq!(tetap.meta.ua, meta.ua, "UA lama harus utuh");
    }

    #[test]
    fn nama_profil_tidak_sah_ditolak() {
        let dir = TempDir::new("nama");
        let store = toko(&dir);
        for buruk in [
            "",
            "Shop",
            "a b",
            "../x",
            "-awal",
            "x".repeat(MAX_NAME_LEN + 1).as_str(),
        ] {
            let err = store
                .upsert_profile(buruk, &ProfileMeta::default(), 1)
                .expect_err("harus menolak {buruk}");
            assert!(
                matches!(err, ProfileError::InvalidName(_)),
                "nama {buruk:?} seharusnya ditolak: {err}"
            );
        }
        assert!(nama_sah("shop-2.id_1"));
    }

    #[test]
    fn daftar_profil_urut_nama() {
        let dir = TempDir::new("daftar");
        let store = toko(&dir);
        for nama in ["zeta", "alpha", "mid"] {
            store
                .upsert_profile(nama, &ProfileMeta::default(), 1)
                .expect("buat");
        }
        let nama: Vec<String> = store
            .list_profiles()
            .expect("daftar")
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(nama, ["alpha", "mid", "zeta"]);
    }

    #[test]
    fn profil_tidak_ada_mengembalikan_none() {
        let dir = TempDir::new("none");
        let store = toko(&dir);
        assert!(store.profile("hantu").expect("baca").is_none());
    }

    #[test]
    fn seed_canvas_berbeda_antar_nama_dan_stabil() {
        let satu = ProfileMeta::seed_dari_nama("shop");
        let dua = ProfileMeta::seed_dari_nama("kerja");
        assert_ne!(satu, dua);
        assert_eq!(satu, ProfileMeta::seed_dari_nama("shop"));
    }
}
