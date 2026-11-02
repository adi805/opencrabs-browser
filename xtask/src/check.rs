//! `cargo xtask check`: gerbang yang sama dengan CI, dijalankan lokal.
//!
//! Semua langkah dijalankan meski ada yang gagal, supaya satu putaran
//! mengungkap semua masalah, bukan satu per satu.

use std::process::Command;

use anyhow::{Result, bail};

struct Step {
    name: &'static str,
    program: &'static str,
    args: &'static [&'static str],
}

const STEPS: &[Step] = &[
    Step {
        name: "fmt",
        program: "cargo",
        args: &["fmt", "--all", "--", "--check"],
    },
    Step {
        name: "clippy",
        program: "cargo",
        args: &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    },
    Step {
        name: "test",
        program: "cargo",
        args: &["test", "--workspace"],
    },
];

pub fn run() -> Result<()> {
    let mut failed: Vec<&str> = Vec::new();

    for step in STEPS {
        print!("-> {:<8}", step.name);
        let output = Command::new(step.program)
            .args(step.args)
            .output()
            .map_err(|err| anyhow::anyhow!("{} tidak bisa dijalankan: {err}", step.program))?;

        if output.status.success() {
            println!("ok");
            continue;
        }

        println!("GAGAL");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for line in stdout.lines().chain(stderr.lines()) {
            println!("   {line}");
        }
        failed.push(step.name);
    }

    if failed.is_empty() {
        Ok(())
    } else {
        bail!("langkah gagal: {}", failed.join(", "))
    }
}
