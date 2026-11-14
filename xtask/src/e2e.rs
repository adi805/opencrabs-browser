//! `cargo xtask e2e`: kontrak CDP ujung ke ujung terhadap daemon sungguhan.
//!
//! # Kenapa ada perintah ini
//!
//! `cargo test --workspace` membuktikan tiap potongan, bukan rangkaiannya.
//! Yang dikirim ke pengguna adalah satu biner daemon yang harus menjawab
//! discovery, membuka websocket, membuat target, menavigasi, mengevaluasi
//! skrip, lalu menutup target. Rangkaian itu hanya bisa dibuktikan dengan
//! menjalankan daemonnya.
//!
//! # Kenapa tanpa Chrome
//!
//! Mesinnya `rust-native`. Kalau perintah ini butuh Chrome, dia berhenti
//! menjadi bukti mesin kita dan mulai menjadi bukti Chrome. Halaman yang
//! dinavigasi juga dilayani server lokal di dalam proses ini, jadi tidak ada
//! jaringan luar yang bisa membuat hasilnya berubah tanpa kode berubah.
//!
//! # Kenapa memakai kunci yang sama dengan uji live
//!
//! Uji live di `cargo test` memegang `<target>/<profil>/.ocb-live.lock`. Perintah
//! ini melahirkan daemon yang juga memakai CPU dan port, jadi dia harus ikut
//! antre, bukan berjalan berdampingan. Berkas kuncinya diturunkan dari lokasi
//! biner `xtask` (`<target>/<profil>/xtask`), yang menunjuk ke berkas yang sama
//! dengan yang dipakai binary uji di `<target>/<profil>/deps/`.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use crate::sensus;

/// Batas menunggu daemon menjawab `/json/version`.
const BATAS_SIAP: Duration = Duration::from_secs(40);

/// Batas menunggu daemon menutup mesinnya sesudah SIGTERM.
const SABAR_MATI: Duration = Duration::from_secs(20);

/// Batas menunggu kunci uji live.
const BATAS_KUNCI: Duration = Duration::from_secs(900);

/// Judul halaman uji. Dipakai server lokal dan diperiksa lewat `document.title`,
/// jadi nilainya harus tidak mungkin muncul kebetulan.
const JUDUL: &str = "Halaman Uji E2E";

fn profil_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("lokasi biner xtask")?;
    let dir = exe
        .parent()
        .context("xtask harus berada di dalam direktori target")?;
    Ok(dir.to_path_buf())
}

fn repo_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask harus berada di dalam repo")
        .map(Path::to_path_buf)
}

/// Mengambil kunci uji live yang sama dengan binary uji.
fn kunci_live() -> Result<File> {
    let path = profil_dir()?.join(".ocb-live.lock");
    let berkas = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("membuka kunci {}", path.display()))?;

    let mulai = Instant::now();
    loop {
        match berkas.try_lock() {
            Ok(()) => return Ok(berkas),
            Err(std::fs::TryLockError::WouldBlock) => {
                if mulai.elapsed() >= BATAS_KUNCI {
                    bail!(
                        "menunggu kunci uji live {} lebih dari {BATAS_KUNCI:?}; ada uji live lain yang menggantung",
                        path.display()
                    );
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(std::fs::TryLockError::Error(err)) => {
                return Err(err).with_context(|| format!("mengunci {}", path.display()));
            }
        }
    }
}

/// Port bebas pilihan kernel, lalu dilepas. Celah antara melepas dan daemon
/// mengikatnya sangat kecil dan seluruh host ini memakai port tetap untuk
/// layanan nyata, jadi tabrakan berarti ada proses lain yang baru saja lahir.
fn port_bebas() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("mencari port bebas")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Daemon yang selalu ditutup, termasuk saat asersi gagal di tengah jalan.
struct Daemon {
    child: Option<Child>,
    port: u16,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM);
        let mulai = Instant::now();
        while mulai.elapsed() < SABAR_MATI {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!(
            "daemon di port {} tidak menutup dalam {SABAR_MATI:?}; SIGKILL",
            self.port
        );
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Satu GET polos ke daemon, mengembalikan (status, badan).
fn minta(port: u16, path: &str) -> Option<(u16, String)> {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut teks = String::new();
    stream.read_to_string(&mut teks).ok()?;
    let (kepala, badan) = teks.split_once("\r\n\r\n")?;
    let status = kepala
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some((status, badan.to_owned()))
}

/// Server halaman satu halaman, hidup selama proses ini hidup.
///
/// Dijalankan di thread terpisah dan tidak pernah ditutup: proses yang berakhir
/// menutupnya. Menutupnya dengan rapi menambah jalur kegagalan tanpa menambah
/// bukti.
fn server_halaman() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("mengikat server halaman")?;
    let port = listener.local_addr()?.port();

    std::thread::spawn(move || {
        let badan = format!(
            "<!doctype html><html><head><title>{JUDUL}</title></head><body><h1>{JUDUL}</h1><p>halaman lokal</p></body></html>"
        );
        for soket in listener.incoming() {
            let Ok(mut soket) = soket else { continue };
            let mut buang = [0u8; 1024];
            let _ = soket.read(&mut buang);
            let jawab = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\ncontent-type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{badan}",
                badan.len()
            );
            let _ = soket.write_all(jawab.as_bytes());
            let _ = soket.flush();
        }
    });

    Ok(port)
}

