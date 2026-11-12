//! State mesin native: tab, sesi CDP, dan penerjemah perintah.
//!
//! Yang ada di sini adalah **sisi server** dari protokol: frame JSON-RPC masuk,
//! panggilan ke [`TabPool`] keluar, frame jawaban kembali. Tidak ada soket di
//! file ini, jadi seluruh perilakunya bisa diuji tanpa membuka port.
//!
//! Dua hal yang sengaja dijawab dengan galat alih-alih pura-pura bisa: metode
//! yang belum dibangun (lihat [`Galat::belum_ada`]) dan URL yang tidak bisa
//! diambil, termasuk skema di luar `http`/`https`.
//!
//! Pengambilan dokumen dari jaringan **tidak** terjadi di dalam file ini.
//! [`Dalam`] dipegang satu kunci, jadi menunggu jaringan sambil memegangnya
//! akan menghentikan seluruh tab. Pemanggil async mengambil dokumennya lebih
//! dulu lewat [`super::jaringan`], lalu menyerahkannya sebagai parameter ke
//! [`NativeState::tangani_ambil`].

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard};

use engine::native::layout::Viewport;
use engine::native::{
    ColdStore, Document, MemoryColdStore, NilaiJs, NodeId, NodeKind, Snapshot, TabId, TabPool,
    axtree, paint,
};
use serde_json::{Map, Value, json};

use super::jaringan::{Ambilan, Sumber};

/// Penyimpanan dingin yang bisa dipakai lewat `dyn`.
///
/// [`ColdStore`] di `engine` punya `type Error`, jadi trait itu tidak dyn-safe
/// dan pool tidak bisa memegang implementasi berbeda di satu tipe. Wrapper ini
/// menormalkan galatnya jadi `String`, sehingga `cdp` bisa menerima
/// `browserd::tabstore::StoreColdStore` (adapter SQLite di jalur produksi)
/// tanpa mengenal crate `store` sama sekali. Yang dipegang state di sini adalah
/// `Box<dyn ColdStoreObj>`: daemon mengisinya lewat [`PabrikDingin`], tes dan
/// mode tanpa database mengisinya lewat [`pabrik_memori`].
pub trait ColdStoreObj: Send {
    fn simpan_obj(&mut self, snapshot: &Snapshot) -> Result<(), String>;
    fn muat_obj(&self, id: TabId) -> Result<Option<Snapshot>, String>;
    fn hapus_obj(&mut self, id: TabId) -> Result<(), String>;
    fn jumlah_obj(&self) -> Result<usize, String>;
}

impl<C> ColdStoreObj for C
where
    C: ColdStore + Send,
    C::Error: std::fmt::Display,
{
    fn simpan_obj(&mut self, snapshot: &Snapshot) -> Result<(), String> {
        self.simpan(snapshot).map_err(|err| err.to_string())
    }

    fn muat_obj(&self, id: TabId) -> Result<Option<Snapshot>, String> {
        self.muat(id).map_err(|err| err.to_string())
    }

    fn hapus_obj(&mut self, id: TabId) -> Result<(), String> {
        self.hapus(id).map_err(|err| err.to_string())
    }

    fn jumlah_obj(&self) -> Result<usize, String> {
        self.jumlah().map_err(|err| err.to_string())
    }
}

impl ColdStore for Box<dyn ColdStoreObj> {
    type Error = String;

    fn simpan(&mut self, snapshot: &Snapshot) -> Result<(), Self::Error> {
        self.as_mut().simpan_obj(snapshot)
    }

    fn muat(&self, id: TabId) -> Result<Option<Snapshot>, Self::Error> {
        self.as_ref().muat_obj(id)
    }

    fn hapus(&mut self, id: TabId) -> Result<(), Self::Error> {
        self.as_mut().hapus_obj(id)
    }

    fn jumlah(&self) -> Result<usize, Self::Error> {
        self.as_ref().jumlah_obj()
    }
}

/// Pabrik penyimpanan dingin. Dipanggil sekali per pool, jadi tiap mesin native
/// mendapat penyimpanannya sendiri dan dua mesin tidak berbagi ruang tab.
pub type PabrikDingin = Arc<dyn Fn() -> Box<dyn ColdStoreObj> + Send + Sync>;

/// Penyimpanan dingin bawaan: di memori. Dipakai tes dan mode tanpa database.
#[must_use]
pub fn pabrik_memori() -> PabrikDingin {
    Arc::new(|| Box::new(MemoryColdStore::default()) as Box<dyn ColdStoreObj>)
}

/// Batas jumlah tab yang bisa dialamatkan, dari PRD v0.2 (FR-005).
pub const MAKS_TAB: usize = 99;

/// Ukuran jendela bawaan saat klien tidak menyebut ukuran apa pun.
///
/// 800x600 sama dengan bawaan Chrome headless baru. Angka ini dipakai apa
/// adanya: mengubahnya diam-diam akan menggeser hasil layout tanpa ada yang
/// meminta.
pub const VIEWPORT_BAWAAN: (f32, f32) = (800.0, 600.0);

/// Galat protokol, sudah dalam bentuk yang siap dikirim ke klien.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Galat {
    pub kode: i64,
    pub pesan: String,
}

impl Galat {
    #[must_use]
    pub fn baru(kode: i64, pesan: impl Into<String>) -> Self {
        Self {
            kode,
            pesan: pesan.into(),
        }
    }

