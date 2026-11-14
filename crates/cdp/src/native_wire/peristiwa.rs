//! Antrean peristiwa CDP yang dikirim mesin native ke klien.
//!
//! Protokol CDP punya dua arah: balasan ber-`id` untuk permintaan, dan
//! peristiwa tanpa `id` yang datang kapan saja. Soket mesin native di
//! [`super::layani`] menjawab satu frame per frame masuk, jadi peristiwa yang
//! lahir **di tengah** penanganan (skrip halaman memanggil `console.log`,
//! dokumen selesai dimuat, navigasi menyentuh jaringan) tidak bisa menumpang
//! balasan. Ia ditaruh di antrean ini dan dikuras setelah frame dijawab.
//!
//! Yang menentukan peristiwa boleh dikirim adalah `*.enable`, sama seperti
//! Chrome: klien yang tidak pernah memanggil `Runtime.enable` tidak akan
//! menerima `Runtime.consoleAPICalled`. Antrean menyimpan domain yang aktif
//! **per sesi**, karena satu soket browser bisa memegang banyak sesi dan
//! masing-masing mengaktifkan domainnya sendiri.
//!
//! Batas yang jujur: antrean ini sekali-kuras. Dua klien yang menyambung ke
//! soket berbeda tidak masing-masing menerima salinan; yang membaca lebih dulu
//! yang mendapat. Itu tercatat di [`engine::native::CELAH`], bukan disembunyikan.

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// Domain yang sudah diminta klien lewat `*.enable` pada satu sesi.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DomainAktif {
    pub runtime: bool,
    pub page: bool,
    pub network: bool,
}

impl DomainAktif {
    /// Menandai domain aktif dari nama metode `*.enable`/`*.disable`.
    ///
    /// Mengembalikan `true` kalau metodenya memang milik domain yang dikenal,
    /// supaya pemanggil tahu frame itu sudah ditangani di sini.
    pub fn ubah(&mut self, metode: &str, aktif: bool) -> bool {
        match metode {
            "Runtime.enable" | "Runtime.disable" => {
                self.runtime = aktif;
                true
            }
            "Page.enable" | "Page.disable" => {
                self.page = aktif;
                true
            }
            "Network.enable" | "Network.disable" => {
                self.network = aktif;
                true
            }
            _ => false,
        }
    }

    fn izinkan(&self, domain: Domain) -> bool {
        match domain {
            Domain::Runtime => self.runtime,
            Domain::Page => self.page,
            Domain::Network => self.network,
        }
    }
}

/// Domain asal sebuah peristiwa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Runtime,
    Page,
    Network,
}

#[derive(Debug)]
struct Tunggu {
    sesi: String,
    isi: Value,
}

/// Peristiwa yang menunggu dikirim, plus domain aktif tiap sesi.
#[derive(Debug, Default)]
pub struct Antrean {
    diaktifkan: BTreeMap<String, DomainAktif>,
    tunggu: Vec<Tunggu>,
}

impl Antrean {
    /// Mengubah status domain untuk satu sesi. `true` kalau metode itu milik
    /// domain yang dikenal (dan karenanya tidak perlu dijawab isi lain).
    pub fn ubah_domain(&mut self, sesi: &str, metode: &str, aktif: bool) -> bool {
        self.diaktifkan
            .entry(sesi.to_owned())
            .or_default()
            .ubah(metode, aktif)
    }

    /// Domain yang sedang aktif untuk satu sesi.
    #[must_use]
    pub fn domain(&self, sesi: &str) -> DomainAktif {
        self.diaktifkan.get(sesi).copied().unwrap_or_default()
    }

    /// Menaruh satu peristiwa di antrean **kalau** domainnya sudah aktif.
    ///
    /// Tidak ada peristiwa yang dikarang untuk klien yang tidak memintanya:
    /// mengirim `Runtime.consoleAPICalled` tanpa `Runtime.enable` akan membuat
    /// klien menerima lalu lintas yang tidak pernah ia langganan.
    pub fn kirim(&mut self, sesi: &str, domain: Domain, isi: Value) {
        if !self.domain(sesi).izinkan(domain) {
            return;
        }
        self.tunggu.push(Tunggu {
            sesi: sesi.to_owned(),
            isi,
        });
    }

