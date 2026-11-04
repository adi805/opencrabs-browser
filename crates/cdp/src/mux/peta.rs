//! Peta identitas multiplex. Murni state: tanpa soket, tanpa async.
//!
//! Kenapa peta ini ada sama sekali, padahal proxy bisa saja meneruskan pesan
//! apa adanya: dua mesin punya ruang id sendiri-sendiri. Mesin A dan mesin B
//! sama-sama bisa memberi `sessionId` yang sama, dan `targetId` Chrome berupa
//! hex acak yang peluangnya bertabrakan kecil tetapi konsekuensinya besar (pesan
//! masuk ke tab yang salah). Klien hanya melihat **satu** soket, jadi dia harus
//! melihat satu ruang id.
//!
//! Yang diterjemahkan hanya identitas: `id`, `sessionId`, `targetId`,
//! `targetInfo.targetId`, `result.sessionId`, `result.targetId`. Field lain
//! diteruskan apa adanya.
//!
//! Id router yang kita buat sendiri berbentuk `s1`, `s2`, … dan `t1`, `t2`, ….
//! Itu bukan kosmetik: id mesin berupa hex 32 karakter, jadi tabrakan dengan id
//! router **mustahil** menurut bentuknya. Akibatnya id yang tidak kita kenal
//! bisa diteruskan tanpa risiko menutupi id router.
//!
//! Pemilihan mesin: `openCrabsOptions.engine` dari klien menang kalau ada, dan
//! tanpa itu aturan host dari `policy` yang menentukan (lihat `mesin_untuk_host`).
//! Aturan memilih **jenis** mesin, sedangkan peta ini memilih **instance**, jadi
//! jenis diterjemahkan lewat `jenis_id`. Tanpa aturan yang berlaku, pesannya
//! mendarat di mesin bawaan seperti sebelumnya.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::engines::{EnginePool, PEMISAH};
use crate::{PemilihMesin, host_dari_url};

/// Daemon menolak permintaan karena konfigurasi atau routing, bukan karena
/// mesin. Pesannya wajib menyebut apa yang tersedia supaya bisa diperbaiki.
pub const KODE_DITOLAK: i64 = -32000;

/// `sessionId` yang dikirim klien tidak ada di peta. Ini yang mencegah pesan
/// nyasar ke mesin yang salah saat pemetaan sesi bocor.
pub const KODE_SESI_TAK_DIKENAL: i64 = -32001;

/// Pilihan non-standar dari klien, dibuang sebelum diteruskan ke mesin.
///
/// Chromium mengabaikan parameter tak dikenal, tetapi meneruskannya berarti
/// daemon membocorkan detail internalnya ke mesin dan ke log mesin.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Opsi {
    pub engine: Option<String>,
    pub profile: Option<String>,
    pub ttl_secs: Option<u64>,
}

/// Satu pesan klien setelah diterjemahkan: ke mesin mana, dan isinya apa.
///
/// `Gabung` dan `Siar` mengembalikan beberapa pesan untuk satu pesan klien,
/// karena keduanya memang harus menyentuh semua mesin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Perintah {
    /// Satu pesan untuk satu mesin.
    Satu { mesin: String, payload: String },
    /// `Target.getTargets`: tanya semua mesin, gabungkan jadi satu balasan.
    Gabung {
        klien_id: i64,
        pesan: Vec<(String, String)>,
    },
    /// `Target.setDiscoverTargets`: siarkan, balasan pertama yang diteruskan.
    Siar {
        klien_id: i64,
        pesan: Vec<(String, String)>,
    },
}

