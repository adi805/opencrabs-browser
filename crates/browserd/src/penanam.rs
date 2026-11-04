//! Penanam: menyuntikkan runtime mata-tangan ke setiap tab, sejak dokumen lahir.
//!
//! Kenapa harus sejak lahir: shadow root bermode `closed` hanya bisa direkam
//! pada detik ia dibuat. Tab yang dibuka lebih dulu daripada suntikan tetap bisa
//! dipakai, tapi isi shadow tertutupnya tidak akan pernah terlihat.
//!
//! Kenapa lewat koneksi sendiri, bukan lewat proxy: proxy meneruskan pesan klien
//! apa adanya. Kalau penanam menyisipkan perintah di jalur itu, sesi klien bisa
//! tercampur, dan frame yang tidak dikenal klien membingungkan pemakai. Satu
//! koneksi websocket terpisah ke mesin membuat penyuntikan tidak terlihat oleh
//! siapa pun yang sedang memakai browser.
//!
//! Sesi hasil `Target.attachToTarget` sengaja **tidak** dilepas. Registrasi
//! `Page.addScriptToEvaluateOnNewDocument` ikut mati saat sesi dilepas, jadi
//! melepasnya berarti suntikan hanya berlaku untuk satu dokumen dan navigasi
//! berikutnya kembali buta.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use engine::Slot;

use crate::runtime;

/// Batas menunggu satu balasan CDP. Mesin lokal menjawab dalam milidetik.
const BALAS_TIMEOUT: Duration = Duration::from_secs(10);

/// Jeda sebelum menyambung ulang. Mesin yang di-reap karena menganggur akan
/// dinyalakan lagi oleh permintaan berikutnya, jadi penanam harus tetap menunggu
/// tanpa membakar CPU.
const JEDA_SAMBUNG: Duration = Duration::from_secs(1);

/// Batas menunggu event saat tidak ada pekerjaan. Habis waktu bukan galat:
/// penanam kembali memeriksa apakah mesin masih hidup.
const JEDA_TUNGGU_EVENT: Duration = Duration::from_secs(5);

/// Menjalankan penanam untuk satu slot mesin sampai proses daemon berhenti.
///
/// Slot dipakai, bukan engine, karena mesin bisa dimatikan lalu dinyalakan lagi
/// dengan port baru di tengah umur daemon. Memegang engine berarti memegang
/// alamat mati.
pub fn mulai(slot: Arc<Slot>) {
    let nama = slot.id().to_string();
    tokio::spawn(async move {
        loop {
            let Some(engine) = slot.hidup() else {
                tokio::time::sleep(JEDA_SAMBUNG).await;
                continue;
            };
            let url = engine.web_socket_url().to_string();
            let port = engine.info().devtools_port;
            match jalankan(&url, &nama, port).await {
                Ok(()) => tracing::info!(mesin = %nama, "penanam berhenti: mesin menutup koneksi"),
                Err(err) => {
                    tracing::warn!(mesin = %nama, %err, "penanam terputus, menyambung ulang")
                }
            }
            tokio::time::sleep(JEDA_SAMBUNG).await;
        }
    });
}

/// Satu umur koneksi: sambung, minta daftar target, suntik, lalu menunggu.
async fn jalankan(url: &str, nama: &str, port: u16) -> Result<()> {
    let mut sesi = Sesi::sambung(url).await?;
    sesi.panggil(
        "Target.setDiscoverTargets",
        json!({ "discover": true }),
        None,
    )
    .await
    .context("mesin menolak Target.setDiscoverTargets")?;
    tracing::info!(
        mesin = %nama,
        port,
        versi = runtime::VERSI,
        "penanam siap: runtime mata-tangan disuntik ke setiap tab"
    );

    loop {
        if let Some(target) = sesi.antrean.pop_front() {
            if sesi.terpasang.contains_key(&target) {
                continue;
            }
            match sesi.tanam(&target).await {
                Ok(sesi_id) => {
                    sesi.terpasang.insert(target.clone(), sesi_id);
                    tracing::debug!(mesin = %nama, target = %target, "runtime disuntik");
                }
                // Satu target gagal tidak boleh menghentikan penanam: tab yang
                // sedang ditutup, atau target non-halaman, akan gagal di sini.
                Err(err) => tracing::warn!(
                    mesin = %nama,
                    target = %target,
                    %err,
                    "suntikan gagal, tab lain tetap dilayani"
                ),
            }
            continue;
        }
        sesi.tunggu_event().await?;
    }
}

pub struct Sesi {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    berikut: i64,
    /// Target yang menunggu disuntik, sesuai urutan kedatangan.
    antrean: VecDeque<String>,
    /// targetId -> sessionId yang masih hidup. Sesi tidak dilepas selama tab
    /// hidup, karena registrasi skrip ikut mati saat sesi dilepas.
    terpasang: HashMap<String, String>,
}

impl Sesi {
    pub async fn sambung(url: &str) -> Result<Self> {
        let (ws, _) = connect_async(url)
            .await
            .with_context(|| format!("websocket mesin tidak bisa dibuka: {url}"))?;
        Ok(Self {
            ws,
            berikut: 1,
            antrean: VecDeque::new(),
            terpasang: HashMap::new(),
        })
    }

