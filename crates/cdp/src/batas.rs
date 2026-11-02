//! Penegakan `max_tabs` per mesin.
//!
//! Sumber kebenaran jumlah tab adalah **mesin itu sendiri** (`/json/list`),
//! bukan penghitung di daemon. Menyimpan hitungan sendiri berarti punya dua
//! angka yang bisa berbeda: tab yang ditutup klien lewat soket CDP tidak akan
//! pernah terlihat oleh penghitung di daemon, dan batas akan menolak tab baru
//! padahal mesinnya kosong. Harga kebenaran itu satu GET localhost sebelum tab
//! dibuat, dan itu jauh lebih murah daripada satu boot Chromium.
//!
//! Yang dihitung hanya target bertipe `page`. Chrome juga melaporkan
//! `service_worker`, `iframe`, dan `background_page` di daftar yang sama, dan
//! menghitung semuanya akan membuat satu halaman dengan tiga iframe memakan
//! empat jatah tab. "Tab" yang dimaksud operator adalah halaman.
//!
//! Tiga batas yang jujur harus disebut, karena tidak bisa ditutup di sini:
//!
//! 1. **Bukan atomik.** Dua permintaan `Target.createTarget` yang benar-benar
//!    bersamaan bisa sama-sama membaca angka di bawah batas lalu sama-sama
//!    lolos. Menutupnya butuh penghitung permintaan yang sedang terbang, dan
//!    itu menambah state kedua yang justru ingin dihindari. Penjaga keras untuk
//!    memori tetap anggaran (`lifecycle::budget`) dan reap.
//! 2. **Tidak melihat tab yang dibuat mesin sendiri** (`window.open`,
//!    `Target.createTarget` dari dalam halaman, popup OAuth). Tab seperti itu
//!    lahir tanpa melewati daemon, jadi jumlahnya baru terlihat pada permintaan
//!    berikutnya.
//! 3. **Gagal-terbuka saat menghitung gagal.** Kalau `/json/list` tidak bisa
//!    dibaca, batas tidak ditegakkan dan itu dicatat di log. Menolak tab karena
//!    masalah pengukuran akan membuat satu gangguan kecil menjadi "browser
//!    tidak bisa dipakai sama sekali".

#![forbid(unsafe_code)]

use std::sync::Arc;

use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use engine::{Engine, Slot};
use serde_json::{Value, json};

use crate::engines::EnginePool;
use crate::errors::CdpError;
use crate::targets::forward;

/// Kode galat JSON-RPC saat tab ditolak karena batas mesin.
///
/// Dipisahkan dari `KODE_DITOLAK` milik multiplex: klien yang menerima kedua
/// kode ini perlu membedakan "permintaanmu salah alamat" dari "mesinnya penuh",
/// karena tindakan yang harus diambil berbeda.
pub const KODE_BATAS_TAB: i64 = -32010;

/// Bukti bahwa satu tab ditolak, lengkap dengan angkanya.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Penolakan {
    pub mesin: String,
    pub terbuka: usize,
    pub batas: u32,
}

impl Penolakan {
    #[must_use]
    pub fn pesan(&self) -> String {
        lifecycle::tabs::Keputusan::Penuh {
            terbuka: self.terbuka,
            batas: self.batas,
        }
        .pesan(&self.mesin)
        .unwrap_or_else(|| "batas tab tercapai".to_owned())
    }

