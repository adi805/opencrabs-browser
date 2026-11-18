//! Regresi issue #1: teardown uji tidak boleh meninggalkan mesin yatim.
//!
//! # Yang dibuktikan
//!
//! Daemon yang dimatikan dengan `Child::kill()` menerima SIGKILL, jadi ia tidak
//! pernah sampai ke `matikan_semua`. Setiap mesin yang sudah dinyalakannya
//! tinggal sebagai proses yatim: di-reparent ke PID 1, memegang RAM, port, dan
//! profil di `/tmp` sampai host di-reboot. Satu run gate pernah meninggalkan 26
//! mesin seperti itu, RAM habis, lalu uji berikutnya gagal boot dan gate terbaca
//! merah padahal kodenya tidak berubah.
//!
//! Perbaikannya ada di dua tempat yang sudah ada: [`common::buang`] mengirim
//! SIGTERM lebih dulu dan baru SIGKILL kalau daemon membandel, dan `Proc`
//! membunuh group prosesnya saat di-drop. Uji ini menjaga keduanya tetap ada:
//! daemon dinyalakan, satu mesin dinyalakan, teardown dijalankan, lalu `/proc`
//! diperiksa. Tanpa uji ini, `buang` bisa kembali menjadi `child.kill()` tanpa
//! ada yang gagal.
//!
//! # Kenapa mesin palsu, bukan Chrome
//!
//! Yang diuji adalah jalur teardown, bukan mesinnya. Kalau uji ini menuntut
//! `chrome-headless-shell`, ia hanya jalan di host yang punya Chrome dan tidak
//! akan pernah menahan regresi di CI: dilewati berarti tidak lulus, dan regresi
//! tetap masuk. Mesin palsu cukup menulis `DevToolsActivePort` dan menjawab
//! `/json/version`, dan itu sudah membuat `ShellEngine` menahannya persis
//! seperti mesin asli, termasuk `process_group(0)` yang membuatnya yatim kalau
//! group-nya tidak dibunuh.
//!
//! Kalau `python3` tidak ada, uji dilewati dengan pesan yang menyebut apa yang
//! kurang. Dilewati bukan berarti lulus.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod common;

const BATAS_SIAP: Duration = Duration::from_secs(60);
const BATAS_MATI: Duration = Duration::from_secs(30);

/// Mesin palsu: meniru bagian `chrome-headless-shell` yang diperiksa
/// `ShellEngine::launch`, lalu hidup diam sampai dibunuh.
const SKRIP_MESIN: &str = r#"#!/usr/bin/env python3
import os
import socket
import sys
import threading

port = 0
profil = None
for arg in sys.argv[1:]:
    if arg.startswith("--remote-debugging-port="):
        port = int(arg.split("=", 1)[1])
    elif arg.startswith("--user-data-dir="):
        profil = arg.split("=", 1)[1]

if profil is None:
    sys.exit("mesin palsu: --user-data-dir tidak diberikan")

server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", port))
server.listen(8)
port = server.getsockname()[1]

with open(os.path.join(profil, "DevToolsActivePort"), "w") as berkas:
    berkas.write("%d\n/devtools/browser/palsu\n" % port)

body = (
    '{"Browser":"HeadlessChrome/palsu","Protocol-Version":"1.3",'
    '"User-Agent":"palsu","webSocketDebuggerUrl":"ws://127.0.0.1:%d/devtools/browser/palsu"}'
    % port
).encode()


def layani(koneksi):
    try:
        koneksi.settimeout(2)
        koneksi.recv(4096)
        koneksi.sendall(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
            + b"Content-Length: "
            + str(len(body)).encode()
            + b"\r\n\r\n"
            + body
        )
    except OSError:
        pass
    finally:
        try:
            koneksi.close()
        except OSError:
            pass


while True:
    koneksi, _ = server.accept()
    threading.Thread(target=layani, args=(koneksi,), daemon=True).start()
"#;

/// Satu proses di `/proc` yang baris perintahnya memuat penanda uji.
struct Proses {
    pid: u32,
    ppid: u32,
    perintah: String,
}

