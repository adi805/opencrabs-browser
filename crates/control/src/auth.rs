//! Token kontrol: satu-satunya yang memisahkan proses lain di mesin yang sama
//! dari kemampuan mengklik di sesi owner.
//!
//! Kenapa token dibandingkan constant-time: perbandingan biasa berhenti di byte
//! pertama yang berbeda, dan waktu yang terukur bisa dipakai menebak token satu
//! byte demi satu byte. Ini bukan ancaman teoretis untuk port yang dibuka di
//! mesin bersama.
//!
//! Kenapa berkas terpisah, bukan `keys.toml`: `keys.toml` milik OpenCrabs dan
//! skemanya dipakai bersama seluruh agen. Menulis kunci daemon ke sana berarti
//! dua program mengubah satu berkas dengan aturan berbeda. Berkas sendiri
//! dengan mode 0600 memberi kontrol yang sama tanpa risiko itu.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};

/// Panjang token dalam byte sebelum di-hex. 32 byte = 256 bit, jauh di atas
/// cukup untuk token yang hanya hidup di loopback.
const TOKEN_BYTES: usize = 32;

/// Error yang semua pesannya boleh dibaca operator, tanpa memuat token.
#[derive(Debug)]
pub enum AuthError {
    Missing,
    Malformed,
    Rejected,
    Io(std::io::Error),
    InsecurePermissions { path: PathBuf, mode: u32 },
    Empty,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "header Authorization tidak ada"),
            Self::Malformed => write!(f, "header Authorization bukan 'Bearer <token>'"),
            Self::Rejected => write!(f, "token ditolak"),
            Self::Io(err) => write!(f, "berkas token tidak bisa dipakai: {err}"),
            Self::InsecurePermissions { path, mode } => write!(
                f,
                "mode berkas token {} adalah {:o}, wajib 600",
                path.display(),
                mode
            ),
            Self::Empty => write!(f, "berkas token kosong"),
        }
    }
}

impl std::error::Error for AuthError {}

impl From<std::io::Error> for AuthError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Token yang tidak pernah mencetak dirinya sendiri.
///
/// `Debug` ditulis manual karena `#[derive(Debug)]` akan mencetak isinya ke log
/// begitu ada satu `tracing::debug!(?token)`.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Membandingkan tanpa keluar lebih awal saat byte pertama berbeda.
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        constant_time_eq(self.0.as_bytes(), candidate.as_bytes())
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(<disembunyikan>)")
    }
}

/// Membaca header `Authorization` menjadi token, atau menolak.
pub fn parse_bearer(header: Option<&str>) -> Result<&str, AuthError> {
    let raw = header.ok_or(AuthError::Missing)?;
    let (scheme, value) = raw.split_once(' ').ok_or(AuthError::Malformed)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(AuthError::Malformed);
    }
    let value = value.trim();
    if value.is_empty() {
        return Err(AuthError::Malformed);
    }
    Ok(value)
}

/// Membaca token dari berkas, dan menolak berkas yang bisa dibaca orang lain.
///
/// Mode diperiksa, bukan diperbaiki: kalau mode-nya salah, itu tanda berkasnya
/// dibuat dengan cara yang tidak seharusnya, dan memperbaikinya diam-diam
/// menghapus jejaknya.
pub fn read_token_file(path: &Path) -> Result<Token, AuthError> {
    periksa_mode(path)?;
    let mut isi = String::new();
    File::open(path)?.read_to_string(&mut isi)?;
    let token = isi.trim().to_owned();
    if token.is_empty() {
        return Err(AuthError::Empty);
    }
    Ok(Token(token))
}

/// Membuat token baru di `path` dengan mode 0600.
///
/// Berkas yang sudah ada **tidak ditimpa**: menimpa berarti setiap restart
/// memutus klien yang sedang memakai token lama, dan itu tidak perlu.
pub fn ensure_token_file(path: &Path) -> Result<Token, AuthError> {
    if path.is_file() {
        return read_token_file(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        periksa_mode_dir(parent)?;
    }

    let token = random_token()?;
    match open_private(path)? {
        Some(mut berkas) => {
            berkas.write_all(token.as_bytes())?;
            berkas.write_all(b"\n")?;
            berkas.sync_all()?;
        }
        // Proses lain membuat berkas yang sama di antara pemeriksaan dan open.
        // Itu bukan kegagalan: berkasnya sudah ada, dan tokennyalah yang
        // dipakai. Membaca ulang di bawah membuat dua jalur ini bertemu.
        None => {}
    }
    read_token_file(path)
}

/// `Ok(None)` berarti berkas sudah dibuat proses lain lebih dulu.
fn open_private(path: &Path) -> Result<Option<File>, AuthError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(file) => Ok(Some(file)),
        Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok(None),
        Err(err) => Err(AuthError::Io(err)),
    }
}

