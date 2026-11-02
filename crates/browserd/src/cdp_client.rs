//! Klien CDP sekecil mungkin: cukup untuk memindahkan cookie.
//!
//! Daemon punya proxy sendiri (`crates/cdp`), tapi proxy itu meneruskan pesan
//! klien lain dan tidak pernah bicara ke mesin sebagai klien. Untuk menyimpan
//! cookie, daemon justru harus jadi klien: dial ke websocket mesin, kirim satu
//! perintah, tunggu balasan dengan id yang sama.
//!
//! Kenapa tidak memakai HTTP `/json` saja: endpoint itu tidak punya perintah
//! cookie. Cookie hanya ada di domain `Storage` lewat websocket.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

/// Batas menunggu satu balasan. Mesin lokal menjawab dalam milidetik; 10 detik
/// sudah berarti ada yang salah dan menunggu lebih lama hanya menutupi sebabnya.
const BALAS_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Client {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: i64,
}

impl Client {
    pub async fn connect(url: &str) -> Result<Self> {
        let (ws, _respon) = connect_async(url)
            .await
            .with_context(|| format!("websocket mesin tidak bisa dibuka: {url}"))?;
        Ok(Self { ws, next_id: 1 })
    }

    /// Memanggil satu perintah CDP di tingkat browser (tanpa sesi).
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.call_with(None, method, params).await
    }

    /// Memanggil satu perintah CDP di dalam satu sesi target.
    ///
    /// Perintah yang menyentuh halaman (`Input.*`, `Runtime.*`, `Page.*`) harus
    /// lewat sesi: dikirim tanpa `sessionId`, mesin menjawab "not attached to
    /// an active target", dan itu galat yang membingungkan kalau tidak tahu.
    pub async fn call_in(
        &mut self,
        session_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        self.call_with(Some(session_id), method, params).await
    }

    /// Memanggil satu perintah CDP dan mengembalikan `result`-nya.
    ///
    /// Event (pesan tanpa `id`) dilewati, bukan dianggap galat: mesin mengirim
    /// `Target.targetCreated` dan kawan-kawan kapan saja, termasuk di sela
    /// menunggu balasan.
    async fn call_with(
        &mut self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let pesan = match session_id {
            Some(session_id) => json!({
                "id": id,
                "method": method,
                "params": params,
                "sessionId": session_id,
            }),
            None => json!({ "id": id, "method": method, "params": params }),
        };
        self.ws
            .send(Message::Text(pesan.to_string().into()))
            .await
            .with_context(|| format!("gagal mengirim {method}"))?;

        loop {
            let balasan = tokio::time::timeout(BALAS_TIMEOUT, self.ws.next())
                .await
                .with_context(|| format!("{method} tidak dijawab dalam {BALAS_TIMEOUT:?}"))?;

            let Some(balasan) = balasan else {
                bail!("websocket mesin tertutup saat menunggu balasan {method}");
            };
            let balasan =
                balasan.with_context(|| format!("websocket gagal saat menunggu {method}"))?;

            let teks = match balasan {
                Message::Text(teks) => teks.to_string(),
                Message::Binary(teks) => String::from_utf8(teks.to_vec())
                    .with_context(|| format!("balasan biner {method} bukan UTF-8"))?,
                Message::Close(frame) => bail!("mesin menutup websocket: {frame:?}"),
                _ => continue,
            };

            let nilai: Value = serde_json::from_str(&teks)
                .with_context(|| format!("balasan {method} bukan JSON: {teks}"))?;

            // Event: tidak punya id. Bukan jawaban kita.
            if nilai.get("id").is_none() {
                continue;
            }
            if nilai["id"].as_i64() != Some(id) {
                // Balasan untuk panggilan lain pada koneksi ini tidak mungkin
                // terjadi (satu koneksi satu penelepon), jadi ini lebih baik
                // dilaporkan daripada diabaikan.
                bail!("balasan {method} ber-id lain: {nilai}");
            }

            if let Some(galat) = nilai.get("error") {
                bail!("{method} ditolak mesin: {galat}");
            }
            return Ok(nilai.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Seluruh cookie yang dipegang mesin, sudah dipetakan ke bentuk store.
    pub async fn cookies(&mut self) -> Result<Vec<store::Cookie>> {
        let result = self.call("Storage.getCookies", json!({})).await?;
        let daftar = result
            .get("cookies")
            .and_then(Value::as_array)
            .context("Storage.getCookies tidak mengembalikan array cookies")?;

        let mut hasil = Vec::with_capacity(daftar.len());
        for satu in daftar {
            match dari_cdp(satu) {
                Some(cookie) => hasil.push(cookie),
                None => {
                    // Cookie tanpa domain/nama tidak bisa disimpan. Dilewati
                    // diam-diam di sini akan menyesatkan, jadi dicatat.
                    tracing::debug!(cookie = %satu, "cookie dilewati: domain/nama kosong");
                }
            }
        }
        Ok(hasil)
    }

    /// Menanam cookie ke mesin. Mengembalikan jumlah yang dikirim.
    pub async fn set_cookies(&mut self, cookies: &[store::Cookie]) -> Result<usize> {
        if cookies.is_empty() {
            return Ok(0);
        }
        let daftar: Vec<Value> = cookies.iter().map(ke_cdp).collect();
        self.call("Storage.setCookies", json!({ "cookies": daftar }))
            .await?;
        Ok(cookies.len())
    }
}

/// Cookie CDP menjadi cookie store. `None` kalau tidak ada yang bisa disimpan.
#[must_use]
pub fn dari_cdp(nilai: &Value) -> Option<store::Cookie> {
    let host = nilai.get("domain").and_then(Value::as_str)?.to_owned();
    let name = nilai.get("name").and_then(Value::as_str)?.to_owned();
    if host.is_empty() || name.is_empty() {
        return None;
    }

    // `expires` di CDP berupa f64 dan `-1` berarti cookie sesi. Dipetakan ke
    // detik bulat karena yang disimpan store juga detik.
    let expires = nilai
        .get("expires")
        .and_then(Value::as_f64)
        .filter(|detik| *detik > 0.0)
        .map(|detik| detik as i64);

    Some(store::Cookie {
        host,
        name,
        path: nilai
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("/")
            .to_owned(),
        value: nilai
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        expires,
        secure: nilai
            .get("secure")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        http_only: nilai
            .get("httpOnly")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        samesite: nilai
            .get("sameSite")
            .and_then(Value::as_str)
            .map(|teks| match teks {
                "None" => 0,
                "Strict" => 2,
                _ => 1,
            }),
    })
}

/// Cookie store menjadi bentuk yang diterima `Storage.setCookies`.
#[must_use]
pub fn ke_cdp(cookie: &store::Cookie) -> Value {
    let same_site = match cookie.samesite {
        Some(0) => Some("None"),
        Some(2) => Some("Strict"),
        Some(1) => Some("Lax"),
        _ => None,
    };

    let mut nilai = json!({
        "name": cookie.name,
        "value": cookie.value,
        "domain": cookie.host,
        "path": cookie.path,
        "secure": cookie.secure,
        "httpOnly": cookie.http_only,
    });

    if let Some(expires) = cookie.expires {
        nilai["expires"] = json!(expires);
    }
    if let Some(same_site) = same_site {
        nilai["sameSite"] = json!(same_site);
    }
    nilai
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_cdp_dipetakan_utuh() {
        let cdp = json!({
            "name": "cf_clearance",
            "value": "abc123",
            "domain": ".shop.example",
            "path": "/toko",
            "expires": 1_900_000_000.5,
            "size": 20,
            "httpOnly": true,
            "secure": true,
            "session": false,
            "sameSite": "Strict"
        });
        let cookie = dari_cdp(&cdp).expect("cookie sah");
        assert_eq!(cookie.host, ".shop.example");
        assert_eq!(cookie.name, "cf_clearance");
        assert_eq!(cookie.path, "/toko");
        assert_eq!(cookie.value, "abc123");
        assert_eq!(cookie.expires, Some(1_900_000_000));
        assert!(cookie.secure);
        assert!(cookie.http_only);
        assert_eq!(cookie.samesite, Some(2));
    }

    #[test]
    fn cookie_sesi_tidak_punya_expires() {
        let cdp = json!({
            "name": "sesi",
            "value": "v",
            "domain": "uji.example",
            "path": "/",
            "expires": -1.0,
            "session": true
        });
        let cookie = dari_cdp(&cdp).expect("cookie sesi");
        assert_eq!(cookie.expires, None);
        assert_eq!(cookie.samesite, None);
    }

    #[test]
    fn cookie_tanpa_domain_dilewati() {
        assert!(dari_cdp(&json!({ "name": "x", "value": "y" })).is_none());
        assert!(dari_cdp(&json!({ "domain": "", "name": "x" })).is_none());
    }

    #[test]
    fn bolak_balik_cdp_menjaga_nilai() {
        let asli = store::Cookie {
            host: "uji.example".to_owned(),
            name: "ocb_proof".to_owned(),
            path: "/".to_owned(),
            value: "nilai-rahasia".to_owned(),
            expires: Some(1_900_000_000),
            secure: true,
            http_only: false,
            samesite: Some(1),
        };
        let kembali = dari_cdp(&ke_cdp(&asli)).expect("hasil mapping");
        assert_eq!(kembali, asli);
    }
}
