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
}