    /// Metode yang dikenal protokol tapi **belum** dibangun di mesin ini.
    ///
    /// Pesannya menyebut lapisan yang kurang, bukan "unknown method": klien
    /// yang membaca galat ini harus bisa membedakan fitur yang belum ada dari
    /// salah ketik nama metode.
    #[must_use]
    pub fn belum_ada(metode: &str) -> Self {
        let alasan = match metode {
            "Page.captureSnapshot" | "Page.printToPDF" => {
                "hanya PNG yang bisa dirasterkan; MHTML dan PDF belum ada"
            }
            "Input.dispatchMouseEvent"
            | "Input.dispatchKeyEvent"
            | "Input.insertText"
            | "Input.dispatchTouchEvent" => "pengiriman peristiwa DOM belum ada; itu lapisan hands",
            "DOM.getBoxModel" => {
                "kotak layout belum memisahkan padding, border, dan margin; \
                 `DOM.getBoxModel` butuh keempatnya, dan mengarangnya jadi kebohongan"
            }
            "Network.enable" | "Network.getCookies" | "Network.setCookie" => {
                "tidak ada tumpukan jaringan di mesin ini; dokumen masuk lewat \
                 `Page.setDocumentContent`"
            }
            _ => "metode ini tidak dikenal mesin native",
        };
        Self::baru(-32601, format!("{metode}: {alasan}"))
    }
}

/// Bentuk dokumen yang bisa dibandingkan sebelum dan sesudah satu aksi.
///
/// Dipakai [`banding`] untuk membuat tanda terima (FR-010). Yang dibandingkan
/// hanya hal yang benar-benar bisa diamati klien: URL, judul, jumlah node, dan
/// sidik isi dokumen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sidik {
    pub url: String,
    pub judul: Option<String>,
    pub node: usize,
    hash: u64,
}

impl Sidik {
    fn baru(url: &str, judul: Option<String>, node: usize, html: &str) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        html.hash(&mut h);
        Self {
            url: url.to_owned(),
            judul,
            node,
            hash: h.finish(),
        }
    }
}

/// Tanda terima satu aksi: apa yang berubah, atau bahwa tidak ada yang berubah.
///
/// `NoObservableChange` bukan kegagalan protokol, jadi ia dikirim sebagai
/// **hasil** yang sah, bukan sebagai galat. Pemanggil yang menganggap aksi
/// berhasil hanya karena tidak ada galat akan tertangkap di sini.
#[must_use]
pub fn banding(sebelum: &Sidik, sesudah: &Sidik) -> Value {
    let url = sebelum.url != sesudah.url;
    let judul = sebelum.judul != sesudah.judul;
    let dom = sebelum.hash != sesudah.hash;
    let berubah = url || judul || dom;
    json!({
        "status": if berubah { "Changed" } else { "NoObservableChange" },
        "sebelum": {
            "url": sebelum.url,
            "judul": sebelum.judul,
            "node": sebelum.node,
        },
        "sesudah": {
            "url": sesudah.url,
            "judul": sesudah.judul,
            "node": sesudah.node,
        },
        "delta": {
            "node": sesudah.node as i64 - sebelum.node as i64,
            "url": url,
            "judul": judul,
            "dom": dom,
        },
    })
}

/// Metadata satu tab yang tetap ada walau tabnya sedang dingin.
struct MetaTab {
    url: String,
    judul: Option<String>,
    id_target: String,
}

struct Dalam {
    pool: TabPool<Box<dyn ColdStoreObj>>,
    tabs: BTreeMap<TabId, MetaTab>,
    sesi: BTreeMap<String, TabId>,
    berikut_sesi: u64,
}

/// State mesin native, dipakai bersama oleh server dan tes.
#[derive(Clone)]
pub struct NativeState {
    dalam: Arc<Mutex<Dalam>>,
}

impl Default for NativeState {
    fn default() -> Self {
        Self::baru()
    }
}

impl NativeState {
    #[must_use]
    pub fn baru() -> Self {
        Self::dengan_dingin(pabrik_memori())
    }

    /// State dengan penyimpanan dingin pilihan pemanggil.
    ///
    /// Daemon memakai ini untuk menyerahkan adapter SQLite, sehingga tab yang
    /// disingkirkan selamat dari restart. Tes memakai [`Self::baru`].
    #[must_use]
    pub fn dengan_dingin(pabrik: PabrikDingin) -> Self {
        Self {
            dalam: Arc::new(Mutex::new(Dalam {
                pool: TabPool::new(pabrik(), MAKS_TAB),
                tabs: BTreeMap::new(),
                sesi: BTreeMap::new(),
                berikut_sesi: 1,
            })),
        }
    }