/// Hasil terjemahan pesan dari mesin.
#[derive(Debug, Clone, PartialEq)]
pub enum Balasan {
    /// Kirim apa adanya ke klien.
    Teruskan(String),
    /// Satu potong hasil `Target.getTargets`; pompa yang menggabungkan.
    Gabung { klien_id: i64, hasil: Value },
    /// Jangan kirim: ini balasan siaran yang bukan pertama.
    Telan,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MuxError {
    #[error("pesan bukan objek JSON")]
    BukanObjek,

    #[error("sessionId '{0}' tidak dikenal di koneksi ini")]
    SesiTakDikenal(String),

    #[error("targetId '{0}' tidak dikenal di koneksi ini")]
    TargetTakDikenal(String),

    #[error("mesin '{diminta}' tidak terpasang; yang ada: {tersedia}")]
    MesinTakAda { diminta: String, tersedia: String },

    /// Aturan host menolak permintaan ini. Dipisah dari [`Self::MesinTakAda`]
    /// bukan soal kode galat (keduanya `KODE_DITOLAK`) melainkan soal sebab:
    /// yang harus diperbaiki ada di `[policy]`, bukan di nama mesin yang dikirim
    /// klien.
    #[error("routing host ditolak: {0}")]
    Routing(String),
}

/// State satu koneksi klien.
///
/// Satu `Peta` per koneksi, tidak dibagi antar klien. Itu yang membuat id
/// klien tidak perlu digeser: dua klien boleh sama-sama memakai id 5 karena
/// masing-masing punya soket sendiri.
pub struct Peta {
    bawaan: String,
    semua: Vec<String>,
    /// Aturan host ke mesin, kalau daemon menyambungkannya. `None` berarti
    /// perilaku lama: semuanya mendarat di mesin bawaan.
    pemilih: Option<Arc<dyn PemilihMesin>>,
    /// Jenis mesin -> id instance pertama jenis itu, urut id. Ini yang
    /// menerjemahkan keputusan `policy` (yang memilih **jenis**) jadi mesin yang
    /// benar-benar ada (instance).
    jenis_id: BTreeMap<String, String>,
    /// Id klien ke id mesin, per mesin. Kuncinya `(mesin, id_klien)` supaya satu
    /// id klien yang disiarkan ke semua mesin tetap dapat id berbeda di tiap
    /// mesin.
    naik: HashMap<(String, i64), i64>,
    turun: HashMap<(String, i64), i64>,
    penghitung_naik: BTreeMap<String, i64>,
    /// `s1` -> (mesin, sessionId mesin).
    sesi: HashMap<String, (String, String)>,
    sesi_balik: HashMap<(String, String), String>,
    /// `s1` -> `t1`, dipakai saat melepas mesin: klien butuh targetId di
    /// `Target.detachedFromTarget` untuk tahu tab mana yang hilang.
    sesi_target: HashMap<String, String>,
    /// `t1` -> (mesin, targetId mesin).
    target: HashMap<String, (String, String)>,
    target_balik: HashMap<(String, String), String>,
    penghitung_sesi: u64,
    penghitung_target: u64,
    /// `(mesin, id mesin)` -> `t1` untuk balasan attach yang belum datang.
    tunggu_attach: HashMap<(String, i64), String>,
}

impl Peta {
    #[must_use]
    pub fn baru(pool: &EnginePool, pemilih: Option<Arc<dyn PemilihMesin>>) -> Self {
        // Jenis -> instance pertama, urut id. Aturannya sengaja sama dengan
        // `EnginePool::id_untuk_jenis`: dua jalur yang memilih instance berbeda
        // untuk jenis yang sama adalah bug yang sulit dilihat.
        let mut jenis_id: BTreeMap<String, String> = BTreeMap::new();
        for (id, slot) in pool.slots() {
            jenis_id
                .entry(slot.kind().to_owned())
                .or_insert_with(|| id.clone());
        }
        Self {
            bawaan: pool.default_id().to_owned(),
            semua: pool.ids().cloned().collect(),
            pemilih,
            jenis_id,
            naik: HashMap::new(),
            turun: HashMap::new(),
            penghitung_naik: BTreeMap::new(),
            sesi: HashMap::new(),
            sesi_balik: HashMap::new(),
            sesi_target: HashMap::new(),
            target: HashMap::new(),
            target_balik: HashMap::new(),
            penghitung_sesi: 0,
            penghitung_target: 0,
            tunggu_attach: HashMap::new(),
        }
    }

    #[must_use]
    pub fn bawaan(&self) -> &str {
        &self.bawaan
    }

    #[must_use]
    pub fn mesin(&self) -> &[String] {
        &self.semua
    }

    /// Jumlah sesi hidup per mesin, untuk log dan diagnosa.
    #[must_use]
    pub fn sesi_per_mesin(&self) -> BTreeMap<String, usize> {
        let mut hasil = BTreeMap::new();
        for (mesin, _) in self.sesi.values() {
            *hasil.entry(mesin.clone()).or_insert(0) += 1;
        }
        hasil
    }