pub fn run(args: &[String]) -> Result<()> {
    let _kunci = kunci_live()?;
    let awal = sensus::ambil();
    let root = repo_root()?;
    let profil = profil_dir()?;

    let browserd = profil.join("browserd");
    if !browserd.is_file() {
        bail!(
            "biner daemon tidak ada: {} (jalankan `cargo build -p browserd` lebih dulu)",
            browserd.display()
        );
    }

    let cap = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ocb-e2e-{}-{cap}", std::process::id()));
    std::fs::create_dir_all(&dir).with_context(|| format!("membuat {}", dir.display()))?;

    let hasil = jalankan(&root, &browserd, &dir, args);

    // Sensus dijalankan sebelum direktori dibersihkan: profil mesin yang masih
    // dipegang proses bocor akan hilang dari pandangan kalau dihapus dulu.
    let akhir = sensus::ambil();
    let sensus_hasil = sensus::bandingkan(&awal, &akhir);

    let _ = std::fs::remove_dir_all(&dir);

    hasil?;
    sensus_hasil
}

fn jalankan(root: &Path, browserd: &Path, dir: &Path, args: &[String]) -> Result<()> {
    let db = dir.join("store.db");
    let key = dir.join("store.key");
    let token = dir.join("control.token");
    let config = dir.join("browserd.toml");
    let log = dir.join("daemon.log");

    let status = Command::new(browserd)
        .args(["keygen", "--key-file"])
        .arg(&key)
        .arg("--db")
        .arg(&db)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("menjalankan browserd keygen")?;
    if !status.success() {
        bail!("browserd keygen keluar dengan status {status}");
    }

    let port = port_bebas()?;
    let halaman = server_halaman()?;

    let isi = format!(
        "[daemon]
bind = \"127.0.0.1:{port}\"
control_bind = \"127.0.0.1:{}\"
idle_reap_secs = 0
store_db = \"{}\"
store_key = \"{}\"
control_token = \"{}\"

[engines.native]
kind = \"rust-native\"
max_tabs = 8

[policy]
default = \"native\"
",
        port + 1,
        db.display(),
        key.display(),
        token.display()
    );
    std::fs::write(&config, isi).with_context(|| format!("menulis {}", config.display()))?;

    let keluaran = File::create(&log).with_context(|| format!("membuat {}", log.display()))?;
    let child = Command::new(browserd)
        .arg("serve")
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--config")
        .arg(&config)
        .stdout(keluaran.try_clone()?)
        .stderr(keluaran)
        .stdin(Stdio::null())
        .spawn()
        .context("menjalankan browserd serve")?;

    let daemon = Daemon {
        child: Some(child),
        port,
    };

    let mulai = Instant::now();
    let mut siap = false;
    while mulai.elapsed() < BATAS_SIAP {
        if let Some((200, badan)) = minta(port, "/json/version")
            && badan.contains("webSocketDebuggerUrl")
        {
            siap = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    if !siap {
        let teks = std::fs::read_to_string(&log).unwrap_or_default();
        bail!(
            "daemon tidak siap dalam {BATAS_SIAP:?}; log {}:\n{teks}",
            log.display()
        );
    }
    println!("-> {:<8}ok (port {port})", "daemon");

    let skrip = root.join("bench/cdp-smoke.mjs");
    if !skrip.is_file() {
        bail!("skrip smoke tidak ada: {}", skrip.display());
    }

    let mut perintah = Command::new("node");
    perintah
        .arg(&skrip)
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{port}"))
        .arg("--url")
        .arg(format!("http://127.0.0.1:{halaman}/"))
        .arg("--expect")
        .arg(JUDUL)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null());

    let status = perintah
        .status()
        .context("node tidak bisa dijalankan; pasang node atau lewati e2e")?;

    // Ditutup sebelum sensus: daemon yang masih hidup adalah proses baru di
    // sensus, dan itu bukan kebocoran.
    drop(daemon);

    if !status.success() {
        bail!("kontrak CDP gagal: {status}");
    }

    Ok(())
}
