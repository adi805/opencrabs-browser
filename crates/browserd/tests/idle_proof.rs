//! Bukti nyata untuk G2 RFC: memori mesin kembali ke baseline setelah
//! menganggur, **tanpa me-restart daemon**, lalu mesin itu hidup lagi saat ada
//! yang memakainya.
//!
//! Yang diukur di sini bukan aturan internal (itu sudah diuji unit dan di
//! `idle_reap.rs`), tapi hal yang cuma bisa dibuktikan dengan proses nyata:
//!
//! 1. PSS total (daemon + mesin) turun setelah reap, dan mesinnya benar-benar
//!    hilang dari `/proc`.
//! 2. Daemon yang sama masih hidup sesudahnya: pid-nya tidak berubah, jadi
//!    penurunan memori itu datang dari reap, bukan dari daemon yang mati lalu
//!    dinyalakan ulang oleh systemd.
//! 3. Mesin tidak dimatikan sebelum ambangnya habis, dan tidak lebih cepat dari
//!    permintaan terakhir yang menyentuhnya.
//! 4. `/json/list` sesudah reap menyalakan mesin **baru** (pid berbeda) yang
//!    benar-benar bisa dipakai: tab dibuat lewat `/json/new`, lalu skrip
//!    dieksekusi di tab itu dan hasilnya dibaca.
//!
//! PSS, bukan RSS: RSS menjumlahkan halaman yang dipakai bersama (biner,font,
//! pustaka) berkali-kali, jadi angka RSS dari dua proses tidak boleh dijumlahkan
//! dan penurunannya akan terlihat lebih besar dari yang nyata.
//!
//! Kalau `chrome-headless-shell` tidak ada di host ini, uji **dilewati** dengan
//! pesan yang menyebut apa yang kurang. Dilewati bukan berarti lulus: pesannya
//! harus terbaca dengan `--nocapture`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Batas kesabaran seluruh uji. Mesin nyata butuh waktu boot, dan host yang
/// sedang sibuk bisa jauh lebih lambat dari dugaan.
const BATAS: Duration = Duration::from_secs(120);

/// Ambang reap yang dipakai uji ini. Cukup lama untuk melihat mesin masih hidup
/// sebelum ambang, cukup pendek untuk tidak membuat uji ini lambat.
const AMBANG_SECS: u64 = 10;

/// Penurunan PSS minimal yang dianggap bukti. `chrome-headless-shell` yang diam
/// memakai puluhan MB; 10 MB adalah lantai yang aman untuk tidak gagal karena
/// mesin yang kebetulan sangat hemat, tapi tetap jauh di atas derau.
const TURUN_MINIMAL: u64 = 10 * 1024 * 1024;

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

fn direktori_uji() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ocb-idle-proof-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("membuat direktori uji");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