    fn kunci(&self) -> MutexGuard<'_, Dalam> {
        self.dalam.lock().unwrap_or_else(|err| {
            tracing::warn!("state mesin native pulih dari mutex poisoned");
            err.into_inner()
        })
    }

    /// Menjawab satu frame dari soket browser. `None` berarti tidak ada balasan
    /// (notifikasi tanpa `id`).
    ///
    /// Tidak mengambil apa pun dari jaringan. Frame yang berisi navigasi
    /// `http`/`https` dijawab dengan galat yang menyebut bahwa dokumennya harus
    /// diserahkan lewat [`Self::tangani_ambil`].
    #[must_use]
    pub fn tangani(&self, teks: &str) -> Option<String> {
        self.kunci().tangani(teks, None, None)
    }

    /// Menjawab satu frame dari soket satu halaman, yang tidak membawa
    /// `sessionId` karena tabnya sudah ditentukan oleh URL soket.
    #[must_use]
    pub fn tangani_halaman(&self, teks: &str, sesi: &str) -> Option<String> {
        self.kunci().tangani(teks, Some(sesi), None)
    }

    /// Menjawab satu frame dengan dokumen jaringan yang sudah diambil lebih
    /// dulu.
    ///
    /// `ambilan` diisi pemanggil async **sebelum** kunci state diambil, supaya
    /// satu navigasi yang lambat tidak membekukan tab lain. `None` berarti
    /// frame ini tidak butuh jaringan.
    #[must_use]
    pub fn tangani_ambil(
        &self,
        teks: &str,
        sesi: Option<&str>,
        ambilan: Option<Result<Ambilan, Galat>>,
    ) -> Option<String> {
        self.kunci().tangani(teks, sesi, ambilan)
    }

    /// Daftar target dalam bentuk `/json/list` milik Chrome, tanpa URL soket
    /// (pemanggil yang menempelkannya, karena hanya dia yang tahu port).
    #[must_use]
    pub fn target(&self) -> Vec<Value> {
        let d = self.kunci();
        d.tabs
            .iter()
            .map(|(id, meta)| {
                let terpasang = d.sesi.values().any(|t| t == id);
                json!({
                    "id": meta.id_target,
                    "type": "page",
                    "title": meta.judul.clone().unwrap_or_default(),
                    "url": meta.url,
                    "description": "",
                    "attached": terpasang,
                })
            })
            .collect()
    }

    pub fn buka(&self, url: &str) -> Result<String, Galat> {
        self.kunci().buat_target(url, None)
    }

    /// Sama dengan [`Self::buka`], tapi dokumen jaringannya sudah diambil
    /// pemanggil lebih dulu.
    pub fn buka_dengan(
        &self,
        url: &str,
        ambilan: Option<Result<Ambilan, Galat>>,
    ) -> Result<String, Galat> {
        self.kunci().buat_target(url, ambilan)
    }

    pub fn tutup(&self, id: &str) -> Result<(), Galat> {
        self.kunci().tutup_target(id)
    }

    pub fn aktifkan(&self, id: &str) -> Result<(), Galat> {
        self.kunci().aktifkan_target(id)
    }

    /// Sesi untuk satu tab, dibuat kalau belum ada. Dipakai soket
    /// `/devtools/page/{id}` yang tidak punya jalan lain untuk memperoleh
    /// `sessionId`.
    pub fn sesi_halaman(&self, id: &str) -> Result<String, Galat> {
        let mut d = self.kunci();
        let tab = d.tab_dari_id(id)?;
        if let Some((sesi, _)) = d.sesi.iter().find(|(_, t)| **t == tab) {
            return Ok(sesi.clone());
        }
        Ok(d.sesi_baru(tab))
    }

    /// Jumlah tab yang dialamatkan saat ini, untuk pemeriksaan kapasitas dari
    /// luar tanpa membuka state.
    #[must_use]
    pub fn jumlah_tab(&self) -> usize {
        self.kunci().tabs.len()
    }
}

