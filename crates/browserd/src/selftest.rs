//! `browserd selftest`: satu proses, satu angka, bisa dipercaya.
//!
//! Yang diukur di sini bukan "apakah browser bisa dibuka". Yang diukur adalah
//! janji M1: bahwa memori benar-benar kembali ke sistem setelah tab ditutup dan
//! setelah engine di-reap. Kalau angka itu tidak ada, selftest gagal, bukan
//! mencetak "OK" dengan keyakinan.
//!
//! Semua angka diambil dari `/proc` milik proses yang baru saja dijalankan,
//! bukan dari perkiraan atau dari RSS. Alasan RSS tidak dipakai ada di
//! `engine::metrics` dan di RFC §1.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use engine::{Engine, LaunchSpec, ShellEngine, discover_shell};
use lifecycle::budget::read_process_group;
use lifecycle::reaper::{ReapReason, ReaperConfig, reap};
use lifecycle::{Counters, MB};

use crate::cli::SelftestArgs;
use crate::reap::{PgidGroup, remove_profile};

/// Mesin hanya butuh sepersekian detik untuk membuka tab. Jeda ini ada supaya
/// angka PSS diukur setelah sistem tenang, bukan di tengah lonjakan.
const SETTLE: Duration = Duration::from_millis(1500);

pub fn run(args: SelftestArgs) -> Result<()> {
    if args.engine != "chrome-shell" && args.engine != "shell" {
        bail!("engine '{}' belum didukung selftest v0.1", args.engine);
    }
    if args.tabs == 0 {
        bail!("--tabs 0 tidak mengukur apa pun");
    }

    let binary = args
        .engine_binary
        .clone()
        .or_else(discover_shell)
        .context("biner chrome-headless-shell tidak ditemukan; pakai --engine-binary")?;

    let profile = std::env::temp_dir().join(format!(
        "ocb-selftest-{}-{}",
        std::process::id(),
        unix_millis()
    ));

    let hasil = jalankan(&args, &binary, &profile);

    // Pembersihan selalu jalan, termasuk saat pengukuran gagal di tengah.
    if !args.keep_profile {
        remove_profile(&profile);
    }

    hasil
}

