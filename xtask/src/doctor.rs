//! `cargo xtask doctor`: memeriksa prasyarat mesin sebelum build dan uji.
//!
//! Yang gagal di sini harus gagal keras. Pemeriksaan yang cuma memberi peringatan
//! cepat atau lambat jadi kegagalan misterius di tengah e2e.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

const PLAYWRIGHT_CACHE: &str = ".cache/ms-playwright";
const PORT_CONTROL: u16 = 9223;
const PORT_DEV: u16 = 9224;

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME tidak diset")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask harus berada di dalam repo")
        .to_path_buf()
}

/// Biner engine default: shell terbaru di cache Playwright.
pub fn default_engine_binary() -> Option<PathBuf> {
    let base = home().join(PLAYWRIGHT_CACHE);
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(&base)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("chromium_headless_shell-"))
        })
        .collect();
    candidates.sort();
    candidates.into_iter().rev().find_map(|dir| {
        let bin = dir.join("chrome-headless-shell-linux64/chrome-headless-shell");
        bin.is_file().then_some(bin)
    })
}

fn port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn free_gib(path: &Path) -> Option<u64> {
    let stats = nix::sys::statvfs::statvfs(path).ok()?;
    Some(stats.blocks_available() * stats.fragment_size() / (1024 * 1024 * 1024))
}

pub fn run() -> Result<()> {
    let mut failed: Vec<String> = Vec::new();

    println!("== mesin ==");

    match Command::new("rustc").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            println!("  ok    rustc            {}", text.trim());
        }
        _ => {
            println!("  GAGAL rustc            tidak bisa dijalankan");
            failed.push("rustc".into());
        }
    }

    let binary = std::env::var_os("OCB_ENGINE_BINARY")
        .map(PathBuf::from)
        .or_else(default_engine_binary);
    match &binary {
        Some(path) if path.is_file() => {
            let size = std::fs::metadata(path).map_or(0, |meta| meta.len() / (1024 * 1024));
            println!("  ok    engine binary    {} ({size} MB)", path.display());
        }
        Some(path) => {
            println!("  GAGAL engine binary    {} tidak ada", path.display());
            failed.push("engine binary".into());
        }
        None => {
            println!("  GAGAL engine binary    tidak ditemukan di $HOME/{PLAYWRIGHT_CACHE}");
            println!(
                "        petunjuk          set OCB_ENGINE_BINARY ke biner chrome-headless-shell"
            );
            failed.push("engine binary".into());
        }
    }

    println!("\n== port ==");
    for (port, label) in [(PORT_CONTROL, "control"), (PORT_DEV, "dev")] {
        if port_free(port) {
            println!("  ok    :{port}             bebas ({label})");
        } else {
            println!("  GAGAL :{port}             sudah dipakai ({label})");
            failed.push(format!("port {port}"));
        }
    }
    let prod = if port_free(9222) {
        "bebas".to_string()
    } else {
        "dipakai (produksi, jangan disentuh)".to_string()
    };
    println!("  info  :9222            {prod}");

    println!("\n== penyimpanan ==");
    let root = repo_root();
    match free_gib(&root) {
        Some(gib) if gib >= 2 => println!("  ok    disk repo        {gib} GiB bebas"),
        Some(gib) => {
            println!("  GAGAL disk repo        hanya {gib} GiB bebas, butuh >= 2 GiB");
            failed.push("disk".into());
        }
        None => {
            println!("  GAGAL disk repo        tidak bisa dibaca");
            failed.push("disk".into());
        }
    }

    println!("\n== perkakas opsional ==");
    for (program, args, needed_by) in [
        ("node", vec!["--version"], "bench"),
        ("jq", vec!["--version"], "gate e2e"),
        ("cargo-deny", vec!["--version"], "lisensi di CI"),
    ] {
        match Command::new(program).args(&args).output() {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout);
                println!(
                    "  ok    {program:<17} {}",
                    text.lines().next().unwrap_or("").trim()
                );
            }
            _ => println!("  --    {program:<17} tidak ada (dipakai oleh: {needed_by})"),
        }
    }

    println!("\n== konfigurasi ==");
    let config = home().join(".opencrabs/browser-daemon.toml");
    if config.is_file() {
        println!("  ok    config           {}", config.display());
    } else {
        println!("  info  config           belum ada, pakai nilai bawaan CLI");
    }

    if failed.is_empty() {
        println!("\nsemua pemeriksaan wajib lulus.");
        Ok(())
    } else {
        bail!("pemeriksaan gagal: {}", failed.join(", "))
    }
}
