//! Uji mata dan tangan pada mesin sungguhan.
//!
//! Yang dibuktikan di sini bukan "fungsi mengembalikan string". Yang dibuktikan:
//! agent bisa **melihat** elemen yang disembunyikan shadow DOM tertutup dan
//! iframe, lalu **menyentuhnya** sampai keadaan halaman benar-benar berubah,
//! dan bahwa sentuhan yang tidak mengubah apa pun dilaporkan sebagai tidak
//! mengubah apa pun.
//!
//! Server fixture dijalankan di dalam proses test, bukan internet. Hasil yang
//! bergantung pada jaringan bukan bukti yang bisa diulang.
//!
//! Dilewati dengan pesan jelas kalau `chrome-headless-shell` tidak ada, supaya
//! `cargo test` tetap hijau di mesin tanpa browser.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::response::Html;
use axum::routing::get;
use browserd::penanam::Sesi;
use browserd::runtime;
use engine::{Engine, LaunchSpec, ShellEngine, discover_shell};
use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use serde_json::{Value, json};

/// Membunuh group proses mesin walau test gagal di tengah jalan.
///
/// Tanpa ini satu assertion merah meninggalkan browser hantu yang memakan RAM
/// sampai mesin di-reboot, dan run berikutnya gagal karena memorinya habis.
struct Reaper {
    pgid: Option<i32>,
}

impl Reaper {
    fn new() -> Self {
        Self { pgid: None }
    }

    fn arm(&mut self, pid: u32) {
        self.pgid = Some(pid as i32);
    }

    fn disarm(&mut self) {
        self.pgid = None;
    }

    /// Benar kalau group proses sudah tidak ada. Dipakai sebagai bukti, bukan
    /// sebagai klaim: `stop()` yang mengaku sukses tidak membuktikan apa pun.
    fn mati(&self) -> bool {
        match self.pgid {
            None => true,
            Some(pgid) => matches!(killpg(Pid::from_raw(pgid), None), Err(Errno::ESRCH)),
        }
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        if let Some(pgid) = self.pgid {
            let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
        }
    }
}

struct Profil(PathBuf);

impl Drop for Profil {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn profil_sementara(label: &str) -> Profil {
    let cap = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("ocb-mata-{label}-{}-{cap}", std::process::id()));
    std::fs::create_dir_all(&path).expect("profil sementara harus bisa dibuat");
    Profil(path)
}

