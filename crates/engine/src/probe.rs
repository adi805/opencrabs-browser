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
