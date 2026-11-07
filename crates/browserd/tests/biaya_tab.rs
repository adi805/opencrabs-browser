//! Berapa biaya satu tab, diukur, bukan dikira.
//!
//! Pertanyaannya bukan "berapa RSS satu browser kosong" melainkan "berapa
//! tambahan biaya tiap tab yang benar-benar dibuka dan dijalankan". Yang
//! diukur karena itu dua angka terpisah:
//!
//! * baseline: mesin hidup dengan satu tab `about:blank`,
//! * marjinal: selisih RSS setiap kali satu tab baru memuat halaman asli.
//!
//! RSS dihitung dari seluruh process group, bukan proses induk. Renderer,
//! zygote, dan GPU process adalah tempat sebagian besar memori pergi;
//! mengukur induknya saja menghasilkan angka yang terlalu bagus untuk
//! dilaporkan ke siapa pun.

use std::time::{Duration, Instant};

use engine::probe::{discover_full, discover_shell, discover_xvfb};
use engine::{Engine, LaunchSpec, PenuhEngine, ShellEngine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

mod common;

const LISTEN: &str = "127.0.0.1";

/// Halaman fixture. Cukup besar untuk memaksa renderer sungguhan bekerja:
/// teks, DOM yang dibangun skrip, dan satu gambar data-URI. Kalau halamannya
/// kosong, angka yang keluar adalah angka browser kosong, bukan angka kerja.
const HALAMAN: &str = "<!doctype html><html><head><meta charset=utf-8>\
<title>biaya tab</title><style>\
body{font:16px system-ui;margin:0;padding:24px}\
.kotak{display:inline-block;width:120px;height:80px;margin:4px;\
background:linear-gradient(45deg,#123,#abc)}\
</style></head><body><h1>biaya tab</h1><div id=isi></div>\
<script>\
const isi = document.getElementById('isi');\
const potongan = [];\
for (let i = 0; i < 400; i += 1) {\
  potongan.push('<div class=kotak data-i=' + i + '>' + i + '</div>');\
}\
isi.innerHTML = potongan.join('');\
document.body.dataset.siap = 'ya';\
</script></body></html>";

struct Cdp {
    aliran: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: u64,
}

impl Cdp {
    async fn sambung(url: &str) -> Result<Self, String> {
        let (aliran, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|err| format!("websocket {url}: {err}"))?;
        Ok(Self { aliran, next_id: 1 })
    }

    async fn perintah(
        &mut self,
        method: &str,
        params: Value,
        sesi: Option<&str>,
    ) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut pesan = json!({"id": id, "method": method, "params": params});
        if let Some(sesi) = sesi {
            pesan["sessionId"] = json!(sesi);
        }
        self.aliran
            .send(Message::Text(pesan.to_string().into()))
            .await
            .map_err(|err| format!("{method}: kirim: {err}"))?;

        let batas = Instant::now() + Duration::from_secs(30);
        loop {
            let sisa = batas.saturating_duration_since(Instant::now());
            if sisa.is_zero() {
                return Err(format!("{method}: tidak ada jawaban dalam 30 detik"));
            }
            let pesan = tokio::time::timeout(sisa, self.aliran.next())
                .await
                .map_err(|_| format!("{method}: timeout menunggu jawaban"))?
                .ok_or_else(|| format!("{method}: socket ditutup"))?
                .map_err(|err| format!("{method}: baca: {err}"))?;
            let teks = match pesan {
                Message::Text(teks) => teks.to_string(),
                Message::Binary(bin) => String::from_utf8_lossy(&bin).to_string(),
                _ => continue,
            };
            let value: Value = match serde_json::from_str(&teks) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(format!("{method}: galat CDP: {error}"));
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn evaluasi(&mut self, sesi: &str, ekspresi: &str) -> Result<String, String> {
        let hasil = self
            .perintah(
                "Runtime.evaluate",
                json!({"expression": ekspresi, "returnByValue": true}),
                Some(sesi),
            )
            .await?;
        let value = hasil.get("result").cloned().unwrap_or(Value::Null);
        Ok(match value.get("value") {
            Some(Value::String(teks)) => teks.clone(),
            Some(lain) => lain.to_string(),
            None => String::new(),
        })
    }
}

/// Argumen tambahan dari `BIAYA_ARGS`, dipisah spasi. Dipakai untuk menguji
/// hipotesis biaya (mis. `--process-per-site`) pada jalur peluncuran yang sama
/// persis, bukan pada skrip peluncuran terpisah yang bisa berbeda diam-diam.
fn argumen_tambahan() -> Vec<String> {
    std::env::var("BIAYA_ARGS")
        .ok()
        .map(|teks| {
            teks.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<String>>()
        })
        .unwrap_or_default()
}

fn port_bebas() -> u16 {
    let listener = std::net::TcpListener::bind((LISTEN, 0)).expect("port bebas");
    listener.local_addr().expect("alamat lokal").port()
}

async fn sajikan_fixture() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind((LISTEN, 0))
        .await
        .expect("bind fixture");
    let port = listener.local_addr().expect("alamat fixture").port();
    let tugas = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let jawab = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                HALAMAN.len(),
                HALAMAN
            );
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let _ = stream.write_all(jawab.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    (format!("http://{LISTEN}:{port}/"), tugas)
}

fn rss_kb(pid: u32) -> u64 {
    let Ok(isi) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
        return 0;
    };
    isi.lines()
        .find_map(|baris| baris.strip_prefix("VmRSS:"))
        .and_then(|nilai| nilai.split_whitespace().next())
        .and_then(|angka| angka.parse::<u64>().ok())
        .unwrap_or(0)
}

