//! AC-019: aksi yang berhasil membawa tanda terima berisi selisih DOM.
//!
//! Tanda terimanya dibuat di `cdp::native_wire::state`, lapisan yang sama yang
//! menjawab protokol di `tests/native_wire.rs`. Yang dibuktikan di sini adalah
//! **isi** tanda terimanya: klien yang membaca `result.receipt.delta` harus
//! bisa menilai sendiri apakah aksinya mendarat, tanpa mempercayai kata
//! "berhasil" dari mesin.

use cdp::native_wire::NativeState;
use serde_json::{Value, json};

/// Satu frame JSON-RPC ke state, tanpa soket. Yang diuji bukan pengangkutan,
/// tapi keputusan di dalamnya.
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
    assert_eq!(
        nilai.get("id").and_then(Value::as_i64),
        Some(id),
        "balasan harus membawa id yang sama: {nilai}"
    );
    if let Some(galat) = nilai.get("error") {
        panic!("{metode} gagal: {galat}");
    }
    nilai["result"].clone()
}

fn tab_baru(state: &NativeState) -> String {
    kirim(
        state,
        None,
        1,
        "Target.createTarget",
        json!({"url": "about:blank"}),
    )["targetId"]
        .as_str()
        .expect("targetId")
        .to_owned()
}

fn lekat(state: &NativeState, id: &str) -> String {
    kirim(
        state,
        None,
        2,
        "Target.attachToTarget",
        json!({"targetId": id, "flatten": true}),
    )["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_owned()
}

#[tokio::test]
async fn aksi_yang_mendarat_membawa_delta_dom_yang_tidak_kosong() {
    let state = NativeState::baru();
    let id = tab_baru(&state);
    let sesi = lekat(&state, &id);

    let hasil = kirim(
        &state,
        Some(&sesi),
        3,
        "Page.setDocumentContent",
        json!({
            "frameId": "FRAME-0",
            "html": "<html><head><title>Form</title></head>\
                     <body><form><input id='nama'><button>Simpan</button></form></body></html>"
        }),
    );

    let receipt = &hasil["receipt"];
    assert_eq!(
        receipt["status"], "Changed",
        "isi dokumen harus terdeteksi berubah"
    );
    assert_eq!(receipt["delta"]["dom"], json!(true), "dom berubah");
    assert_eq!(receipt["delta"]["judul"], json!(true), "judul berubah");

    let node = receipt["delta"]["node"]
        .as_i64()
        .expect("delta.node berupa angka");
    assert!(
        node > 0,
        "delta node harus positif setelah dokumen diisi, dapat: {node} (receipt: {receipt})"
    );
    assert_eq!(receipt["sesudah"]["judul"], "Form");

    // `about:blank` pun menghasilkan kerangka html/head/body, jadi "dokumen awal
    // kosong" bukan berarti nol node. Yang harus benar adalah dokumen BERTAMBAH,
    // dan `delta.node` memang selisih keduanya (lihat `banding`).
    let sebelum = receipt["sebelum"]["node"]
        .as_i64()
        .expect("sebelum.node angka");
    let sesudah = receipt["sesudah"]["node"]
        .as_i64()
        .expect("sesudah.node angka");
    assert!(
        sesudah > sebelum,
        "dokumen harus bertambah node setelah diisi: {sebelum} -> {sesudah}"
    );
    assert_eq!(
        sesudah - sebelum,
        node,
        "delta.node harus sama dengan selisih sebelum/sesudah: {sebelum} -> {sesudah}"
    );
}

#[tokio::test]
async fn akar_dokumen_terbaca_setelah_aksi_sehingga_delta_bisa_dicek_silang() {
    let state = NativeState::baru();
    let id = tab_baru(&state);
    let sesi = lekat(&state, &id);

    kirim(
        &state,
        Some(&sesi),
        3,
        "Page.setDocumentContent",
        json!({"frameId": "FRAME-0", "html": "<html><body><p>satu</p></body></html>"}),
    );

    // Tanda terima saja tidak cukup: isi dokumen yang sesudah harus benar-benar
    // ada saat dibaca lewat jalur lain. Kalau tidak, "Changed" bisa berarti
    // dokumennya hilang.
    let dok = kirim(
        &state,
        Some(&sesi),
        4,
        "DOM.getDocument",
        json!({"depth": 3}),
    );
    let root = &dok["root"];
    assert_eq!(root["nodeName"].as_str(), Some("#document"));
    let anak = root["children"].as_array().expect("anak akar");
    assert!(
        !anak.is_empty(),
        "dokumen yang diisi harus punya anak: {root}"
    );

    // `jumlah_tab` memastikan tab yang sama masih dialamatkan, bukan tab baru.
    assert_eq!(state.jumlah_tab(), 1);
}

#[tokio::test]
async fn evaluasi_skrip_melaporkan_tanda_terima_tanpa_mengubah_dokumen() {
    let state = NativeState::baru();
    let id = tab_baru(&state);
    let sesi = lekat(&state, &id);

    kirim(
        &state,
        Some(&sesi),
        3,
        "Page.setDocumentContent",
        json!({"frameId": "FRAME-0", "html": "<html><body><p>x</p></body></html>"}),
    );

    let hasil = kirim(
        &state,
        Some(&sesi),
        4,
        "Runtime.evaluate",
        json!({"expression": "document.title === '' ? 'kosong' : 'ada'"}),
    );
    assert_eq!(hasil["result"]["value"], "kosong");
    assert_eq!(
        hasil["receipt"]["status"], "NoObservableChange",
        "skrip yang hanya membaca tidak boleh diklaim mengubah apa pun: {hasil}"
    );
}