impl Dalam {
    fn tangani(
        &mut self,
        teks: &str,
        sesi_paksa: Option<&str>,
        ambilan: Option<Result<Ambilan, Galat>>,
    ) -> Option<String> {
        let mut frame: Value = match serde_json::from_str(teks) {
            Ok(nilai) => nilai,
            Err(err) => {
                return Some(bingkai(
                    Value::Null,
                    Err(Galat::baru(-32700, format!("frame bukan JSON: {err}"))),
                ));
            }
        };
        let Some(obj) = frame.as_object_mut() else {
            return Some(bingkai(
                Value::Null,
                Err(Galat::baru(-32600, "frame harus objek JSON")),
            ));
        };
        if let Some(sesi) = sesi_paksa
            && !obj.contains_key("sessionId")
        {
            obj.insert("sessionId".to_owned(), Value::String(sesi.to_owned()));
        }
        let id = obj.get("id").cloned();
        let Some(metode) = obj.get("method").and_then(Value::as_str).map(str::to_owned) else {
            // Tanpa `method` bukan permintaan. Chrome juga mengabaikannya.
            return None;
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let sesi = obj
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let hasil = self.jalankan(&metode, &params, sesi.as_deref(), ambilan);
        // Notifikasi (tanpa `id`) tidak dijawab.
        let id = id?;
        Some(bingkai(id, hasil))
    }

    fn jalankan(
        &mut self,
        metode: &str,
        params: &Value,
        sesi: Option<&str>,
        ambilan: Option<Result<Ambilan, Galat>>,
    ) -> Result<Value, Galat> {
        match metode {
            "Browser.getVersion" => Ok(json!({
                "protocolVersion": super::PROTOKOL,
                "product": super::PRODUK,
                "revision": env!("CARGO_PKG_VERSION"),
                "userAgent": super::USER_AGENT,
                "jsVersion": "QuickJS",
            })),
            "Target.getTargets" => Ok(json!({ "targetInfos": self.info_target() })),
            "Target.createTarget" => {
                let url = param_teks(params, "url")?;
                Ok(json!({ "targetId": self.buat_target(&url, ambilan)? }))
            }
            "Target.closeTarget" => {
                let id = param_teks(params, "targetId")?;
                self.tutup_target(&id)?;
                Ok(json!({ "success": true }))
            }
            "Target.activateTarget" => {
                let id = param_teks(params, "targetId")?;
                self.aktifkan_target(&id)?;
                Ok(json!({}))
            }
            "Target.attachToTarget" => self.lekat(params),
            "Target.detachFromTarget" => {
                let s = match params.get("sessionId").and_then(Value::as_str) {
                    Some(s) => s.to_owned(),
                    None => sesi
                        .ok_or_else(|| {
                            Galat::baru(-32602, "Target.detachFromTarget butuh `sessionId`")
                        })?
                        .to_owned(),
                };
                self.lepas(&s)?;
                Ok(json!({}))
            }
            "Page.navigate" => self.navigasi(params, sesi, ambilan),
            "Page.reload" => {
                let tab = self.tab_dari_sesi(sesi)?;
                self.muat_ulang(tab)?;
                Ok(json!({}))
            }
            "Page.setDocumentContent" => self.set_konten(params, sesi),
            "Page.getFrameTree" => self.pohon_frame(sesi),
            "Page.captureScreenshot" => self.tangkap_layar(params, sesi),
            "Accessibility.getFullAXTree" => self.pohon_ax(sesi),
            "Page.bringToFront" => {
                let tab = self.tab_dari_sesi(sesi)?;
                self.aktifkan_tab(tab)?;
                Ok(json!({}))
            }
            "DOM.getDocument" => self.dokumen(params, sesi),
            "DOM.querySelector" => self.dom_pilih(params, sesi, false),
            "DOM.querySelectorAll" => self.dom_pilih(params, sesi, true),
            "DOM.getOuterHTML" => self.dom_luar_html(params, sesi),
            "DOM.resolveNode" => self.dom_simpul(params, sesi),
            "Runtime.evaluate" => self.evaluasi(params, sesi),
            // Metode yang tidak mengubah apa pun di mesin ini. Dijawab `{}`
            // karena memang tidak ada yang perlu dilakukan, bukan karena
            // fiturnya ada: mesin ini tidak mengirim peristiwa.
            "Page.enable"
            | "Page.disable"
            | "Page.setLifecycleEventsEnabled"
            | "Runtime.enable"
            | "Runtime.disable"
            | "DOM.enable"
            | "DOM.disable"
            | "Log.enable"
            | "Log.clear"
            | "Target.setDiscoverTargets"
            | "Target.setAutoAttach" => Ok(json!({})),
            lain => Err(Galat::belum_ada(lain)),
        }
    }

    fn info_target(&self) -> Vec<Value> {
        self.tabs
            .iter()
            .map(|(id, meta)| {
                json!({
                    "targetId": meta.id_target,
                    "type": "page",
                    "title": meta.judul.clone().unwrap_or_default(),
                    "url": meta.url,
                    "attached": self.sesi.values().any(|t| t == id),
                    "canAccessOpener": false,
                    "browserContextId": "bawaan",
                })
            })
            .collect()
    }

    fn buat_target(
        &mut self,
        url: &str,
        ambilan: Option<Result<Ambilan, Galat>>,
    ) -> Result<String, Galat> {
        let dok = dokumen_awal(url, ambilan)?;
        let id = self.pool.buka(&dok.url, &dok.html).map_err(pool_galat)?;
        let judul = self.pool.tab(id).and_then(|t| t.dokumen.judul());
        self.tabs.insert(
            id,
            MetaTab {
                url: dok.url,
                judul,
                id_target: id.0.to_string(),
            },
        );
        Ok(id.0.to_string())
    }

    fn tutup_target(&mut self, id: &str) -> Result<(), Galat> {
        let tab = self.tab_dari_id(id)?;
        self.pool.tutup(tab).map_err(pool_galat)?;
        self.tabs.remove(&tab);
        self.sesi.retain(|_, t| *t != tab);
        Ok(())
    }

    fn aktifkan_target(&mut self, id: &str) -> Result<(), Galat> {
        let tab = self.tab_dari_id(id)?;
        self.aktifkan_tab(tab)
    }

    fn aktifkan_tab(&mut self, tab: TabId) -> Result<(), Galat> {
        self.pool.hidupkan(tab).map_err(pool_galat)?;
        if let Some(t) = self.pool.tab_mut(tab) {
            t.terakhir_dipakai = std::time::Instant::now();
        }
        Ok(())
    }

    fn lekat(&mut self, params: &Value) -> Result<Value, Galat> {
        let id = param_teks(params, "targetId")?;
        let tab = self.tab_dari_id(&id)?;
        Ok(json!({ "sessionId": self.sesi_baru(tab) }))
    }

    fn sesi_baru(&mut self, tab: TabId) -> String {
        let sesi = format!("{:032X}", self.berikut_sesi);
        self.berikut_sesi += 1;
        self.sesi.insert(sesi.clone(), tab);
        sesi
    }

    fn lepas(&mut self, sesi: &str) -> Result<(), Galat> {
        self.sesi
            .remove(sesi)
            .map(|_| ())
            .ok_or_else(|| Galat::baru(-32001, format!("sesi '{sesi}' tidak dikenal")))
    }

    fn tab_dari_id(&self, id: &str) -> Result<TabId, Galat> {
        let nomor: u64 = id
            .parse()
            .map_err(|_| Galat::baru(-32602, format!("targetId '{id}' bukan angka")))?;
        let tab = TabId(nomor);
        if self.tabs.contains_key(&tab) {
            Ok(tab)
        } else {
            Err(Galat::baru(-32001, format!("target '{id}' tidak dikenal")))
        }
    }

    fn tab_dari_sesi(&self, sesi: Option<&str>) -> Result<TabId, Galat> {
        let sesi = sesi.ok_or_else(|| {
            Galat::baru(
                -32602,
                "perintah ini butuh `sessionId`: attach ke target dulu",
            )
        })?;
        self.sesi
            .get(sesi)
            .copied()
            .ok_or_else(|| Galat::baru(-32001, format!("sesi '{sesi}' tidak dikenal")))
    }

    fn sidik(&mut self, tab: TabId) -> Result<Sidik, Galat> {
        if self.pool.tab(tab).is_none() {
            self.pool.hidupkan(tab).map_err(pool_galat)?;
        }
        let t = self
            .pool
            .tab(tab)
            .ok_or_else(|| Galat::baru(-32001, format!("target '{}' tidak dikenal", tab.0)))?;
        Ok(Sidik::baru(
            &t.url,
            t.dokumen.judul(),
            t.dokumen.jumlah_node(),
            &t.dokumen.to_html(),
        ))
    }
}

/// Aksi yang mengubah dokumen, beserta tanda terimanya.
///
/// Setiap aksi di sini mengukur keadaan tab **sebelum** dan **sesudah** lalu
/// menempelkan hasil [`banding`] ke dalam `result.receipt`. Klien yang hanya
/// membaca `result` tetap mendapat jawaban yang sah; klien yang ingin tahu
/// apakah aksinya benar-benar mendarat membaca `receipt.status`.
impl Dalam {
    fn navigasi(
        &mut self,
        params: &Value,
        sesi: Option<&str>,
        ambilan: Option<Result<Ambilan, Galat>>,
    ) -> Result<Value, Galat> {
        let tab = self.tab_dari_sesi(sesi)?;
        let url = param_teks(params, "url")?;
        let dok = dokumen_awal(&url, ambilan)?;
        let sebelum = self.sidik(tab)?;
        self.pool
            .ganti_dokumen(tab, &dok.url, &dok.html)
            .map_err(pool_galat)?;
        let judul = self.pool.tab(tab).and_then(|t| t.dokumen.judul());
        if let Some(meta) = self.tabs.get_mut(&tab) {
            meta.url = dok.url.clone();
            meta.judul = judul;
        }
        let sesudah = self.sidik(tab)?;
        Ok(json!({
            "frameId": frame_id(tab),
            "loaderId": loader_id(tab),
            // `sumber` hanya terisi kalau dokumennya datang dari jaringan.
            // Klien yang ingin tahu status HTTP dan charset membacanya di sini
            // alih-alih menebak dari isi halaman.
            "sumber": dok.jaringan.as_ref().map(|a| json!({
                "urlFinal": a.url,
                "status": a.status,
                "byte": a.byte,
                "charset": a.charset,
                "contentType": a.content_type,
            })),
            "receipt": banding(&sebelum, &sesudah),
        }))
    }

