//! Berbicara protokol. Meneruskan, menerjemahkan, dan tidak memutuskan apa pun.
//!
//! Aturan crate ini:
//!
//! - **Pesan klien tidak diubah isinya.** Id, `sessionId`, urutan, dan byte-nya
//!   diteruskan apa adanya selama hanya ada satu mesin.
//! - **Dengan lebih dari satu mesin, identitas diterjemahkan, bukan isinya.**
//!   `id`, `sessionId`, dan `targetId` dipetakan ke ruang id milik klien karena
//!   dua mesin bisa memakai nilai yang sama untuk hal yang berbeda. Field lain
//!   tidak disentuh. Lihat [`mux`].
//! - **Crate ini tidak tahu mesin mana yang dipakai.** Dia hanya pegang trait
//!   [`engine::Engine`] di dalam [`EnginePool`], jadi mesin palsu bisa dipakai
//!   di test.
//! - **Keputusan routing bukan di sini.** Pemilihan mesin milik `policy`;
//!   [`mux`] hanya menjalankan peta yang sudah ada.

#![forbid(unsafe_code)]

pub mod batas;
pub mod engines;
pub mod errors;
pub mod framing;
pub mod ids;
pub mod mux;
pub mod native_wire;
pub mod passthrough;
pub mod targets;

pub use engines::{EnginePool, PoolError};
pub use errors::CdpError;

use std::sync::Arc;

use axum::Router;
use engine::{Engine, Slot};

/// Pembaca pesan yang lewat, tanpa hak mengubahnya.
///
/// Dipanggil sinkron di dalam jalur penerusan, jadi implementasinya wajib
/// cepat: parsing ringan dan `try_send` ke antrean. Pekerjaan yang butuh
/// perjalanan bolak-balik ke mesin (mis. probe DOM) dikerjakan di tempat lain,
/// bukan di sini, karena menahannya di jalur ini berarti memperlambat setiap
/// pesan klien.
///
/// `observe` tidak mengembalikan apa pun dan tidak menerima pesan yang bisa
/// diubah: satu-satunya cara observer merusak penerusan adalah panik, dan itu
/// ditangkap pemanggil.
pub trait FrameObserver: Send + Sync {
    fn observe(&self, payload: &str);
}

/// Satu keputusan routing per host dari lapisan `policy`.
///
/// Yang dibawa cuma nama jenis mesin (`chrome-shell`, `chrome-full`, `oxi`) dan
/// nama aturan yang menang: crate ini tidak tahu isi `[policy]`, dan tidak perlu
/// tahu. Aturan yang menang ikut dikembalikan supaya pertanyaan "kenapa mesin
/// ini" bisa dijawab di log tanpa membaca kode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingHost {
    pub jenis: String,
    pub aturan: String,
}

/// Pemilihan mesin per host, dan pemiliknya `policy`.
///
/// Trait ini ada supaya crate protokol tetap tidak menyimpan satu aturan pun:
/// dia hanya bertanya "host ini jenis mesin apa". `Ok(None)` berarti tidak ada
/// aturan yang berlaku, dan pemanggil memakai mesin bawaannya.
///
/// `Err` berarti aturan **menolak** permintaan itu (mis. host stealth sementara
/// `chrome-full` tidak terpasang). Penolakan wajib sampai ke klien apa adanya;
/// menggantinya dengan mesin bawaan adalah fallback senyap, dan itu dilarang di
/// seluruh repo ini.
pub trait PemilihMesin: Send + Sync {
    fn jenis_untuk_host(&self, host: &str) -> Result<Option<RoutingHost>, String>;
}