fn minta(port: u16, metode: &str, path: &str) -> (u16, String) {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return (0, String::new());
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let permintaan =
        format!("{metode} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
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

/// Potret `/healthz`: apa yang daemon katakan tentang mesinnya sekarang.
///
/// `pid` di sini adalah sumber kebenaran untuk PSS: memindai `/proc` sendiri
/// akan menemukan proses lain yang kebetulan memakai profil yang sama.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sehat {
    state: String,
    pid: Option<u32>,
    ok: bool,
}

fn sehat(port: u16) -> Option<Sehat> {
    let (status, body) = minta(port, "GET", "/healthz");
    if status == 0 {
        return None;
    }
    let nilai: Value = serde_json::from_str(&body).ok()?;
    Some(Sehat {
        state: nilai["state"].as_str()?.to_owned(),
        pid: nilai["pid"].as_u64().map(|pid| pid as u32),
        ok: nilai["ok"].as_bool().unwrap_or(false),
    })
}

fn pss(pid: u32) -> Option<u64> {
    browserd::metrics::read_pss_bytes(pid)
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn tulis_config(dir: &Path, biner: &Path, cdp_port: u16, control_port: u16) -> PathBuf {
    let config = dir.join("browser-daemon.toml");
    let isi = format!(
        "[daemon]\n\
         bind = \"127.0.0.1:{cdp_port}\"\n\
         control_bind = \"127.0.0.1:{control_port}\"\n\
         control_token = \"{token}\"\n\
         store_db = \"{store_db}\"\n\
         store_key = \"{store_key}\"\n\
         \n\
         [engines.kiri]\n\
         binary = \"{biner}\"\n\
         user_data_dir = \"{profil}\"\n",
        token = dir.join("control.token").display(),
        store_db = dir.join("store.db").display(),
        store_key = dir.join("store.key").display(),
        biner = biner.display(),
        profil = dir.join("kiri-profil").display(),
    );
    std::fs::write(&config, isi).expect("menulis config uji");
    config
}

fn buang(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Jalankan satu skrip di tab lewat WebSocket yang **diberikan proxy**, bukan
/// alamat mesin: jalur klien yang diuji adalah jalur yang dipakai OpenCrabs.
fn evaluasi(url: &str, ekspresi: &str) -> Option<Value> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(async move {
        let (mut soket, _) = tokio_tungstenite::connect_async(url).await.ok()?;
        let pesan = json!({
            "id": 1,
            "method": "Runtime.evaluate",
            "params": {"expression": ekspresi, "returnByValue": true},
        });
        soket
            .send(Message::Text(pesan.to_string().into()))
            .await
            .ok()?;

        let batas = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let sisa = batas.saturating_duration_since(tokio::time::Instant::now());
            let pesan = tokio::time::timeout(sisa, soket.next()).await.ok()??.ok()?;
            let Message::Text(teks) = pesan else { continue };
            let nilai: Value = serde_json::from_str(&teks).ok()?;
            if nilai["id"] == json!(1) {
                return Some(nilai["result"]["result"]["value"].clone());
            }
        }
    })
}

#[test]
fn pss_kembali_ke_baseline_lalu_mesin_hidup_lagi() {
    let Some(biner) = biner_mesin() else {
        eprintln!(
            "SKIP idle_proof: chrome-headless-shell tidak ditemukan. Set OCB_ENGINE_BINARY \
             atau pasang di ~/.cache/ms-playwright."
        );
        return;
    };

    let dir = direktori_uji();
    let cdp_port = port_kosong();
    let control_port = port_kosong();
    let config = tulis_config(&dir, &biner, cdp_port, control_port);

    // Store butuh kunci per direktori uji; tanpa ini daemon menolak start dan
    // kegagalannya tidak ada hubungannya dengan reap.
    let kunci = Command::new(env!("CARGO_BIN_EXE_browserd"))
        .args(["keygen", "--key-file"])
        .arg(dir.join("store.key"))
        .arg("--db")
        .arg(dir.join("store.db"))
        .output()
        .expect("menjalankan keygen");
    assert!(
        kunci.status.success(),
        "keygen gagal: {}",
        String::from_utf8_lossy(&kunci.stderr)
    );

    let log_path = dir.join("daemon.log");
    let log = std::fs::File::create(&log_path).expect("log daemon");
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_browserd"))
        .args(["serve", "--config"])
        .arg(&config)
        // Ambang lewat CLI, bukan config: itu jalur yang dipakai untuk mencoba
        // nilai lain tanpa menyunting berkas, dan ikut dibuktikan di sini.
        .arg("--idle-reap-secs")
        .arg(AMBANG_SECS.to_string())
        .stdout(Stdio::from(log.try_clone().expect("kloning log")))
        .stderr(Stdio::from(log))
        .spawn()
        .expect("menjalankan browserd");

    let daemon_pid = daemon.id();
    let hasil = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        jalankan_uji(&mut daemon, cdp_port)
    }));
    buang(&mut daemon);

    match hasil {
        Ok(()) => eprintln!(
            "OK idle_proof: daemon pid {daemon_pid} tidak pernah restart; log di {}",
            log_path.display()
        ),
        Err(panik) => {
            let log = std::fs::read_to_string(&log_path).unwrap_or_default();
            eprintln!("--- log daemon ---\n{log}\n------------------");
            std::panic::resume_unwind(panik);
        }
    }
}