    /// Menerjemahkan satu pesan klien jadi satu atau beberapa pesan untuk mesin.
    ///
    /// Urutan pemeriksaan adalah urutan kekhususan: `sessionId` paling spesifik,
    /// lalu `targetId`, lalu `Target.createTarget` (satu-satunya pesan yang
    /// boleh memilih mesin), lalu dua method lintas-mesin, lalu sisanya ke mesin
    /// bawaan.
    pub fn terjemahkan_klien(&mut self, payload: &str) -> Result<Perintah, MuxError> {
        let mut nilai: Value = serde_json::from_str(payload).map_err(|_| MuxError::BukanObjek)?;
        if !nilai.is_object() {
            return Err(MuxError::BukanObjek);
        }
        // Semua field dibaca lewat borrow pendek. `obj` tidak dipegang
        // sepanjang fungsi supaya cabang-cabang di bawah bebas mengubah nilai.
        let opsi = nilai.as_object_mut().map(ambil_opsi).unwrap_or_default();
        let (id, method) = {
            let obj = nilai.as_object().expect("sudah dipastikan objek");
            (
                obj.get("id").and_then(Value::as_i64),
                obj.get("method").and_then(Value::as_str).map(str::to_owned),
            )
        };

        // `sessionId` menang atas apa pun: pesan milik sesi hanya boleh sampai
        // ke mesin yang memiliki sesi itu.
        if let Some(sid) = nilai
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let Some((mesin, upstream)) = self.sesi.get(&sid).cloned() else {
                return Err(MuxError::SesiTakDikenal(sid));
            };
            if let Some(obj) = nilai.as_object_mut() {
                obj.insert("sessionId".into(), Value::String(upstream));
                self.pasang_id(obj, &mesin, id);
            }
            return Ok(Perintah::Satu {
                mesin,
                payload: baku(&nilai)?,
            });
        }

        // `Target.createTarget` adalah satu-satunya pesan yang memilih mesin.
        if method.as_deref() == Some("Target.createTarget") {
            // Tanpa `openCrabsOptions.engine`, aturan `[policy.rules]` yang
            // menentukan mesinnya. Host dibaca dari `params.url`, satu-satunya
            // tempat host muncul di pesan ini.
            let host = nilai
                .get("params")
                .and_then(|params| params.get("url"))
                .and_then(Value::as_str)
                .and_then(host_dari_url);
            let mesin = match opsi.engine.as_deref() {
                Some(diminta) => self.pilih_mesin(Some(diminta))?,
                None => self.mesin_untuk_host(host.as_deref())?,
            };
            if let Some(obj) = nilai.as_object_mut() {
                self.pasang_id(obj, &mesin, id);
            }
            return Ok(Perintah::Satu {
                mesin,
                payload: baku(&nilai)?,
            });
        }

        // Method lintas-mesin. Keduanya hanya berarti tanpa `sessionId`; kalau
        // ada sesi, cabang di atas sudah menangkapnya.
        if let Some(klien_id) = id
            && matches!(
                method.as_deref(),
                Some("Target.getTargets") | Some("Target.setDiscoverTargets")
            )
        {
            let get_targets = method.as_deref() == Some("Target.getTargets");
            let mut pesan = Vec::with_capacity(self.semua.len());
            for mesin in self.semua.clone() {
                let uid = self.id_masuk(&mesin, klien_id);
                let mut salinan = nilai.clone();
                if let Some(o) = salinan.as_object_mut() {
                    o.insert("id".into(), Value::from(uid));
                }
                pesan.push((mesin, baku(&salinan)?));
            }
            return Ok(if get_targets {
                Perintah::Gabung { klien_id, pesan }
            } else {
                Perintah::Siar { klien_id, pesan }
            });
        }