/// Yang dibutuhkan lapisan protokol untuk melayani klien.
///
/// `public_ws_url` adalah URL yang **kita** iklankan ke klien. URL milik mesin
/// sengaja tidak pernah dibocorkan: kalau klien bisa menyambung langsung ke
/// mesin, proxy ini kehilangan seluruh gunanya.
#[derive(Clone)]
pub struct ProxyState {
    pub engines: EnginePool,
    pub public_ws_url: String,
    /// `None` berarti tidak ada yang mengamati: perilaku persis sama dengan
    /// sebelum fitur ini ada.
    pub observer: Option<Arc<dyn FrameObserver>>,
    /// Aturan host ke mesin dari `policy`. `None` berarti tidak ada routing
    /// yang tersambung: perilaku persis sama dengan sebelum fitur ini ada.
    pub pemilih: Option<Arc<dyn PemilihMesin>>,
}

impl ProxyState {
    /// Satu mesin, perilaku sama seperti sebelum multiplex ada.
    pub fn new(engine: Arc<dyn Engine>, public_ws_url: impl Into<String>) -> Self {
        Self::pool(EnginePool::single(engine), public_ws_url)
    }

    pub fn pool(pool: EnginePool, public_ws_url: impl Into<String>) -> Self {
        Self {
            engines: pool,
            public_ws_url: public_ws_url.into(),
            observer: None,
            pemilih: None,
        }
    }

    /// Slot mesin bawaan. Endpoint discovery yang tidak menyebut mesin memakai ini.
    #[must_use]
    pub fn slot(&self) -> &Arc<Slot> {
        self.engines.default_slot()
    }

    /// Mesin yang **hidup**, dinyalakan dulu kalau sedang tidur.
    ///
    /// Semua jalur yang benar-benar akan berbicara ke mesin lewat sini, bukan
    /// lewat [`Self::slot`]: mesin yang di-reap karena menganggur harus bangun
    /// sendiri saat ada klien, tanpa daemon perlu di-restart. Boot berjalan di
    /// `spawn_blocking` karena menunggu Chromium siap berarti mem-poll port
    /// selama belasan detik.
    ///
    /// `mesin = None` berarti mesin bawaan.
    pub async fn bangunkan(
        &self,
        mesin: Option<&str>,
        timeout_ms: u64,
    ) -> Result<Bangun, CdpError> {
        let (id, slot) = match mesin {
            None => (
                self.engines.default_id().to_owned(),
                Arc::clone(self.slot()),
            ),
            Some(id) => {
                let slot = self.engines.get(id).ok_or_else(|| CdpError::MesinTakAda {
                    diminta: id.to_owned(),
                    tersedia: self.engines.ids().cloned().collect::<Vec<_>>().join(", "),
                })?;
                (id.to_owned(), Arc::clone(slot))
            }
        };
        let slot_id = id.clone();
        let sekarang = engine::slot::ms_sekarang();
        let untuk_boot = Arc::clone(&slot);
        let hasil = tokio::task::spawn_blocking(move || untuk_boot.pastikan(timeout_ms, sekarang))
            .await
            .map_err(|err| {
                CdpError::Upstream(format!("tugas boot mesin '{slot_id}' gagal: {err}"))
            })?;
        let engine = hasil.map_err(|err| CdpError::Upstream(err.to_string()))?;
        Ok(Bangun { id, slot, engine })
    }

    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn FrameObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Aturan host ke mesin. Tanpa ini semua permintaan mendarat di mesin
    /// bawaan, persis seperti sebelum `policy` tersambung ke jalur nyata.
    #[must_use]
    pub fn with_pemilih(mut self, pemilih: Arc<dyn PemilihMesin>) -> Self {
        self.pemilih = Some(pemilih);
        self
    }
}

/// Mesin yang sudah dipastikan hidup, plus slot pemiliknya.
///
/// Slotnya ikut dikembalikan karena pemanggil yang menyambung ke mesin harus
/// menandai sesi terbuka di slot itu: hitungan sesi itulah yang menahan reap
/// selama klien masih tersambung, seberapa lama pun dia diam.
pub struct Bangun {
    pub id: String,
    pub slot: Arc<Slot>,
    pub engine: Arc<dyn Engine>,
}

