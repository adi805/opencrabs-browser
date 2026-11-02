//! Satu mesin yang bisa tidur dan bangun lagi.
//!
//! Sebelum modul ini, daemon memegang `Arc<dyn Engine>` sejak boot sampai mati.
//! Itu membuat reap idle mustahil: mesin yang sudah dimatikan tidak punya jalan
//! kembali, jadi satu-satunya cara memakainya lagi adalah me-restart daemon.
//! `lifecycle` sudah menyimpan fungsi keputusannya (`idle_reap_due`), dan
//! `04-OPERASIONAL` §3.3 menyebut `Stopped -> Booting` sebagai transisi sah
//! justru karena mesin lahir dan mati berkali-kali selama daemon hidup.
//!
//! Yang disimpan di sini adalah **resep**, bukan hanya proses: [`LaunchSpec`]
//! tetap ada setelah mesinnya mati, sehingga mesin yang sama bisa dinyalakan
//! ulang dengan port, profil, dan argumen yang persis sama. Alamat port boleh
//! berubah antar boot (port 0 = mesin memilih sendiri), dan itu sebabnya
//! pemanggil harus selalu memakai [`Slot::pastikan`] untuk mendapatkan alamat
//! yang **sekarang**, bukan alamat yang diingat dari boot pertama.
//!
//! Kenapa [`Penggerak`] dipisah dari [`Slot`]: test butuh mesin yang lahir
//! tanpa proses. Tanpa titik suntik ini, menguji "reap lalu bangun lagi" berarti
//! menyalakan Chromium sungguhan, dan test seperti itu tidak akan pernah jalan
//! di CI.

#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{Engine, EngineError, EngineInfo, LaunchSpec, ShellEngine};

/// Kegagalan yang datang dari satu slot, bukan dari protokol.
#[derive(Debug, thiserror::Error)]
pub enum SlotError {
    #[error("mesin '{id}' gagal dinyalakan: {sebab}")]
    GagalNyalakan { id: String, sebab: String },

    #[error("mesin '{id}' tidak mau berhenti: {sebab}")]
    GagalBerhenti { id: String, sebab: String },
}

/// Cara menyalakan satu mesin dari resepnya.
///
/// Trait ini sengaja sinkron: `ShellEngine::launch` mem-poll port dan berkas
/// `DevToolsActivePort`, jadi pemanggil yang async wajib membungkusnya dengan
/// `spawn_blocking`. Membuatnya async di sini hanya menyembunyikan fakta itu.
pub trait Penggerak: Send + Sync {
    fn nyalakan(&self, spec: &LaunchSpec, timeout_ms: u64) -> Result<Arc<dyn Engine>, EngineError>;
}

/// Penggerak sungguhan: `chrome-headless-shell` lewat [`ShellEngine`].
#[derive(Debug, Clone, Copy, Default)]
pub struct PenggerakShell;

/// Detik epoch sekarang.
///
/// Satu-satunya tempat jam sistem dibaca. Logika [`Slot`] menerima waktu sebagai
/// parameter supaya uji bisa memajukan waktu tanpa tidur, dan supaya
/// keputusan "sudah menganggur berapa lama" bisa diuji tanpa menunggu.
#[must_use]
pub fn detik_sekarang() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Penggerak for PenggerakShell {
    fn nyalakan(&self, spec: &LaunchSpec, timeout_ms: u64) -> Result<Arc<dyn Engine>, EngineError> {
        ShellEngine::launch(spec.clone(), timeout_ms)
            .map(|engine| Arc::new(engine) as Arc<dyn Engine>)
    }
}

/// Keadaan satu slot pada satu saat.
///
/// Dibedakan dari "hidup atau tidak" karena tiga keadaan tidak-hidup punya arti
/// berbeda bagi pemantau: mesin yang **sengaja** ditidurkan itu sehat, mesin
/// yang mati sendiri itu kegagalan, dan mesin yang belum pernah dipakai belum
/// bisa dinilai.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keadaan {
    /// Proses ada dan menjawab.
    Hidup,
    /// Prosesnya sudah kita matikan. Resepnya masih ada, permintaan berikutnya
    /// menyalakannya lagi.
    Tidur,
    /// Proses hilang di luar kendali kita (crash, OOM, dibunuh orang lain).
    Mati,
    /// Belum pernah dinyalakan sejak daemon lahir.
    BelumPernah,
}

