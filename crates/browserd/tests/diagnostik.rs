//! Bukti milestone 2 dengan mesin nyata: tiga halaman lokal, tiga kategori.
//!
//! Yang diuji bukan unit aturan, tapi rantai penuh pada `chrome-headless-shell`
//! sungguhan: navigasi lewat daemon, pengamatan respons dokumen, probe DOM,
//! klasifikasi, artefak, dan jejak eskalasi. Unit test sudah membuktikan tiap
//! potongan; yang belum terbukti dan hanya bisa dibuktikan di sini adalah
//! apakah potongan-potongan itu menghasilkan kategori **berbeda** untuk halaman
//! yang memang berbeda.
//!
//! Tiga halaman fixture, sengaja dibuat dari yang paling tidak bisa
//! disalahartikan:
//!
//! | halaman      | apa yang dilihat mesin                        | kategori         |
//! |--------------|-----------------------------------------------|------------------|
//! | `/normal`    | 200, dokumen selesai, tanpa tanda tantangan   | terbuka          |
//! | `/tantangan` | 403, `#challenge-form`, teks "Just a moment"  | `blok_anti_bot`  |
//! | `/otp`       | 200, ada field OTP/2FA                        | `butuh_manusia`  |
//! | port mati    | `net::ERR_CONNECTION_REFUSED`                 | `jaringan`       |
//!
//! Halaman kedua dan ketiga sengaja sama-sama "tidak bisa dipakai", dan hanya
//! urutan pemeriksaan yang membuat keduanya tidak tertukar: OTP diperiksa
//! sebelum tantangan, karena halaman 2FA sering memuat widget captcha di
//! dalamnya.
//!
//! Kalau `chrome-headless-shell` tidak ada di host ini, uji dilewati dengan
//! pesan yang menyebut apa yang kurang. Dilewati bukan lulus: pesannya harus
//! terbaca di `--nocapture`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use browserd::artefak::{Isi, Kuota, Penulis};
use browserd::eskalasi::Eskalasi;
use browserd::klasifikasi::{Hasil, Kategori, Kenyataan};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const BATAS: Duration = Duration::from_secs(60);
/// Batas tunggu satu navigasi. Cukup untuk halaman lokal, dan sengaja lebih
/// pendek dari [`BATAS`] supaya halaman yang tidak pernah selesai memuat
/// menghasilkan `render_lambat` dengan angka, bukan uji yang menggantung.
const BATAS_NAVIGASI_DETIK: f64 = 8.0;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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
    let dir = std::env::temp_dir().join(format!("ocb-diagnostik-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("membuat direktori uji");
    dir
}

/// Isi satu halaman fixture.
enum Halaman {
    Normal,
    Tantangan,
    Otp,
}

impl Halaman {
    fn respons(&self) -> (u16, &'static str, &'static str) {
        match self {
            Self::Normal => (
                200,
                "OK",
                "<html><head><title>Halaman Normal</title></head><body>\
                 <h1>Laporan harian</h1><p>Semua angka sudah masuk.</p></body></html>",
            ),
            Self::Tantangan => (
                403,
                "Forbidden",
                "<html><head><title>Just a moment</title></head><body>\
                 <div id=\"challenge-form\"></div><div class=\"cf-turnstile\"></div>\
                 <p>Verify you are human</p></body></html>",
            ),
            Self::Otp => (
                200,
                "OK",
                "<html><head><title>Verifikasi dua langkah</title></head><body>\
                 <form><label>Kode verifikasi</label>\
                 <input id=\"otp\" name=\"otp\" autocomplete=\"one-time-code\">\
                 </form></body></html>",
            ),
        }
    }
}

/// Server fixture: satu port, tiga path, jawab apa pun selain itu dengan 404.
fn server_fixture() -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
    let port = listener.local_addr().expect("alamat fixture").port();
    let handle = std::thread::spawn(move || {
        for koneksi in listener.incoming() {
            let Ok(mut stream) = koneksi else { break };
            let mut buf = [0_u8; 2048];
            let dibaca = stream.read(&mut buf).unwrap_or(0);
            let permintaan = String::from_utf8_lossy(&buf[..dibaca]).into_owned();
            let path = permintaan
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .to_owned();
            let halaman = match path.as_str() {
                "/normal" => Some(Halaman::Normal),
                "/tantangan" => Some(Halaman::Tantangan),
                "/otp" => Some(Halaman::Otp),
                _ => None,
            };
            let (kode, frasa, body) = match &halaman {
                Some(halaman) => halaman.respons(),
                None => (404, "Not Found", "<html><body>tidak ada</body></html>"),
            };
            let respons = format!(
                "HTTP/1.1 {kode} {frasa}\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(respons.as_bytes());
            let _ = stream.flush();
        }
    });
    (port, handle)
}

fn minta(port: u16, path: &str) -> (u16, String) {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return (0, String::new());
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let permintaan = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
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

fn buang(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Apa yang diamati selama satu navigasi.
#[derive(Debug, Default)]
struct Fakta {
    status: Option<u16>,
    url: Option<String>,
    galat: Option<String>,
    otp: bool,
    selesai: bool,
    judul: Option<String>,
    log: Vec<String>,
}

impl Fakta {
    /// Simpan satu event sebagai satu baris log. Yang disimpan event yang
    /// relevan saja; seluruh event mentah akan menghabiskan kuota log untuk
    /// hal yang tidak dibaca siapa pun.
    fn rekam(&mut self, method: &str, params: &Value) {
        match method {
            "Network.responseReceived" => {
                let jenis = params["type"].as_str().unwrap_or("");
                if jenis != "Document" {
                    return;
                }
                let status = params["response"]["status"].as_u64().unwrap_or(0);
                let url = params["response"]["url"].as_str().unwrap_or("");
                self.status = Some(u16::try_from(status).unwrap_or(0));
                self.url.get_or_insert_with(|| url.to_owned());
                self.log
                    .push(format!("respons dokumen status={status} url={url}"));
            }
            "Network.loadingFailed" => {
                let galat = params["errorText"].as_str().unwrap_or("").to_owned();
                self.log.push(format!("loadingFailed={galat}"));
                if self.galat.is_none() && !galat.is_empty() {
                    self.galat = Some(galat);
                }
            }
            "Page.frameNavigated" if params["frame"]["parentId"].is_null() => {
                // Frame utama saja: iframe tantangan sering memuat judul
                // "Just a moment", dan URL anak yang dipakai sebagai URL akhir
                // akan menyesatkan klasifikasi.
                let url = params["frame"]["url"].as_str().unwrap_or("");
                self.log.push(format!("frameNavigated url={url}"));
                self.url = Some(url.to_owned());
            }
            _ => {}
        }
    }
}

/// Kirim satu perintah dan kumpulkan event yang lewat sampai balasannya datang.
async fn panggil(
    tulis: &mut futures_util::stream::SplitSink<Ws, Message>,
    baca: &mut futures_util::stream::SplitStream<Ws>,
    id: i64,
    method: &str,
    params: Value,
    session: Option<&str>,
    fakta: &mut Fakta,
) -> Option<Value> {
    let mut pesan = json!({"id": id, "method": method, "params": params});
    if let Some(session) = session {
        pesan["sessionId"] = json!(session);
    }
    tulis
        .send(Message::Text(pesan.to_string().into()))
        .await
        .ok()?;
    loop {
        let pesan = tokio::time::timeout(Duration::from_secs(15), baca.next())
            .await
            .ok()??
            .ok()?;
        let Message::Text(teks) = pesan else { continue };
        let nilai: Value = serde_json::from_str(teks.as_str()).ok()?;
        if nilai.get("id") == Some(&json!(id)) {
            return Some(nilai);
        }
        if let Some(method) = nilai.get("method").and_then(Value::as_str)
            && let Some(params) = nilai.get("params")
        {
            fakta.rekam(method, params);
        }
    }
}

/// Script pembaca keadaan dokumen. Dijalankan berulang sampai dokumen selesai
/// atau batas waktu habis, jadi satu pembacaan yang terlalu dini tidak
/// menyimpulkan apa pun.
const JS_KEADAAN: &str = r#"(() => ({
  ready: document.readyState,
  judul: document.title || '',
  otp: !!document.querySelector('input[autocomplete="one-time-code"], input[name*="otp" i], input[id*="otp" i]')
}))()"#;