#[test]
fn teardown_daemon_tidak_meninggalkan_mesin() {
    if !ada_python() {
        eprintln!(
            "SKIP teardown_yatim: python3 tidak ditemukan, mesin palsu tidak bisa \
             dijalankan. Uji ini tidak butuh Chrome, tapi butuh python3 untuk \
             meniru discovery-nya."
        );
        return;
    }

    // Uji live diserialkan supaya tidak berebut CPU dengan uji live lain yang
    // berjalan paralel. Alasan lengkapnya di `tests/common/mod.rs`.
    let _kunci = common::kunci_live("teardown_yatim");

    let dir = direktori_uji();
    let profil = dir.join("profil");
    let skrip = dir.join("mesin-palsu.py");
    std::fs::write(&skrip, SKRIP_MESIN).expect("menulis mesin palsu");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&skrip, std::fs::Permissions::from_mode(0o755))
            .expect("menandai mesin palsu bisa dieksekusi");
    }

    let kunci = dir.join("store.key");
    let db = dir.join("store.db");
    let token_path = dir.join("control.token");
    let config_path = dir.join("browser-daemon.toml");
    let control_port = port_kosong();
    let cdp_port = port_kosong();

    // `idle_reap_secs = 0` supaya mesin tidak di-reap di tengah uji: yang
    // diperiksa adalah teardown, bukan reaper.
    std::fs::write(
        &config_path,
        format!(
            "[daemon]\ncontrol_bind = \"127.0.0.1:{control_port}\"\n\
             control_token = \"{}\"\nstore_db = \"{}\"\nstore_key = \"{}\"\n\
             idle_reap_secs = 0\n",
            token_path.display(),
            db.display(),
            kunci.display()
        ),
    )
    .expect("menulis config uji");

    let keygen = Command::new(env!("CARGO_BIN_EXE_browserd"))
        .args([
            "keygen",
            "--key-file",
            kunci.to_str().expect("path utf8"),
            "--db",
            db.to_str().expect("path utf8"),
        ])
        .output()
        .expect("menjalankan keygen");
    assert!(
        keygen.status.success(),
        "keygen gagal: {}",
        String::from_utf8_lossy(&keygen.stderr)
    );

    let mut daemon = Command::new(env!("CARGO_BIN_EXE_browserd"))
        .args([
            "serve",
            "--bind",
            &format!("127.0.0.1:{cdp_port}"),
            "--config",
            config_path.to_str().expect("path utf8"),
            "--engine-binary",
            skrip.to_str().expect("path utf8"),
            "--user-data-dir",
            profil.to_str().expect("path utf8"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("menjalankan browserd serve");

    let siap = tunggu(BATAS_SIAP, || minta(cdp_port, "/healthz") == 200);
    if !siap {
        common::buang(&mut daemon);
        panic!("daemon tidak siap dalam {BATAS_SIAP:?}");
    }

    // Uji yang tidak pernah melihat mesin hidup tidak membuktikan apa pun soal
    // teardown: ia hanya membuktikan daemon bisa mati. Mesin harus benar-benar
    // ada di `/proc` sebelum teardown dijalankan.
    let penanda = profil.to_str().expect("path utf8").to_owned();
    let mesin_hidup = tunggu(Duration::from_secs(10), || {
        !proses_dengan_penanda(&penanda).is_empty()
    });
    if !mesin_hidup {
        common::buang(&mut daemon);
        panic!("mesin palsu tidak pernah tercatat hidup; uji teardown tidak berlaku");
    }

    common::buang(&mut daemon);

    let sisa = tunggu_sampai_nol(&penanda, BATAS_MATI);
    let daftar = sisa.iter().map(ringkas).collect::<Vec<_>>().join("\n");
    assert!(
        sisa.is_empty(),
        "teardown meninggalkan {} proses dengan profil uji (harusnya nol):\n{daftar}",
        sisa.len()
    );
}

/// Satu baris laporan untuk proses yang tersisa: pid, induk, dan perintahnya.
///
/// `ppid` ikut dicetak karena bentuk regresinya adalah `ppid=1`: mesin yang
/// di-reparent ke PID 1, bukan mesin yang masih punya induk.
fn ringkas(proses: &Proses) -> String {
    format!("  pid={} ppid={} {}", proses.pid, proses.ppid, proses.perintah)
}

fn ada_python() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn direktori_uji() -> PathBuf {
    // Di bawah `/tmp/ocb-*` supaya `scripts/leak-check.sh` ikut melihat sisa
    // proses dari uji ini, bukan hanya dari uji Chrome.
    let dir = std::env::temp_dir().join(format!("ocb-yatim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("membuat direktori uji");
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

fn port_kosong() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind port sementara");
    let port = listener.local_addr().expect("alamat lokal").port();
    drop(listener);
    port
}

fn minta(port: u16, path: &str) -> u16 {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return 0;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let permintaan = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    if stream.write_all(permintaan.as_bytes()).is_err() {
        return 0;
    }
    let mut respons = String::new();
    let _ = stream.read_to_string(&mut respons);
    respons
        .split_whitespace()
        .nth(1)
        .and_then(|kode| kode.parse().ok())
        .unwrap_or(0)
}

fn tunggu<F: FnMut() -> bool>(batas: Duration, mut cek: F) -> bool {
    let mulai = Instant::now();
    while mulai.elapsed() < batas {
        if cek() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Menunggu sampai tidak ada lagi proses yang membawa penanda, lalu
/// mengembalikan sisanya (kosong berarti bersih).
fn tunggu_sampai_nol(penanda: &str, batas: Duration) -> Vec<Proses> {
    let mulai = Instant::now();
    loop {
        let sisa = proses_dengan_penanda(penanda);
        if sisa.is_empty() || mulai.elapsed() >= batas {
            return sisa;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Proses yang baris perintahnya memuat `penanda`.
///
/// Penanda sengaja berupa path profil: proses uji sendiri tidak memuatnya,
/// jadi pemeriksa tidak bisa menuduh dirinya sendiri.
fn proses_dengan_penanda(penanda: &str) -> Vec<Proses> {
    let mut hasil = Vec::new();
    let Ok(isi) = std::fs::read_dir("/proc") else {
        return hasil;
    };
    for masuk in isi.flatten() {
        let Some(pid) = masuk
            .file_name()
            .to_str()
            .and_then(|nama| nama.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(mentah) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let perintah = String::from_utf8_lossy(&mentah).replace('\0', " ");
        if !perintah.contains(penanda) {
            continue;
        }
        hasil.push(Proses {
            pid,
            ppid: ppid_dari(pid).unwrap_or(0),
            perintah: perintah.trim().to_owned(),
        });
    }
    hasil
}

/// PID induk dari `/proc/<pid>/stat`.
///
/// Nama proses bisa memuat spasi dan tanda kurung, jadi pemisahan dilakukan
/// dari kurung tutup terakhir: medan sesudahnya adalah `state ppid pgrp ...`.
fn ppid_dari(pid: u32) -> Option<u32> {
    let isi = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let sesudah_nama = isi.rsplit_once(')')?.1;
    sesudah_nama.split_whitespace().nth(1)?.parse().ok()
}