    /// Menguras antrean jadi frame siap kirim.
    ///
    /// `sesi` adalah sesi milik soket yang meminta: `None` (soket browser)
    /// menerima semua, `Some(s)` (soket satu halaman) hanya miliknya sendiri.
    /// Setiap frame membawa `sessionId` supaya klien tahu peristiwa itu milik
    /// sesi mana, persis bentuk flat-session CDP.
    pub fn ambil(&mut self, sesi: Option<&str>) -> Vec<String> {
        let (diambil, sisa): (Vec<Tunggu>, Vec<Tunggu>) = std::mem::take(&mut self.tunggu)
            .into_iter()
            .partition(|p| sesi.is_none_or(|s| s == p.sesi));
        self.tunggu = sisa;
        diambil
            .into_iter()
            .map(|p| {
                let mut frame = p.isi;
                if let Some(obj) = frame.as_object_mut() {
                    obj.insert("sessionId".to_owned(), Value::String(p.sesi));
                }
                frame.to_string()
            })
            .collect()
    }

    /// Membuang state satu sesi: dipanggil saat sesi dilepas atau tabnya
    /// ditutup, supaya peristiwa tab mati tidak menumpuk selamanya.
    pub fn lupakan(&mut self, sesi: &str) {
        self.diaktifkan.remove(sesi);
        self.tunggu.retain(|p| p.sesi != sesi);
    }
}

/// `Runtime.executionContextCreated`.
///
/// `context_id` memakai nomor tab: stabil selama tab hidup, dan berbeda antar
/// tab, jadi klien bisa memakainya untuk memisahkan keluaran beberapa halaman
/// di satu soket. Chrome memakai nomor yang naik terus; yang penting di sini
/// hanya kestabilan dan keunikan.
#[must_use]
pub fn konteks_dibuat(sesi: &str, context_id: i64, url: &str) -> Value {
    json!({
        "method": "Runtime.executionContextCreated",
        "params": {
            "context": {
                "id": context_id,
                "origin": asal(url),
                "name": "",
                "uniqueId": format!("{sesi}-{context_id}"),
                "auxData": { "frameId": sesi, "isDefault": true },
            },
        },
    })
}

/// `Runtime.consoleAPICalled`.
///
/// Satu baris `console.log` jadi satu peristiwa dengan satu argumen string.
/// Mesin ini menyerialkan argumen di dalam isolat (`String(...)`), jadi tipe
/// aslinya sudah hilang sebelum sampai ke sini; mengirim `type: "string"`
/// adalah yang benar, mengirim `type: "object"` untuk sesuatu yang sudah jadi
/// teks akan mengarang bentuk yang tidak ada.
#[must_use]
pub fn console_dipanggil(baris: &str, context_id: i64, timestamp_ms: f64) -> Value {
    json!({
        "method": "Runtime.consoleAPICalled",
        "params": {
            "type": "log",
            "args": [{ "type": "string", "value": baris }],
            "executionContextId": context_id,
            "timestamp": timestamp_ms,
            "stackTrace": { "callFrames": [] },
        },
    })
}

/// `Page.loadEventFired`.
#[must_use]
pub fn muat_selesai(timestamp_ms: f64) -> Value {
    json!({
        "method": "Page.loadEventFired",
        "params": { "timestamp": timestamp_ms },
    })
}

/// `Network.requestWillBeSent`.
#[must_use]
pub fn permintaan_dikirim(request_id: &str, url: &str, timestamp_ms: f64) -> Value {
    json!({
        "method": "Network.requestWillBeSent",
        "params": {
            "requestId": request_id,
            "loaderId": request_id,
            "documentURL": url,
            "request": {
                "url": url,
                "method": "GET",
                "headers": {},
                "initialPriority": "VeryHigh",
            },
            "timestamp": timestamp_ms,
            "wallTime": timestamp_ms / 1000.0,
            "type": "Document",
            "frameId": request_id,
        },
    })
}

/// `Network.responseReceived`.
#[must_use]
pub fn jawaban_diterima(
    request_id: &str,
    url: &str,
    status: u16,
    mime: &str,
    timestamp_ms: f64,
) -> Value {
    json!({
        "method": "Network.responseReceived",
        "params": {
            "requestId": request_id,
            "loaderId": request_id,
            "timestamp": timestamp_ms,
            "type": "Document",
            "response": {
                "url": url,
                "status": status,
                "statusText": "",
                "mimeType": mime,
                "headers": {},
                "connectionReused": false,
                "encodedDataLength": 0,
                "fromDiskCache": false,
                "fromServiceWorker": false,
            },
        },
    })
}