impl Keadaan {
    #[must_use]
    pub const fn nama(self) -> &'static str {
        match self {
            Self::Hidup => "hidup",
            Self::Tidur => "tidur",
            Self::Mati => "mati",
            Self::BelumPernah => "belum_pernah",
        }
    }

    /// Sehat artinya daemon masih bisa melayani: proses yang tidur akan
    /// dibangunkan saat dipakai, dan mesin yang belum pernah dipakai bukan
    /// kesalahan siapa pun.
    #[must_use]
    pub const fn sehat(self) -> bool {
        !matches!(self, Self::Mati)
    }
}

struct Isi {
    /// Mesin yang **sekarang** hidup. `None` berarti tidur, bukan rusak: resep
    /// masih ada di [`Slot::spec`] dan mesin bisa dinyalakan lagi.
    mesin: Option<Arc<dyn Engine>>,
    /// Fakta terakhir yang dilaporkan mesin. Disimpan karena `/json/version`
    /// harus tetap bisa dijawab saat mesin sedang tidur: membangunkan mesin
    /// hanya untuk membaca string versi akan membatalkan seluruh penghematan.
    info: Option<EngineInfo>,
    /// Koneksi klien yang sedang terbuka ke mesin ini.
    ///
    /// Hitungan ini yang menahan reap: mesin yang sedang dipakai tidak boleh
    /// dimatikan, seberapa lama pun kliennya diam.
    sesi: usize,
    /// Detik epoch saat mesin terakhir dipakai.
    terakhir: u64,
    /// Mesin ditidurkan oleh kita, bukan mati sendiri. Tanpa pembeda ini,
    /// mesin yang sengaja di-reap terlihat sama dengan mesin yang crash, dan
    /// pemantau akan menyala setiap kali daemon berhemat.
    tidur: bool,
}

/// Satu mesin yang bisa dinyalakan, dimatikan, dan dinyalakan lagi.
pub struct Slot {
    id: String,
    kind: String,
    spec: LaunchSpec,
    penggerak: Arc<dyn Penggerak>,
    isi: Mutex<Isi>,
    /// Kunci terpisah untuk boot. Menahan kunci `isi` selama 15 detik menunggu
    /// Chromium siap akan ikut menahan `/metrics` dan loop reap.
    boot: Mutex<()>,
}

impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slot")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("hidup", &self.hidup().is_some())
            .field("sesi", &self.sesi())
            .finish_non_exhaustive()
    }
}