    fn set_konten(&mut self, params: &Value, sesi: Option<&str>) -> Result<Value, Galat> {
        let tab = self.tab_dari_sesi(sesi)?;
        let html = param_teks(params, "html")?;
        let url = self
            .tabs
            .get(&tab)
            .map(|meta| meta.url.clone())
            .unwrap_or_default();
        let sebelum = self.sidik(tab)?;
        self.pool
            .ganti_dokumen(tab, &url, &html)
            .map_err(pool_galat)?;
        let judul = self.pool.tab(tab).and_then(|t| t.dokumen.judul());
        if let Some(meta) = self.tabs.get_mut(&tab) {
            meta.judul = judul;
        }
        let sesudah = self.sidik(tab)?;
        Ok(json!({ "receipt": banding(&sebelum, &sesudah) }))
    }

    /// Memuat ulang dokumen yang sekarang, lewat serialisasi DOM-nya sendiri.
    ///
    /// Tidak ada tanda terima di sini: dokumen yang di-serialisasi lalu
    /// di-parse ulang belum tentu menghasilkan byte yang sama, jadi
    /// membandingkannya akan melaporkan perubahan palsu.
    fn muat_ulang(&mut self, tab: TabId) -> Result<(), Galat> {
        if self.pool.tab(tab).is_none() {
            self.pool.hidupkan(tab).map_err(pool_galat)?;
        }
        let (url, html) = {
            let t = self.pool.tab(tab).ok_or_else(|| tab_hilang(tab))?;
            (t.url.clone(), t.dokumen.to_html())
        };
        self.pool
            .ganti_dokumen(tab, &url, &html)
            .map_err(pool_galat)
    }

    fn pohon_frame(&mut self, sesi: Option<&str>) -> Result<Value, Galat> {
        let tab = self.tab_dari_sesi(sesi)?;
        if self.pool.tab(tab).is_none() {
            self.pool.hidupkan(tab).map_err(pool_galat)?;
        }
        let url = self
            .pool
            .tab(tab)
            .map(|t| t.url.clone())
            .ok_or_else(|| tab_hilang(tab))?;
        Ok(json!({
            "frameTree": {
                "frame": {
                    "id": frame_id(tab),
                    "loaderId": loader_id(tab),
                    "url": url,
                    "domainAndRegistry": "",
                    "securityOrigin": "",
                    "mimeType": "text/html",
                    "secureContextType": "Secure",
                    "crossOriginIsolatedContextType": "NotIsolated",
                    "gatedAPIFeatures": [],
                },
                "childFrames": [],
            }
        }))
    }

    /// `Accessibility.getFullAXTree`: isi halaman dalam bentuk yang bisa
    /// dibaca agen tanpa penglihatan.
    ///
    /// Simpul yang `ignored` tetap dikirim, sama seperti Chrome: klien yang
    /// memutuskan apa yang dilewati, bukan mesin ini.
    fn pohon_ax(&mut self, sesi: Option<&str>) -> Result<Value, Galat> {
        let tab = self.tab_dari_sesi(sesi)?;
        if self.pool.tab(tab).is_none() {
            self.pool.hidupkan(tab).map_err(pool_galat)?;
        }
        let t = self.pool.tab(tab).ok_or_else(|| tab_hilang(tab))?;
        Ok(axtree::json_pohon(&t.dokumen))
    }

