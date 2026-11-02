//! Bukti M4 dengan **dua mesin nyata**.
//!
//! Yang diuji bukan aturan peta identitas (itu di `crates/cdp/tests`), tapi
//! rantainya dengan dua `chrome-headless-shell` sungguhan: daemon meluncurkan
//! dua mesin dari satu berkas config, `/json/list` menggabungkan tab keduanya,
//! satu koneksi klien menavigasi tab di mesin berbeda ke dua fixture berbeda,
//! dan tiap tab melaporkan URL serta judul miliknya sendiri.
//!
//! Penutupnya mengikuti syarat roadmap §6: menutup tab di mesin satu tidak
//! boleh memengaruhi tab di mesin lain.
//!
//! Kalau `chrome-headless-shell` tidak ada di host ini, uji **dilewati** dengan
//! pesan yang menyebut apa yang kurang. Dilewati bukan berarti lulus: pesannya
//! harus terbaca di `--nocapture`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const BATAS: Duration = Duration::from_secs(90);

/// Tunggu daemon siap, atau berhenti lebih awal kalau prosesnya sudah keluar.
fn tunggu_atau_mati(daemon: &mut Child, cdp_port: u16) -> bool {
    let batas = Instant::now() + BATAS;
    while Instant::now() < batas {
        if let Ok(Some(status)) = daemon.try_wait() {
            eprintln!("daemon keluar lebih awal: {status}");
            return false;
        }
        if minta(cdp_port, "GET", "/json/version").0 == 200 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

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
    let dir = std::env::temp_dir().join(format!("ocb-m4-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("membuat direktori uji");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

/// Fixture HTTP: satu halaman dengan judul sendiri.
///
/// `Content-Length` wajib benar: tanpa itu Chrome menunggu koneksi ditutup, dan
/// `loadEventFired` baru datang setelah batas waktu, yang membuat uji ini lulus
/// atau gagal karena alasan yang salah.
fn server_fixture(judul: &'static str) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
    let port = listener.local_addr().expect("alamat fixture").port();
    let handle = std::thread::spawn(move || {
        for koneksi in listener.incoming() {
            let Ok(mut stream) = koneksi else { break };
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf);
            let body = format!(
                "<!doctype html><html><head><title>{judul}</title></head>\
                 <body><h1>{judul}</h1></body></html>"
            );
            let jawaban = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(jawaban.as_bytes());
        }
    });
    (port, handle)
}

fn minta(port: u16, metode: &str, path: &str) -> (u16, String) {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return (0, String::new());
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
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

fn buang(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Satu tab yang dilaporkan `/json/list`, sudah ber-prefiks mesin.
struct Tab {
    id: String,
    ws: String,
}

fn daftar(cdp_port: u16) -> Vec<Tab> {
    let (status, body) = minta(cdp_port, "GET", "/json/list");
    if status != 200 {
        return Vec::new();
    }
    serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|nilai| nilai.as_array().cloned())
        .map(|items| {
            items
                .iter()
                .filter_map(|tab| {
                    Some(Tab {
                        id: tab["id"].as_str()?.to_owned(),
                        ws: tab["webSocketDebuggerUrl"].as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

type Soket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type Tulis = futures_util::stream::SplitSink<Soket, tokio_tungstenite::tungstenite::Message>;
type Baca = futures_util::stream::SplitStream<Soket>;

/// Bikin tab di mesin tertentu, kembalikan `targetId` berprefiks yang dilihat
/// klien.
async fn bikin_tab(tulis: &mut Tulis, baca: &mut Baca, id: i64, mesin: &str) -> String {
    let balasan = panggil(
        tulis,
        baca,
        id,
        "Target.createTarget",
        json!({"url": "about:blank", "openCrabsOptions": {"engine": mesin}}),
        None,
    )
    .await
    .expect("balasan Target.createTarget");
    balasan["result"]["targetId"]
        .as_str()
        .expect("targetId di balasan createTarget")
        .to_owned()
}

async fn sambung(url: &str) -> Option<(Tulis, Baca)> {
    use futures_util::StreamExt;
    let (ws, _) = tokio_tungstenite::connect_async(url).await.ok()?;
    Some(ws.split())
}

/// Kirim satu perintah, kembalikan balasan ber-`id` yang sama.
///
/// Event yang lewat sebelum balasan dilewati di sini; tes yang butuh event
/// memakai [`navigasi`], yang mengumpulkan keduanya dalam satu putaran baca.
async fn panggil(
    tulis: &mut Tulis,
    baca: &mut Baca,
    id: i64,
    method: &str,
    params: Value,
    sesi: Option<&str>,
) -> Option<Value> {
    kirim(tulis, id, method, params, sesi).await?;
    tunggu_balasan(baca, id).await
}

async fn kirim(
    tulis: &mut Tulis,
    id: i64,
    method: &str,
    params: Value,
    sesi: Option<&str>,
) -> Option<()> {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let mut pesan = json!({"id": id, "method": method, "params": params});
    if let Some(sesi) = sesi {
        pesan["sessionId"] = json!(sesi);
    }
    tulis
        .send(Message::Text(pesan.to_string().into()))
        .await
        .ok()
}

async fn tunggu_balasan(baca: &mut Baca, id: i64) -> Option<Value> {
    let (balasan, _) = baca_sampai(baca, id, false).await;
    balasan
}

/// Baca pesan sampai balasan `id` datang, sekaligus menangkap
/// `Page.frameNavigated` kalau lewat.
async fn navigasi_selesai(baca: &mut Baca, id: i64) -> (Option<Value>, Option<Value>) {
    baca_sampai(baca, id, true).await
}

/// Baca pesan sampai balasan `id` datang. `tunggu_frame` juga menunggu
/// `Page.frameNavigated`: perintah biasa tidak akan pernah mendapatkannya, dan
/// menunggu event yang tidak dijanjikan cuma membuang batas waktu.
async fn baca_sampai(
    baca: &mut Baca,
    id: i64,
    tunggu_frame: bool,
) -> (Option<Value>, Option<Value>) {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    let mut balasan = None;
    let mut frame = None;
    let batas = tokio::time::Instant::now() + BATAS;
    // `Page.navigate` dibalas dulu, `Page.frameNavigated` menyusul: berhenti
    // begitu balasan datang akan kehilangan event yang justru dibuktikan.
    while balasan.is_none() || (tunggu_frame && frame.is_none()) {
        let sisa = batas.saturating_duration_since(tokio::time::Instant::now());
        // Batas waktu habis atau soket ditutup: berhenti, jangan menggantung
        // selamanya di test yang gagal.
        let Ok(Some(Ok(pesan))) = tokio::time::timeout(sisa, baca.next()).await else {
            break;
        };
        let Message::Text(teks) = pesan else {
            continue;
        };
        let Ok(nilai) = serde_json::from_str::<Value>(teks.as_str()) else {
            continue;
        };
        if nilai["method"] == "Page.frameNavigated" {
            frame = Some(nilai.clone());
        }
        if nilai["id"] == json!(id) {
            balasan = Some(nilai);
        }
    }
    (balasan, frame)
}

/// Evaluasi ekspresi di sesi tertentu dan kembalikan nilainya sebagai teks.
async fn evaluasi(tulis: &mut Tulis, baca: &mut Baca, id: i64, ekspresi: &str) -> Option<String> {
    let balasan = panggil(
        tulis,
        baca,
        id,
        "Runtime.evaluate",
        json!({"expression": ekspresi, "returnByValue": true}),
        None,
    )
    .await?;
    balasan["result"]["result"]["value"]
        .as_str()
        .map(str::to_owned)
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
         user_data_dir = \"{kiri}\"\n\
         \n\
         [engines.kanan]\n\
         binary = \"{biner}\"\n\
         user_data_dir = \"{kanan}\"\n",
        token = dir.join("control.token").display(),
        store_db = dir.join("store.db").display(),
        store_key = dir.join("store.key").display(),
        biner = biner.display(),
        kiri = dir.join("kiri-profil").display(),
        kanan = dir.join("kanan-profil").display(),
    );
    std::fs::write(&config, isi).expect("menulis config uji");
    config
}

#[test]
fn dua_mesin_nyata_tidak_saling_mengganggu() {
    let Some(biner) = biner_mesin() else {
        eprintln!(
            "SKIP m4_proof: chrome-headless-shell tidak ditemukan. Set OCB_ENGINE_BINARY \
             atau pasang di ~/.cache/ms-playwright."
        );
        return;
    };

    let dir = direktori_uji();
    let cdp_port = port_kosong();
    let control_port = port_kosong();
    let config = tulis_config(&dir, &biner, cdp_port, control_port);

    let (kiri_port, _kiri_fixture) = server_fixture("FIXTURE-KIRI");
    let (kanan_port, _kanan_fixture) = server_fixture("FIXTURE-KANAN");

    // Store butuh kunci per direktori uji. Tanpa ini daemon menolak start dan
    // test gagal dengan pesan yang tidak ada hubungannya dengan multiplex.
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

    let log = std::fs::File::create(dir.join("daemon.log")).expect("log daemon");
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_browserd"))
        .args(["serve", "--config"])
        .arg(&config)
        .stdout(Stdio::from(log.try_clone().expect("kloning log")))
        .stderr(Stdio::from(log))
        .spawn()
        .expect("menjalankan browserd");

    // Tunggu siap, tapi berhenti begitu prosesnya mati: menunggu 90 detik untuk
    // daemon yang sudah keluar cuma menyembunyikan penyebabnya.
    let siap = tunggu_atau_mati(&mut daemon, cdp_port);

    if !siap {
        buang(&mut daemon);
        let log = std::fs::read_to_string(dir.join("daemon.log")).unwrap_or_default();
        panic!("daemon tidak siap dalam {BATAS:?}; log:\n{log}");
    }

    // chrome-headless-shell tidak membuka tab sendiri, jadi tab dibuat lewat
    // `Target.createTarget`. Mesin dipilih `openCrabsOptions.engine`; tanpa itu
    // pesan mendarat di mesin bawaan dan tidak ada yang dibuktikan.
    let (_, info) = minta(cdp_port, "GET", "/json/version");
    let browser_ws = serde_json::from_str::<Value>(&info)
        .ok()
        .and_then(|nilai| nilai["webSocketDebuggerUrl"].as_str().map(str::to_owned))
        .expect("webSocketDebuggerUrl dari /json/version");

    let hasil = std::panic::catch_unwind(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime tokio");
        runtime.block_on(async move {
            let (mut tb, mut bb) = sambung(&browser_ws).await.expect("soket browser");
            // Id yang dilihat klien di soket adalah alias router (`t1`, `t2`),
            // sementara `/json/list` memakai `<mesin>~<id>`; dua sisi itu
            // sengaja tidak sama persis, jadi tab dicari lewat prefiks mesin.
            let _id_kiri = bikin_tab(&mut tb, &mut bb, 101, "kiri").await;
            let _id_kanan = bikin_tab(&mut tb, &mut bb, 111, "kanan").await;

            let tabs = daftar(cdp_port);
            let kiri = tabs
                .iter()
                .find(|tab| tab.id.starts_with("kiri~"))
                .map(|tab| Tab {
                    id: tab.id.clone(),
                    ws: tab.ws.clone(),
                })
                .expect("tab mesin kiri di /json/list");
            let kanan = tabs
                .iter()
                .find(|tab| tab.id.starts_with("kanan~"))
                .map(|tab| Tab {
                    id: tab.id.clone(),
                    ws: tab.ws.clone(),
                })
                .expect("tab mesin kanan di /json/list");

            let (mut tk, mut bk) = sambung(&kiri.ws).await.expect("soket tab kiri");
            let (mut tn, mut bn) = sambung(&kanan.ws).await.expect("soket tab kanan");

            for (tulis, baca, id) in [(&mut tk, &mut bk, 11_i64), (&mut tn, &mut bn, 21_i64)] {
                for (n, method) in [(id, "Page.enable"), (id + 1, "Runtime.enable")] {
                    panggil(tulis, baca, n, method, json!({}), None)
                        .await
                        .expect("mengaktifkan domain");
                }
            }

            // Navigasi kedua tab ke fixture berbeda. Event `frameNavigated`
            // yang kembali harus menyebut URL tab itu sendiri.
            kirim(
                &mut tk,
                31,
                "Page.navigate",
                json!({"url": format!("http://127.0.0.1:{kiri_port}/")}),
                None,
            )
            .await
            .expect("navigasi kiri");
            let (balasan, frame) = navigasi_selesai(&mut bk, 31).await;
            assert!(balasan.is_some(), "balasan navigasi kiri tidak datang");
            let frame = frame.unwrap_or_else(|| panic!("frameNavigated tab kiri: {balasan:?}"));
            assert!(
                frame["params"]["frame"]["url"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(&format!(":{kiri_port}")),
                "event tab kiri menyebut URL tab lain: {frame}"
            );

            kirim(
                &mut tn,
                41,
                "Page.navigate",
                json!({"url": format!("http://127.0.0.1:{kanan_port}/")}),
                None,
            )
            .await
            .expect("navigasi kanan");
            let (balasan, frame) = navigasi_selesai(&mut bn, 41).await;
            assert!(balasan.is_some(), "balasan navigasi kanan tidak datang");
            let frame = frame.expect("frameNavigated tab kanan");
            assert!(
                frame["params"]["frame"]["url"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(&format!(":{kanan_port}")),
                "event tab kanan menyebut URL tab lain: {frame}"
            );

            // Bukti isi: tiap tab memuat judul fixture-nya sendiri, bukan
            // judul tetangganya.
            let judul_kiri = evaluasi(&mut tk, &mut bk, 51, "document.title")
                .await
                .expect("judul tab kiri");
            assert_eq!(judul_kiri, "FIXTURE-KIRI", "tab kiri memuat halaman lain");
            let judul_kanan = evaluasi(&mut tn, &mut bn, 61, "document.title")
                .await
                .expect("judul tab kanan");
            assert_eq!(
                judul_kanan, "FIXTURE-KANAN",
                "tab kanan memuat halaman lain"
            );

            // Roadmap §6: tutup tab mesin kanan, tab kiri harus tetap menjawab.
            let (status, body) = minta(cdp_port, "GET", &format!("/json/close/{}", kanan.id));
            assert_eq!(status, 200, "menutup tab kanan: {body}");

            let sesudah = evaluasi(&mut tk, &mut bk, 71, "document.title")
                .await
                .expect("tab kiri masih menjawab setelah tab kanan ditutup");
            assert_eq!(
                sesudah, "FIXTURE-KIRI",
                "tab kiri berubah setelah tab kanan ditutup"
            );
        });
    });

    buang(&mut daemon);
    // `OCB_KEEP=1` menahan direktori uji untuk diperiksa setelah lulus; bawaan
    // bersih supaya /tmp tidak menumpuk profil Chromium tiap run.
    if std::env::var("OCB_KEEP").is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    if hasil.is_err() {
        panic!("bukti M4 gagal");
    }
}