    /// Jawaban untuk `/json/new`.
    ///
    /// `429` dipilih, bukan `500`: ini bukan kegagalan daemon, dan klien yang
    /// membedakan keduanya bisa menunggu lalu mencoba lagi alih-alih
    /// melaporkan "browser rusak".
    #[must_use]
    pub fn respons_http(&self) -> Response {
        (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::CONTENT_TYPE, "application/json")],
            format!("{{\"error\":{}}}\n", Value::String(self.pesan())),
        )
            .into_response()
    }

    /// Jawaban untuk permintaan `Target.createTarget` di soket browser.
    ///
    /// `id` dan `sessionId` diambil dari permintaan klien: klien CDP mencocokkan
    /// balasan lewat keduanya, dan galat tanpa `id` akan membuat pustaka klien
    /// menunggu selamanya.
    #[must_use]
    pub fn galat_rpc(&self, id: Option<Value>, sesi: Option<&str>) -> String {
        let mut body = json!({
            "id": id.unwrap_or(Value::Null),
            "error": {"code": KODE_BATAS_TAB, "message": self.pesan()},
        });
        if let Some(sesi) = sesi
            && let Some(map) = body.as_object_mut()
        {
            map.insert("sessionId".to_owned(), Value::String(sesi.to_owned()));
        }
        body.to_string()
    }
}

/// Satu mesin yang batasnya perlu ditegakkan.
///
/// Dibangun lewat [`Penjaga::dari_pool`], yang mengembalikan `None` kalau mesin
/// itu tidak punya batas. Itu bukan kerapian belaka: jalur tab baru adalah jalur
/// panas, dan mesin tanpa `max_tabs` tidak boleh membayar satu GET pun.
#[derive(Clone)]
pub struct Penjaga {
    slot: Arc<Slot>,
    batas: u32,
}

impl Penjaga {
    /// `None` berarti tidak ada yang perlu ditegakkan untuk mesin ini.
    #[must_use]
    pub fn dari_pool(pool: &EnginePool, mesin: &str) -> Option<Self> {
        let batas = pool.batas_tab(mesin)?;
        let slot = Arc::clone(pool.get(mesin)?);
        Some(Self { slot, batas })
    }

    #[must_use]
    pub fn batas(&self) -> u32 {
        self.batas
    }

    #[must_use]
    pub fn mesin(&self) -> &str {
        self.slot.id()
    }

    /// Apakah tab baru masih boleh dibuka sekarang.
    ///
    /// `None` = boleh. Mesin yang sedang tidur juga boleh: nol tab, dan boot
    /// berikutnya memang akan mulai dari halaman kosong.
    pub async fn periksa(&self) -> Option<Penolakan> {
        let engine = self.slot.hidup()?;
        let terbuka = match jumlah_halaman(&engine).await {
            Ok(jumlah) => jumlah,
            Err(sebab) => {
                // Gagal-terbuka, dengan alasan tertulis. Memblokir tab karena
                // pengukuran gagal akan mengubah satu gangguan kecil menjadi
                // browser yang tidak bisa dipakai, dan operator tidak akan
                // pernah tahu sebabnya.
                tracing::warn!(
                    mesin = %self.slot.id(),
                    %sebab,
                    "jumlah tab tidak terbaca; max_tabs tidak ditegakkan kali ini"
                );
                return None;
            }
        };
        let keputusan = lifecycle::tabs::periksa(terbuka, Some(self.batas));
        keputusan.ditolak().then(|| Penolakan {
            mesin: self.slot.id().to_owned(),
            terbuka,
            batas: self.batas,
        })
    }

    /// Kalau `teks` meminta tab baru dan mesinnya penuh, balikkan balasan galat
    /// yang siap dikirim ke klien. `None` berarti pesan boleh diteruskan.
    ///
    /// Pemanggil **tidak boleh** meneruskan pesan yang `Some`: dua jalur yang
    /// berbeda (satu mesin dan multiplex) sama-sama memakai fungsi ini supaya
    /// batasnya tidak bisa berlaku di satu jalur saja.
    pub async fn tolak_create(&self, teks: &str) -> Option<String> {
        let (id, sesi) = permintaan_create(teks)?;
        let tolak = self.periksa().await?;
        tracing::info!(
            mesin = %tolak.mesin,
            terbuka = tolak.terbuka,
            batas = tolak.batas,
            "tab baru ditolak: batas mesin tercapai"
        );
        Some(tolak.galat_rpc(id, sesi.as_deref()))
    }
}