/// PSS satu proses, dalam kB. Berbeda dari RSS, halaman yang dipakai bersama
/// dibagi rata ke semua pemakainya, jadi jumlah PSS mendekati RAM yang
/// benar-benar terpakai. RSS menjumlahkan biner Chrome yang sama sekali di
/// setiap proses dan melaporkan angka yang terlalu tinggi.
fn pss_kb(pid: u32) -> u64 {
    let Ok(isi) = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")) else {
        return 0;
    };
    isi.lines()
        .find_map(|baris| baris.strip_prefix("Pss:"))
        .and_then(|nilai| nilai.split_whitespace().next())
        .and_then(|angka| angka.parse::<u64>().ok())
        .unwrap_or(0)
}

/// RSS, PSS, dan jumlah proses untuk satu process group.
fn rss_grup_kb(pgid: u32) -> (u64, u64, usize) {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return (0, 0, 0);
    };
    let mut total = 0;
    let mut total_pss = 0;
    let mut jumlah = 0;
    for entry in entries.flatten() {
        let Some(nama) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(pid) = nama.parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // `pid (comm) state ppid pgrp ...`; `comm` bisa berisi spasi dan
        // kurung, jadi pembacaan dimulai sesudah kurung tutup terakhir.
        let Some(ekor) = stat.rsplit_once(')').map(|(_, ekor)| ekor) else {
            continue;
        };
        let kolom: Vec<&str> = ekor.split_whitespace().collect();
        if kolom.get(2).and_then(|teks| teks.parse::<u32>().ok()) == Some(pgid) {
            total += rss_kb(pid);
            total_pss += pss_kb(pid);
            jumlah += 1;
        }
    }
    (total, total_pss, jumlah)
}

/// Satu baris pengukuran: RSS (kB) dan jumlah proses pada satu titik.
struct Ukur {
    rss: u64,
    pss: u64,
    proses: usize,
}

