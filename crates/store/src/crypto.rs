//! Kunci penyimpanan dan enkripsi nilai rahasia (nilai cookie).
//!
//! Kenapa XChaCha20-Poly1305 dan bukan "hash saja": nilai cookie harus bisa
//! dibaca kembali untuk dipakai ulang, jadi butuh enkripsi dua arah yang
//! sekaligus mendeteksi perubahan byte. Nonce 24 byte dipilih supaya aman
//! dibuat acak tanpa penghitung bersama.
//!
//! Berkas kunci **tidak pernah ditimpa**. Menimpa kunci berarti seluruh cookie
//! lama jadi tidak terbaca, dan itu kegagalan yang sunyi: daemon tetap jalan,
//! tapi semua situs minta login ulang tanpa sebab yang terlihat.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, Generate, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

/// Panjang kunci (byte). XChaCha20 memakai kunci 256 bit.
pub const KEY_BYTES: usize = 32;
/// Panjang nonce (byte). 24 byte adalah yang membuatnya "X"ChaCha.
pub const NONCE_BYTES: usize = 24;
/// Versi format amplop. Byte pertama, supaya format bisa berubah tanpa menebak.
const ENVELOPE_VERSION: u8 = 1;
/// Tag autentikasi Poly1305 selalu 16 byte.
const TAG_BYTES: usize = 16;
/// Amplop terpendek yang mungkin: versi + nonce + tag.
const MIN_ENVELOPE: usize = 1 + NONCE_BYTES + TAG_BYTES;

/// Kegagalan yang mungkin terjadi saat memuat atau memakai kunci.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("berkas kunci {0} tidak ada; jalankan `browserd keygen` lebih dulu")]
    Missing(PathBuf),

    #[error("membaca berkas kunci {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("berkas kunci {path} bermode {mode:o}, harus 0600 (jalankan `chmod 600 {path}`)")]
    Permissions { path: PathBuf, mode: u32 },

    #[error("isi berkas kunci {0} bukan hex 32 byte dan bukan TOML [browser_daemon].store_key")]
    Malformed(PathBuf),

    #[error("berkas kunci {0} sudah ada; kunci lama sengaja tidak ditimpa")]
    Exists(PathBuf),

    #[error("kunci salah, amplop rusak, atau ciphertext diubah")]
    Aead,

    #[error("amplop hanya {0} byte, minimal {MIN_ENVELOPE}")]
    EnvelopeTooShort(usize),

    #[error("versi amplop {0} tidak dikenal (kode ini paham versi {ENVELOPE_VERSION})")]
    EnvelopeVersion(u8),
}

/// Kunci 32 byte. `Debug` sengaja tidak mencetak isinya: kunci yang muncul di
/// log sama saja dengan kunci yang bocor.
#[derive(Clone)]
pub struct StoreKey {
    bytes: [u8; KEY_BYTES],
}

impl std::fmt::Debug for StoreKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreKey(<disembunyikan>)")
    }
}

impl StoreKey {
    /// Kunci acak dari sumber entropi sistem.
    #[must_use]
    pub fn generate() -> Self {
        let key = chacha20poly1305::Key::generate();
        let mut bytes = [0u8; KEY_BYTES];
        bytes.copy_from_slice(key.as_slice());
        Self { bytes }
    }

