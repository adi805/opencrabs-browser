//! Penemuan biner engine di mesin ini.
//!
//! Ini bukan deteksi CDP dan bukan routing. Tugasnya satu: menjawab "biner
//! chrome-headless-shell mana yang ada" tanpa menebak. Kalau tidak ada, jawab
//! `None` dan biarkan pemanggil gagal dengan pesan yang jelas.

use std::path::{Path, PathBuf};

/// Lokasi relatif terhadap `$HOME` tempat Playwright menyimpan biner.
pub const PLAYWRIGHT_CACHE: &str = ".cache/ms-playwright";
pub const SHELL_SUFFIX: &str = "chrome-headless-shell-linux64/chrome-headless-shell";

/// Memilih shell dengan versi build tertinggi di dalam satu direktori cache.
///
/// Dipisah dari `discover_shell` supaya bisa diuji tanpa menyentuh `$HOME`.
pub fn pick_shell_in(base: &Path) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(base)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("chromium_headless_shell-"))
        })
        .collect();
    dirs.sort();
    dirs.into_iter().rev().find_map(|dir| {
        let bin = dir.join(SHELL_SUFFIX);
        bin.is_file().then_some(bin)
    })
}

/// Biner shell terbaru di `$HOME`, kalau ada.
#[must_use]
pub fn discover_shell() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    pick_shell_in(&Path::new(&home).join(PLAYWRIGHT_CACHE))
}

/// Kandidat biner oxi, dari yang paling spesifik ke paling umum.
///
/// `$OXI_BIN` didahulukan karena CI mengunduh atau membangun biner ke lokasi
/// yang ditentukan sendiri, dan menebak dari `$PATH` di sana hanya menambah
/// satu cara untuk salah. Oxi **tidak** punya aset rilis Linux, jadi biner
/// biasanya hasil `cargo install` di `~/.cargo/bin`.
pub const OXI_ENV: &str = "OXI_BIN";
pub const OXI_KANDIDAT: &[&str] = &[".cargo/bin/oxibrowser", ".local/bin/oxibrowser"];

/// Biner oxi yang ada di mesin ini, kalau ada.
///
/// Yang diperiksa hanya keberadaan berkas yang bisa dieksekusi; menjalankan
/// `oxibrowser version` untuk memastikan berarti menyalakan proses hanya demi
/// sebuah kecurigaan, dan boot nanti tetap akan melaporkan kegagalan yang jelas.
#[must_use]
pub fn discover_oxi() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(OXI_ENV) {
        let path = PathBuf::from(explicit);
        return bisa_dijalankan(&path).then_some(path);
    }
    let home = std::env::var_os("HOME")?;
    oxi_di_home(Path::new(&home))
}

/// Urutan pencarian di dalam satu `$HOME`. Dipisah supaya bisa diuji tanpa
/// menyentuh `$HOME` maupun `$OXI_BIN`.
fn oxi_di_home(home: &Path) -> Option<PathBuf> {
    let mut kandidat: Vec<PathBuf> = OXI_KANDIDAT.iter().map(|rel| home.join(rel)).collect();
    kandidat.push(PathBuf::from("/usr/local/bin/oxibrowser"));
    kandidat.into_iter().find(|path| bisa_dijalankan(path))
}

/// Variabel lingkungan untuk menunjuk biner Chromium penuh secara eksplisit.
///
/// Alasannya sama dengan [`OXI_ENV`]: di host yang binernya tidak ada di `$PATH`
/// (atau ada dua versi dan yang benar bukan yang pertama ketemu), menebak
/// lokasi hanya menambah satu cara untuk salah.
pub const CHROME_FULL_ENV: &str = "CHROME_FULL_BIN";

/// Nama biner Chromium penuh, dari yang paling sering dipakai ke yang paling
/// jarang. Urutannya berarti: `google-chrome` menang atas `chromium` kalau dua-
/// duanya terpasang, karena itulah yang dipakai host ini dan yang paling stabil
/// di antara keduanya.
pub const NAMA_CHROME_PENUH: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "chrome",
];

/// Direktori yang diperiksa kalau `$PATH` tidak memuat biner Chromium penuh.
pub const CHROME_PENUH_DIR: &[&str] = &[
    "/usr/bin",
    "/usr/local/bin",
    "/snap/bin",
    "/opt/google/chrome",
];

/// Variabel lingkungan untuk menunjuk biner Xvfb.
pub const XVFB_ENV: &str = "XVFB_BIN";

/// Lokasi Xvfb yang lazim. `$PATH` diperiksa lebih dulu, daftar ini cadangan.
pub const XVFB_KANDIDAT: &[&str] = &["/usr/bin/Xvfb", "/usr/local/bin/Xvfb"];

/// Chromium penuh yang terpasang di mesin ini, kalau ada.
///
/// Yang diperiksa hanya berkas yang bisa dieksekusi, sama seperti
/// [`discover_oxi`]: menjalankan `--version` untuk memastikan berarti menyalakan
/// proses hanya demi sebuah kecurigaan.
#[must_use]
pub fn discover_full() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(CHROME_FULL_ENV) {
        let path = PathBuf::from(explicit);
        return bisa_dijalankan(&path).then_some(path);
    }
    if let Some(path) = NAMA_CHROME_PENUH
        .iter()
        .find_map(|nama| di_path(nama).filter(|path| bisa_dijalankan(path)))
    {
        return Some(path);
    }
    penuh_dalam(&CHROME_PENUH_DIR.iter().map(Path::new).collect::<Vec<_>>())
}