fn jalankan(args: &SelftestArgs, binary: &Path, profile: &Path) -> Result<()> {
    let spec = LaunchSpec::new(binary.to_path_buf(), profile.to_path_buf());
    let mulai = Instant::now();
    let engine = ShellEngine::launch(spec, args.ready_timeout_ms)
        .context("engine tidak mau menyala; berkas stderr-nya ada di direktori profil")?;

    let pid = engine.process_id().context("engine hidup tanpa pid")?;
    // `LaunchSpec` dijalankan dengan `process_group(0)`, jadi pgid == pid.
    let group = PgidGroup::new(pid);
    let port = port_of(&engine.http_base())?;
    let counters = Counters::new();

    let mut laporan = serde_json::Map::new();
    laporan.insert("engine".into(), args.engine.clone().into());
    laporan.insert("binary".into(), binary.display().to_string().into());
    laporan.insert("pid".into(), pid.into());
    laporan.insert(
        "boot_ms".into(),
        (mulai.elapsed().as_millis() as u64).into(),
    );

    let (pss_boot_mb, proc_boot, _) = group_pss(group);
    let (rss_boot_mb, _) = group_rss(group);

    // Tab dibuka lewat HTTP discovery milik mesin. Tanpa websocket: yang diuji
    // di sini memori, bukan protokol.
    let tabs = buka_tabs(port, args.tabs, &args.url)
        .with_context(|| format!("membuka {} tab gagal", args.tabs))?;
    std::thread::sleep(SETTLE);
    let (pss_tabs_mb, proc_tabs, pss_tabs_bytes) = group_pss(group);

    for id in &tabs {
        tutup_tab(port, id);
    }
    std::thread::sleep(SETTLE);
    let (pss_after_close_mb, proc_after_close, _) = group_pss(group);

    let reaper_cfg = ReaperConfig::default();
    let reap_report = reap(&group, &reaper_cfg, ReapReason::Manual)
        .context("mengirim sinyal ke process group gagal")?;
    counters.record_reap(&reap_report);
    std::thread::sleep(Duration::from_millis(200));
    let (pss_after_reap_mb, proc_after_reap, pss_after_reap_bytes) = group_pss(group);

    laporan.insert("tabs".into(), args.tabs.into());
    laporan.insert("url".into(), args.url.clone().into());
    laporan.insert("tabs_opened".into(), tabs.len().into());
    laporan.insert("pss_boot_mb".into(), pss_boot_mb.into());
    laporan.insert("rss_boot_mb".into(), rss_boot_mb.into());
    laporan.insert("pss_tabs_mb".into(), pss_tabs_mb.into());
    laporan.insert("pss_after_close_mb".into(), pss_after_close_mb.into());
    laporan.insert("pss_after_reap_mb".into(), pss_after_reap_mb.into());
    laporan.insert("processes_boot".into(), proc_boot.into());
    laporan.insert("processes_tabs".into(), proc_tabs.into());
    laporan.insert("processes_after_close".into(), proc_after_close.into());
    laporan.insert("processes_after_reap".into(), proc_after_reap.into());
    laporan.insert(
        "reap_outcome".into(),
        reap_report.outcome.to_string().into(),
    );
    laporan.insert("reap_sent_term".into(), reap_report.sent_term.into());
    laporan.insert("reap_sent_kill".into(), reap_report.sent_kill.into());
    laporan.insert(
        "reap_waited_ms".into(),
        (reap_report.waited.as_millis() as u64).into(),
    );

    // Perbandingan ambang dilakukan pada BYTE, bukan pada angka MB yang sudah
    // dibulatkan untuk dibaca manusia. Ambang yang dibandingkan lewat angka
    // tampilan bisa lolos atau gagal karena pembulatan, dan itu keputusan yang
    // dibuat oleh format cetak, bukan oleh pengukuran.
    let mut checks = Vec::new();
    checks.push(serde_json::json!({
        "name": "pss_peak",
        "limit_mb": args.expect_peak_mb,
        "measured_mb": pss_tabs_mb,
        "measured_bytes": pss_tabs_bytes,
        "pass": pss_tabs_bytes <= args.expect_peak_mb * MB,
    }));

    checks.push(serde_json::json!({
        "name": "pss_after_reap",
        "limit_mb": args.expect_pss_after_reap_mb,
        "measured_mb": pss_after_reap_mb,
        "measured_bytes": pss_after_reap_bytes,
        "pass": pss_after_reap_bytes < args.expect_pss_after_reap_mb * MB
            && proc_after_reap == 0,
    }));

    let mut gagal: Vec<String> = Vec::new();
    for check in &checks {
        let lulus = check["pass"].as_bool().unwrap_or(false);
        if !lulus && args.memory {
            gagal.push(format!(
                "{}: {} MB melebihi batas {} MB",
                check["name"].as_str().unwrap_or("?"),
                check["measured_mb"],
                check["limit_mb"]
            ));
        }
    }
    laporan.insert("checks".into(), checks.into());
    laporan.insert("memory_gate".into(), args.memory.into());
    laporan.insert("pass".into(), gagal.is_empty().into());

    let teks = serde_json::to_string_pretty(&serde_json::Value::Object(laporan))?;
    println!("{teks}");

    if let Some(path) = &args.out {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("membuat direktori {} gagal", parent.display()))?;
        }
        std::fs::write(path, format!("{teks}\n"))
            .with_context(|| format!("menulis {} gagal", path.display()))?;
        println!("hasil ditulis ke {}", path.display());
    }

    // Mesin dimatikan lewat jalur engine juga supaya `Child` tidak jadi zombie
    // di tabel proses; reap di atas sudah memastikan group-nya bersih.
    if let Err(err) = engine.stop() {
        tracing::warn!(%err, "engine tidak berhenti bersih setelah reap");
    }

    if !gagal.is_empty() {
        let mut pesan = String::from("selftest gagal:");
        for item in gagal {
            let _ = write!(pesan, "\n  - {item}");
        }
        bail!(pesan);
    }

    Ok(())
}

fn group_pss(group: PgidGroup) -> (f64, usize, u64) {
    read_process_group(group.pgid()).map_or((0.0, 0, 0), |reading| {
        (
            bytes_ke_mb(reading.pss_bytes),
            reading.processes,
            reading.pss_bytes,
        )
    })
}

fn group_rss(group: PgidGroup) -> (f64, usize) {
    read_process_group(group.pgid()).map_or((0.0, 0), |reading| {
        (bytes_ke_mb(reading.rss_bytes), reading.processes)
    })
}

/// MB desimal (1 MB = 1_000_000 byte), sama seperti [`lifecycle::budget`].
/// Dibulatkan ke satu desimal di SATU tempat saja supaya angka yang
/// dibandingkan dan angka yang dicetak selalu nilai yang sama.
fn bytes_ke_mb(bytes: u64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "selisih di bawah 2^53 byte tidak berarti untuk angka MB"
    )]
    let mb = bytes as f64 / MB as f64;
    (mb * 10.0).round() / 10.0
}

fn unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |dur| dur.as_millis())
}

fn port_of(http_base: &str) -> Result<u16> {
    http_base
        .rsplit(':')
        .next()
        .and_then(|port| port.parse().ok())
        .with_context(|| format!("port tidak terbaca dari {http_base}"))
}

