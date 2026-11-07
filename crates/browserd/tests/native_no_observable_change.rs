//! AC-020: aksi yang tidak mengubah apa pun **ditandai**, bukan dianggap sukses.
//!
//! Ini kriteria negatif, dan justru itu intinya: agen yang menyimpulkan "berhasil"
//! dari tidak adanya galat harus tertangkap. Tiga bentuk "tidak ada perubahan"
//! yang diuji di sini: navigasi ke keadaan yang sama, mengisi dokumen dengan isi
//! yang sama, dan skrip yang hanya membaca.

use cdp::native_wire::NativeState;
use serde_json::{Value, json};

fn kirim(state: &NativeState, sesi: Option<&str>, id: i64, metode: &str, params: Value) -> Value {
    let mut frame = json!({"id": id, "method": metode, "params": params});
    if let Some(sesi) = sesi {
        frame["sessionId"] = json!(sesi);
    }
    let jawaban = match sesi {
        Some(sesi) => state.tangani_halaman(&frame.to_string(), sesi),
        None => state.tangani(&frame.to_string()),
    }
    .unwrap_or_else(|| panic!("{metode} harus dijawab"));

    let nilai: Value = serde_json::from_str(&jawaban).expect("balasan JSON");
    if let Some(galat) = nilai.get("error") {
        panic!("{metode} gagal: {galat}");
    }
    nilai["result"].clone()
}

struct Tab {
    state: NativeState,
    sesi: String,
}

impl Tab {
    fn new() -> Self {
        let state = NativeState::baru();
        let id = kirim(
            &state,
            None,
            1,
            "Target.createTarget",
            json!({"url": "about:blank"}),
        )["targetId"]
            .as_str()
            .expect("targetId")
            .to_owned();
        let sesi = kirim(
            &state,
            None,
            2,
            "Target.attachToTarget",
            json!({"targetId": id}),
        )["sessionId"]
            .as_str()
            .expect("sessionId")
            .to_owned();
        Self { state, sesi }
    }

    fn kirim(&self, id: i64, metode: &str, params: Value) -> Value {
        kirim(&self.state, Some(&self.sesi), id, metode, params)
    }
}

const HALAMAN: &str = "<html><head><title>Tetap</title></head><body><p>isi</p></body></html>";

fn status(receipt: &Value) -> &str {
    receipt["status"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn navigasi_ke_url_yang_sama_tidak_diklaim_mengubah_apa_pun() {
    let tab = Tab::new();
    let url = "data:text/html,<html><head><title>Sama</title></head><body><p>x</p></body></html>";

    let pertama = tab.kirim(3, "Page.navigate", json!({"url": url}));
    assert_eq!(
        status(&pertama["receipt"]),
        "Changed",
        "navigasi awal mengubah"
    );

    let kedua = tab.kirim(4, "Page.navigate", json!({"url": url}));
    let receipt = &kedua["receipt"];
    assert_eq!(
        status(receipt),
        "NoObservableChange",
        "URL, judul, dan dokumen sama berarti tidak ada yang berubah: {receipt}"
    );
    assert_eq!(receipt["delta"]["dom"], json!(false));
    assert_eq!(receipt["delta"]["url"], json!(false));
    assert_eq!(receipt["delta"]["judul"], json!(false));
    assert_eq!(receipt["delta"]["node"], json!(0));
}

#[tokio::test]
async fn mengisi_dokumen_dengan_isi_yang_sama_tidak_diklaim_mengubah() {
    let tab = Tab::new();
    let awal = tab.kirim(
        3,
        "Page.setDocumentContent",
        json!({"frameId": "FRAME-0", "html": HALAMAN}),
    );
    assert_eq!(status(&awal["receipt"]), "Changed");

    let ulang = tab.kirim(
        4,
        "Page.setDocumentContent",
        json!({"frameId": "FRAME-0", "html": HALAMAN}),
    );
    assert_eq!(
        status(&ulang["receipt"]),
        "NoObservableChange",
        "HTML identik menghasilkan dokumen identik: {}",
        ulang["receipt"]
    );
}

#[tokio::test]
async fn skrip_baca_saja_tidak_diklaim_mengubah_dokumen() {
    let tab = Tab::new();
    tab.kirim(
        3,
        "Page.setDocumentContent",
        json!({"frameId": "FRAME-0", "html": HALAMAN}),
    );

    let hasil = tab.kirim(4, "Runtime.evaluate", json!({"expression": "1 + 1"}));
    assert_eq!(hasil["result"]["value"], "2");
    assert_eq!(
        status(&hasil["receipt"]),
        "NoObservableChange",
        "membaca nilai bukan aksi: {}",
        hasil["receipt"]
    );
}

/// Batas yang jujur: skrip halaman **belum** bisa mengubah pohon DOM di mesin
/// ini. `document` di dalam isolat adalah objek biasa, bukan DOM sungguhan
/// (`crates/engine/src/native/js.rs`, fungsi `pasang_judul`). Karena itu skrip
/// yang menulis judul dilaporkan `NoObservableChange`, dan itu **benar**: tidak
/// ada yang berubah. Tes ini mengunci batasnya supaya ia tidak diam-diam
/// berubah jadi klaim mampu ketika binding DOM disusulkan tanpa tes.
#[tokio::test]
async fn skrip_yang_menulis_judul_belum_menyentuh_pohon_dom() {
    let tab = Tab::new();
    let hasil = tab.kirim(
        3,
        "Runtime.evaluate",
        json!({"expression": "document.title = 'berubah'"}),
    );
    assert_eq!(
        status(&hasil["receipt"]),
        "NoObservableChange",
        "menulis ke objek `document` palsu tidak mengubah DOM: {}",
        hasil["receipt"]
    );
    assert_eq!(hasil["receipt"]["sesudah"]["judul"], Value::Null);
}
