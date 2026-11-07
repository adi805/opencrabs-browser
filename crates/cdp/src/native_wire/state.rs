//! State mesin native: tab, sesi CDP, dan penerjemah perintah.
//!
//! Yang ada di sini adalah **sisi server** dari protokol: frame JSON-RPC masuk,
//! panggilan ke [`TabPool`] keluar, frame jawaban kembali. Tidak ada soket di
//! file ini, jadi seluruh perilakunya bisa diuji tanpa membuka port.
//!
//! Dua hal yang sengaja dijawab dengan galat alih-alih pura-pura bisa: metode
//! yang belum dibangun (lihat [`Galat::belum_ada`]) dan navigasi ke URL
//! jaringan, yang butuh klien HTTP yang belum ada di mesin ini.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard};

use engine::native::{Document, MemoryColdStore, NodeId, NodeKind, TabId, TabPool};
use serde_json::{Map, Value, json};

/// Batas jumlah tab yang bisa dialamatkan, dari PRD v0.2 (FR-005).
pub const MAKS_TAB: usize = 99;

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
            "Page.captureScreenshot" | "Page.captureSnapshot" | "Page.printToPDF" => {
                "rasterisasi bitmap belum ada; itu lapisan eyes (`tiny-skia`)"
            }
            "Input.dispatchMouseEvent"
            | "Input.dispatchKeyEvent"
            | "Input.insertText"
            | "Input.dispatchTouchEvent" => "pengiriman peristiwa DOM belum ada; itu lapisan hands",
            "DOM.querySelector"
            | "DOM.querySelectorAll"
            | "DOM.resolveNode"
            | "DOM.getOuterHTML"
            | "DOM.getBoxModel" => "selektor CSS belum ada; pencarian node hanya lewat atribut id",
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
    pool: TabPool<MemoryColdStore>,
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
        Self {
            dalam: Arc::new(Mutex::new(Dalam {
                pool: TabPool::new(MemoryColdStore::default(), MAKS_TAB),
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
    #[must_use]
    pub fn tangani(&self, teks: &str) -> Option<String> {
        self.kunci().tangani(teks, None)
    }

    /// Menjawab satu frame dari soket satu halaman, yang tidak membawa
    /// `sessionId` karena tabnya sudah ditentukan oleh URL soket.
    #[must_use]
    pub fn tangani_halaman(&self, teks: &str, sesi: &str) -> Option<String> {
        self.kunci().tangani(teks, Some(sesi))
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
        self.kunci().buat_target(url)
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
    fn tangani(&mut self, teks: &str, sesi_paksa: Option<&str>) -> Option<String> {
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
        let hasil = self.jalankan(&metode, &params, sesi.as_deref());
        // Notifikasi (tanpa `id`) tidak dijawab.
        let id = id?;
        Some(bingkai(id, hasil))
    }

    fn jalankan(
        &mut self,
        metode: &str,
        params: &Value,
        sesi: Option<&str>,
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
                Ok(json!({ "targetId": self.buat_target(&url)? }))
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
            "Page.navigate" => self.navigasi(params, sesi),
            "Page.reload" => {
                let tab = self.tab_dari_sesi(sesi)?;
                self.muat_ulang(tab)?;
                Ok(json!({}))
            }
            "Page.setDocumentContent" => self.set_konten(params, sesi),
            "Page.getFrameTree" => self.pohon_frame(sesi),
            "Page.bringToFront" => {
                let tab = self.tab_dari_sesi(sesi)?;
                self.aktifkan_tab(tab)?;
                Ok(json!({}))
            }
            "DOM.getDocument" => self.dokumen(params, sesi),
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

    fn buat_target(&mut self, url: &str) -> Result<String, Galat> {
        let html = dokumen_awal(url)?;
        let id = self.pool.buka(url, &html).map_err(pool_galat)?;
        let judul = self.pool.tab(id).and_then(|t| t.dokumen.judul());
        self.tabs.insert(
            id,
            MetaTab {
                url: url.to_owned(),
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
    fn navigasi(&mut self, params: &Value, sesi: Option<&str>) -> Result<Value, Galat> {
        let tab = self.tab_dari_sesi(sesi)?;
        let url = param_teks(params, "url")?;
        let html = dokumen_awal(&url)?;
        let sebelum = self.sidik(tab)?;
        self.pool
            .ganti_dokumen(tab, &url, &html)
            .map_err(pool_galat)?;
        let judul = self.pool.tab(tab).and_then(|t| t.dokumen.judul());
        if let Some(meta) = self.tabs.get_mut(&tab) {
            meta.url = url;
            meta.judul = judul;
        }
        let sesudah = self.sidik(tab)?;
        Ok(json!({
            "frameId": frame_id(tab),
            "loaderId": loader_id(tab),
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
                .eval(&ekspresi)
                .map_err(|err| Galat::baru(-32000, format!("skrip gagal: {err}")))?
        };
        let sesudah = self.sidik(tab)?;
        Ok(json!({
            // Nilai dijadikan string: tipe JS tidak dipertahankan. Itu disebut
            // di `engine::native::CELAH`, bukan disembunyikan.
            "result": { "type": "string", "value": keluaran, "description": keluaran },
            "receipt": banding(&sebelum, &sesudah),
        }))
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

fn pool_galat(err: engine::native::PoolError) -> Galat {
    Galat::baru(-32000, format!("tab pool: {err}"))
}

/// HTML awal untuk satu URL, atau galat yang menyebut apa yang belum ada.
///
/// Hanya `about:blank` dan `data:text/html` yang bisa dibuka tanpa tumpukan
/// jaringan. Skema lain ditolak dengan pesan yang menyebut jalan keluarnya,
/// bukan dengan halaman kosong yang membuat klien mengira navigasinya berhasil.
pub fn dokumen_awal(url: &str) -> Result<String, Galat> {
    if url.is_empty() || url == "about:blank" {
        return Ok(String::new());
    }
    let Some(sisa) = url.strip_prefix("data:") else {
        return Err(Galat::baru(
            -32000,
            format!(
                "mesin native belum punya klien HTTP, jadi '{url}' tidak bisa dimuat; \
                 buka `about:blank` lalu kirim dokumennya lewat `Page.setDocumentContent`"
            ),
        ));
    };
    let (meta, isi) = sisa
        .split_once(',')
        .ok_or_else(|| Galat::baru(-32000, "data URL tanpa pemisah koma"))?;
    if !meta.starts_with("text/html") {
        return Err(Galat::baru(
            -32000,
            format!("data URL '{meta}' bukan text/html"),
        ));
    }
    Ok(percent_decode(isi))
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