/// Membuka `maks` tab satu per satu, memuat halaman asli di masing-masing,
/// dan mengukur RSS grup sesudah setiap tab siap.
async fn ukur(engine: &dyn Engine, url: &str, maks: usize) -> Result<Vec<Ukur>, String> {
    let pid = engine.process_id().ok_or("mesin tanpa pid")?;
    let pgid = pid;
    let mut cdp = Cdp::sambung(engine.web_socket_url()).await?;

    // Tab pertama: `about:blank`, dipakai sebagai baseline. Baseline yang
    // diukur dari browser tanpa tab sama sekali bukan baseline: renderer
    // utama baru lahir ketika ada tab.
    cdp.perintah("Target.createTarget", json!({"url": "about:blank"}), None)
        .await?;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let (rss_awal, pss_awal, proses_awal) = rss_grup_kb(pgid);
    println!(
        "UKUR tab  0: rss {:>5} MB, pss {:>5} MB, proses {proses_awal:>3} (baseline)",
        rss_awal / 1024,
        pss_awal / 1024
    );
    let mut sesi_semua: Vec<String> = Vec::new();
    let mut hasil = vec![Ukur {
        rss: rss_awal,
        pss: pss_awal,
        proses: proses_awal,
    }];

    for nomor in 1..=maks {
        let target = cdp
            .perintah(
                "Target.createTarget",
                json!({"url": format!("{url}?tab={nomor}")}),
                None,
            )
            .await?;
        let target_id = target
            .get("targetId")
            .and_then(Value::as_str)
            .ok_or("Target.createTarget tanpa targetId")?
            .to_owned();
        let sesi = cdp
            .perintah(
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
                None,
            )
            .await?
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or("attachToTarget tanpa sessionId")?
            .to_owned();

        cdp.perintah("Runtime.enable", json!({}), Some(&sesi))
            .await?;
        cdp.perintah("Page.enable", json!({}), Some(&sesi)).await?;
        sesi_semua.push(sesi.clone());

        // Tab dianggap siap ketika skrip halamannya sendiri sudah jalan.
        // `readyState` saja tidak cukup: rantai skrip di dalam fixture yang
        // membangun 400 elemen adalah bagian dari beban yang diukur.
        let batas = Instant::now() + Duration::from_secs(30);
        loop {
            if Instant::now() > batas {
                return Err(format!("tab {nomor} tidak siap dalam 30 detik"));
            }
            match cdp
                .evaluasi(&sesi, "document.body && document.body.dataset.siap")
                .await
            {
                Ok(nilai) if nilai == "ya" => break,
                _ => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        // Jeda singkat supaya alokasi renderer stabil sebelum dibaca.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let (rss, pss, proses) = rss_grup_kb(pgid);
        println!(
            "UKUR tab {nomor:>2}: rss {:>5} MB, pss {:>5} MB, proses {proses:>3}, \
             marjinal pss {:>3} MB/tab",
            rss / 1024,
            pss / 1024,
            (pss.saturating_sub(pss_awal)) / 1024 / nomor as u64
        );
        hasil.push(Ukur { rss, pss, proses });
    }
    // Fase beku: tab yang tidak dipakai dibekukan, lalu diukur lagi. Ini yang
    // dipakai browser besar untuk menahan puluhan tab tanpa menahan RAM-nya:
    // dokumennya tetap ada, timernya berhenti, dan renderer-nya bisa dilepas.
    if std::env::var("BIAYA_FASE").map(|teks| teks.contains("freeze")) == Ok(true)
        && let Some(panas) = sesi_semua.last().cloned()
    {
        let dingin: Vec<String> = sesi_semua
            .iter()
            .filter(|sesi| **sesi != panas)
            .cloned()
            .collect();
        let mut beku = 0;
        for sesi in &dingin {
            if cdp
                .perintah(
                    "Page.setWebLifecycleState",
                    json!({"state": "frozen"}),
                    Some(sesi),
                )
                .await
                .is_ok()
            {
                beku += 1;
            }
        }
        tokio::time::sleep(Duration::from_secs(4)).await;
        let (rss, pss, proses) = rss_grup_kb(pgid);
        println!(
            "RINGKAS beku: {beku} tab dibekukan, pss {} MB, rss {} MB, proses {proses}",
            pss / 1024,
            rss / 1024
        );
    }
    Ok(hasil)
}

/// Menunggu process group benar-benar hilang.
fn tunggu_mati(pgid: u32, batas: Duration) -> (u64, usize) {
    let akhir = Instant::now() + batas;
    loop {
        let (rss, _pss, jumlah) = rss_grup_kb(pgid);
        if jumlah == 0 || Instant::now() > akhir {
            return (rss, jumlah);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn biaya_per_tab_dua_engine() {
    let maks: usize = std::env::var("TAB_MAKS")
        .ok()
        .and_then(|teks| teks.parse().ok())
        .unwrap_or(20);

    // Uji live lain memakai kunci yang sama: dua Chromium besar yang berebut
    // RAM membuat angka biaya tab jadi angka host, bukan angka browser.
    let _kunci = common::kunci_live("biaya_tab");

    let (url, tugas_fixture) = sajikan_fixture().await;

    // ---- shell ----
    if let Some(biner) = discover_shell() {
        let profil = std::env::temp_dir().join(format!("ocb-biaya-shell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&profil);
        std::fs::create_dir_all(&profil).expect("folder profil shell");
        let spec = LaunchSpec::new(&biner, &profil)
            .with_port(port_bebas())
            .with_args(argumen_tambahan());
        println!("ARGS shell: {:?}", spec.extra_args);
        let mulai = Instant::now();
        let mesin = ShellEngine::launch(spec, 60_000).expect("shell gagal dinyalakan");
        let boot = mulai.elapsed();
        let pid = mesin.process_id().expect("pid shell");
        println!("\n=== ENGINE shell (chrome-headless-shell) ===");
        println!("BOOT shell: {} ms", boot.as_millis());
        let hasil = ukur(&mesin, &url, maks).await.expect("ukur shell");
        let akhir = hasil.last().expect("ada hasil");
        let (rss, pss) = (akhir.rss, akhir.pss);
        let (rss_awal, pss_awal) = (hasil[0].rss, hasil[0].pss);
        println!(
            "RINGKAS shell: baseline pss {} MB, {maks} tab pss {} MB, marjinal pss {} MB/tab \
             (rss {} MB -> {} MB)",
            pss_awal / 1024,
            pss / 1024,
            (pss.saturating_sub(pss_awal)) / 1024 / maks as u64,
            rss_awal / 1024,
            rss / 1024
        );
        mesin.stop().expect("stop shell");
        let (sisa_rss, sisa) = tunggu_mati(pid, Duration::from_secs(10));
        println!("SISA shell: {sisa} proses, {sisa_rss} kB");
        assert_eq!(sisa, 0, "process group shell harus habis, sisa {sisa}");
        let _ = std::fs::remove_dir_all(&profil);
    } else {
        println!("DILEWATI shell: chrome-headless-shell tidak ditemukan");
    }

    // ---- full ----
    if discover_full().is_none() || discover_xvfb().is_none() {
        println!("DILEWATI full: google-chrome atau Xvfb tidak ditemukan");
        tugas_fixture.abort();
        return;
    }
    let biner = discover_full().expect("chrome penuh");
    let profil = std::env::temp_dir().join(format!("ocb-biaya-full-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profil);
    std::fs::create_dir_all(&profil).expect("folder profil full");
    let spec = LaunchSpec::new(&biner, &profil)
        .with_port(port_bebas())
        .with_args(argumen_tambahan());
    println!("ARGS full: {:?}", spec.extra_args);
    let mulai = Instant::now();
    let mesin = PenuhEngine::launch(spec, 60_000).expect("chrome-full gagal dinyalakan");
    let boot = mulai.elapsed();
    let pid = mesin.process_id().expect("pid full");
    println!("\n=== ENGINE full (chrome di Xvfb) ===");
    println!("BOOT full: {} ms", boot.as_millis());
    let hasil = ukur(&mesin, &url, maks).await.expect("ukur full");
    let akhir = hasil.last().expect("ada hasil");
    let (rss, pss, proses) = (akhir.rss, akhir.pss, akhir.proses);
    let (rss_awal, pss_awal) = (hasil[0].rss, hasil[0].pss);
    println!(
        "RINGKAS full: baseline pss {} MB, {maks} tab pss {} MB, marjinal pss {} MB/tab \
         (rss {} MB -> {} MB)",
        pss_awal / 1024,
        pss / 1024,
        (pss.saturating_sub(pss_awal)) / 1024 / maks as u64,
        rss_awal / 1024,
        rss / 1024
    );
    mesin.stop().expect("stop full");
    let (sisa_rss, sisa) = tunggu_mati(pid, Duration::from_secs(10));
    println!("SISA full: {sisa} proses, {sisa_rss} kB");
    assert_eq!(sisa, 0, "process group full harus habis, sisa {sisa}");
    let _ = std::fs::remove_dir_all(&profil);

    assert!(
        proses > 0,
        "browser yang menjalankan {maks} tab tidak mungkin nol proses"
    );
    tugas_fixture.abort();
}
