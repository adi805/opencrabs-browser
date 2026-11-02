//! Satu pintu untuk semua perintah build, cek, bench, dan rilis.
//!
//! Tidak memakai `just`: mesin pengembangan tidak punya `just`, dan logika di
//! sini adalah Rust yang bisa diuji serta diberi pesan error yang jelas.

#![forbid(unsafe_code)]

mod bench;
mod check;
mod doctor;

use std::process::ExitCode;

fn print_help() {
    println!(
        "penggunaan: cargo xtask <perintah>

perintah:
  doctor   cek mesin: biner engine, port bebas, disk, konfigurasi
  check    fmt + clippy -D warnings + test unit
  bench    jalankan harness bench/ (argumen diteruskan ke node)
"
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();

    let result = match cmd {
        "doctor" => doctor::run(),
        "check" => check::run(),
        "bench" => bench::run(&rest),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("perintah tidak dikenal: {other}");
            print_help();
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("\ngagal: {err:#}");
            ExitCode::FAILURE
        }
    }
}