        // `params.targetId` menentukan mesin untuk method yang bekerja pada
        // satu target, mis. `Target.attachToTarget` dan `Target.closeTarget`.
        if let Some(tid) = nilai
            .get("params")
            .and_then(|params| params.get("targetId"))
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let (mesin, upstream) = if let Some(pasangan) = self.target.get(&tid).cloned() {
                pasangan
            } else if let Some((mesin, sisa)) = tid.split_once(PEMISAH)
                && self.semua.iter().any(|m| m == mesin)
                && !sisa.is_empty()
            {
                // Bentuk `<mesin>~<id>` datang dari `/json/list`, bukan dari
                // peta: klien boleh memakai id itu tanpa pernah melihatnya di
                // event CDP.
                (mesin.to_owned(), sisa.to_owned())
            } else {
                return Err(MuxError::TargetTakDikenal(tid));
            };
            let uid = id.map(|klien_id| self.id_masuk(&mesin, klien_id));
            if let Some(obj) = nilai.as_object_mut() {
                if let Some(Value::Object(params)) = obj.get_mut("params") {
                    params.insert("targetId".into(), Value::String(upstream));
                }
                self.pasang_id(obj, &mesin, id);
            }
            if method.as_deref() == Some("Target.attachToTarget")
                && let Some(uid) = uid
            {
                self.tunggu_attach.insert((mesin.clone(), uid), tid);
            }
            return Ok(Perintah::Satu {
                mesin,
                payload: baku(&nilai)?,
            });
        }