    /// Kunci dari hex 64 karakter. Spasi dan baris baru diabaikan.
    pub fn from_hex(text: &str) -> Result<Self, KeyError> {
        let bersih: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if bersih.len() != KEY_BYTES * 2 {
            return Err(KeyError::Malformed(PathBuf::from("<hex>")));
        }
        let mut bytes = [0u8; KEY_BYTES];
        // `as_chunks` (bukan `chunks_exact`) karena ukurannya konstanta: clippy
        // menandai `chunks_exact(2)` dan hasilnya setara tanpa cabang sisa.
        for (i, pasangan) in bersih.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let tinggi = hex_nilai(pasangan[0])
                .ok_or_else(|| KeyError::Malformed(PathBuf::from("<hex>")))?;
            let rendah = hex_nilai(pasangan[1])
                .ok_or_else(|| KeyError::Malformed(PathBuf::from("<hex>")))?;
            bytes[i] = (tinggi << 4) | rendah;
        }
        Ok(Self { bytes })
    }

    /// Hex huruf kecil, 64 karakter.
    #[must_use]
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(KEY_BYTES * 2);
        for byte in self.bytes {
            out.push(hex_karakter(byte >> 4));
            out.push(hex_karakter(byte & 0x0f));
        }
        out
    }

    /// Mengenkripsi plaintext jadi amplop `versi || nonce || ciphertext+tag`.
    ///
    /// Nonce dibuat acak tiap panggilan. Dua `seal` dengan plaintext sama
    /// menghasilkan byte berbeda, dan itu memang yang diinginkan: nonce yang
    /// terpakai ulang mematikan seluruh jaminan AEAD.
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, KeyError> {
        let cipher = self.cipher()?;
        let nonce = XNonce::generate();
        let ciphertext = cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| KeyError::Aead)?;

        let mut amplop = Vec::with_capacity(1 + NONCE_BYTES + ciphertext.len());
        amplop.push(ENVELOPE_VERSION);
        amplop.extend_from_slice(nonce.as_slice());
        amplop.extend_from_slice(&ciphertext);
        Ok(amplop)
    }

    /// Membuka amplop. Byte yang diubah satu saja membuat ini gagal, bukan
    /// mengembalikan plaintext yang salah.
    pub fn open(&self, amplop: &[u8]) -> Result<Vec<u8>, KeyError> {
        if amplop.len() < MIN_ENVELOPE {
            return Err(KeyError::EnvelopeTooShort(amplop.len()));
        }
        if amplop[0] != ENVELOPE_VERSION {
            return Err(KeyError::EnvelopeVersion(amplop[0]));
        }
        let nonce_bytes: [u8; NONCE_BYTES] = amplop[1..1 + NONCE_BYTES]
            .try_into()
            .map_err(|_| KeyError::Aead)?;
        let nonce = XNonce::from(nonce_bytes);
        let cipher = self.cipher()?;
        cipher
            .decrypt(&nonce, &amplop[1 + NONCE_BYTES..])
            .map_err(|_| KeyError::Aead)
    }

    /// Membaca kunci dari berkas. Dua bentuk diterima:
    ///
    /// - hex mentah (64 karakter), bentuk bawaan `browserd keygen`;
    /// - TOML dengan `[browser_daemon] store_key = "..."`, bentuk yang
    ///   dirancang di dokumen 02, supaya berkas `keys.toml` OpenCrabs bisa
    ///   dipakai langsung tanpa menyalin apa pun.
    ///
    /// Berkas yang bisa dibaca pengguna lain ditolak, bukan diperbaiki diam-diam:
    /// chmod adalah tindakan operator, dan daemon tidak boleh mengambil alih.
    pub fn load(path: &Path) -> Result<Self, KeyError> {
        if !path.is_file() {
            return Err(KeyError::Missing(path.to_path_buf()));
        }

        let metadata = fs::metadata(path).map_err(|source| KeyError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(KeyError::Permissions {
                path: path.to_path_buf(),
                mode,
            });
        }

        let text = fs::read_to_string(path).map_err(|source| KeyError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        if let Ok(kunci) = Self::from_hex(&text) {
            return Ok(kunci);
        }

        let parsed: KeysFile =
            toml::from_str(&text).map_err(|_| KeyError::Malformed(path.to_path_buf()))?;
        let hex = parsed
            .browser_daemon
            .and_then(|bagian| bagian.store_key)
            .ok_or_else(|| KeyError::Malformed(path.to_path_buf()))?;
        Self::from_hex(&hex).map_err(|_| KeyError::Malformed(path.to_path_buf()))
    }

    /// Membuat kunci baru di `path`, menolak kalau berkasnya sudah ada.
    /// Direktori induk dibuat dengan mode 0700.
    pub fn create(path: &Path) -> Result<Self, KeyError> {
        if path.exists() {
            return Err(KeyError::Exists(path.to_path_buf()));
        }
        if let Some(induk) = path.parent()
            && !induk.as_os_str().is_empty()
        {
            fs::create_dir_all(induk).map_err(|source| KeyError::Io {
                path: induk.to_path_buf(),
                source,
            })?;
            fs::set_permissions(induk, fs::Permissions::from_mode(0o700)).map_err(|source| {
                KeyError::Io {
                    path: induk.to_path_buf(),
                    source,
                }
            })?;
        }

        let kunci = Self::generate();
        let mut berkas = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|source| KeyError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        writeln!(berkas, "{}", kunci.to_hex()).map_err(|source| KeyError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        berkas.flush().map_err(|source| KeyError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(kunci)
    }

    fn cipher(&self) -> Result<XChaCha20Poly1305, KeyError> {
        XChaCha20Poly1305::new_from_slice(&self.bytes).map_err(|_| KeyError::Aead)
    }
}

#[derive(Debug, serde::Deserialize)]
struct KeysFile {
    #[serde(default)]
    browser_daemon: Option<BrowserDaemonSection>,
}

#[derive(Debug, serde::Deserialize)]
struct BrowserDaemonSection {
    #[serde(default)]
    store_key: Option<String>,
}

fn hex_nilai(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_karakter(nilai: u8) -> char {
    match nilai {
        0..=9 => char::from(b'0' + nilai),
        _ => char::from(b'a' + (nilai - 10)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn roundtrip_menghasilkan_plaintext_sama() {
        let kunci = StoreKey::generate();
        let amplop = kunci.seal(b"cf_clearance=rahasia").expect("segel");
        assert_ne!(amplop, b"cf_clearance=rahasia");
        assert_eq!(kunci.open(&amplop).expect("buka"), b"cf_clearance=rahasia");
    }

    #[test]
    fn nonce_acak_membuat_dua_segel_berbeda() {
        let kunci = StoreKey::generate();
        let satu = kunci.seal(b"sama").expect("segel");
        let dua = kunci.seal(b"sama").expect("segel");
        assert_ne!(satu, dua, "nonce yang dipakai ulang mematikan AEAD");
        assert_eq!(
            kunci.open(&satu).expect("buka"),
            kunci.open(&dua).expect("buka")
        );
    }

    #[test]
    fn ciphertext_tidak_memuat_plaintext() {
        let kunci = StoreKey::generate();
        let rahasia = b"nilai-cookie-yang-tidak-boleh-terbaca";
        let amplop = kunci.seal(rahasia).expect("segel");
        assert!(
            !amplop
                .windows(rahasia.len())
                .any(|jendela| jendela == rahasia),
            "plaintext bocor ke ciphertext"
        );
    }

    #[test]
    fn satu_byte_diubah_ditolak() {
        let kunci = StoreKey::generate();
        let mut amplop = kunci.seal(b"asli").expect("segel");
        let terakhir = amplop.len() - 1;
        amplop[terakhir] ^= 0x01;
        assert!(matches!(kunci.open(&amplop), Err(KeyError::Aead)));
    }

    #[test]
    fn kunci_berbeda_tidak_bisa_membuka() {
        let amplop = StoreKey::generate().seal(b"asli").expect("segel");
        let lain = StoreKey::generate();
        assert!(matches!(lain.open(&amplop), Err(KeyError::Aead)));
    }

    #[test]
    fn amplop_pendek_ditolak_sebelum_dekripsi() {
        let kunci = StoreKey::generate();
        assert!(matches!(
            kunci.open(&[ENVELOPE_VERSION; MIN_ENVELOPE - 1]),
            Err(KeyError::EnvelopeTooShort(_))
        ));
    }

    #[test]
    fn versi_amplop_asing_ditolak() {
        let kunci = StoreKey::generate();
        let mut amplop = kunci.seal(b"asli").expect("segel");
        amplop[0] = 99;
        assert!(matches!(
            kunci.open(&amplop),
            Err(KeyError::EnvelopeVersion(99))
        ));
    }

    #[test]
    fn hex_bolak_balik_mempertahankan_kunci() {
        let kunci = StoreKey::generate();
        let lagi = StoreKey::from_hex(&kunci.to_hex()).expect("parse");
        let amplop = kunci.seal(b"x").expect("segel");
        assert_eq!(lagi.open(&amplop).expect("buka"), b"x");
    }

    #[test]
    fn hex_pendek_ditolak() {
        assert!(StoreKey::from_hex("abcd").is_err());
    }

    #[test]
    fn berkas_kunci_dibuat_0600_dan_bisa_dibaca() {
        let dir = TempDir::new("kunci");
        let path = dir.path().join("store.key");
        let asli = StoreKey::create(&path).expect("buat");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "kunci harus 0600, bukan {mode:o}");
        let dibaca = StoreKey::load(&path).expect("muat");
        let amplop = asli.seal(b"uji").expect("segel");
        assert_eq!(dibaca.open(&amplop).expect("buka"), b"uji");
    }

    #[test]
    fn kunci_lama_tidak_ditimpa() {
        let dir = TempDir::new("timpa");
        let path = dir.path().join("store.key");
        StoreKey::create(&path).expect("buat");
        assert!(matches!(StoreKey::create(&path), Err(KeyError::Exists(_))));
    }

    #[test]
    fn berkas_hilang_ditolak_dengan_jalur_jelas() {
        let dir = TempDir::new("hilang");
        let path = dir.path().join("tidak-ada.key");
        assert!(matches!(StoreKey::load(&path), Err(KeyError::Missing(_))));
    }

    #[test]
    fn berkas_terlalu_terbuka_ditolak() {
        let dir = TempDir::new("perm");
        let path = dir.path().join("store.key");
        StoreKey::create(&path).expect("buat");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(matches!(
            StoreKey::load(&path),
            Err(KeyError::Permissions { .. })
        ));
    }

    #[test]
    fn kunci_dibaca_dari_toml_browser_daemon() {
        let dir = TempDir::new("toml");
        let path = dir.path().join("keys.toml");
        let kunci = StoreKey::generate();
        let isi = format!(
            "[agent]\ntoken = \"jangan-disentuh\"\n\n[browser_daemon]\nstore_key = \"{}\"\n",
            kunci.to_hex()
        );
        let mut berkas = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .expect("buat");
        berkas.write_all(isi.as_bytes()).expect("tulis");
        // Berkasnya harus tertutup sebelum dibaca ulang: buffering bukan teman.
        drop(berkas);

        let dibaca = StoreKey::load(&path).expect("muat dari toml");
        let amplop = kunci.seal(b"toml").expect("segel");
        assert_eq!(dibaca.open(&amplop).expect("buka"), b"toml");
    }

    #[test]
    fn toml_tanpa_store_key_ditolak() {
        let dir = TempDir::new("toml-kosong");
        let path = dir.path().join("keys.toml");
        fs::write(&path, "[agent]\ntoken = \"x\"\n").expect("tulis");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
        assert!(matches!(StoreKey::load(&path), Err(KeyError::Malformed(_))));
    }
}