    /// `Page.captureScreenshot`: rasterisasi kotak layout menjadi PNG.
    ///
    /// Hanya PNG. JPEG butuh encoder lain, dan mengaku bisa JPEG lalu mengirim
    /// PNG adalah kebohongan yang baru ketahuan di klien.
    fn tangkap_layar(&mut self, params: &Value, sesi: Option<&str>) -> Result<Value, Galat> {
        let format = params
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("png");
        if !format.eq_ignore_ascii_case("png") {
            return Err(Galat::baru(
                -32602,
                format!("format '{format}' belum didukung: mesin native hanya mengeluarkan PNG"),
            ));
        }
        let klip = klip_dari(params)?;
        let tab = self.tab_dari_sesi(sesi)?;
        if self.pool.tab(tab).is_none() {
            self.pool.hidupkan(tab).map_err(pool_galat)?;
        }
        let t = self.pool.tab(tab).ok_or_else(|| tab_hilang(tab))?;
        // Viewport harus memuat klip; kalau tidak, layout dihitung pada kanvas
        // yang lebih kecil daripada potongan yang diminta dan kotaknya terpotong
        // sebelum sampai ke raster.
        let viewport = match klip {
            Some(k) => Viewport::new((k.x + k.width).max(1.0), (k.y + k.height).max(1.0)),
            None => Viewport::new(VIEWPORT_BAWAAN.0, VIEWPORT_BAWAAN.1),
        };
        let raster = paint::lukis(&t.dokumen, viewport, klip)
            .map_err(|err| Galat::baru(-32000, format!("tangkapan layar gagal: {err}")))?;
        Ok(json!({
            "data": crate::base64::kodekan(&raster.png),
            // Tanda terima, bukan hiasan: klien bisa tahu berapa kotak yang
            // benar-benar dilukis dan deklarasi mana yang tidak didukung.
            "receipt": {
                "width": raster.width,
                "height": raster.height,
                "kotakDilukis": raster.kotak_dilukis,
                "diabaikan": raster.diabaikan,
            },
        }))
    }

    fn dokumen(&mut self, params: &Value, sesi: Option<&str>) -> Result<Value, Galat> {
        let tab = self.tab_dari_sesi(sesi)?;
        // `depth` bawaan Chrome adalah 1: akar dan anak langsungnya.
        let kedalaman = params.get("depth").and_then(Value::as_i64).unwrap_or(1);
        if self.pool.tab(tab).is_none() {
            self.pool.hidupkan(tab).map_err(pool_galat)?;
        }
        let t = self.pool.tab(tab).ok_or_else(|| tab_hilang(tab))?;
        Ok(json!({ "root": node_json(&t.dokumen, t.dokumen.root(), kedalaman, 0) }))
    }

    /// `DOM.querySelector` dan `DOM.querySelectorAll`.
    ///
    /// Pencarian dibatasi ke subtree `nodeId` kalau parameter itu dikirim,
    /// sama seperti Chrome. Tanpa `nodeId`, seluruh dokumen yang dicari.
    ///
    /// `nodeId` yang dikembalikan adalah indeks arena [`Document`], bukan
    /// nomor stabil seumur halaman: `Page.setDocumentContent` membangun ulang
    /// dokumen, jadi nomor lama tidak berlaku lagi setelahnya.
    fn dom_pilih(
        &mut self,
        params: &Value,
        sesi: Option<&str>,
        banyak: bool,
    ) -> Result<Value, Galat> {
        let selektor = param_teks(params, "selector")?;
        let akar = match params.get("nodeId") {
            None | Some(Value::Null) => None,
            Some(_) => Some(param_node_id(params)?),
        };
        let tab = self.tab_dari_sesi(sesi)?;
        if self.pool.tab(tab).is_none() {
            self.pool.hidupkan(tab).map_err(pool_galat)?;
        }
        let t = self.pool.tab(tab).ok_or_else(|| tab_hilang(tab))?;
        let hasil = match akar {
            Some(n) => t.dokumen.pilih_dalam(n, &selektor),
            None => t.dokumen.pilih(&selektor),
        }
        .map_err(|err| Galat::baru(-32000, err.to_string()))?;
        if banyak {
            Ok(json!({ "nodeIds": hasil }))
        } else {
            // Chrome menjawab `nodeId: 0` kalau tidak ketemu, dan 0 di arena
            // ini adalah node dokumen. `null` lebih jujur daripada angka yang
            // bisa disalahartikan sebagai node sungguhan.
            Ok(json!({ "nodeId": hasil.first().copied() }))
        }
    }

    /// `DOM.getOuterHTML`: HTML satu node beserta turunannya.
    fn dom_luar_html(&mut self, params: &Value, sesi: Option<&str>) -> Result<Value, Galat> {
        let id = param_node_id(params)?;
        let tab = self.tab_dari_sesi(sesi)?;
        if self.pool.tab(tab).is_none() {
            self.pool.hidupkan(tab).map_err(pool_galat)?;
        }
        let t = self.pool.tab(tab).ok_or_else(|| tab_hilang(tab))?;
        let html = t.dokumen.luar_html(id).ok_or_else(|| {
            Galat::baru(
                -32000,
                format!("node {id} bukan elemen, jadi tidak punya HTML luar"),
            )
        })?;
        Ok(json!({ "outerHTML": html }))
    }

    /// `DOM.resolveNode`: pegangan ke node untuk `Runtime.callFunctionOn`.
    ///
    /// Mesin ini belum punya registri objek jarak jauh, jadi `objectId` adalah
    /// penanda `node-<id>` dan `Runtime.callFunctionOn` belum menerimanya.
    /// Bentuk jawabannya tetap sesuai protokol supaya klien tidak bingung.
    fn dom_simpul(&mut self, params: &Value, sesi: Option<&str>) -> Result<Value, Galat> {
        let id = param_node_id(params)?;
        let tab = self.tab_dari_sesi(sesi)?;
        if self.pool.tab(tab).is_none() {
            self.pool.hidupkan(tab).map_err(pool_galat)?;
        }
        let t = self.pool.tab(tab).ok_or_else(|| tab_hilang(tab))?;
        let node = t
            .dokumen
            .node(id)
            .ok_or_else(|| Galat::baru(-32000, format!("node {id} tidak ada")))?;
        let tag = node
            .tag()
            .ok_or_else(|| Galat::baru(-32000, format!("node {id} bukan elemen")))?;
        let mut keterangan = tag.to_owned();
        if let Some(v) = node.attr("id") {
            keterangan.push('#');
            keterangan.push_str(v);
        }
        if let Some(v) = node.attr("class") {
            for k in v.split_ascii_whitespace() {
                keterangan.push('.');
                keterangan.push_str(k);
            }
        }
        Ok(json!({
            "object": {
                "type": "object",
                "subtype": "node",
                "className": "Node",
                "description": keterangan,
                "objectId": format!("node-{id}"),
            }
        }))
    }