        let mesin = self.bawaan.clone();
        if let Some(obj) = nilai.as_object_mut() {
            self.pasang_id(obj, &mesin, id);
        }
        Ok(Perintah::Satu {
            mesin,
            payload: baku(&nilai)?,
        })
    }

    /// Menerjemahkan satu pesan dari mesin kembali ke ruang id klien.
    ///
    /// Tidak pernah gagal: pesan mesin yang tidak bisa dipahami lebih berguna
    /// diteruskan (klien akan mengabaikan yang tidak relevan) daripada ditelan
    /// diam-diam. Yang tidak boleh terjadi adalah pesan **hilang** tanpa jejak.
    pub fn terjemahkan_mesin(&mut self, mesin: &str, payload: &str) -> Balasan {
        let Ok(mut nilai) = serde_json::from_str::<Value>(payload) else {
            return Balasan::Teruskan(payload.to_owned());
        };
        let Value::Object(ref mut obj) = nilai else {
            return Balasan::Teruskan(payload.to_owned());
        };

        if let Some(uid) = obj.get("id").and_then(Value::as_i64) {
            let Some(klien_id) = self.turun.get(&(mesin.to_owned(), uid)).copied() else {
                // Id yang tidak pernah kita kirim: bukan percakapan kita.
                return Balasan::Telan;
            };
            obj.insert("id".into(), Value::from(klien_id));

            // Balasan attach membawa sessionId pertama kali. Di sinilah peta
            // sesi terbentuk, karena permintaan attach hanya membawa targetId.
            if let Some(Value::Object(hasil)) = obj.get_mut("result")
                && let Some(Value::String(upstream)) = hasil.get("sessionId").cloned()
            {
                let router = self.sesi_baru(mesin, &upstream);
                if let Some(tid) = self.tunggu_attach.remove(&(mesin.to_owned(), uid)) {
                    self.sesi_target.insert(router.clone(), tid);
                }
                hasil.insert("sessionId".into(), Value::String(router));
            }

            // Balasan createTarget memperkenalkan target baru ke klien.
            if let Some(Value::Object(hasil)) = obj.get_mut("result")
                && let Some(Value::String(upstream)) = hasil.get("targetId").cloned()
            {
                let router = self.target_baru(mesin, &upstream);
                hasil.insert("targetId".into(), Value::String(router));
            }

            if let Some(Value::Object(hasil)) = obj.get_mut("result")
                && let Some(Value::Array(items)) = hasil.get_mut("targetInfos")
            {
                for item in items.iter_mut() {
                    tulis_target(item, &mut |upstream| self.target_baru(mesin, upstream));
                }
            }

            // `Target.getTargets` dijawab banyak mesin; pompa menggabung.
            let ada_target_infos = obj
                .get("result")
                .and_then(|hasil| hasil.get("targetInfos"))
                .is_some();
            if ada_target_infos && self.semua.len() > 1 {
                let hasil = obj.get("result").cloned().unwrap_or(Value::Null);
                return Balasan::Gabung { klien_id, hasil };
            }

            return Balasan::Teruskan(baku(&nilai).unwrap_or_else(|_| payload.to_owned()));
        }

        // Event: `sessionId` dulu supaya sesi baru (auto-attach) ikut terpetakan,
        // lalu target di dalam `targetInfo`.
        if let Some(upstream) = obj
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let router = self.sesi_untuk(mesin, &upstream);
            obj.insert("sessionId".into(), Value::String(router));
        }
        if let Some(method) = obj.get("method").and_then(Value::as_str)
            && method == "Target.detachedFromTarget"
            && let Some(Value::Object(params)) = obj.get_mut("params")
            && let Some(Value::String(upstream)) = params.get("sessionId").cloned()
            && let Some(router) = self
                .sesi_balik
                .get(&(mesin.to_owned(), upstream.clone()))
                .cloned()
        {
            self.sesi.remove(&router);
            self.sesi_balik.remove(&(mesin.to_owned(), upstream));
            self.sesi_target.remove(&router);
        }
        if let Some(Value::Object(params)) = obj.get_mut("params") {
            if let Some(Value::Object(info)) = params.get_mut("targetInfo") {
                tulis_target_di(info, &mut |upstream| self.target_baru(mesin, upstream));
            }
            if let Some(Value::String(upstream)) = params.get("targetId").cloned() {
                let router = self
                    .target_balik
                    .get(&(mesin.to_owned(), upstream.clone()))
                    .cloned()
                    .unwrap_or_else(|| self.target_baru(mesin, &upstream));
                params.insert("targetId".into(), Value::String(router));
            }
        }

        Balasan::Teruskan(baku(&nilai).unwrap_or_else(|_| payload.to_owned()))
    }

    /// Melepas semua jejak satu mesin, dan mengembalikan pesan
    /// `Target.detachedFromTarget` yang harus dikirim ke klien.
    ///
    /// Inilah yang membuat mesin mati tidak menghapus tab mesin lain di mata
    /// klien: yang dikirim hanya sesi milik mesin yang pergi, masing-masing
    /// dengan targetId-nya sendiri.
    pub fn lepas_mesin(&mut self, mesin: &str) -> Vec<String> {
        let mut pergi: Vec<(String, Option<String>)> = self
            .sesi
            .iter()
            .filter(|(_, (m, _))| m == mesin)
            .map(|(router, _)| {
                let tid = self.sesi_target.get(router).cloned();
                (router.clone(), tid)
            })
            .collect();
        pergi.sort_by(|a, b| a.0.cmp(&b.0));

        let mut pesan = Vec::with_capacity(pergi.len());
        for (router, tid) in &pergi {
            let Some((_, upstream)) = self.sesi.remove(router) else {
                continue;
            };
            self.sesi_balik.remove(&(mesin.to_owned(), upstream));
            self.sesi_target.remove(router);
            let mut params = Map::new();
            params.insert("sessionId".into(), Value::String(router.clone()));
            if let Some(tid) = tid {
                params.insert("targetId".into(), Value::String(tid.clone()));
            }
            let mut isi = Map::new();
            isi.insert(
                "method".into(),
                Value::String("Target.detachedFromTarget".into()),
            );
            isi.insert("params".into(), Value::Object(params));
            if let Ok(teks) = serde_json::to_string(&Value::Object(isi)) {
                pesan.push(teks);
            }
        }

        let target_pergi: Vec<String> = self
            .target
            .iter()
            .filter(|(_, (m, _))| m == mesin)
            .map(|(router, _)| router.clone())
            .collect();
        for router in target_pergi {
            if let Some((_, upstream)) = self.target.remove(&router) {
                self.target_balik.remove(&(mesin.to_owned(), upstream));
            }
        }
        self.tunggu_attach.retain(|(m, _), _| m != mesin);
        self.naik.retain(|(m, _), _| m != mesin);
        self.turun.retain(|(m, _), _| m != mesin);
        self.penghitung_naik.remove(mesin);

        pesan
    }

    // ── pembantu ────────────────────────────────────────────────────────────

    /// Mesin untuk sebuah host menurut aturan `policy`, atau mesin bawaan kalau
    /// tidak ada aturan yang berlaku.
    ///
    /// Aturan yang **menolak** tidak diturunkan jadi mesin bawaan. Penolakan
    /// biasanya berarti config menunjuk mesin yang tidak terpasang, dan
    /// menggantinya diam-diam membuat tab muncul di mesin yang salah, jauh dari
    /// penyebabnya.
    fn mesin_untuk_host(&self, host: Option<&str>) -> Result<String, MuxError> {
        let (Some(pemilih), Some(host)) = (self.pemilih.as_ref(), host) else {
            return Ok(self.bawaan.clone());
        };
        match pemilih.jenis_untuk_host(host) {
            Ok(None) => Ok(self.bawaan.clone()),
            Ok(Some(keputusan)) => self.jenis_id.get(&keputusan.jenis).cloned().ok_or_else(|| {
                MuxError::Routing(format!(
                    "aturan {} memilih jenis {} tapi tidak ada instance jenis itu terpasang; \
                         yang ada: {}",
                    keputusan.aturan,
                    keputusan.jenis,
                    self.semua.join(", ")
                ))
            }),
            Err(pesan) => Err(MuxError::Routing(pesan)),
        }
    }

    fn pilih_mesin(&self, diminta: Option<&str>) -> Result<String, MuxError> {
        match diminta {
            None => Ok(self.bawaan.clone()),
            Some(mesin) if self.semua.iter().any(|m| m == mesin) => Ok(mesin.to_owned()),
            Some(mesin) => Err(MuxError::MesinTakAda {
                diminta: mesin.to_owned(),
                tersedia: self.semua.join(", "),
            }),
        }
    }

    fn id_masuk(&mut self, mesin: &str, klien_id: i64) -> i64 {
        if let Some(uid) = self.naik.get(&(mesin.to_owned(), klien_id)) {
            return *uid;
        }
        let penghitung = self.penghitung_naik.entry(mesin.to_owned()).or_insert(0);
        *penghitung += 1;
        let uid = *penghitung;
        self.naik.insert((mesin.to_owned(), klien_id), uid);
        self.turun.insert((mesin.to_owned(), uid), klien_id);
        uid
    }

    fn pasang_id(&mut self, obj: &mut Map<String, Value>, mesin: &str, id: Option<i64>) {
        if let Some(klien_id) = id {
            let uid = self.id_masuk(mesin, klien_id);
            obj.insert("id".into(), Value::from(uid));
        }
    }

    fn sesi_baru(&mut self, mesin: &str, upstream: &str) -> String {
        if let Some(router) = self
            .sesi_balik
            .get(&(mesin.to_owned(), upstream.to_owned()))
        {
            return router.clone();
        }
        self.penghitung_sesi += 1;
        let router = format!("s{}", self.penghitung_sesi);
        self.sesi
            .insert(router.clone(), (mesin.to_owned(), upstream.to_owned()));
        self.sesi_balik
            .insert((mesin.to_owned(), upstream.to_owned()), router.clone());
        router
    }

    fn sesi_untuk(&mut self, mesin: &str, upstream: &str) -> String {
        self.sesi_baru(mesin, upstream)
    }

    fn target_baru(&mut self, mesin: &str, upstream: &str) -> String {
        if let Some(router) = self
            .target_balik
            .get(&(mesin.to_owned(), upstream.to_owned()))
        {
            return router.clone();
        }
        self.penghitung_target += 1;
        let router = format!("t{}", self.penghitung_target);
        self.target
            .insert(router.clone(), (mesin.to_owned(), upstream.to_owned()));
        self.target_balik
            .insert((mesin.to_owned(), upstream.to_owned()), router.clone());
        router
    }
}

