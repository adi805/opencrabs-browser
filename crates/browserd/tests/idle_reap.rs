//! Reap mesin yang menganggur, lalu bangunkan lagi saat ada yang memakainya.
//!
//! Rantai penuhnya yang dibuktikan di sini, bukan potongannya:
//!
//! 1. Mesin tanpa klien dimatikan setelah ambangnya, dan **tidak** sebelum itu.
//! 2. Mesin yang masih punya sesi terbuka tidak dimatikan, seberapa lama pun
//!    kliennya diam.
//! 3. Permintaan nyata sesudah reap (`/json/list`) menyalakan mesin itu lagi
//!    **tepat satu kali**, di port baru, tanpa me-restart daemon. `/json/version`
//!    yang cuma menjawab string tidak ikut menyalakannya.
//! 4. Mesin lain tidak ikut mati hanya karena tetangganya menganggur.
//! 5. Handoff yang belum dijawab menahan reap: tab yang ditahan untuk captcha
//!    bukan tab yang boleh ikut hilang bersama mesinnya.
//!
//! Mesinnya palsu, tapi **sungguhan sebagai server HTTP**: `/json/list` dijawab
//! axum di port berbeda untuk tiap boot, jadi yang diuji benar-benar jalur
//! "bangun lalu teruskan", bukan tiruan yang menjawab nilai yang diharapkan.
//!
//! Group prosesnya kosong secara konstruksi (pid di atas `pid_max` kernel),
//! karena `PgidGroup` memindai `/proc` atas pgid itu: pid karangan yang kebetulan
//! bertabrakan dengan proses host akan mengirim sinyal ke proses yang salah.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use browserd::daemon::{Penahan, putaran_reap};
use cdp::{EnginePool, ProxyState};
use engine::slot::Penggerak;
use engine::{Engine, EngineError, Keadaan, LaunchSpec, Slot};
use lifecycle::Counters;
use lifecycle::reaper::ReaperConfig;
use testing::FakeEngine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Pid mesin palsu, di atas `pid_max` kernel (4 194 304 pada 64-bit): group-nya
/// dijamin kosong, jadi `PgidGroup::alive` tidak menemukan apa pun dan reap
/// melaporkan `AlreadyGone` tanpa mengirim satu sinyal pun.
const PID_AMAN: u32 = 0x7fff_fff0;

/// Waktu dasar test, sengaja kecil supaya jelas semua jam di sini karangan.
/// `Slot` menerima waktu sebagai parameter, dan itu yang membuat test ini tidak
/// perlu tidur sedetik pun.
const T0: u64 = 1_000_000;

/// Ambang reap yang dipakai seluruh test.
const AMBANG: u64 = 60;

/// Penggerak palsu: mencatat berapa kali mesin dinyalakan, dan mengembalikan
/// mesin palsu di port yang sudah disiapkan berurutan.
struct PenggerakUji {
    ports: Mutex<VecDeque<u16>>,
    boots: AtomicU32,
}

impl PenggerakUji {
    fn baru(ports: impl IntoIterator<Item = u16>) -> Arc<Self> {
        Arc::new(Self {
            ports: Mutex::new(ports.into_iter().collect()),
            boots: AtomicU32::new(0),
        })
    }

    fn boots(&self) -> u32 {
        self.boots.load(Ordering::SeqCst)
    }
}

impl Penggerak for PenggerakUji {
    fn nyalakan(
        &self,
        _spec: &LaunchSpec,
        _timeout_ms: u64,
    ) -> Result<Arc<dyn Engine>, EngineError> {
        self.boots.fetch_add(1, Ordering::SeqCst);
        let port = self
            .ports
            .lock()
            .expect("kunci port")
            .pop_front()
            .expect("test menyiapkan satu mesin per boot yang diharapkan");
        Ok(Arc::new(FakeEngine::at_port(port).with_pid(PID_AMAN)))
    }
}

fn spec() -> LaunchSpec {
    LaunchSpec::new("/tidak/ada/shell", "/tmp/ocb-idle-reap")
}

fn cfg() -> ReaperConfig {
    ReaperConfig {
        idle_reap: Duration::from_secs(AMBANG),
        ..ReaperConfig::default()
    }
}