impl Slot {
    /// Slot yang belum punya mesin. Boot pertama terjadi saat ada yang
    /// memintanya lewat [`Self::pastikan`].
    pub fn baru(
        id: impl Into<String>,
        kind: impl Into<String>,
        spec: LaunchSpec,
        penggerak: Arc<dyn Penggerak>,
        sekarang: u64,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            spec,
            penggerak,
            isi: Mutex::new(Isi {
                mesin: None,
                info: None,
                sesi: 0,
                terakhir: sekarang,
                tidur: false,
            }),
            boot: Mutex::new(()),
        }
    }

    /// Slot yang mesinnya sudah hidup, mis. karena daemon menyalakannya lebih
    /// dulu saat boot. Info-nya ikut dicatat supaya `/json/version` langsung
    /// bisa dijawab tanpa menyentuh mesin.
    pub fn siap(
        id: impl Into<String>,
        kind: impl Into<String>,
        spec: LaunchSpec,
        penggerak: Arc<dyn Penggerak>,
        engine: Arc<dyn Engine>,
        sekarang: u64,
    ) -> Self {
        let slot = Self::baru(id, kind, spec, penggerak, sekarang);
        {
            let mut isi = slot.kunci();
            isi.info = Some(engine.info().clone());
            isi.mesin = Some(engine);
        }
        slot
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    #[must_use]
    pub fn spec(&self) -> &LaunchSpec {
        &self.spec
    }

    /// Mesin yang hidup sekarang, atau `None` kalau sedang tidur/mati.
    ///
    /// Mesin yang prosesnya sudah hilang dibuang dari slot di sini, supaya
    /// pemanggil berikutnya tidak memakai `Arc` ke proses yang sudah mati.
    /// Tidak ada boot di jalur ini: pembaca metrik dan pemeriksa kesehatan
    /// tidak boleh membangunkan mesin hanya karena mereka bertanya.
    #[must_use]
    pub fn hidup(&self) -> Option<Arc<dyn Engine>> {
        let mut isi = self.kunci();
        let engine = isi.mesin.clone()?;
        if engine.is_running() {
            Some(engine)
        } else {
            isi.mesin = None;
            None
        }
    }

    /// Fakta terakhir dari mesin, juga saat mesinnya sudah mati.
    #[must_use]
    pub fn info(&self) -> Option<EngineInfo> {
        self.kunci().info.clone()
    }

    /// Mesin yang hidup, dinyalakan dulu kalau perlu.
    ///
    /// Hanya satu boot yang boleh berjalan untuk satu slot: dua klien yang
    /// datang bersamaan saat mesin tidur harus berbagi satu proses, bukan
    /// menyalakan dua Chromium di port yang sama.
    pub fn pastikan(&self, timeout_ms: u64, sekarang: u64) -> Result<Arc<dyn Engine>, SlotError> {
        if let Some(engine) = self.hidup() {
            self.sentuh(sekarang);
            return Ok(engine);
        }

        let _serial = self.boot.lock().unwrap_or_else(|err| err.into_inner());
        // Diperiksa ulang: yang menunggu kunci mungkin kalah balapan dengan
        // boot yang baru saja selesai.
        if let Some(engine) = self.hidup() {
            self.sentuh(sekarang);
            return Ok(engine);
        }

        let engine = self
            .penggerak
            .nyalakan(&self.spec, timeout_ms)
            .map_err(|err| SlotError::GagalNyalakan {
                id: self.id.clone(),
                sebab: err.to_string(),
            })?;

        {
            let mut isi = self.kunci();
            isi.info = Some(engine.info().clone());
            isi.mesin = Some(Arc::clone(&engine));
            isi.terakhir = sekarang;
            isi.tidur = false;
        }
        tracing::info!(
            mesin = %self.id,
            port = engine.info().devtools_port,
            pid = ?engine.process_id(),
            "mesin dinyalakan ulang"
        );
        Ok(engine)
    }

    /// Mencatat bahwa mesin dipakai sekarang.
    pub fn sentuh(&self, sekarang: u64) {
        self.kunci().terakhir = sekarang;
    }

    /// Klien membuka koneksi ke mesin ini.
    pub fn buka_sesi(&self, sekarang: u64) {
        let mut isi = self.kunci();
        isi.sesi = isi.sesi.saturating_add(1);
        isi.terakhir = sekarang;
    }

    /// Klien menutup koneksi. Hitungan tidak boleh di bawah nol: slot yang
    /// dianggap punya sesi negatif akan menahan reap selamanya.
    pub fn tutup_sesi(&self, sekarang: u64) {
        let mut isi = self.kunci();
        isi.sesi = isi.sesi.saturating_sub(1);
        isi.terakhir = sekarang;
    }

    #[must_use]
    pub fn sesi(&self) -> usize {
        self.kunci().sesi
    }

    /// Sudah berapa lama mesin ini tidak dipakai. Mesin dengan sesi terbuka
    /// selalu dianggap baru dipakai (`0`), karena klien yang masih tersambung
    /// mungkin sedang mengerjakan sesuatu yang tidak mengirim pesan.
    #[must_use]
    pub fn diam(&self, sekarang: u64) -> Duration {
        let isi = self.kunci();
        if isi.sesi > 0 {
            return Duration::ZERO;
        }
        Duration::from_secs(sekarang.saturating_sub(isi.terakhir))
    }

    /// Mengambil mesin **hanya kalau** memang layak dimatikan sekarang.
    ///
    /// Pemeriksaan sesi, umur diam, dan pengambilan `Arc` terjadi di dalam satu
    /// kunci. Kalau pemanggil memeriksa dulu lalu memanggil [`Self::matikan`],
    /// klien yang datang di antara dua panggilan itu akan kehilangan mesinnya.
    ///
    /// Sisa balapan yang tidak bisa ditutup di sini: klien yang sudah memegang
    /// `Arc` sebelum slot memutuskan mesin itu diam tetap bisa kehilangan
    /// soketnya. Itu diterima dan terlihat sebagai galat sambungan yang jelas,
    /// bukan sebagai pesan yang salah alamat.
    #[must_use]
    pub fn ambil_kalau_diam(&self, sekarang: u64, ambang: Duration) -> Option<Arc<dyn Engine>> {
        let mut isi = self.kunci();
        if isi.sesi > 0 {
            return None;
        }
        if Duration::from_secs(sekarang.saturating_sub(isi.terakhir)) < ambang {
            return None;
        }
        isi.tidur = true;
        isi.mesin.take()
    }

    /// Mengembalikan mesin yang gagal dihentikan ke slotnya.
    ///
    /// Pemanggil yang mengambil mesin lewat [`Self::ambil_kalau_diam`] lalu
    /// gagal mematikannya wajib memanggil ini. Tanpa itu prosesnya hidup
    /// tanpa ada yang mengurusnya, dan tidak akan pernah di-reap lagi.
    pub fn kembalikan(&self, engine: Arc<dyn Engine>) {
        let mut isi = self.kunci();
        isi.mesin = Some(engine);
        isi.tidur = false;
    }

    /// Keadaan slot sekarang, tanpa menyalakan apa pun.
    #[must_use]
    pub fn keadaan(&self) -> Keadaan {
        let mut isi = self.kunci();
        match isi.mesin.as_ref() {
            Some(engine) if engine.is_running() => Keadaan::Hidup,
            Some(_) => {
                // Proses hilang di luar kendali: buang pegangan ke proses mati
                // supaya pemanggil berikutnya menyalakan yang baru.
                isi.mesin = None;
                Keadaan::Mati
            }
            None if isi.tidur => Keadaan::Tidur,
            None if isi.info.is_some() => Keadaan::Mati,
            None => Keadaan::BelumPernah,
        }
    }

    /// Mematikan mesin yang sekarang hidup. `Ok(false)` berarti memang sudah
    /// tidur, dan itu bukan kegagalan.
    pub fn matikan(&self) -> Result<bool, SlotError> {
        let mesin = self.kunci().mesin.take();
        let Some(engine) = mesin else {
            return Ok(false);
        };
        match engine.stop() {
            Ok(()) => {
                self.kunci().tidur = true;
                Ok(true)
            }
            Err(err) => {
                // Proses yang menolak mati dikembalikan ke slot supaya putaran
                // reap berikutnya bisa mencoba lagi; membuangnya berarti proses
                // itu hidup tanpa ada yang mengurusnya.
                if engine.is_running() {
                    self.kunci().mesin = Some(engine);
                    self.kunci().tidur = false;
                }
                Err(SlotError::GagalBerhenti {
                    id: self.id.clone(),
                    sebab: err.to_string(),
                })
            }
        }
    }

    fn kunci(&self) -> std::sync::MutexGuard<'_, Isi> {
        self.isi.lock().unwrap_or_else(|err| err.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// Mesin uji: mencatat berapa kali dinyalakan dan dimatikan, tanpa proses.
    struct MesinPalsu {
        info: EngineInfo,
        spec: LaunchSpec,
        hidup: AtomicBool,
        berhenti: AtomicBool,
    }

    impl MesinPalsu {
        fn baru(port: u16) -> Self {
            Self {
                info: EngineInfo {
                    browser: "Palsu/1.0".to_owned(),
                    protocol_version: "1.3".to_owned(),
                    user_agent: "palsu".to_owned(),
                    v8_version: None,
                    webkit_version: None,
                    devtools_port: port,
                    web_socket_debugger_url: format!(
                        "ws://127.0.0.1:{port}/devtools/browser/palsu"
                    ),
                },
                spec: LaunchSpec::new(PathBuf::from("/palsu"), PathBuf::from("/tmp/palsu")),
                hidup: AtomicBool::new(true),
                berhenti: AtomicBool::new(false),
            }
        }
    }

    impl Engine for MesinPalsu {
        fn kind(&self) -> &'static str {
            "palsu"
        }
        fn info(&self) -> &EngineInfo {
            &self.info
        }
        fn spec(&self) -> &LaunchSpec {
            &self.spec
        }
        fn http_base(&self) -> String {
            format!("http://127.0.0.1:{}", self.info.devtools_port)
        }
        fn web_socket_url(&self) -> &str {
            &self.info.web_socket_debugger_url
        }
        fn process_id(&self) -> Option<u32> {
            Some(1)
        }
        fn is_running(&self) -> bool {
            self.hidup.load(Ordering::SeqCst)
        }
        fn stop(&self) -> Result<(), EngineError> {
            self.berhenti.store(true, Ordering::SeqCst);
            self.hidup.store(false, Ordering::SeqCst);
            Ok(())
        }
    }

    /// Penggerak uji: setiap panggilan menghasilkan mesin baru di port yang
    /// naik, sehingga test bisa membuktikan alamatnya benar-benar dibaca ulang.
    struct PenggerakUji {
        boot: AtomicU32,
    }

    impl Penggerak for PenggerakUji {
        fn nyalakan(
            &self,
            _spec: &LaunchSpec,
            _timeout_ms: u64,
        ) -> Result<Arc<dyn Engine>, EngineError> {
            let nomor = self.boot.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(MesinPalsu::baru(9000 + nomor as u16)))
        }
    }

    fn slot_uji() -> (Arc<Slot>, Arc<PenggerakUji>) {
        let penggerak = Arc::new(PenggerakUji {
            boot: AtomicU32::new(0),
        });
        let slot = Slot::baru(
            "kiri",
            "palsu",
            LaunchSpec::new(PathBuf::from("/palsu"), PathBuf::from("/tmp/palsu")),
            Arc::clone(&penggerak) as Arc<dyn Penggerak>,
            1000,
        );
        (Arc::new(slot), penggerak)
    }

    #[test]
    fn slot_tidur_belum_menyalakan_apa_pun() {
        let (slot, penggerak) = slot_uji();
        assert!(slot.hidup().is_none());
        assert!(slot.info().is_none());
        assert_eq!(penggerak.boot.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn pastikan_menyalakan_sekali_lalu_memakai_yang_sama() {
        let (slot, penggerak) = slot_uji();
        let pertama = slot.pastikan(1000, 1000).expect("boot pertama");
        let kedua = slot.pastikan(1000, 1001).expect("boot kedua");
        assert!(
            Arc::ptr_eq(&pertama, &kedua),
            "mesin hidup tidak boleh diganti"
        );
        assert_eq!(penggerak.boot.load(Ordering::SeqCst), 1);
        assert_eq!(slot.info().expect("info tercatat").devtools_port, 9000);
    }

    #[test]
    fn mesin_yang_dimatikan_bangkit_dengan_alamat_baru() {
        let (slot, penggerak) = slot_uji();
        assert!(slot.pastikan(1000, 1000).is_ok());
        assert!(slot.matikan().expect("berhenti"));
        assert!(slot.hidup().is_none(), "sesudah mati tidak boleh ada mesin");
        assert!(
            slot.info().is_some(),
            "fakta terakhir tetap disimpan supaya /json/version bisa dijawab"
        );

        let lagi = slot.pastikan(1000, 2000).expect("boot kedua");
        assert_eq!(penggerak.boot.load(Ordering::SeqCst), 2);
        assert_eq!(
            lagi.info().devtools_port,
            9001,
            "alamat harus dibaca dari boot terbaru, bukan yang diingat"
        );
        assert_eq!(slot.info().expect("info baru").devtools_port, 9001);
    }

    #[test]
    fn mesin_yang_prosesnya_hilang_dibuang_dari_slot() {
        // Penggerak yang mengembalikan mesin yang sudah mati: ini bentuk
        // kejadian nyata saat Chromium di-OOM-kill di luar kendali daemon.
        struct PenggerakBangkai(Arc<Mutex<Arc<MesinPalsu>>>);

        impl Penggerak for PenggerakBangkai {
            fn nyalakan(
                &self,
                _spec: &LaunchSpec,
                _timeout_ms: u64,
            ) -> Result<Arc<dyn Engine>, EngineError> {
                let mesin = MesinPalsu::baru(9100);
                mesin.hidup.store(false, Ordering::SeqCst);
                *self.0.lock().expect("kunci") = Arc::new(mesin);
                Ok(Arc::clone(&*self.0.lock().expect("kunci")) as Arc<dyn Engine>)
            }
        }

        let tempat = Arc::new(Mutex::new(Arc::new(MesinPalsu::baru(9100))));
        let slot = Slot::baru(
            "kiri",
            "palsu",
            LaunchSpec::new(PathBuf::from("/palsu"), PathBuf::from("/tmp/palsu")),
            Arc::new(PenggerakBangkai(Arc::clone(&tempat))),
            1000,
        );
        let _ = slot.pastikan(1000, 1000).expect("boot");
        assert!(
            slot.hidup().is_none(),
            "mesin yang prosesnya hilang tidak boleh dilaporkan hidup"
        );
    }

    #[test]
    fn sesi_terbuka_menahan_reap() {
        let (slot, _) = slot_uji();
        assert!(slot.pastikan(1000, 1000).is_ok());
        slot.buka_sesi(1000);
        assert_eq!(slot.sesi(), 1);
        assert_eq!(slot.diam(9999), Duration::ZERO, "ada sesi berarti dipakai");
        assert!(
            slot.ambil_kalau_diam(9999, Duration::from_secs(1))
                .is_none(),
            "mesin dengan sesi tidak boleh diambil"
        );

        slot.tutup_sesi(2000);
        assert_eq!(slot.sesi(), 0);
        assert_eq!(slot.diam(2000), Duration::ZERO);
        assert!(
            slot.ambil_kalau_diam(2000, Duration::from_secs(1))
                .is_none()
        );
        assert!(
            slot.ambil_kalau_diam(2002, Duration::from_secs(1))
                .is_some()
        );
    }

    #[test]
    fn tutup_sesi_tidak_pernah_negatif() {
        let (slot, _) = slot_uji();
        slot.tutup_sesi(1000);
        slot.tutup_sesi(1000);
        assert_eq!(slot.sesi(), 0);
    }

    #[test]
    fn ambil_kalau_diam_mengosongkan_slot_sekali_saja() {
        let (slot, _) = slot_uji();
        assert!(slot.pastikan(1000, 1000).is_ok());
        slot.sentuh(1000);
        let diambil = slot.ambil_kalau_diam(1100, Duration::from_secs(60));
        assert!(diambil.is_some());
        assert!(slot.hidup().is_none());
        assert!(
            slot.ambil_kalau_diam(1200, Duration::from_secs(60))
                .is_none(),
            "putaran reap kedua tidak boleh mendapat mesin yang sama"
        );
    }

    #[test]
    fn sentuh_menunda_reap() {
        let (slot, _) = slot_uji();
        assert!(slot.pastikan(1000, 1000).is_ok());
        slot.sentuh(1090);
        assert!(
            slot.ambil_kalau_diam(1100, Duration::from_secs(60))
                .is_none()
        );
        assert!(
            slot.ambil_kalau_diam(1160, Duration::from_secs(60))
                .is_some()
        );
    }

    #[test]
    fn matikan_slot_yang_memang_tidur_bukan_kegagalan() {
        let (slot, _) = slot_uji();
        assert!(!slot.matikan().expect("tidur bukan galat"));
    }

    #[test]
    fn boot_bersamaan_hanya_menyalakan_satu_mesin() {
        let (slot, penggerak) = slot_uji();
        let slot = Arc::clone(&slot);
        let mut tugas = Vec::new();
        for _ in 0..8 {
            let slot = Arc::clone(&slot);
            tugas.push(std::thread::spawn(move || {
                slot.pastikan(1000, 1000)
                    .expect("boot")
                    .info()
                    .devtools_port
            }));
        }
        let port: Vec<u16> = tugas
            .into_iter()
            .map(|t| t.join().expect("thread"))
            .collect();
        assert_eq!(penggerak.boot.load(Ordering::SeqCst), 1, "harus satu boot");
        assert!(port.iter().all(|p| *p == port[0]), "satu mesin untuk semua");
    }

    #[test]
    fn penggerak_gagal_tidak_meninggalkan_mesin_palsu() {
        struct SelaluGagal;
        impl Penggerak for SelaluGagal {
            fn nyalakan(
                &self,
                _spec: &LaunchSpec,
                _timeout_ms: u64,
            ) -> Result<Arc<dyn Engine>, EngineError> {
                Err(EngineError::BinaryMissing(PathBuf::from("/tidak/ada")))
            }
        }

        let slot = Slot::baru(
            "kanan",
            "palsu",
            LaunchSpec::new(PathBuf::from("/tidak/ada"), PathBuf::from("/tmp/palsu")),
            Arc::new(SelaluGagal),
            1000,
        );
        let Err(err) = slot.pastikan(10, 1000) else {
            panic!("penggerak yang selalu gagal tidak boleh melaporkan sukses");
        };
        assert!(
            err.to_string().contains("kanan"),
            "pesan sebut mesin: {err}"
        );
        assert!(slot.hidup().is_none());
    }
}
