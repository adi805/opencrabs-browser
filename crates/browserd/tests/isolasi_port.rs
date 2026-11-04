//! AC-019: isolasi port produksi, dari prosedur manual jadi uji otomatis.
//!
//! Prosedur audit sebelumnya masih manual: catat jumlah target di port produksi
//! 9222, jalankan uji live, catat lagi. Uji ini melakukan hal yang sama dengan
//! cara yang bisa dijalankan ulang siapa pun:
//!
//! 1. Baca jumlah target 9222 sebelum (read-only; tidak ada yang mendengarkan
//!    berarti `None`).
//! 2. Nyalakan daemon pengembangan di port acak dengan profil sementara, lalu
//!    buat satu tab supaya mesin benar-benar lahir.
//! 3. Baca lagi jumlah target 9222 dan bandingkan dengan keadaan sebelum.
//! 4. Baca port debug mesin dari `DevToolsActivePort` yang mesin tulis sendiri,
//!    lalu pastikan angkanya bukan 9222.
//!
//! Kenapa membandingkan jumlah target, bukan menguji "tidak ada yang
//! mendengarkan di 9222": kalau port produksi sedang dipakai browser
//! sungguhan, browser itu tidak boleh diganggu, jadi yang bisa dibandingkan
//! hanya keadaannya. Perubahan `None` menjadi `Some` tetap tertangkap: daemon
//! pengembangan yang menyerobot 9222 akan membuat port itu tiba-tiba hidup.
//!
//! Port 9222 hanya **dibaca**. Tidak ada satu pun permintaan yang menulis ke
//! sana.
//!
//! Dilewati dengan pesan jelas kalau `chrome-headless-shell` tidak ada, supaya
//! `cargo test` tetap hijau di mesin tanpa browser.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// Port browser produksi di host ini. Dibaca, tidak pernah ditulis.
const PORT_PRODUKSI: u16 = 9222;

/// Batas kesabaran seluruh uji: mesin nyata butuh waktu boot.
const BATAS: Duration = Duration::from_secs(120);

fn biner_mesin() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os("OCB_ENGINE_BINARY") {
        let path = PathBuf::from(raw);
        if path.is_file() {
            return Some(path);
        }
    }
    engine::discover_shell().filter(|path| path.is_file())
}

fn port_kosong() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind port sementara");
    let port = listener.local_addr().expect("alamat lokal").port();
    drop(listener);
    port
}

fn direktori_uji(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ocb-isolasi-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("membuat direktori uji");
    dir
}

/// Satu permintaan HTTP polos, tanpa token: port CDP tidak memakai otorisasi.
fn minta(port: u16, metode: &str, path: &str) -> (u16, String) {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return (0, String::new());
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let permintaan = format!(
        "{metode} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(permintaan.as_bytes()).is_err() {
        return (0, String::new());
    }
    let mut respons = String::new();
    let _ = stream.read_to_string(&mut respons);
    let status = respons
        .split_whitespace()
        .nth(1)
        .and_then(|kode| kode.parse().ok())
        .unwrap_or(0);
    let isi = respons
        .split_once("\r\n\r\n")
        .map_or(String::new(), |(_, isi)| isi.to_owned());
    (status, isi)
}

