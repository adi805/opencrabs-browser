//! Metrik `/metrics` saat daemon menjalankan lebih dari satu mesin.
//!
//! Yang dibuktikan: tiap mesin mendapat baris `browser_engine_state` sendiri
//! dengan label **id mesin**, dan penghitung milik daemon (`browser_reap_total`
//! dan kawan-kawan) tetap **satu** baris. Mencetak blok global per mesin akan
//! membuat dashboard menjumlahkan kejadian dua kali.
//!
//! Mesin di sini `FakeEngine`: yang diuji penyusunan teks metrik, bukan
//! pembacaan `/proc` (itu sudah diuji di unit test `metrics`).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Router;
use cdp::EnginePool;
use engine::Engine;
use lifecycle::Counters;
use policy::counters::CaptchaCounters;
use testing::FakeEngine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

struct Rig {
    port: u16,
    mesin: BTreeMap<&'static str, Arc<FakeEngine>>,
    _tugas: tokio::task::JoinHandle<()>,
}

async fn rig() -> Rig {
    let mut peta: BTreeMap<String, Arc<dyn Engine>> = BTreeMap::new();
    let mut pegang: BTreeMap<&'static str, Arc<FakeEngine>> = BTreeMap::new();
    for id in ["kiri", "kanan"] {
        let engine = Arc::new(FakeEngine::at_port(1));
        pegang.insert(id, Arc::clone(&engine));
        peta.insert(id.to_owned(), engine as Arc<dyn Engine>);
    }
    let pool = EnginePool::new("kiri", peta).expect("pool dua mesin");

    let app: Router = browserd::metrics::router(
        pool,
        Arc::new(Counters::new()),
        Arc::new(CaptchaCounters::new()),
        // Nol karangan: test ini tidak mengukur kegagalan buka-web, jadi
        // penghitungnya kosong dan blok `browser_open_failures_total` memang
        // tidak boleh muncul.
        Arc::new(browserd::metrics::Kegagalan::baru()),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("alamat").port();
    let tugas = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Rig {
        port,
        mesin: pegang,
        _tugas: tugas,
    }
}

async fn metrik(port: u16) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("menyambung");
    let permintaan =
        format!("GET /metrics HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(permintaan.as_bytes())
        .await
        .expect("mengirim");
    let mut buf = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        stream.read_to_end(&mut buf),
    )
    .await
    .expect("waktu habis")
    .expect("membaca");
    let teks = String::from_utf8_lossy(&buf).into_owned();
    teks.split_once("\r\n\r\n")
        .map(|(_, badan)| badan.to_owned())
        .expect("badan HTTP")
}

/// Jumlah **baris sampel** yang diawali nama metrik.
///
/// Tidak memakai `matches` biasa: baris `# TYPE browser_reap_total counter`
/// juga memuat nama itu, dan menghitungnya membuat satu sampel terlihat dua.
fn hitung(teks: &str, awal: &str) -> usize {
    teks.lines().filter(|baris| baris.starts_with(awal)).count()
}

#[tokio::test]
async fn tiap_mesin_punya_label_sendiri_dan_blok_global_cuma_sekali() {
    let rig = rig().await;
    let isi = metrik(rig.port).await;

    assert!(
        isi.contains("browser_engine_state{engine=\"kiri\",state=\"ready\"} 1"),
        "mesin kiri siap: {isi}"
    );
    assert!(
        isi.contains("browser_engine_state{engine=\"kanan\",state=\"ready\"} 1"),
        "mesin kanan siap: {isi}"
    );
    assert_eq!(
        hitung(&isi, "browser_reap_total "),
        1,
        "penghitung daemon tidak boleh digandakan per mesin: {isi}"
    );
    // Label `kind` tidak dipakai di mode banyak mesin: dua mesin boleh berjenis
    // sama, dan metrik yang mencampurnya tidak bisa dipakai memutuskan mesin
    // mana yang harus dimatikan.
    assert!(
        !isi.contains("engine=\"fake\""),
        "mode banyak mesin memakai id, bukan kind: {isi}"
    );
}

#[tokio::test]
async fn mesin_yang_mati_dilaporkan_stopped() {
    let rig = rig().await;
    rig.mesin["kanan"].stop().expect("mematikan kanan");

    let isi = metrik(rig.port).await;
    assert!(
        isi.contains("browser_engine_state{engine=\"kanan\",state=\"stopped\"} 1"),
        "mesin mati jujur dilaporkan stopped: {isi}"
    );
    assert!(
        isi.contains("browser_engine_state{engine=\"kiri\",state=\"ready\"} 1"),
        "mesin yang hidup tidak ikut dilaporkan mati: {isi}"
    );
}