/// Waktu sekarang dalam detik sejak epoch, bentuk yang dipakai `timestamp`
/// CDP (`MonotonicTime` di Chrome; mesin ini memakai jam dinding karena tidak
/// punya titik nol navigasi yang bisa dipertanggungjawabkan).
#[must_use]
pub fn sekarang() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Asal (origin) sebuah URL, untuk `executionContextCreated`.
///
/// `about:blank` dan URL tanpa skema tidak punya asal; Chrome mengirim string
/// kosong untuk keduanya, dan menebak `null` akan salah tipe.
fn asal(url: &str) -> String {
    let Some((skema, sisa)) = url.split_once("://") else {
        return String::new();
    };
    let otoritas = sisa.split(['/', '?', '#']).next().unwrap_or_default();
    if otoritas.is_empty() {
        return String::new();
    }
    format!("{skema}://{otoritas}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peristiwa_tidak_dikirim_tanpa_enable() {
        let mut antrean = Antrean::default();
        antrean.kirim("s1", Domain::Runtime, console_dipanggil("halo", 1, 0.0));
        assert!(
            antrean.ambil(None).is_empty(),
            "tanpa Runtime.enable tidak ada yang dikirim"
        );
    }

    #[test]
    fn enable_membuka_domainnya_saja() {
        let mut antrean = Antrean::default();
        assert!(antrean.ubah_domain("s1", "Runtime.enable", true));
        antrean.kirim("s1", Domain::Runtime, console_dipanggil("halo", 1, 0.0));
        antrean.kirim("s1", Domain::Page, muat_selesai(0.0));
        let frame = antrean.ambil(None);
        assert_eq!(frame.len(), 1, "hanya domain yang aktif: {frame:?}");
        assert!(frame[0].contains("Runtime.consoleAPICalled"));
    }

    #[test]
    fn disable_menutup_kembali() {
        let mut antrean = Antrean::default();
        antrean.ubah_domain("s1", "Runtime.enable", true);
        antrean.ubah_domain("s1", "Runtime.disable", false);
        antrean.kirim("s1", Domain::Runtime, console_dipanggil("halo", 1, 0.0));
        assert!(antrean.ambil(None).is_empty(), "disable menutup domain");
    }

    #[test]
    fn soket_halaman_hanya_menerima_sesinya() {
        let mut antrean = Antrean::default();
        antrean.ubah_domain("s1", "Runtime.enable", true);
        antrean.ubah_domain("s2", "Runtime.enable", true);
        antrean.kirim("s1", Domain::Runtime, console_dipanggil("satu", 1, 0.0));
        antrean.kirim("s2", Domain::Runtime, console_dipanggil("dua", 2, 0.0));
        let milik_s1 = antrean.ambil(Some("s1"));
        assert_eq!(milik_s1.len(), 1);
        assert!(milik_s1[0].contains("\"satu\""));
        let sisa = antrean.ambil(None);
        assert_eq!(sisa.len(), 1, "peristiwa s2 masih menunggu: {sisa:?}");
        assert!(sisa[0].contains("\"dua\""));
    }

    #[test]
    fn frame_membawa_session_id() {
        let mut antrean = Antrean::default();
        antrean.ubah_domain("sesi-uji", "Page.enable", true);
        antrean.kirim("sesi-uji", Domain::Page, muat_selesai(1.5));
        let frame: Value = serde_json::from_str(&antrean.ambil(None)[0]).expect("JSON");
        assert_eq!(frame["sessionId"], "sesi-uji");
        assert_eq!(frame["method"], "Page.loadEventFired");
        assert_eq!(frame["params"]["timestamp"], 1.5);
    }

    #[test]
    fn asal_diambil_dari_url() {
        assert_eq!(asal("https://contoh.id/a/b?c=1"), "https://contoh.id");
        assert_eq!(asal("http://127.0.0.1:8080/x"), "http://127.0.0.1:8080");
        assert_eq!(asal("about:blank"), "");
        assert_eq!(asal(""), "");
    }
}
