//! Uji kapabilitas `chrome-full` (Chromium headed di dalam Xvfb) pada mesin
//! sungguhan.
//!
//! Uji ini **mengukur**, bukan mengasumsikan. Yang dicetak adalah jawaban mesin
//! sendiri lewat CDP: field `/json/version`, hasil `Target.createTarget`,
//! `Runtime.evaluate`, `DOM`, screenshot, cookie, input, dan RSS seluruh group
//! proses. Angka dan `ok`/`GAGAL` di laporan itulah yang dipakai mengisi
//! `policy::TERUKUR_FULL`, sehingga katalog tidak mengarang kemampuan yang
//! tidak ada.
//!
//! Bedanya dengan `oxi_probe.rs`: di sini mesin dinyalakan lewat
//! [`engine::PenuhEngine`], bukan lewat launcher tulis sendiri. Alasannya
//! adapter inilah yang akan dipakai daemon, jadi yang diukur memang jalur
//! produksinya: kalau adapter lupa menyalakan Xvfb atau lupa meneruskan
//! `DISPLAY`, hasilnya harus terlihat GAGAL di sini, bukan di produksi.
//!
//! Selain empat kemampuan, uji ini mencatat dua fakta yang menjadi alasan
//! mesin ini ada: nilai `navigator.webdriver` dan user agent yang dilaporkan
//! `/json/version`. Keduanya fakta terukur, bukan klaim "tidak terdeteksi".
//!
//! Dilewati dengan pesan jelas kalau Chromium penuh atau Xvfb tidak ada, supaya
//! `cargo test` tetap hijau di mesin yang tidak punya keduanya.

use std::time::{Duration, Instant};

use engine::{Engine, LaunchSpec, PenuhEngine, discover_full, discover_xvfb};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const LISTEN: &str = "127.0.0.1";

/// Halaman fixture. Skrip di dalamnya berjalan sendiri, jadi kemampuan
/// `script` diukur dari **halaman**, bukan dari `Runtime.evaluate` milik CDP.
const HALAMAN: &str = "<!doctype html><html><head><title>probe-full</title></head>\
<body style=\"margin:0;padding:0;width:1200px;height:600px\">\
<h1 id=\"judul\">probe</h1>\
<script>document.body.dataset.skrip = 'ya';</script>\
</body></html>";

/// Klien CDP minimal. Cukup untuk bertanya, tidak lebih.
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

    /// Mengirim satu perintah dan menunggu balasan dengan id yang sama. Event di
    /// antaranya diabaikan: uji ini menilai jawaban, bukan aliran event.
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

        let batas = Instant::now() + Duration::from_secs(20);
        loop {
            let sisa = batas.saturating_duration_since(Instant::now());
            if sisa.is_zero() {
                return Err(format!("{method}: tidak ada jawaban dalam 20 detik"));
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

    /// Jalan pintas untuk `Runtime.evaluate` yang mengembalikan teks.
    async fn evaluasi(&mut self, sesi: &str, ekspresi: &str) -> Result<String, String> {
        let hasil = self
            .perintah(
                "Runtime.evaluate",
                json!({"expression": ekspresi, "returnByValue": true}),
                Some(sesi),
            )
            .await?;
        let value = hasil.get("result").cloned().unwrap_or(Value::Null);
        if let Some(keterangan) = value.get("description").and_then(Value::as_str) {
            return Ok(keterangan.to_owned());
        }
        Ok(match value.get("value") {
            Some(Value::String(teks)) => teks.clone(),
            Some(lain) => lain.to_string(),
            None => String::new(),
        })
    }
}

/// Satu baris hasil yang bisa dibaca manusia dan mesin.
fn baris(laporan: &mut String, nama: &str, hasil: Result<String, String>) {
    match hasil {
        Ok(detail) => {
            println!("PROBE {nama}: ok {detail}");
            laporan.push_str(&format!("| `{nama}` | ok | {detail} |\n"));
        }
        Err(sebab) => {
            println!("PROBE {nama}: GAGAL {sebab}");
            laporan.push_str(&format!("| `{nama}` | GAGAL | {sebab} |\n"));
        }
    }
}

fn port_bebas() -> u16 {
    let listener = std::net::TcpListener::bind((LISTEN, 0)).expect("port bebas");
    listener.local_addr().expect("alamat lokal").port()
}

/// HTTP sekadarnya untuk melayani satu halaman fixture.
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

/// Entri di `/tmp/.X11-unix`. Dipakai untuk menemukan display yang dipilih
/// adapter tanpa harus membocorkan detail internalnya ke API publik.
fn socket_x() -> Vec<String> {
    std::fs::read_dir(engine::penuh::SOCKET_DIR)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect()
}

/// RSS satu proses, dalam kB, dibaca dari `/proc/<pid>/status`.
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

/// RSS seluruh proses di dalam satu group, dalam kB dan jumlah prosesnya.
///
/// Yang dihitung bukan cuma proses induk: renderer, zygote, dan GPU process
/// adalah tempat sebagian besar memori pergi, dan mengukur induknya saja
/// menghasilkan angka yang terlalu bagus untuk dilaporkan.
fn rss_grup_kb(pgid: u32) -> (u64, usize) {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return (0, 0);
    };
    let mut total = 0;
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
        // Format: `pid (comm) state ppid pgrp ...`. `comm` bisa berisi spasi dan
        // tanda kurung, jadi pembacaan dimulai sesudah kurung tutup terakhir.
        let Some(ekor) = stat.rsplit_once(')').map(|(_, ekor)| ekor) else {
            continue;
        };
        let kolom: Vec<&str> = ekor.split_whitespace().collect();
        if kolom.get(2).and_then(|teks| teks.parse::<u32>().ok()) == Some(pgid) {
            total += rss_kb(pid);
            jumlah += 1;
        }
    }
    (total, jumlah)
}