fn buka_tabs(port: u16, jumlah: u32, url: &str) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for nomor in 0..jumlah {
        let target = tunggu_json(port, "PUT", &format!("/json/new?{url}"))
            .with_context(|| format!("tab ke-{} tidak bisa dibuka", nomor + 1))?;
        let id = target["id"]
            .as_str()
            .context("target tanpa id")?
            .to_string();
        ids.push(id);
    }
    Ok(ids)
}

fn tutup_tab(port: u16, id: &str) {
    if let Err(err) = http(port, "GET", &format!("/json/close/{id}")) {
        tracing::warn!(%err, id, "tab tidak bisa ditutup; pengukuran sesudahnya jadi tidak sah");
    }
}

/// Endpoint `/json/new` baru siap beberapa saat setelah port debug terbuka.
/// Mencoba sekali lalu menyerah akan melaporkan kegagalan mesin yang sebenarnya
/// sehat.
fn tunggu_json(port: u16, method: &str, path: &str) -> Result<serde_json::Value> {
    let mut terakhir = String::from("belum dicoba");
    for _ in 0..40 {
        match http(port, method, path) {
            Ok(body) => match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(value) => return Ok(value),
                Err(err) => terakhir = format!("respons bukan JSON: {err}"),
            },
            Err(err) => terakhir = err.to_string(),
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!("tidak ada respons sah setelah 10 detik: {terakhir}")
}

fn http(port: u16, method: &str, path: &str) -> Result<String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(1000))
        .with_context(|| format!("menyambung ke 127.0.0.1:{port} gagal"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    )?;

    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if body_lengkap(&buf) {
                    break;
                }
            }
            // Chrome tidak menutup socket walau diminta; timeout di sini normal.
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(err) => return Err(err.into()),
        }
    }

    let teks = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = teks
        .split_once("\r\n\r\n")
        .context("respons tanpa pemisah header")?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("000");
    if !status.starts_with('2') {
        bail!("HTTP {status} untuk {method} {path}");
    }
    Ok(body.to_string())
}

fn body_lengkap(buf: &[u8]) -> bool {
    let teks = String::from_utf8_lossy(buf);
    let Some((head, body)) = teks.split_once("\r\n\r\n") else {
        return false;
    };
    head.lines()
        .find_map(|line| line.strip_prefix("Content-Length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .is_some_and(|len| body.len() >= len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Server uji harus menghabiskan permintaan klien sebelum menulis jawaban.
    /// Kalau tidak, `close()` mengirim RST dan tulisannya dibuang kernel, lalu
    /// tulis berikutnya gagal dengan `Broken pipe` yang tidak ada hubungannya
    /// dengan yang sedang diuji.
    fn baca_permintaan(socket: &mut std::net::TcpStream) {
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(socket.try_clone().expect("klon socket"));
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) if line == "\r\n" || line == "\n" => break,
                Ok(_) => {}
            }
        }
    }

    #[test]
    fn body_lengkap_menghitung_content_length() {
        assert!(body_lengkap(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}"
        ));
        assert!(!body_lengkap(
            b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n{}"
        ));
        assert!(!body_lengkap(b"HTTP/1.1 200 OK\r\n\r\n"));
    }

    #[test]
    fn port_dibaca_dari_http_base() {
        assert_eq!(port_of("http://127.0.0.1:41234").expect("port"), 41234);
        assert!(port_of("http://127.0.0.1").is_err(), "tanpa port = galat");
    }

    #[test]
    fn mb_dicetak_pada_satu_desimal() {
        assert!((bytes_ke_mb(450 * MB) - 450.0).abs() < f64::EPSILON);
        assert!((bytes_ke_mb(356_000_000) - 356.0).abs() < f64::EPSILON);
        assert!((bytes_ke_mb(1_049) - 0.0).abs() < f64::EPSILON, "0,001 MB");
        assert!(
            (bytes_ke_mb(1_250_000) - 1.3).abs() < f64::EPSILON,
            "1,25 MB dibulatkan ke atas"
        );
    }

    #[test]
    fn http_membaca_respons_dari_server_lokal() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("port bebas");
        let port = listener.local_addr().expect("alamat").port();

        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("satu koneksi");
            baca_permintaan(&mut socket);
            let body = r#"{"id":"ABC"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes());
        });

        let body = http(port, "PUT", "/json/new?about:blank").expect("respons terbaca");
        assert_eq!(body, r#"{"id":"ABC"}"#);
        server.join().expect("thread server");
    }

    #[test]
    fn http_menolak_status_bukan_2xx() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("port bebas");
        let port = listener.local_addr().expect("alamat").port();

        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("satu koneksi");
            baca_permintaan(&mut socket);
            let response = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
            let _ = socket.write_all(response.as_bytes());
        });

        let err = http(port, "GET", "/json/close/tidak-ada").expect_err("harus galat");
        assert!(err.to_string().contains("404"), "pesan: {err}");
        server.join().expect("thread server");
    }
}
