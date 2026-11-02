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
        let sekarang = engine::slot::detik_sekarang();
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
        slot.buka_sesi(engine::slot::detik_sekarang());
        Self { slot }
    }

    #[must_use]
    pub fn slot(&self) -> &Arc<Slot> {
        &self.slot
    }
}

impl Drop for SesiSlot {
    fn drop(&mut self) {
        self.slot.tutup_sesi(engine::slot::detik_sekarang());
    }
}

/// Router lengkap: discovery HTTP plus endpoint websocket.
pub fn router(state: Arc<ProxyState>) -> Router {
    targets::discovery_router(Arc::clone(&state)).merge(passthrough::ws_router(state))
}