/// `Some((id, sessionId))` kalau pesan ini `Target.createTarget`.
///
/// Pesan lain, pesan tanpa `method`, atau JSON rusak sengaja dianggap "bukan
/// permintaan tab": jalur ini hanya memeriksa satu method, dan menebak lebih
/// jauh berarti ikut menilai pesan yang bukan urusannya.
fn permintaan_create(teks: &str) -> Option<(Option<Value>, Option<String>)> {
    let value: Value = serde_json::from_str(teks).ok()?;
    if value.get("method")?.as_str()? != "Target.createTarget" {
        return None;
    }
    let id = value.get("id").cloned();
    let sesi = value
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some((id, sesi))
}

/// Jumlah target `page` yang dilaporkan satu mesin.
async fn jumlah_halaman(engine: &Arc<dyn Engine>) -> Result<usize, CdpError> {
    let upstream = forward(engine, &Method::GET, "/json/list").await?;
    if upstream.status != 200 {
        return Err(CdpError::Upstream(format!(
            "/json/list menjawab {}",
            upstream.status
        )));
    }
    hitung_halaman(&upstream.body)
}

/// Menghitung target bertipe `page` dari body `/json/list`.
///
/// Body yang bukan array **bukan** "nol tab": itu jawaban yang tidak kita
/// pahami, dan menganggapnya nol berarti batas tidak pernah berlaku pada mesin
/// yang menjawab dengan bentuk lain.
fn hitung_halaman(body: &str) -> Result<usize, CdpError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|err| CdpError::Upstream(format!("/json/list: {err}")))?;
    let Value::Array(items) = value else {
        return Err(CdpError::Upstream(
            "/json/list bukan array target".to_owned(),
        ));
    };
    Ok(items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("page"))
        .count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hanya_tipe_page_yang_dihitung() {
        let body = r#"[
            {"id":"A","type":"page","url":"about:blank"},
            {"id":"B","type":"iframe","url":"https://ads.example"},
            {"id":"C","type":"service_worker","url":"https://x/sw.js"},
            {"id":"D","type":"page","url":"https://example.com"}
        ]"#;
        assert_eq!(hitung_halaman(body).expect("body sah"), 2);
    }

    #[test]
    fn body_datar_bukan_nol_tab() {
        // Kalau bentuk yang tidak dikenal dianggap nol tab, batas tidak pernah
        // berlaku pada mesin yang jawabannya berbeda. Lebih baik gagal-terbuka
        // dengan log daripada gagal-diam-diam.
        assert!(hitung_halaman(r#"{"id":"X","type":"page"}"#).is_err());
        assert!(hitung_halaman("bukan json").is_err());
    }

    #[test]
    fn pesan_galat_rpc_membawa_id_dan_session() {
        let tolak = Penolakan {
            mesin: "chrome_full".to_owned(),
            terbuka: 2,
            batas: 2,
        };
        let teks = tolak.galat_rpc(Some(json!(7)), Some("SESI-1"));
        let value: Value = serde_json::from_str(&teks).expect("galat harus JSON");
        assert_eq!(value["id"], json!(7));
        assert_eq!(value["sessionId"], json!("SESI-1"));
        assert_eq!(value["error"]["code"], json!(KODE_BATAS_TAB));
        assert!(
            value["error"]["message"]
                .as_str()
                .expect("pesan")
                .contains("chrome_full")
        );
    }

    #[test]
    fn pesan_bukan_create_dibiarkan_lewat() {
        assert!(permintaan_create(r#"{"id":1,"method":"Target.getTargets"}"#).is_none());
        assert!(permintaan_create(r#"{"id":1}"#).is_none());
        assert!(permintaan_create("bukan json").is_none());
        let (id, sesi) = permintaan_create(
            r#"{"id":3,"method":"Target.createTarget","sessionId":"S1","params":{}}"#,
        )
        .expect("createTarget dikenali");
        assert_eq!(id, Some(json!(3)));
        assert_eq!(sesi.as_deref(), Some("S1"));
    }
}