/// Penanda sesi yang menutup sendiri saat dijatuhkan.
///
/// Dijatuhkan otomatis, termasuk saat tugas penerusan berakhir karena galat
/// atau klien memutus sepihak. Hitungan yang hanya ditutup di jalur sukses
/// akan bocor satu per klien yang kabur, dan mesin itu berhenti di-reap
/// selamanya.
pub struct SesiSlot {
    slot: Arc<Slot>,
}

impl SesiSlot {
    #[must_use]
    pub fn buka(slot: Arc<Slot>) -> Self {
        slot.buka_sesi(engine::slot::ms_sekarang());
        Self { slot }
    }

    #[must_use]
    pub fn slot(&self) -> &Arc<Slot> {
        &self.slot
    }
}

impl Drop for SesiSlot {
    fn drop(&mut self) {
        self.slot.tutup_sesi(engine::slot::ms_sekarang());
    }
}

/// Router lengkap: discovery HTTP plus endpoint websocket.
pub fn router(state: Arc<ProxyState>) -> Router {
    targets::discovery_router(Arc::clone(&state)).merge(passthrough::ws_router(state))
}

/// Host dari sebuah URL, untuk dicocokkan dengan `[policy.rules]`.
///
/// Sengaja tanpa parser URL: yang dibutuhkan cuma otoritas (`skema://` sampai
/// `/`, `?`, atau `#`), dan aturan host selalu ditulis tanpa port maupun
/// userinfo. Hasilnya huruf kecil tanpa titik di ujung, jadi `HTTP://Toko.Test./`
/// menghadapi aturan yang sama dengan `http://toko.test/x`.
///
/// `None` untuk URL yang memang tidak punya host (`about:blank`, `data:`,
/// `file:`). Itu berarti tidak ada yang dicocokkan, bukan host kosong yang bisa
/// tidak sengaja cocok dengan aturan.
#[must_use]
pub fn host_dari_url(url: &str) -> Option<String> {
    let sesudah_skema = url.split_once("://")?.1;
    let otoritas = sesudah_skema
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let tanpa_userinfo = otoritas.rsplit_once('@').map_or(otoritas, |(_, host)| host);
    // IPv6 ditulis dalam kurung siku (`[::1]:9224`); memotong di titik dua
    // pertama akan mengubahnya jadi `[`.
    let host = match tanpa_userinfo.strip_prefix('[') {
        Some(sisa) => sisa.split_once(']').map_or(sisa, |(host, _)| host),
        None => tanpa_userinfo
            .split_once(':')
            .map_or(tanpa_userinfo, |(host, _)| host),
    };
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod uji_host_dari_url {
    use super::host_dari_url;

    #[test]
    fn mengambil_host_tanpa_skema_port_dan_userinfo() {
        assert_eq!(
            host_dari_url("http://toko.test/halaman?x=1"),
            Some("toko.test".to_owned())
        );
        assert_eq!(
            host_dari_url("https://Toko.Test:8443/"),
            Some("toko.test".to_owned())
        );
        assert_eq!(
            host_dari_url("http://user:rahasia@toko.test/x"),
            Some("toko.test".to_owned())
        );
        assert_eq!(
            host_dari_url("HTTP://toko.test./"),
            Some("toko.test".to_owned())
        );
        assert_eq!(
            host_dari_url("wss://toko.test"),
            Some("toko.test".to_owned())
        );
        assert_eq!(
            host_dari_url("http://[::1]:9224/json"),
            Some("::1".to_owned())
        );
    }

    #[test]
    fn url_tanpa_host_bukan_host_kosong() {
        assert_eq!(host_dari_url("about:blank"), None);
        assert_eq!(host_dari_url("data:text/html,<p>x</p>"), None);
        assert_eq!(host_dari_url("file:///tmp/x.html"), None);
        assert_eq!(host_dari_url(""), None);
        assert_eq!(host_dari_url("http://"), None);
    }
}