/// Mesin palsu yang menjawab `/json/list` lewat HTTP sungguhan.
///
/// Port ephemeral-nya dikembalikan ke pemanggil: itulah yang membedakan boot
/// pertama dari boot kedua, dan itu juga yang membuat "mesin dibangunkan ulang"
/// bisa dibuktikan, bukan cuma diklaim.
async fn mesin_http(tab: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mesin");
    let port = listener.local_addr().expect("alamat mesin").port();
    let app = Router::new().route(
        "/json/list",
        get(move || {
            let url = format!("ws://127.0.0.1:{port}/devtools/page/{tab}");
            async move {
                axum::Json(serde_json::json!([{
                    "id": tab,
                    "type": "page",
                    "title": "tab palsu",
                    "url": "about:blank",
                    "webSocketDebuggerUrl": url,
                }]))
            }
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    port
}

/// Slot yang mesinnya sudah hidup sejak daemon boot, plus penggerak untuk boot
/// kebangkitan berikutnya.
///
/// Boot pertama lewat [`Slot::siap`] karena di daemon sungguhan mesin diluncurkan
/// saat startup, bukan oleh reap. Portnya juga tidak lewat penggerak, sehingga
/// hitungan `boots` berarti jumlah kebangkitan, bukan jumlah mesin yang ada.
fn slot_hidup(
    id: &str,
    penggerak: Arc<PenggerakUji>,
    port: u16,
    sekarang: u64,
) -> (Arc<Slot>, Arc<FakeEngine>) {
    let mesin = Arc::new(FakeEngine::at_port(port).with_pid(PID_AMAN));
    let slot = Arc::new(Slot::siap(
        id,
        "fake",
        spec(),
        penggerak,
        Arc::clone(&mesin) as Arc<dyn Engine>,
        sekarang,
    ));
    (slot, mesin)
}

fn pool_satu(id: &str, slot: &Arc<Slot>) -> EnginePool {
    let mut peta: BTreeMap<String, Arc<Slot>> = BTreeMap::new();
    peta.insert(id.to_owned(), Arc::clone(slot));
    EnginePool::dari_slot(id, peta).expect("pool satu mesin")
}

fn pool_dua(kiri: &Arc<Slot>, kanan: &Arc<Slot>, default: &str) -> EnginePool {
    let mut peta: BTreeMap<String, Arc<Slot>> = BTreeMap::new();
    peta.insert(kiri.id().to_owned(), Arc::clone(kiri));
    peta.insert(kanan.id().to_owned(), Arc::clone(kanan));
    EnginePool::dari_slot(default, peta).expect("pool dua mesin")
}

/// Proxy CDP di atas satu pool, siap menerima permintaan HTTP.
async fn proxy(pool: EnginePool) -> u16 {
    let state = Arc::new(ProxyState::pool(
        pool,
        "ws://127.0.0.1:9224/devtools/browser/uji",
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
    let port = listener.local_addr().expect("alamat proxy").port();
    let app: Router = cdp::router(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    port
}

async fn http_get(port: u16, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("sambung ke proxy");
    let permintaan =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(permintaan.as_bytes())
        .await
        .expect("kirim permintaan");

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.expect("baca respons");
    let teks = String::from_utf8_lossy(&buf).into_owned();
    let status = teks
        .split_whitespace()
        .nth(1)
        .and_then(|kode| kode.parse().ok())
        .unwrap_or(0);
    let badan = teks
        .split_once("\r\n\r\n")
        .map_or(String::new(), |(_, rest)| rest.to_owned());
    (status, badan)
}

#[tokio::test]
async fn mesin_menganggur_dimatikan_setelah_ambang_bukan_sebelumnya() {
    let port = mesin_http("tab-lama").await;
    let penggerak = PenggerakUji::baru([]);
    let (slot, mesin) = slot_hidup("kiri", Arc::clone(&penggerak), port, T0);
    let pool = pool_satu("kiri", &slot);
    let counters = Counters::new();

    assert_eq!(slot.keadaan(), Keadaan::Hidup);

    // Satu detik sebelum ambang: belum boleh disentuh.
    let dimatikan =
        putaran_reap(&pool, &counters, Penahan::kosong(), &cfg(), T0 + AMBANG - 1).await;
    assert!(
        dimatikan.is_empty(),
        "mesin dimatikan sebelum ambangnya habis: {dimatikan:?}"
    );
    assert_eq!(slot.keadaan(), Keadaan::Hidup);
    assert_eq!(mesin.stop_calls(), 0, "stop dipanggil sebelum ambang habis");

    // Tepat di ambang: baru boleh.
    let dimatikan = putaran_reap(&pool, &counters, Penahan::kosong(), &cfg(), T0 + AMBANG).await;
    assert_eq!(dimatikan, vec!["kiri".to_owned()]);
    assert_eq!(slot.keadaan(), Keadaan::Tidur);
    assert_eq!(mesin.stop_calls(), 1, "stop harus dipanggil sekali");

    // Putaran berikutnya tidak menemukan apa pun: mesin tidur bukan bahan reap.
    let lagi = putaran_reap(
        &pool,
        &counters,
        Penahan::kosong(),
        &cfg(),
        T0 + AMBANG * 10,
    )
    .await;
    assert!(lagi.is_empty(), "mesin tidur di-reap dua kali: {lagi:?}");
    assert_eq!(mesin.stop_calls(), 1);
}

#[tokio::test]
async fn mesin_dengan_sesi_terbuka_tidak_dimatikan() {
    let port = mesin_http("tab-aktif").await;
    let penggerak = PenggerakUji::baru([]);
    let (slot, mesin) = slot_hidup("kiri", penggerak, port, T0);
    let pool = pool_satu("kiri", &slot);
    let counters = Counters::new();

    slot.buka_sesi(T0);

    // Sejam menganggur pun tidak cukup selama sesinya masih terbuka.
    let dimatikan = putaran_reap(&pool, &counters, Penahan::kosong(), &cfg(), T0 + 3600).await;
    assert!(
        dimatikan.is_empty(),
        "mesin dengan sesi terbuka ikut dimatikan: {dimatikan:?}"
    );
    assert_eq!(slot.keadaan(), Keadaan::Hidup);
    assert_eq!(mesin.stop_calls(), 0);

    // Setelah sesi ditutup, jam diamnya mulai dari nol lagi, bukan dari T0:
    // klien yang baru saja pergi tidak boleh langsung kehilangan mesinnya.
    slot.tutup_sesi(T0 + 3600);
    let belum = putaran_reap(&pool, &counters, Penahan::kosong(), &cfg(), T0 + 3600).await;
    assert!(belum.is_empty(), "reap terlalu cepat setelah sesi ditutup");

    let dimatikan = putaran_reap(
        &pool,
        &counters,
        Penahan::kosong(),
        &cfg(),
        T0 + 3600 + AMBANG,
    )
    .await;
    assert_eq!(dimatikan, vec!["kiri".to_owned()]);
    assert_eq!(mesin.stop_calls(), 1);
}

#[tokio::test]
async fn handoff_yang_belum_dijawab_menahan_reap() {
    let port = mesin_http("tab-captcha").await;
    let penggerak = PenggerakUji::baru([]);
    let (slot, mesin) = slot_hidup("kiri", penggerak, port, T0);
    let pool = pool_satu("kiri", &slot);
    let counters = Counters::new();

    let dimatikan = putaran_reap(&pool, &counters, Penahan::handoff(1), &cfg(), T0 + 3600).await;
    assert!(
        dimatikan.is_empty(),
        "mesin dimatikan padahal ada handoff menunggu: {dimatikan:?}"
    );
    assert_eq!(mesin.stop_calls(), 0);

    // Handoff-nya selesai: mesin kembali layak di-reap.
    let dimatikan = putaran_reap(&pool, &counters, Penahan::kosong(), &cfg(), T0 + 3600).await;
    assert_eq!(dimatikan, vec!["kiri".to_owned()]);
}

#[tokio::test]
async fn mesin_kedua_tidak_ikut_mati() {
    let port_kiri = mesin_http("tab-kiri").await;
    let port_kanan = mesin_http("tab-kanan").await;
    let penggerak = PenggerakUji::baru([]);
    let (kiri, mesin_kiri) = slot_hidup("kiri", Arc::clone(&penggerak), port_kiri, T0);
    let (kanan, mesin_kanan) = slot_hidup("kanan", penggerak, port_kanan, T0);
    let pool = pool_dua(&kiri, &kanan, "kiri");
    let counters = Counters::new();

    kanan.buka_sesi(T0);

    let dimatikan = putaran_reap(&pool, &counters, Penahan::kosong(), &cfg(), T0 + AMBANG).await;
    assert_eq!(dimatikan, vec!["kiri".to_owned()]);
    assert_eq!(kiri.keadaan(), Keadaan::Tidur);
    assert_eq!(kanan.keadaan(), Keadaan::Hidup);
    assert_eq!(mesin_kiri.stop_calls(), 1);
    assert_eq!(mesin_kanan.stop_calls(), 0);
}

#[tokio::test]
async fn json_list_membangunkan_mesin_yang_sudah_di_reap() {
    let port_lama = mesin_http("tab-lama").await;
    let port_baru = mesin_http("tab-baru").await;
    let penggerak = PenggerakUji::baru([port_baru]);
    let (slot, mesin_lama) = slot_hidup("kiri", Arc::clone(&penggerak), port_lama, T0);
    let pool = pool_satu("kiri", &slot);
    let counters = Counters::new();

    // Reap dulu, baru minta tab: itu urutan yang terjadi di produksi.
    let dimatikan = putaran_reap(&pool, &counters, Penahan::kosong(), &cfg(), T0 + AMBANG).await;
    assert_eq!(dimatikan, vec!["kiri".to_owned()]);
    assert_eq!(slot.keadaan(), Keadaan::Tidur);
    assert_eq!(penggerak.boots(), 0, "reap menyalakan mesin baru");

    let proxy_port = proxy(pool_satu("kiri", &slot)).await;

    // `/json/version` tidak boleh membangunkan apa pun: versi tidak berubah
    // antar boot, dan menjawab string tidak sebanding dengan biaya satu Chromium.
    let (status, body) = http_get(proxy_port, "/json/version").await;
    assert_eq!(status, 200, "version gagal: {body}");
    assert_eq!(penggerak.boots(), 0, "version membangunkan mesin");
    assert_eq!(slot.keadaan(), Keadaan::Tidur);

    // `/json/list` butuh tab yang nyata, jadi mesinnya harus bangun.
    let (status, body) = http_get(proxy_port, "/json/list").await;
    assert_eq!(status, 200, "list gagal: {body}");
    assert!(
        body.contains("tab-baru"),
        "tab tidak datang dari mesin yang baru dibangunkan: {body}"
    );
    assert!(
        !body.contains("tab-lama"),
        "tab lama masih dilayani mesin yang sudah di-reap: {body}"
    );
    assert_eq!(penggerak.boots(), 1, "mesin tidak dibangunkan tepat sekali");
    assert_eq!(slot.sesi(), 0, "sesi bocor setelah satu permintaan");
    assert_eq!(mesin_lama.stop_calls(), 1);

    // Permintaan kedua dilayani mesin yang sama: yang diuji kebangkitan, bukan
    // kebangkitan berulang tiap permintaan.
    let (status, _) = http_get(proxy_port, "/json/list").await;
    assert_eq!(status, 200);
    assert_eq!(penggerak.boots(), 1, "mesin dibangunkan lagi tanpa perlu");
}

/// AC-009 di tingkat pemutus reap: lease yang masih hidup menahan mesin, dan
/// begitu lease-nya kedaluwarsa mesin kembali layak di-reap pada ambang yang
/// sama. Keduanya tanpa tidur: waktunya disuntik.
#[tokio::test]
async fn lease_hidup_menahan_reap_dan_kedaluwarsa_tidak() {
    let port = mesin_http("tab-lease").await;
    let penggerak = PenggerakUji::baru([]);
    let (slot, mesin) = slot_hidup("kiri", penggerak, port, T0);
    let pool = pool_satu("kiri", &slot);
    let counters = Counters::new();
    let mut gudang = lifecycle::Gudang::baru(1);
    gudang
        .pegang("sesi-a", "tab-lease", 3600, T0)
        .expect("pegang");

    let penahan = Penahan::dari_gudang(0, &gudang, T0 + 3600);
    assert_eq!(penahan.lease, 1);
    let dimatikan = putaran_reap(&pool, &counters, penahan, &cfg(), T0 + 3600).await;
    assert!(
        dimatikan.is_empty(),
        "mesin di-reap padahal lease masih hidup: {dimatikan:?}"
    );
    assert_eq!(mesin.stop_calls(), 0);

    // Satu detik sesudah kedaluwarsa: lease berhenti menahan, mesin ikut
    // aturan idle biasa.
    let sekarang = T0 + 3600 + AMBANG;
    let penahan = Penahan::dari_gudang(0, &gudang, sekarang);
    assert_eq!(penahan.lease, 0, "lease kedaluwarsa masih dihitung");
    let dimatikan = putaran_reap(&pool, &counters, penahan, &cfg(), sekarang).await;
    assert_eq!(dimatikan, vec!["kiri".to_owned()]);
    assert_eq!(mesin.stop_calls(), 1);
}