/// Menyajikan fixture di port acak, lalu mengembalikan basis URL-nya.
///
/// Fixture dikirim sebagai `text/html`: kalau axum menjawab `text/plain`,
/// Chromium membungkus seluruh berkas dalam satu `<pre>`, dan yang diuji bukan
/// lagi halaman yang kita tulis.
async fn sajikan_fixture() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route(
            "/consent.html",
            get(|| async { Html(runtime::fixture::CONSENT) }),
        )
        .route(
            "/shadow.html",
            get(|| async { Html(runtime::fixture::SHADOW) }),
        )
        .route(
            "/iframe.html",
            get(|| async { Html(runtime::fixture::IFRAME) }),
        )
        .route(
            "/isi-iframe.html",
            get(|| async { Html(runtime::fixture::ISI_IFRAME) }),
        )
        .route(
            "/overlay.html",
            get(|| async { Html(runtime::fixture::OVERLAY) }),
        )
        .route("/diam.html", get(|| async { Html(runtime::fixture::DIAM) }))
        .route(
            "/padat.html",
            get(|| async { Html(runtime::fixture::PADAT) }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("port lokal harus bisa dipakai");
    let alamat = listener.local_addr().expect("alamat lokal");
    let tugas = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{alamat}"), tugas)
}

/// Membuka tab kosong, memasang runtime, lalu menavigasi. Urutannya wajib:
/// skrip hanya berlaku untuk dokumen yang lahir sesudah registrasi.
async fn tab_dengan_runtime(sesi: &mut Sesi, url: &str) -> String {
    let hasil = sesi
        .panggil("Target.createTarget", json!({ "url": "about:blank" }), None)
        .await
        .expect("tab kosong harus bisa dibuat");
    let target = hasil["targetId"]
        .as_str()
        .expect("mesin harus memberi targetId")
        .to_string();
    let sesi_id = sesi.tanam(&target).await.expect("suntikan harus berhasil");
    sesi.panggil("Page.navigate", json!({ "url": url }), Some(&sesi_id))
        .await
        .expect("navigasi harus diterima");
    // Menunggu dokumen selesai dimuat tanpa tidur panjang: tanya kesiapan
    // halaman sampai siap, dengan batas waktu yang jelas.
    let batas = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let nilai = evaluasi(
            sesi,
            &sesi_id,
            "document.readyState === 'complete' && document.body ? document.body.innerText.length : -1",
        )
        .await;
        if nilai.as_i64().unwrap_or(-1) >= 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < batas,
            "halaman tidak selesai dimuat dalam 10 detik"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    sesi_id
}

async fn evaluasi(sesi: &mut Sesi, sesi_id: &str, ekspresi: &str) -> Value {
    let hasil = sesi
        .panggil(
            "Runtime.evaluate",
            json!({ "expression": ekspresi, "returnByValue": true, "awaitPromise": false }),
            Some(sesi_id),
        )
        .await
        .expect("Runtime.evaluate harus dijawab");
    if let Some(pesan) = hasil["exceptionDetails"]["exception"]["description"].as_str() {
        panic!("skrip halaman melempar: {pesan}");
    }
    hasil["result"]["value"].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn mata_dan_tangan_bekerja_di_halaman_nyata() {
    let Some(binary) = discover_shell() else {
        eprintln!("dilewati: chrome-headless-shell tidak ditemukan di mesin ini");
        return;
    };
    let profil = profil_sementara("utama");
    let spec = LaunchSpec::new(binary, profil.0.clone());
    let engine = ShellEngine::launch(spec, 20_000).expect("mesin harus siap dalam 20 detik");
    let mut reaper = Reaper::new();
    reaper.arm(engine.process_id().expect("mesin punya pid"));
    let (basis, tugas_fixture) = sajikan_fixture().await;

    let mut sesi = Sesi::sambung(engine.web_socket_url())
        .await
        .expect("websocket mesin harus terbuka");

    // --- Mata: halaman consent, elemen custom tanpa role ---------------------
    let tab = tab_dengan_runtime(&mut sesi, &format!("{basis}/consent.html")).await;
    let snapshot = evaluasi(&mut sesi, &tab, "__oc.mata()").await;
    assert_eq!(
        snapshot["versi"], "1",
        "runtime harus versi yang diharapkan"
    );
    assert_eq!(snapshot["judul"], "Consent tiruan");
    let elemen = snapshot["elemen"].as_array().expect("elemen harus array");
    let kotak = elemen
        .iter()
        .filter(|satu| satu["tag"] == "kotak-centang")
        .count();
    assert_eq!(kotak, 14, "14 kotak custom harus terlihat oleh mata");
    // --- Tangan: 14 kotak dicentang lewat satu perintah ---------------------
    let hasil_klik = evaluasi(
        &mut sesi,
        &tab,
        "__oc.klik(__oc.mata().elemen.filter((e) => e.tag === 'kotak-centang').map((e) => e.i))",
    )
    .await;
    let daftar = hasil_klik
        .as_array()
        .expect("klik array harus mengembalikan array");
    assert_eq!(daftar.len(), 14, "14 hasil harus dilaporkan");
    let berubah = daftar
        .iter()
        .filter(|satu| satu["hasil"] == "berubah")
        .count();
    assert_eq!(berubah, 14, "semua kotak harus berubah: {daftar:?}");
    // Bukti dari keadaan halaman, bukan dari balasan fungsi.
    let status = evaluasi(
        &mut sesi,
        &tab,
        "document.getElementById('status').textContent",
    )
    .await;
    assert_eq!(status, "14 dari 14 tercentang");
    let siap = evaluasi(
        &mut sesi,
        &tab,
        "document.getElementById('lanjut').disabled",
    )
    .await;
    assert_eq!(
        siap, false,
        "tombol lanjut harus terbuka setelah semua dicentang"
    );

    // --- Mata: shadow DOM tertutup + terbuka -------------------------------
    let tab = tab_dengan_runtime(&mut sesi, &format!("{basis}/shadow.html")).await;
    let snapshot = evaluasi(&mut sesi, &tab, "__oc.mata()").await;
    let teks =
        serde_json::to_string(&snapshot["elemen"]).expect("snapshot harus bisa diserialisasi");
    assert!(
        teks.contains("Tombol terbuka"),
        "isi shadow terbuka harus terbaca"
    );
    assert!(
        teks.contains("Tombol tertutup"),
        "isi shadow tertutup harus terbaca"
    );
    let klik = evaluasi(&mut sesi, &tab, "__oc.klik({ nama: 'Tombol tertutup' })").await;
    assert_eq!(
        klik["hasil"], "berubah",
        "klik di shadow tertutup: {klik:?}"
    );
    let catatan = evaluasi(
        &mut sesi,
        &tab,
        "document.getElementById('catatan').textContent",
    )
    .await;
    assert_eq!(catatan, "tertutup diklik");

    // --- Mata dan tangan: iframe se-asal -----------------------------------
    let tab = tab_dengan_runtime(&mut sesi, &format!("{basis}/iframe.html")).await;
    let snapshot = evaluasi(&mut sesi, &tab, "__oc.mata()").await;
    let dalam_iframe = snapshot["elemen"]
        .as_array()
        .expect("elemen harus array")
        .iter()
        .filter(|satu| satu["asal"] == "iframe")
        .count();
    assert!(
        dalam_iframe >= 2,
        "elemen dalam iframe harus terlihat: {snapshot:?}"
    );
    let klik = evaluasi(&mut sesi, &tab, "__oc.klik({ nama: 'Simpan di iframe' })").await;
    assert_eq!(klik["hasil"], "berubah", "klik di iframe: {klik:?}");
    let isi = evaluasi(
        &mut sesi,
        &tab,
        "document.querySelector('iframe').contentDocument.getElementById('hasil').textContent",
    )
    .await;
    assert_eq!(isi, "tersimpan");

    // --- Ketik: nilai masuk ke input di dalam iframe -----------------------
    let ketik = evaluasi(
        &mut sesi,
        &tab,
        "__oc.ketik({ nama: 'Nama di dalam iframe' }, 'Adi')",
    )
    .await;
    assert_eq!(ketik["hasil"], "berubah", "ketik: {ketik:?}");
    let nilai = evaluasi(
        &mut sesi,
        &tab,
        "document.querySelector('iframe').contentDocument.getElementById('nama').value",
    )
    .await;
    assert_eq!(nilai, "Adi");

    // --- Tangan: elemen tertutup overlay dilaporkan gagal ------------------
    let tab = tab_dengan_runtime(&mut sesi, &format!("{basis}/overlay.html")).await;
    let klik = evaluasi(&mut sesi, &tab, "__oc.klik('#tombol')").await;
    assert_eq!(
        klik["hasil"], "gagal",
        "overlay harus menggagalkan klik: {klik:?}"
    );
    assert_eq!(klik["kategori"], "tertutup");
    assert!(
        klik["penghalang"].is_object(),
        "penghalang harus disebut: {klik:?}"
    );
    let catatan = evaluasi(
        &mut sesi,
        &tab,
        "document.getElementById('catatan').textContent",
    )
    .await;
    assert_eq!(
        catatan, "belum",
        "klik tertutup tidak boleh mengubah halaman"
    );

    // --- Tangan: klik tanpa efek dilaporkan apa adanya ---------------------
    let tab = tab_dengan_runtime(&mut sesi, &format!("{basis}/diam.html")).await;
    let klik = evaluasi(&mut sesi, &tab, "__oc.klik('#diam')").await;
    assert_eq!(
        klik["hasil"], "tidak_ada_perubahan",
        "klik tanpa efek: {klik:?}"
    );
    assert!(
        klik["perubahan"]
            .as_array()
            .map(Vec::is_empty)
            .unwrap_or(false),
        "daftar perubahan harus kosong: {klik:?}"
    );

    // --- Batas ukuran snapshot pada halaman padat --------------------------
    let tab = tab_dengan_runtime(&mut sesi, &format!("{basis}/padat.html")).await;
    let snapshot = evaluasi(&mut sesi, &tab, "__oc.mata({ maks: 80 })").await;
    let jumlah = snapshot["jumlah"]["elemen"].as_i64().unwrap_or(0);
    assert!(jumlah <= 80, "batas elemen harus dipatuhi, dapat {jumlah}");
    let node = snapshot["jumlah"]["node"].as_i64().unwrap_or(0);
    assert!(
        node > 900,
        "halaman padat harus punya >900 node, dapat {node}"
    );
    let ukuran = serde_json::to_string(&snapshot).expect("snapshot").len();
    assert!(
        ukuran < 60_000,
        "snapshot harus di bawah 60 KB, dapat {ukuran} byte"
    );

    let _ = sesi.panggil("Browser.close", json!({}), None).await;
    tugas_fixture.abort();
    engine.stop().expect("stop() harus berhasil");

    // Bukti, bukan klaim. Browser yang tinggal hidup sesudah stop() adalah
    // kebocoran yang sama yang akan terjadi di browserd: RAM habis pelan-pelan
    // dan tidak ada yang menyadarinya sampai mesin mulai membunuh proses lain.
    let tenggat = std::time::Instant::now() + Duration::from_secs(5);
    while !reaper.mati() {
        assert!(
            std::time::Instant::now() < tenggat,
            "group proses mesin masih hidup 5 detik sesudah stop()"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    reaper.disarm();
}

#[test]
fn runtime_tidak_membocorkan_api_berbahaya() {
    // Skrip ini berjalan di halaman orang. Membuka akses ke chrome.* atau
    // menyisakan eval bebas akan membuat siapa pun bisa memakainya.
    let skrip = runtime::SCRIPT;
    assert!(!skrip.contains("chrome.loadTimes"));
    assert!(!skrip.contains("document.write"));
}