/// Pencarian di dalam daftar direktori. Dipisah supaya bisa diuji tanpa
/// menyentuh `$PATH` maupun `$CHROME_FULL_BIN` milik proses uji.
fn penuh_dalam(dirs: &[&Path]) -> Option<PathBuf> {
    dirs.iter().find_map(|dir| {
        NAMA_CHROME_PENUH
            .iter()
            .map(|nama| dir.join(nama))
            .find(|path| bisa_dijalankan(path))
    })
}

/// Server display X virtual yang terpasang, kalau ada.
///
/// Tanpa Xvfb, `chrome-full` tidak punya tempat untuk menggambar window-nya,
/// dan adapter-nya berhenti dengan [`crate::EngineError::XvfbMissing`] alih-alih
/// mencoba `--headless` diam-diam.
#[must_use]
pub fn discover_xvfb() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(XVFB_ENV) {
        let path = PathBuf::from(explicit);
        return bisa_dijalankan(&path).then_some(path);
    }
    di_path("Xvfb").or_else(|| {
        XVFB_KANDIDAT
            .iter()
            .map(PathBuf::from)
            .find(|path| bisa_dijalankan(path))
    })
}

/// Satu nama biner di dalam `$PATH`.
fn di_path(nama: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(nama))
        .find(|kandidat| bisa_dijalankan(kandidat))
}

fn bisa_dijalankan(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_base(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ocb-probe-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("direktori uji bisa dibuat");
        dir
    }

    fn plant(base: &Path, build: &str) {
        let bin = base
            .join(format!("chromium_headless_shell-{build}"))
            .join(SHELL_SUFFIX);
        fs::create_dir_all(bin.parent().expect("punya induk")).expect("folder build");
        fs::write(&bin, b"#!/bin/sh\n").expect("biner palsu bisa ditulis");
    }

    fn biner_palsu(dir: &Path, nama: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(nama);
        fs::create_dir_all(dir).expect("folder");
        fs::write(&path, b"#!/bin/sh\n").expect("tulis");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    #[test]
    fn chrome_full_ditemukan_di_direktori_kandidat() {
        let dir = temp_base("full");
        let bin = biner_palsu(&dir, "google-chrome");
        assert_eq!(penuh_dalam(&[dir.as_path()]), Some(bin));
        fs::remove_dir_all(&dir).ok();
    }

    /// Dua biner terpasang berarti satu harus menang, dan pilihannya tidak boleh
    /// bergantung pada urutan `read_dir`. `google-chrome` yang dipakai host ini,
    /// jadi dia yang menang.
    #[test]
    fn google_chrome_menang_atas_chromium() {
        let dir = temp_base("urutan-full");
        let chrome = biner_palsu(&dir, "google-chrome");
        biner_palsu(&dir, "chromium");
        assert_eq!(penuh_dalam(&[dir.as_path()]), Some(chrome));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn direktori_tanpa_chrome_dilewati() {
        let kosong = temp_base("kosong-full");
        let isi = temp_base("isi-full");
        let bin = biner_palsu(&isi, "chromium");
        assert_eq!(penuh_dalam(&[kosong.as_path(), isi.as_path()]), Some(bin));
        fs::remove_dir_all(&kosong).ok();
        fs::remove_dir_all(&isi).ok();
    }

    #[test]
    fn chrome_tanpa_bit_eksekusi_bukan_kandidat() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_base("full-noexec");
        let path = dir.join("google-chrome");
        fs::write(&path, b"bukan-biner").expect("tulis");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
        assert_eq!(penuh_dalam(&[dir.as_path()]), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn memilih_build_tertinggi_bukan_yang_pertama_terbaca() {
        let base = temp_base("pick");
        plant(&base, "1000");
        plant(&base, "1248");
        plant(&base, "1100");

        let picked = pick_shell_in(&base).expect("harus menemukan satu");
        let text = picked.display().to_string();
        assert!(text.contains("1248"), "dapat: {text}");
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn direktori_tanpa_biner_diabaikan() {
        let base = temp_base("empty");
        fs::create_dir_all(base.join("chromium_headless_shell-1248")).expect("folder kosong");

        assert!(
            pick_shell_in(&base).is_none(),
            "folder tanpa biner bukan kandidat"
        );
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn direktori_tidak_ada_balik_none_tanpa_panic() {
        assert!(pick_shell_in(Path::new("/tidak/ada/sama/sekali")).is_none());
    }

    fn temp_oxi(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ocb-oxi-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("direktori uji");
        dir
    }

    #[test]
    fn oxi_ditemukan_di_cargo_bin() {
        use std::os::unix::fs::PermissionsExt;
        let home = temp_oxi("cargo");
        let bin = home.join(".cargo/bin/oxibrowser");
        fs::create_dir_all(bin.parent().expect("induk")).expect("folder");
        fs::write(&bin, b"#!/bin/sh\n").expect("tulis");
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).expect("chmod");

        assert_eq!(oxi_di_home(&home), Some(bin));
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn berkas_oxi_tanpa_bit_eksekusi_bukan_kandidat() {
        use std::os::unix::fs::PermissionsExt;
        let home = temp_oxi("noexec");
        let bin = home.join(".local/bin/oxibrowser");
        fs::create_dir_all(bin.parent().expect("induk")).expect("folder");
        fs::write(&bin, b"bukan-biner").expect("tulis");
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o644)).expect("chmod");

        let hasil = oxi_di_home(&home);
        assert!(
            hasil.is_none() || hasil.as_deref() != Some(bin.as_path()),
            "berkas tanpa bit eksekusi tidak boleh dipilih: {hasil:?}"
        );
        fs::remove_dir_all(&home).ok();
    }
}