#[cfg(unix)]
fn periksa_mode(path: &Path) -> Result<(), AuthError> {
    use std::os::unix::fs::PermissionsExt as _;
    let meta = std::fs::metadata(path)?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(AuthError::InsecurePermissions {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn periksa_mode(_path: &Path) -> Result<(), AuthError> {
    Ok(())
}

#[cfg(unix)]
fn periksa_mode_dir(path: &Path) -> Result<(), AuthError> {
    // Direktori boleh 700 atau lebih ketat. 755 ditolak karena di mesin
    // bersama artinya orang lain bisa mengganti berkas tokennya.
    use std::os::unix::fs::PermissionsExt as _;
    let meta = std::fs::metadata(path)?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(AuthError::InsecurePermissions {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn periksa_mode_dir(_path: &Path) -> Result<(), AuthError> {
    Ok(())
}

fn random_token() -> Result<String, AuthError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut out = String::with_capacity(TOKEN_BYTES * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    // Panjang token bukan rahasia: token selalu 64 karakter hex. Yang dijaga
    // adalah isi byte-nya, jadi perbandingan di bawah tidak pernah berhenti
    // lebih awal.
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Berkas uji selalu di dalam direktori mode 0700 milik test ini.
    /// Bukan langsung di `/tmp`: `/tmp` itu 1777, dan `ensure_token_file`
    /// menolak direktori yang bisa ditulis orang lain, jadi menaruh berkas uji
    /// di sana akan menguji penolakan itu, bukan pembuatan token.
    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ocb-token-uji-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("direktori uji");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        dir.join(name)
    }

    #[test]
    fn bearer_dibaca_dan_scheme_tidak_peka_huruf() {
        assert_eq!(parse_bearer(Some("Bearer abc")).unwrap(), "abc");
        assert_eq!(parse_bearer(Some("bearer   abc  ")).unwrap(), "abc");
        assert!(matches!(parse_bearer(None), Err(AuthError::Missing)));
        assert!(matches!(
            parse_bearer(Some("Basic abc")),
            Err(AuthError::Malformed)
        ));
        assert!(matches!(
            parse_bearer(Some("Bearer")),
            Err(AuthError::Malformed)
        ));
        assert!(matches!(
            parse_bearer(Some("Bearer   ")),
            Err(AuthError::Malformed)
        ));
    }

    #[test]
    fn perbandingan_menolak_yang_beda_di_akhir() {
        let token = Token::new("abcdef");
        assert!(token.matches("abcdef"));
        assert!(!token.matches("abcdeX"));
        assert!(!token.matches("abcdefg"));
        assert!(!token.matches(""));
    }

    #[test]
    fn debug_tidak_mencetak_isi_token() {
        let token = Token::new("rahasia-sekali");
        let teks = format!("{token:?}");
        assert!(!teks.contains("rahasia"), "token bocor ke Debug: {teks}");
    }

    #[test]
    fn token_dibuat_dengan_mode_600_dan_isi_hex() {
        let path = temp_path("buat");
        let _ = std::fs::remove_file(&path);
        let token = ensure_token_file(&path).expect("token harus dibuat");

        assert_eq!(token.as_str().len(), TOKEN_BYTES * 2);
        assert!(token.as_str().chars().all(|c| c.is_ascii_hexdigit()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "mode berkas token: {mode:o}");
        }

        let lagi = ensure_token_file(&path).expect("token lama harus dipakai");
        assert_eq!(token.as_str(), lagi.as_str(), "berkas tidak boleh ditimpa");
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn berkas_yang_terbuka_ditolak() {
        use std::os::unix::fs::PermissionsExt as _;
        let path = temp_path("terbuka");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "token").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let err = read_token_file(&path).expect_err("mode 644 harus ditolak");
        assert!(
            matches!(err, AuthError::InsecurePermissions { .. }),
            "dapat: {err}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn berkas_kosong_ditolak() {
        let path = temp_path("kosong");
        let _ = std::fs::remove_file(&path);
        ensure_token_file(&path).expect("siapkan berkas");
        std::fs::write(&path, "  \n").unwrap();
        let err = read_token_file(&path).expect_err("kosong harus ditolak");
        assert!(matches!(err, AuthError::Empty));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn token_acak_berbeda_tiap_kali() {
        let a = random_token().expect("urandom");
        let b = random_token().expect("urandom");
        assert_ne!(a, b);
    }
}
