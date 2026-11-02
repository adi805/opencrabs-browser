//! `cargo xtask bench`: menjalankan harness di `bench/`.
//!
//! Harness-nya JavaScript karena aslinya dari `/tmp/bx` dan sudah terbukti
//! mengukur RSS/PSS dengan benar. Menulis ulang di Rust berarti membuang
//! alat ukur yang sudah tervalidasi, dan alat ukur yang salah lebih buruk
//! daripada tidak ada alat ukur.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

fn repo_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask harus berada di dalam repo")
        .map(Path::to_path_buf)
}

pub fn run(args: &[String]) -> Result<()> {
    let root = repo_root()?;
    let script = root.join("bench/bench-tabs.mjs");
    if !script.is_file() {
        bail!("harness tidak ada: {}", script.display());
    }

    let status = Command::new("node")
        .arg(&script)
        .args(args)
        .current_dir(&root)
        .status()
        .context("node tidak bisa dijalankan; pasang node atau lewati bench")?;

    if status.success() {
        Ok(())
    } else {
        bail!("harness bench keluar dengan status {status}")
    }
}