/// Membuang `params.openCrabsOptions` dan mengembalikan isinya.
fn ambil_opsi(obj: &mut Map<String, Value>) -> Opsi {
    let Some(Value::Object(params)) = obj.get_mut("params") else {
        return Opsi::default();
    };
    let Some(Value::Object(isi)) = params.remove("openCrabsOptions") else {
        return Opsi::default();
    };
    Opsi {
        engine: isi.get("engine").and_then(Value::as_str).map(str::to_owned),
        profile: isi
            .get("profile")
            .and_then(Value::as_str)
            .map(str::to_owned),
        ttl_secs: isi.get("ttlSecs").and_then(Value::as_u64),
    }
}

fn tulis_target(item: &mut Value, f: &mut impl FnMut(&str) -> String) {
    if let Value::Object(map) = item {
        tulis_target_di(map, f);
    }
}

fn tulis_target_di(map: &mut Map<String, Value>, f: &mut impl FnMut(&str) -> String) {
    if let Some(Value::String(upstream)) = map.get("targetId").cloned() {
        map.insert("targetId".into(), Value::String(f(&upstream)));
    }
}

fn baku(nilai: &Value) -> Result<String, MuxError> {
    serde_json::to_string(nilai).map_err(|_| MuxError::BukanObjek)
}

#[cfg(test)]
mod uji;