/// Hasil satu percobaan buka-web: apa yang diamati, baris log, dan DOM mentah.
struct Percobaan {
    kenyataan: Kenyataan,
    log: Vec<String>,
    dom: String,
}

/// Buka satu URL lewat daemon dan kembalikan apa yang benar-benar diamati.
fn buka(cdp_port: u16, url: &str) -> Option<Percobaan> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .ok()?;

    runtime.block_on(async move {
        let alamat = format!("ws://127.0.0.1:{cdp_port}/devtools/browser/uji");
        let (ws, _) = tokio_tungstenite::connect_async(&alamat).await.ok()?;
        let (mut tulis, mut baca) = ws.split();
        let mut fakta = Fakta::default();
        let mut berikut = 0_i64;
        let mut id = || {
            berikut += 1;
            berikut
        };

        let dibuat = panggil(
            &mut tulis,
            &mut baca,
            id(),
            "Target.createTarget",
            json!({"url": "about:blank"}),
            None,
            &mut fakta,
        )
        .await?;
        let target_id = dibuat["result"]["targetId"].as_str()?.to_owned();

        let sesi = panggil(
            &mut tulis,
            &mut baca,
            id(),
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
            None,
            &mut fakta,
        )
        .await?;
        let session = sesi["result"]["sessionId"].as_str()?.to_owned();

        // Network dan Page harus aktif SEBELUM navigasi. Kalau tidak, pengamat
        // tidak melihat respons dokumen dan halaman tantangan akan terlihat
        // seperti halaman normal dengan status `None`.
        panggil(&mut tulis, &mut baca, id(), "Network.enable", json!({}), Some(&session), &mut fakta).await?;
        panggil(&mut tulis, &mut baca, id(), "Page.enable", json!({}), Some(&session), &mut fakta).await?;

        let mulai = Instant::now();
        panggil(
            &mut tulis,
            &mut baca,
            id(),
            "Page.navigate",
            json!({"url": url}),
            Some(&session),
            &mut fakta,
        )
        .await?;

        while mulai.elapsed().as_secs_f64() < BATAS_NAVIGASI_DETIK {
            let nilai = panggil(
                &mut tulis,
                &mut baca,
                id(),
                "Runtime.evaluate",
                json!({"expression": JS_KEADAAN, "returnByValue": true}),
                Some(&session),
                &mut fakta,
            )
            .await?;
            let hasil = &nilai["result"]["result"]["value"];
            if let Some(judul) = hasil["judul"].as_str()
                && !judul.is_empty()
            {
                fakta.judul = Some(judul.to_owned());
            }
            fakta.otp = hasil["otp"].as_bool().unwrap_or(false);
            if hasil["ready"].as_str() == Some("complete") {
                fakta.selesai = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        // Probe DOM dijalankan setelah dokumen selesai (atau batas habis):
        // probe di halaman yang belum diparse membaca DOM kosong dan
        // menyimpulkan "tidak ada tantangan" untuk halaman yang justru
        // tantangan.
        let probe = panggil(
            &mut tulis,
            &mut baca,
            id(),
            "Runtime.evaluate",
            json!({"expression": policy::challenge::JS_DETECT, "returnByValue": true}),
            Some(&session),
            &mut fakta,
        )
        .await?;
        let mentah = probe["result"]["result"]["value"].as_str().unwrap_or("");
        let sinyal_dom = policy::challenge::DomFinding::parse_json(mentah);
        fakta.log.push(format!("probe_dom={mentah}"));

        // DOM disimpan apa adanya ke artefak: saat ada bug klasifikasi, yang
        // pertama dibaca orang adalah halaman yang benar-benar dilihat mesin.
        let html = panggil(
            &mut tulis,
            &mut baca,
            id(),
            "Runtime.evaluate",
            json!({"expression": "document.documentElement ? document.documentElement.outerHTML : ''", "returnByValue": true}),
            Some(&session),
            &mut fakta,
        )
        .await?;
        let dom = html["result"]["result"]["value"]
            .as_str()
            .unwrap_or("")
            .to_owned();

        let detik = mulai.elapsed().as_secs_f64();
        let _ = panggil(
            &mut tulis,
            &mut baca,
            id(),
            "Target.closeTarget",
            json!({"targetId": target_id}),
            None,
            &mut fakta,
        )
        .await;

        Some(Percobaan {
            log: fakta.log.clone(),
            dom,
            kenyataan: Kenyataan {
            status_dokumen: fakta.status,
            url_akhir: fakta.url.clone(),
            judul: fakta.judul.clone(),
            galat_jaringan: fakta.galat.clone(),
            sinyal_dom,
            otp_terlihat: fakta.otp,
            dokumen_selesai: Some(fakta.selesai),
            detik: Some(detik),
            batas_detik: Some(BATAS_NAVIGASI_DETIK),
            mesin_hidup: Some(true),
            ..Kenyataan::default()
            },
        })
    })
}

/// Jalankan rantai eskalasi sampai habis, catat jejaknya sebagai baris log.
///
/// Yang diuji di sini bukan angka karangan: pemanggil memberi tahu apakah
/// langkahnya berhasil, persis seperti pemanggil sungguhan. Percobaan dihitung
/// dari langkah yang benar-benar mengirim permintaan baru.
fn eskalasi_tercatat(kategori: Kategori) -> (Vec<String>, u32) {
    let mut rantai = Eskalasi::mulai(
        kategori,
        browserd::eskalasi::MesinLain {
            ada: false,
            nama: "",
        },
    );
    while let Some(langkah) = rantai.langkah() {
        // Mesin tidak pernah berhasil di uji ini: yang dicari batasnya, bukan
        // hasilnya.
        tracing_langkah(&langkah, kategori);
        if langkah == browserd::eskalasi::Langkah::Handoff {
            break;
        }
    }
    let jejak = rantai.jejak();
    (jejak.ringkas(), jejak.percobaan)
}

fn tracing_langkah(langkah: &browserd::eskalasi::Langkah, kategori: Kategori) {
    eprintln!("eskalasi {}: {}", kategori, langkah.alasan(kategori));
}

#[test]
fn tiga_halaman_tiga_kategori_dan_artefak() {
    let Some(biner) = biner_mesin() else {
        eprintln!(
            "SKIP diagnostik: chrome-headless-shell tidak ditemukan. Set OCB_ENGINE_BINARY \
             atau pasang di ~/.cache/ms-playwright."
        );
        return;
    };

    let dir = direktori_uji();
    let cdp_port = port_kosong();
    let mati_port = port_kosong();
    let (fixture_port, _fixture) = server_fixture();

    // Port kontrol sengaja ephemeral: daemon uji tidak boleh berebut port
    // dengan daemon yang sedang melayani di host ini.
    let control_port = port_kosong();
    let kunci = dir.join("store.key");
    let db = dir.join("store.db");
    let config_path = dir.join("browser-daemon.toml");
    let profil = dir.join("profil");
    std::fs::write(
        &config_path,
        format!(
            "[daemon]\ncontrol_bind = \"127.0.0.1:{control_port}\"\n\
             store_db = \"{}\"\nstore_key = \"{}\"\n",
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
            biner.to_str().expect("path utf8"),
            "--user-data-dir",
            profil.to_str().expect("path utf8"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("menjalankan browserd serve");

    let siap = tunggu(BATAS, || minta(cdp_port, "/healthz").0 == 200);
    if !siap {
        buang(&mut daemon);
        panic!("daemon tidak siap dalam {BATAS:?}");
    }
    assert_eq!(
        minta(fixture_port, "/normal").0,
        200,
        "server fixture harus hidup sebelum halaman dibuka"
    );
    assert_eq!(
        minta(fixture_port, "/tantangan").0,
        403,
        "halaman tantangan harus benar-benar 403"
    );

    let penulis = Penulis::baru(dir.join("artefak"), Kuota::default())
        .expect("direktori artefak di luar working tree");

    // Halaman mati memakai port yang tidak ada pendengarnya: itu kegagalan
    // jaringan yang nyata, bukan yang dibuat-buat dengan mengosongkan URL.
    let kasus = [
        (
            format!("http://127.0.0.1:{fixture_port}/normal"),
            None,
            "normal",
        ),
        (
            format!("http://127.0.0.1:{fixture_port}/tantangan"),
            Some(Kategori::BlokAntiBot),
            "tantangan",
        ),
        (
            format!("http://127.0.0.1:{fixture_port}/otp"),
            Some(Kategori::ButuhManusia),
            "otp",
        ),
        (
            format!("http://127.0.0.1:{mati_port}/"),
            Some(Kategori::Jaringan),
            "port-mati",
        ),
    ];

    let mut kategori_dapat = Vec::new();
    let mut artefak = Vec::new();
    for (url, harap, nama) in &kasus {
        let percobaan = buka(cdp_port, url);
        let Some(percobaan) = percobaan else {
            buang(&mut daemon);
            panic!("navigasi ke {url} tidak menghasilkan pengamatan");
        };
        let hasil = browserd::klasifikasi::nilai(&percobaan.kenyataan);
        eprintln!("{nama}: {url} -> {hasil:?}");
        assert_eq!(
            hasil.kategori(),
            *harap,
            "{nama} ({url}) salah kategori; kenyataan: {:?}",
            percobaan.kenyataan
        );
        let Some(harap) = harap else {
            continue;
        };

        // Kategori yang ditemukan harus sama dengan kategori yang diminta
        // eskalasi: rantai yang dijalankan untuk kategori lain akan mencatat
        // langkah yang tidak relevan.
        let (baris_eskalasi, percobaan_eskalasi) = eskalasi_tercatat(*harap);
        assert!(
            percobaan_eskalasi <= 1 + browserd::eskalasi::MAKS_ULANG,
            "{nama}: {percobaan_eskalasi} percobaan, batas 1 + {}",
            browserd::eskalasi::MAKS_ULANG
        );

        let Hasil::Gagal(bukti) = &hasil else {
            unreachable!("kategori ada berarti gagal");
        };
        let mut isi = Isi::dari_bukti(bukti);
        isi.dom = percobaan.dom.clone();
        isi.log = percobaan
            .log
            .iter()
            .cloned()
            .chain(baris_eskalasi)
            .collect();
        let stempel = format!("{}", std::process::id());
        let path = penulis
            .tulis(&stempel, nama, &isi)
            .expect("menulis artefak percobaan");

        assert!(path.join("dom.html").is_file(), "{nama}: dom.html hilang");
        assert!(path.join("mesin.log").is_file(), "{nama}: mesin.log hilang");
        assert!(
            path.join("ringkasan.json").is_file(),
            "{nama}: ringkasan.json hilang"
        );
        let ringkasan = std::fs::read_to_string(path.join("ringkasan.json")).expect("baca");
        assert!(
            ringkasan.contains(harap.as_str()),
            "{nama}: ringkasan harus menyebut kategori {}, dapat: {ringkasan}",
            harap.as_str()
        );
        let log = std::fs::read_to_string(path.join("mesin.log")).expect("baca log");
        assert!(
            log.contains("berhenti:"),
            "{nama}: log harus memuat alasan berhenti eskalasi"
        );

        kategori_dapat.push(harap);
        artefak.push(path);
    }

    buang(&mut daemon);

    assert_eq!(
        kategori_dapat.len(),
        3,
        "tiga kategori berbeda harus terbukti pada mesin nyata"
    );
    assert_eq!(artefak.len(), 3, "satu direktori artefak per kegagalan");
    eprintln!("artefak: {}", penulis.dasar().display());
}