/// Apakah masih ada Xvfb yang memegang display `:n`.
fn xvfb_hidup(display: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        let Ok(isi) = std::fs::read_to_string(entry.path().join("cmdline")) else {
            return false;
        };
        isi.contains("Xvfb") && isi.contains(display)
    })
}

async fn tunggu_halaman(cdp: &mut Cdp, sesi: &str) -> Result<(), String> {
    let batas = Instant::now() + Duration::from_secs(20);
    loop {
        let siap = cdp.evaluasi(sesi, "document.readyState").await?;
        if siap == "complete" {
            return Ok(());
        }
        if Instant::now() >= batas {
            return Err(format!("readyState tetap '{siap}' sesudah 20 detik"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn ukur_layout(cdp: &mut Cdp, sesi: &str, url: &str) -> Result<String, String> {
    cdp.perintah("Page.navigate", json!({"url": url}), Some(sesi))
        .await?;
    tunggu_halaman(cdp, sesi).await?;
    let judul = cdp.evaluasi(sesi, "document.title").await?;
    let kotak = cdp
        .evaluasi(
            sesi,
            "JSON.stringify(document.body.getBoundingClientRect())",
        )
        .await?;
    let screenshot = cdp
        .perintah(
            "Page.captureScreenshot",
            json!({"format": "png"}),
            Some(sesi),
        )
        .await?;
    let panjang = screenshot
        .get("data")
        .and_then(Value::as_str)
        .map(str::len)
        .unwrap_or(0);
    if judul != "probe-full" {
        return Err(format!("judul '{judul}', harusnya 'probe-full'"));
    }
    if kotak.contains("\"width\":0") {
        return Err(format!("body tanpa ukuran: {kotak}"));
    }
    if panjang < 1000 {
        return Err(format!("screenshot cuma {panjang} byte base64"));
    }
    Ok(format!(
        "judul '{judul}', {kotak}, screenshot {panjang} B64"
    ))
}

async fn ukur_cookies(cdp: &mut Cdp, sesi: &str, url: &str) -> Result<String, String> {
    cdp.perintah(
        "Network.setCookie",
        json!({"name": "probe", "value": "ya", "url": url}),
        Some(sesi),
    )
    .await?;
    let hasil = cdp
        .perintah("Network.getCookies", json!({"urls": [url]}), Some(sesi))
        .await?;
    let nilai = hasil
        .get("cookies")
        .and_then(Value::as_array)
        .and_then(|daftar| {
            daftar
                .iter()
                .find(|cookie| cookie.get("name").and_then(Value::as_str) == Some("probe"))
        })
        .and_then(|cookie| cookie.get("value").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned();
    if nilai != "ya" {
        return Err(format!("cookie terbaca '{nilai}', harusnya 'ya'"));
    }
    Ok("cookie 'probe=ya' terbaca lagi".to_owned())
}

async fn ukur_input(cdp: &mut Cdp, sesi: &str) -> Result<String, String> {
    cdp.evaluasi(
        sesi,
        "document.body.addEventListener('click', () => { document.body.dataset.klik = 'ya'; }); 'dipasang'",
    )
    .await?;
    for jenis in ["mousePressed", "mouseReleased"] {
        cdp.perintah(
            "Input.dispatchMouseEvent",
            json!({
                "type": jenis,
                "x": 50,
                "y": 50,
                "button": "left",
                "clickCount": 1,
            }),
            Some(sesi),
        )
        .await?;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let nilai = cdp
        .evaluasi(sesi, "document.body.dataset.klik ?? ''")
        .await?;
    if nilai != "ya" {
        return Err(format!("dataset.klik='{nilai}', klik tidak mendarat"));
    }
    Ok("klik mendarat, dataset.klik='ya'".to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_kapabilitas_full() {
    let Some(biner) = discover_full() else {
        eprintln!("Dilewati: Chromium penuh tidak ditemukan (google-chrome/chromium).");
        return;
    };
    if discover_xvfb().is_none() {
        eprintln!("Dilewati: Xvfb tidak ditemukan, dan chrome-full tidak jalan tanpanya.");
        return;
    }

    let profil = std::env::temp_dir().join(format!("ocb-penuh-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profil);
    std::fs::create_dir_all(&profil).expect("folder profil");

    // Display yang dipilih adapter ditemukan dari selisih socket sebelum dan
    // sesudah boot. Tidak perlu API tambahan hanya untuk uji.
    let socket_sebelum = socket_x();

    // Port eksplisit, bukan 0: daemon produksi memakai port eksplisit, dan yang
    // diukur harus jalur yang sama.
    let port = port_bebas();
    let spec = LaunchSpec::new(&biner, &profil).with_port(port);

    // Batas 60 detik, bukan 30: di laptop ini boot terukur 3,3 sampai 7,3 detik
    // saat host sepi, dan pernah melewati 30 detik ketika puluhan browser sisa
    // test lain masih hidup dan memakan memori. Batas ini hanya langit-langit
    // kesabaran; angka yang dilaporkan tetap waktu boot yang sebenarnya.
    let mulai = Instant::now();
    let mesin = match PenuhEngine::launch(spec, 60_000) {
        Ok(mesin) => mesin,
        Err(err) => panic!("chrome-full gagal dinyalakan: {err}"),
    };
    let waktu_boot = mulai.elapsed();

    let display = socket_x()
        .into_iter()
        .find(|nama| !socket_sebelum.contains(nama))
        .map(|nama| format!(":{}", nama.trim_start_matches('X')))
        .expect("adapter harus membuat satu display X baru");
    println!("PROBE display: ok {display}");

    let info = mesin.info().clone();
    let pid = mesin.process_id().expect("proses induk punya pid");
    println!(
        "PROBE boot: ok {} ms, pid {pid}, browser {}",
        waktu_boot.as_millis(),
        info.browser
    );
    assert!(
        info.browser.to_lowercase().contains("chrome"),
        "discovery harus melaporkan Chrome, dapat: {}",
        info.browser
    );
    assert_eq!(info.devtools_port, port, "port harus yang diminta");

    let (url_fixture, tugas_fixture) = sajikan_fixture().await;
    let mut laporan = String::new();
    let mut cdp = Cdp::sambung(mesin.web_socket_url())
        .await
        .expect("websocket browser harus bisa disambung");

    let target = cdp
        .perintah("Target.createTarget", json!({"url": "about:blank"}), None)
        .await
        .expect("Target.createTarget");
    let target_id = target
        .get("targetId")
        .and_then(Value::as_str)
        .expect("targetId")
        .to_owned();
    let sesi = cdp
        .perintah(
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
            None,
        )
        .await
        .expect("Target.attachToTarget")
        .get("sessionId")
        .and_then(Value::as_str)
        .expect("sessionId")
        .to_owned();

    for domain in ["Page.enable", "Runtime.enable", "Network.enable"] {
        cdp.perintah(domain, json!({}), Some(&sesi))
            .await
            .unwrap_or_else(|err| panic!("{domain}: {err}"));
    }

    // Tiga fakta yang menjadi alasan mesin ini ada.
    baris(
        &mut laporan,
        "webdriver",
        cdp.evaluasi(&sesi, "String(navigator.webdriver)").await,
    );
    baris(&mut laporan, "user_agent", Ok(info.user_agent.clone()));
    baris(
        &mut laporan,
        "boot_ms",
        Ok(waktu_boot.as_millis().to_string()),
    );

    baris(
        &mut laporan,
        "layout",
        ukur_layout(&mut cdp, &sesi, &url_fixture).await,
    );
    baris(
        &mut laporan,
        "script",
        cdp.evaluasi(&sesi, "document.body.dataset.skrip ?? ''")
            .await
            .map(|nilai| format!("dataset.skrip='{nilai}'")),
    );
    baris(
        &mut laporan,
        "cookies",
        ukur_cookies(&mut cdp, &sesi, &url_fixture).await,
    );
    baris(&mut laporan, "input", ukur_input(&mut cdp, &sesi).await);

    let (rss, jumlah) = rss_grup_kb(pid);
    baris(
        &mut laporan,
        "rss_mb",
        Ok(format!("{} MB di {} proses", rss / 1024, jumlah)),
    );
    drop(cdp);

    println!("\nLAPORAN PROBE chrome-full\n| Probe | Hasil | Detail |\n|---|---|---|\n{laporan}");

    mesin.stop().expect("mesin harus mau berhenti");
    assert!(
        !mesin.is_running(),
        "sesudah stop, chrome-full tidak boleh hidup lagi"
    );
    assert!(
        !xvfb_hidup(&display),
        "Xvfb {display} masih hidup sesudah stop: display bocor"
    );

    tugas_fixture.abort();
    let _ = std::fs::remove_dir_all(&profil);

    assert!(
        !laporan.contains("GAGAL"),
        "ada probe yang gagal, lihat laporan di atas"
    );
}