    pub async fn panggil(
        &mut self,
        method: &str,
        params: Value,
        sesi: Option<&str>,
    ) -> Result<Value> {
        let id = self.berikut;
        self.berikut += 1;
        let pesan = match sesi {
            Some(sesi_id) => {
                json!({ "id": id, "method": method, "params": params, "sessionId": sesi_id })
            }
            None => json!({ "id": id, "method": method, "params": params }),
        };
        self.ws
            .send(Message::Text(pesan.to_string().into()))
            .await
            .with_context(|| format!("gagal mengirim {method}"))?;

        loop {
            let nilai = self.baca(method).await?;
            if nilai.get("id").and_then(Value::as_i64) == Some(id) {
                if let Some(galat) = nilai.get("error") {
                    bail!("{method} ditolak mesin: {galat}");
                }
                return Ok(nilai.get("result").cloned().unwrap_or(Value::Null));
            }
            // Event yang datang di sela menunggu balasan tetap diproses: target
            // baru bisa terlewat kalau dibuang di sini.
            self.serap(&nilai);
        }
    }

    async fn tunggu_event(&mut self) -> Result<()> {
        match tokio::time::timeout(JEDA_TUNGGU_EVENT, self.baca("event")).await {
            Ok(hasil) => {
                let nilai = hasil?;
                self.serap(&nilai);
                Ok(())
            }
            // Habis waktu: bukan galat, hanya tidak ada kabar.
            Err(_) => Ok(()),
        }
    }

    async fn baca(&mut self, konteks: &str) -> Result<Value> {
        loop {
            let pesan = tokio::time::timeout(BALAS_TIMEOUT, self.ws.next())
                .await
                .with_context(|| format!("{konteks} tidak dijawab dalam {BALAS_TIMEOUT:?}"))?;
            let Some(pesan) = pesan else {
                bail!("websocket mesin tertutup saat menunggu {konteks}");
            };
            let pesan =
                pesan.with_context(|| format!("websocket gagal saat menunggu {konteks}"))?;
            let teks = match pesan {
                Message::Text(teks) => teks.to_string(),
                Message::Binary(teks) => String::from_utf8(teks.to_vec())
                    .with_context(|| format!("pesan biner {konteks} bukan UTF-8"))?,
                Message::Close(frame) => bail!("mesin menutup websocket: {frame:?}"),
                _ => continue,
            };
            return serde_json::from_str(&teks)
                .with_context(|| format!("pesan {konteks} bukan JSON: {teks}"));
        }
    }

    /// Menyerap event yang relevan. Event lain diabaikan dengan sengaja: penanam
    /// tidak perlu tahu isi halaman, hanya kapan ada tab baru atau tab hilang.
    fn serap(&mut self, nilai: &Value) {
        let Some(method) = nilai.get("method").and_then(Value::as_str) else {
            return;
        };
        match method {
            "Target.targetCreated" => {
                let info = &nilai["params"]["targetInfo"];
                let jenis = info.get("type").and_then(Value::as_str).unwrap_or("");
                if jenis != "page" {
                    return;
                }
                if let Some(id) = info.get("targetId").and_then(Value::as_str)
                    && !self.antrean.iter().any(|sudah| sudah == id)
                {
                    self.antrean.push_back(id.to_string());
                }
            }
            "Target.targetDestroyed" => {
                if let Some(id) = nilai["params"].get("targetId").and_then(Value::as_str) {
                    self.terpasang.remove(id);
                }
            }
            "Target.detachedFromTarget" => {
                if let Some(id) = nilai["params"].get("targetId").and_then(Value::as_str) {
                    self.terpasang.remove(id);
                }
            }
            _ => {}
        }
    }

    /// Memasang runtime pada satu target dan mengembalikan sessionId yang harus
    /// dipertahankan.
    pub async fn tanam(&mut self, target: &str) -> Result<String> {
        let lampiran = self
            .panggil(
                "Target.attachToTarget",
                json!({ "targetId": target, "flatten": true }),
                None,
            )
            .await?;
        let sesi_id = lampiran
            .get("sessionId")
            .and_then(Value::as_str)
            .context("mesin tidak memberi sessionId untuk target")?
            .to_string();

        // Page.enable dibutuhkan sebagian versi Chromium sebelum skrip baru
        // diterima. Kegagalannya tidak fatal.
        let _ = self.panggil("Page.enable", json!({}), Some(&sesi_id)).await;

        self.panggil(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": runtime::SCRIPT }),
            Some(&sesi_id),
        )
        .await
        .context("Page.addScriptToEvaluateOnNewDocument ditolak")?;

        // Dokumen yang sudah termuat sebelum suntikan tetap diberi runtime,
        // supaya tab yang dibuka lebih dulu tidak butuh reload manual. Batasnya
        // jujur: shadow tertutup yang sudah dibuat tidak ikut terekam.
        let _ = self
            .panggil(
                "Runtime.evaluate",
                json!({ "expression": runtime::SCRIPT, "returnByValue": false }),
                Some(&sesi_id),
            )
            .await;

        Ok(sesi_id)
    }
}