    fn evaluasi(&mut self, params: &Value, sesi: Option<&str>) -> Result<Value, Galat> {
        let tab = self.tab_dari_sesi(sesi)?;
        let ekspresi = param_teks(params, "expression")?;
        let sebelum = self.sidik(tab)?;
        if self.pool.tab(tab).is_none() {
            self.pool.hidupkan(tab).map_err(pool_galat)?;
        }
        let keluaran = {
            let t = self.pool.tab(tab).ok_or_else(|| tab_hilang(tab))?;
            t.isolat
                .eval_bertipe(&ekspresi)
                .map_err(|err| Galat::baru(-32000, format!("skrip gagal: {err}")))?
        };
        let sesudah = self.sidik(tab)?;
        Ok(json!({
            "result": remote_object(keluaran),
            "receipt": banding(&sebelum, &sesudah),
        }))
    }
}

/// Menerjemahkan [`NilaiJs`] jadi `RemoteObject` CDP.
///
/// Bentuknya mengikuti `Runtime.RemoteObject`: `type` wajib, `value` hanya ada
/// untuk nilai yang bisa dikirim, `description` untuk yang perlu dibaca
/// manusia. Objek selalu *by value* karena mesin ini belum punya penyimpanan
/// objek; batas itu tercatat di `engine::native::CELAH`.
fn remote_object(nilai: NilaiJs) -> Value {
    match nilai {
        NilaiJs::TanpaNilai => json!({ "type": "undefined" }),
        NilaiJs::Null => json!({ "type": "object", "subtype": "null", "value": null }),
        NilaiJs::Boolean(benar) => json!({
            "type": "boolean",
            "value": benar,
            "description": benar.to_string(),
        }),
        NilaiJs::Bulat(angka) => json!({
            "type": "number",
            "value": angka,
            "description": angka.to_string(),
        }),
        NilaiJs::Pecahan(angka) => {
            if angka.is_finite() {
                json!({
                    "type": "number",
                    "value": angka,
                    "description": angka.to_string(),
                })
            } else {
                // JSON tidak punya NaN/Infinity. CDP menaruhnya di
                // `unserializableValue`, dan tanpa itu `serde_json` akan
                // diam-diam mengubahnya jadi null.
                let teks = if angka.is_nan() {
                    "NaN".to_owned()
                } else if angka.is_sign_negative() {
                    "-Infinity".to_owned()
                } else {
                    "Infinity".to_owned()
                };
                json!({
                    "type": "number",
                    "unserializableValue": teks,
                    "description": teks,
                })
            }
        }
        NilaiJs::Teks(teks) => json!({
            "type": "string",
            "value": teks,
            "description": teks,
        }),
        NilaiJs::Objek {
            json: Some(teks),
            deskripsi,
        } => {
            // Teksnya sudah JSON sah dari `JSON.stringify`, jadi bisa langsung
            // jadi `value`. Kalau ternyata tidak sah, teksnya dikirim apa
            // adanya alih-alih menghilangkan nilai.
            let isi = serde_json::from_str(&teks).unwrap_or(Value::String(teks));
            json!({ "type": "object", "value": isi, "description": deskripsi })
        }
        NilaiJs::Objek {
            json: None,
            deskripsi,
        } => {
            json!({ "type": "object", "description": deskripsi })
        }
        NilaiJs::Fungsi(deskripsi) => json!({ "type": "function", "description": deskripsi }),
        NilaiJs::Simbol(deskripsi) => json!({ "type": "symbol", "description": deskripsi }),
        NilaiJs::BigInt(deskripsi) => json!({
            "type": "bigint",
            "unserializableValue": deskripsi,
            "description": deskripsi,
        }),
    }
}

fn tab_hilang(tab: TabId) -> Galat {
    Galat::baru(-32001, format!("target '{}' tidak dikenal", tab.0))
}

fn frame_id(tab: TabId) -> String {
    format!("FRAME-{}", tab.0)
}

fn loader_id(tab: TabId) -> String {
    format!("LOADER-{}", tab.0)
}

fn bingkai(id: Value, hasil: Result<Value, Galat>) -> String {
    let mut map = Map::new();
    map.insert("id".to_owned(), id);
    match hasil {
        Ok(nilai) => {
            map.insert("result".to_owned(), nilai);
        }
        Err(galat) => {
            map.insert(
                "error".to_owned(),
                json!({ "code": galat.kode, "message": galat.pesan }),
            );
        }
    }
    Value::Object(map).to_string()
}

fn param_teks(params: &Value, nama: &str) -> Result<String, Galat> {
    params
        .get(nama)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Galat::baru(-32602, format!("parameter `{nama}` wajib berupa string")))
}

fn param_node_id(params: &Value) -> Result<usize, Galat> {
    let n = params
        .get("nodeId")
        .and_then(Value::as_i64)
        .ok_or_else(|| Galat::baru(-32602, "parameter `nodeId` wajib berupa angka"))?;
    usize::try_from(n).map_err(|_| Galat::baru(-32602, "`nodeId` tidak boleh negatif"))
}