fn tunggu<F: FnMut() -> bool>(batas: Duration, mut cek: F) -> bool {
    let mulai = Instant::now();
    while mulai.elapsed() < batas {
        if cek() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Jumlah target di port produksi, atau `None` kalau tidak ada yang menjawab.
///
/// Hanya GET. Permintaan tulis ke 9222 dilarang: itu browser produksi.
fn target_produksi() -> Option<usize> {
    let (status, isi) = minta(PORT_PRODUKSI, "GET", "/json/list");
    if status != 200 {
        return None;
    }
    let nilai: serde_json::Value = serde_json::from_str(&isi).ok()?;
    nilai.as_array().map(Vec::len)
}

/// Port debug mesin, dibaca dari berkas yang mesin tulis sendiri.
///
/// Baris pertama `DevToolsActivePort` adalah port. Yang diuji adalah angka yang
/// benar-benar dipakai mesin, bukan angka yang kita harap.
fn port_mesin(profil: &Path) -> Option<u16> {
    let isi = std::fs::read_to_string(profil.join("DevToolsActivePort")).ok()?;
    isi.lines().next()?.trim().parse().ok()
}

struct Daemon {
    child: Child,
    cdp_port: u16,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // SIGTERM lebih dulu supaya daemon mematikan mesinnya sebagai process
        // group. Membunuh induknya saja meninggalkan Chromium yatim, dan itu
        // yang membuat run berikutnya gagal karena memori habis.
        let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGTERM);
        let mulai = Instant::now();
        while mulai.elapsed() < Duration::from_secs(20) {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn config_isi(control_port: u16, token: &Path, db: &Path, kunci: &Path, biner: &Path) -> String {
    format!(
        "[daemon]\ncontrol_bind = \"127.0.0.1:{control_port}\"\n\
         control_token = \"{}\"\nstore_db = \"{}\"\nstore_key = \"{}\"\n\
         \n[engines.chrome_shell]\nbinary = \"{}\"\n",
        token.display(),
        db.display(),
        kunci.display(),
        biner.display()
    )
}

fn nyalakan(dir: &Path, biner: &Path, cdp_port: u16) -> Daemon {
    let control_port = port_kosong();
    let kunci = dir.join("store.key");
    let db = dir.join("store.db");
    let token = dir.join("control.token");
    let config = dir.join("browser-daemon.toml");
    std::fs::write(
        &config,
        config_isi(control_port, &token, &db, &kunci, biner),
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

    let child = Command::new(env!("CARGO_BIN_EXE_browserd"))
        .args([
            "serve",
            "--bind",
            &format!("127.0.0.1:{cdp_port}"),
            "--config",
            config.to_str().expect("path utf8"),
            "--engine-binary",
            biner.to_str().expect("path utf8"),
            "--user-data-dir",
            dir.join("profil").to_str().expect("path utf8"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("menjalankan browserd serve");

    let mut daemon = Daemon { child, cdp_port };
    let siap = tunggu(BATAS, || {
        if let Ok(Some(status)) = daemon.child.try_wait() {
            panic!("daemon keluar lebih awal: {status}");
        }
        minta(cdp_port, "GET", "/healthz").0 == 200
    });
    assert!(siap, "daemon tidak siap dalam {BATAS:?}");
    daemon
}

#[test]
fn mesin_pengembangan_tidak_menyentuh_port_produksi() {
    let Some(biner) = biner_mesin() else {
        eprintln!(
            "Dilewati: chrome-headless-shell tidak ada di host ini (set OCB_ENGINE_BINARY \
             untuk menunjuk binernya). Uji ini butuh mesin nyata."
        );
        return;
    };

    // Satu uji live pada satu waktu. Tanpa kunci ini, uji lain melahirkan
    // Chromium bersamaan dan hasilnya jadi soal beban host, bukan soal produk.
    let _kunci = common::kunci_live("isolasi_port");

    let sebelum = target_produksi();

    let dir = direktori_uji("isolasi");
    let cdp_port = port_kosong();
    assert_ne!(cdp_port, PORT_PRODUKSI, "port uji tidak boleh 9222");

    let daemon = nyalakan(&dir, &biner, cdp_port);

    // Tab baru memaksa mesin benar-benar lahir. Tanpa ini yang diuji cuma
    // daemon yang menganggur.
    let (status, isi) = minta(cdp_port, "PUT", "/json/new?about:blank");
    assert_eq!(status, 200, "membuat tab harus 200: {isi}");

    let profil = dir.join("profil");
    let muncul = tunggu(BATAS, || port_mesin(&profil).is_some());
    assert!(
        muncul,
        "mesin tidak menulis DevToolsActivePort di {}",
        profil.display()
    );
    let port_mesin = port_mesin(&profil).expect("port mesin terbaca");

    assert_ne!(
        port_mesin, PORT_PRODUKSI,
        "port debug mesin ({port_mesin}) menyentuh port browser produksi"
    );
    assert_ne!(daemon.cdp_port, PORT_PRODUKSI);

    let sesudah = target_produksi();
    assert_eq!(
        sebelum.is_some(),
        sesudah.is_some(),
        "keberadaan listener di 9222 berubah: sebelum {sebelum:?}, sesudah {sesudah:?}. \
         Daemon pengembangan tidak boleh menyalakan apa pun di port produksi."
    );
    if let (Some(awal), Some(akhir)) = (sebelum, sesudah) {
        assert_eq!(
            awal, akhir,
            "jumlah target di 9222 berubah selama uji live: {awal} menjadi {akhir}"
        );
        eprintln!("9222 hidup sebelum dan sesudah, jumlah target tetap {awal}");
    } else {
        eprintln!("9222 tidak ada yang mendengarkan, sebelum maupun sesudah");
    }

    drop(daemon);
    let _ = std::fs::remove_dir_all(&dir);
}