fn jalankan_uji(daemon: &mut Child, cdp_port: u16) {
    let daemon_pid = daemon.id();

    // Tunggu daemon siap, tapi berhenti begitu prosesnya keluar: menunggu 120
    // detik untuk daemon yang sudah mati hanya menyembunyikan penyebabnya.
    let batas = Instant::now() + BATAS;
    let mut siap = false;
    while Instant::now() < batas {
        if let Ok(Some(status)) = daemon.try_wait() {
            panic!("daemon keluar lebih awal: {status}");
        }
        if minta(cdp_port, "GET", "/json/version").0 == 200 {
            siap = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(siap, "daemon tidak siap dalam {BATAS:?}");

    // Permintaan pertama yang menyentuh mesin. Waktu ini yang jadi patokan
    // "sudah menganggur berapa lama": ambang dihitung dari sentuhan terakhir,
    // bukan dari saat daemon lahir.
    let (status, _) = minta(cdp_port, "GET", "/json/list");
    assert_eq!(status, 200, "mesin tidak melaporkan tab di awal");
    let disentuh = Instant::now();

    let awal = sehat(cdp_port).expect("healthz awal");
    assert!(awal.ok, "mesin tidak sehat setelah boot: {awal:?}");
    let pid_mesin = awal.pid.expect("mesin hidup tidak melaporkan pid");
    let pss_mesin = pss(pid_mesin).expect("PSS mesin awal terbaca");
    let pss_daemon = pss(daemon_pid).expect("PSS daemon terbaca");
    let total_awal = pss_mesin + pss_daemon;
    eprintln!(
        "awal: pid mesin {pid_mesin} = {:.1} MB, pid daemon {daemon_pid} = {:.1} MB, total {:.1} MB",
        mb(pss_mesin),
        mb(pss_daemon),
        mb(total_awal)
    );

    // Sebelum ambang: mesin WAJIB masih hidup. Tanpa pemeriksaan ini, uji bisa
    // lulus dengan mesin yang mati sejak awal karena alasan yang tidak ada
    // hubungannya dengan reap.
    std::thread::sleep(Duration::from_secs(AMBANG_SECS / 3));
    let sebelum = sehat(cdp_port).expect("healthz sebelum ambang");
    assert_eq!(
        sebelum.pid,
        Some(pid_mesin),
        "mesin sudah berganti sebelum ambang habis: {sebelum:?}"
    );
    assert!(
        pss(pid_mesin).is_some(),
        "proses mesin hilang sebelum ambang habis"
    );
    let umur = disentuh.elapsed();
    eprintln!(
        "sebelum ambang ({:.1} s dari sentuhan): pid {pid_mesin} masih hidup, PSS {:.1} MB",
        umur.as_secs_f64(),
        mb(pss(pid_mesin).expect("PSS mesin sebelum ambang"))
    );

    // Tunggu reap. Batasnya ambang + kelonggaran: reap adalah loop, bukan alarm
    // presisi, dan host yang sibuk bisa butuh satu-dua tick tambahan.
    let batas_reap = disentuh + Duration::from_secs(AMBANG_SECS) + Duration::from_secs(40);
    let mut tidur = None;
    while Instant::now() < batas_reap {
        if let Some(kabar) = sehat(cdp_port)
            && kabar.pid.is_none()
            && kabar.state != "hidup"
        {
            tidur = Some(kabar);
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let tidur = tidur.unwrap_or_else(|| {
        let kabar = sehat(cdp_port);
        panic!("mesin tidak di-reap dalam {BATAS:?} setelah sentuhan terakhir: {kabar:?}")
    });
    let umur = disentuh.elapsed();
    assert!(
        umur >= Duration::from_secs(AMBANG_SECS),
        "reap terjadi sebelum ambang: {umur:?} < {AMBANG_SECS} s"
    );
    eprintln!(
        "reap: {:.1} s setelah sentuhan terakhir, state = {}",
        umur.as_secs_f64(),
        tidur.state
    );

    // Slot sudah melepas mesinnya, tapi protokol reap belum tentu selesai:
    // `SIGTERM` dikirim dulu, dan proses yang tidak menurut baru kena `SIGKILL`
    // setelah tenggangnya. Yang dibuktikan adalah prosesnya **benar-benar**
    // hilang, jadi tunggu sampai /proc bersih, jangan menganggap keputusan reap
    // sama dengan proses yang sudah mati.
    let batas_hilang = Instant::now() + Duration::from_secs(30);
    while Path::new(&format!("/proc/{pid_mesin}")).exists() && Instant::now() < batas_hilang {
        std::thread::sleep(Duration::from_millis(100));
    }
    // Zombie masih terlihat di /proc sampai induknya memungutnya. Itu bukan
    // proses hidup: PSS-nya nol dan tidak memakai memori apa pun.
    let sisa = std::fs::read_to_string(format!("/proc/{pid_mesin}/stat")).ok();
    let zombie = sisa
        .as_deref()
        .and_then(|isi| isi.rsplit_once(')').map(|(_, rest)| rest.trim_start()))
        .is_some_and(|rest| rest.starts_with('Z'));
    assert!(
        sisa.is_none() || zombie,
        "proses mesin {pid_mesin} masih hidup setelah reap"
    );
    assert!(
        pss(pid_mesin).is_none(),
        "PSS mesin {pid_mesin} masih terbaca setelah reap"
    );
    eprintln!(
        "proses mesin hilang: {}",
        if zombie {
            "zombie, PSS nol"
        } else {
            "/proc bersih"
        }
    );

    // Daemon yang sama masih hidup: tanpa ini, penurunan memori bisa saja
    // datang dari daemon yang mati lalu dinyalakan ulang systemd.
    assert!(
        matches!(daemon.try_wait(), Ok(None)),
        "daemon berhenti saat reap; bukti ini tidak sah"
    );
    let pss_daemon_sesudah = pss(daemon_pid).expect("PSS daemon sesudah reap");
    let total_sesudah = pss_daemon_sesudah;
    eprintln!(
        "sesudah reap: daemon {:.1} MB, total {:.1} MB (turun {:.1} MB)",
        mb(pss_daemon_sesudah),
        mb(total_sesudah),
        mb(total_awal.saturating_sub(total_sesudah))
    );
    assert!(
        total_awal.saturating_sub(total_sesudah) >= TURUN_MINIMAL,
        "PSS tidak kembali ke baseline: awal {:.1} MB, sesudah {:.1} MB",
        mb(total_awal),
        mb(total_sesudah)
    );

    // Mesin dipakai lagi: `/json/list` harus membangunkan mesin baru, bukan
    // menjawab daftar kosong.
    let (status, isi) = minta(cdp_port, "GET", "/json/list");
    assert_eq!(status, 200, "list sesudah reap gagal: {isi}");
    let bangun = sehat(cdp_port).expect("healthz sesudah dibangunkan");
    assert!(bangun.ok, "mesin baru tidak sehat: {bangun:?}");
    let pid_baru = bangun.pid.expect("mesin baru tidak melaporkan pid");
    assert_ne!(
        pid_baru, pid_mesin,
        "pid tidak berubah: mesin lama dipakai, bukan dibangunkan ulang"
    );
    let pss_baru = pss(pid_baru).expect("PSS mesin baru terbaca");
    eprintln!(
        "bangun lagi: pid {pid_mesin} -> {pid_baru}, PSS {:.1} MB",
        mb(pss_baru)
    );
    assert!(
        pss_baru >= TURUN_MINIMAL,
        "mesin baru tidak benar-benar berjalan: PSS {:.1} MB",
        mb(pss_baru)
    );

    // Terakhir, buktikan tabnya bisa dipakai: buat tab lewat `/json/new`, lalu
    // jalankan skrip di dalamnya lewat WebSocket yang diberikan proxy.
    let (status, isi) = minta(cdp_port, "PUT", "/json/new?about:blank");
    assert_eq!(status, 200, "membuat tab sesudah bangun gagal: {isi}");
    let tab: Value = serde_json::from_str(&isi).expect("tab baru berbentuk JSON");
    let ws = tab["webSocketDebuggerUrl"]
        .as_str()
        .expect("tab baru punya webSocketDebuggerUrl")
        .to_owned();
    let hasil = evaluasi(&ws, "6 * 7").expect("Runtime.evaluate di tab mesin baru");
    assert_eq!(
        hasil.as_i64(),
        Some(42),
        "tab di mesin baru tidak bisa menjalankan skrip: {hasil}"
    );
    eprintln!("tab baru di mesin {pid_baru}: Runtime.evaluate -> {hasil}");

    // Daemon masih yang sama sampai akhir: seluruh rantai ini terjadi tanpa
    // restart, dan itu inti G2.
    assert!(
        matches!(daemon.try_wait(), Ok(None)),
        "daemon berhenti sebelum uji selesai"
    );
}