/// Membaca `params.clip` menjadi potongan yang bisa dilukis.
///
/// `scale` ditolak kalau bukan 1: mesin ini belum menskalakan raster, dan
/// menerima lalu mengabaikannya akan mengirim gambar dengan ukuran yang tidak
/// diminta klien.
fn klip_dari(params: &Value) -> Result<Option<paint::Klip>, Galat> {
    let Some(k) = params.get("clip").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    if let Some(skala) = k.get("scale").and_then(Value::as_f64)
        && (skala - 1.0).abs() > f64::EPSILON
    {
        return Err(Galat::baru(
            -32602,
            format!("clip.scale {skala} belum didukung: raster hanya pada skala 1"),
        ));
    }
    // f64 -> f32 di sini aman untuk koordinat CSS: nilai di luar jangkauan f32
    // bukan koordinat halaman yang masuk akal.
    #[allow(clippy::cast_possible_truncation)]
    let angka = |nama: &str| -> Result<f32, Galat> {
        k.get(nama)
            .and_then(Value::as_f64)
            .map(|v| v as f32)
            .ok_or_else(|| Galat::baru(-32602, format!("clip.{nama} wajib berupa angka")))
    };
    let (x, y, width, height) = (angka("x")?, angka("y")?, angka("width")?, angka("height")?);
    if width <= 0.0 || height <= 0.0 {
        return Err(Galat::baru(
            -32602,
            "clip.width dan clip.height harus lebih besar dari nol",
        ));
    }
    Ok(Some(paint::Klip {
        x,
        y,
        width,
        height,
    }))
}

fn pool_galat(err: engine::native::PoolError) -> Galat {
    Galat::baru(-32000, format!("tab pool: {err}"))
}

/// Dokumen yang siap dipasang ke tab.
///
/// `url` adalah alamat **final**: setelah pengalihan, bukan yang diminta. Itu
/// yang dipakai `Page.getFrameTree` dan tanda terima, karena alamat akhir
/// adalah halaman yang benar-benar sedang dibaca.
pub struct DokumenAwal {
    pub url: String,
    pub html: String,
    /// Terisi hanya kalau dokumennya datang dari jaringan.
    pub jaringan: Option<Sumber>,
}

/// Mengubah satu URL menjadi dokumen, atau galat yang menyebut apa yang salah.
///
/// Empat skema yang dilayani: kosong/`about:blank`, `data:text/html`, `http`,
/// dan `https`. Dua yang terakhir memakai dokumen yang sudah diambil pemanggil
/// lewat [`super::jaringan`]; fungsi ini tidak pernah menunggu jaringan, karena
/// pemanggilnya memegang kunci state.
pub fn dokumen_awal(
    url: &str,
    ambilan: Option<Result<Ambilan, Galat>>,
) -> Result<DokumenAwal, Galat> {
    if url.is_empty() || url == "about:blank" {
        return Ok(DokumenAwal {
            url: url.to_owned(),
            html: String::new(),
            jaringan: None,
        });
    }
    if let Some(sisa) = url.strip_prefix("data:") {
        let (meta, isi) = sisa
            .split_once(',')
            .ok_or_else(|| Galat::baru(-32000, "data URL tanpa pemisah koma"))?;
        if !meta.starts_with("text/html") {
            return Err(Galat::baru(
                -32000,
                format!("data URL '{meta}' bukan text/html"),
            ));
        }
        return Ok(DokumenAwal {
            url: url.to_owned(),
            html: percent_decode(isi),
            jaringan: None,
        });
    }
    if super::jaringan::butuh_jaringan(url) {
        return match ambilan {
            Some(Ok(a)) => {
                let sumber = a.sumber();
                Ok(DokumenAwal {
                    url: a.url,
                    html: a.html,
                    jaringan: Some(sumber),
                })
            }
            Some(Err(galat)) => Err(galat),
            None => Err(Galat::baru(
                -32000,
                format!(
                    "'{url}' butuh pengambilan jaringan, tapi dokumennya belum diambil; \
                     panggil lewat jalur yang mengambilnya lebih dulu"
                ),
            )),
        };
    }
    let skema = url.split_once(':').map_or(url, |(skema, _)| skema);
    Err(Galat::baru(
        -32000,
        format!(
            "skema '{skema}' tidak didukung: mesin ini memuat `about:blank`, \
             `data:text/html`, `http`, dan `https`"
        ),
    ))
}

/// Mengurai escape persen pada URL `data:` dan query string.
#[must_use]
pub fn percent_decode(teks: &str) -> String {
    let bytes = teks.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Satu node DOM dalam bentuk `DOM.Node` milik protokol.
fn node_json(doc: &Document, id: NodeId, kedalaman: i64, level: i64) -> Value {
    let Some(n) = doc.node(id) else {
        return Value::Null;
    };
    let (tipe, nama, nilai) = match &n.kind {
        NodeKind::Document => (9, "#document".to_owned(), ""),
        NodeKind::Doctype => (10, "html".to_owned(), ""),
        NodeKind::Element(tag) => (1, tag.to_uppercase(), ""),
        NodeKind::Text(teks) => (3, "#text".to_owned(), teks.as_str()),
    };
    let mut map = Map::new();
    map.insert("nodeId".to_owned(), json!(id));
    map.insert("backendNodeId".to_owned(), json!(id));
    map.insert("nodeType".to_owned(), json!(tipe));
    map.insert("nodeName".to_owned(), json!(nama));
    map.insert("nodeValue".to_owned(), json!(nilai));
    if !n.attrs.is_empty() {
        let mut rata = Vec::with_capacity(n.attrs.len() * 2);
        for (k, v) in &n.attrs {
            rata.push(Value::String(k.clone()));
            rata.push(Value::String(v.clone()));
        }
        map.insert("attributes".to_owned(), Value::Array(rata));
    }
    map.insert("childNodeCount".to_owned(), json!(n.children.len()));
    let turun = kedalaman < 0 || level < kedalaman;
    if turun && !n.children.is_empty() {
        let anak: Vec<Value> = n
            .children
            .iter()
            .map(|c| node_json(doc, *c, kedalaman, level + 1))
            .collect();
        map.insert("children".to_owned(), Value::Array(anak));
    }
    Value::Object(map)
}
