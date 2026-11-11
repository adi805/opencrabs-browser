//! Terjemahan frame JSON-RPC ke `TabPool` tanpa soket.
//!
//! Yang diuji di sini **tanpa jaringan sama sekali**: satu masuk satu keluar.
//! Tes soket ada di `tests/native_wire.rs`; kalau lapisan ini diuji lewat
//! jaringan, kegagalan transport akan terlihat seperti kegagalan protokol.
//!
//! Semua balasan yang diklaim berhasil di sini diperiksa isinya, bukan cuma
//! ada/tidaknya `error`: balasan `{}` untuk metode yang tidak melakukan apa pun
//! akan lolos dari pemeriksaan galat, dan itu justru kegagalan yang paling
//! mahal.

use cdp::native_wire::{NativeState, PRODUK};
use serde_json::{Value, json};

fn balas(state: &NativeState, id: i64, metode: &str, params: Value) -> Value {
    let frame = json!({"id": id, "method": metode, "params": params});
    let jawaban = state.tangani(&frame.to_string()).expect("balasan");
    serde_json::from_str(&jawaban).expect("balasan JSON")
}

/// Frame dengan `sessionId` di tingkat atas, seperti yang dikirim klien
/// sungguhan. `sessionId` **tidak** diterima di dalam `params`: Chrome
/// menaruhnya di frame, dan lapisan ini mengikuti bentuk itu.
fn balas_sesi(state: &NativeState, id: i64, metode: &str, params: Value, sesi: &str) -> Value {
    let frame = json!({"id": id, "method": metode, "params": params, "sessionId": sesi});
    let jawaban = state.tangani(&frame.to_string()).expect("balasan");
    serde_json::from_str(&jawaban).expect("balasan JSON")
}

fn ok_sesi(state: &NativeState, id: i64, metode: &str, params: Value, sesi: &str) -> Value {
    let nilai = balas_sesi(state, id, metode, params, sesi);
    assert!(
        nilai.get("error").is_none(),
        "{metode} harus berhasil, dapat: {nilai}"
    );
    nilai["result"].clone()
}

fn ok(state: &NativeState, id: i64, metode: &str, params: Value) -> Value {
    let nilai = balas(state, id, metode, params);
    assert!(
        nilai.get("error").is_none(),
        "{metode} harus berhasil, dapat: {nilai}"
    );
    nilai["result"].clone()
}

fn tab(state: &NativeState) -> String {
    ok(
        state,
        1,
        "Target.createTarget",
        json!({"url": "about:blank"}),
    )["targetId"]
        .as_str()
        .expect("targetId")
        .to_owned()
}

fn lekat(state: &NativeState, id: &str) -> String {
    ok(state, 2, "Target.attachToTarget", json!({"targetId": id}))["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_owned()
}

#[test]
fn frame_bukan_json_dijawab_galat_parse_dengan_id_null() {
    let state = NativeState::baru();
    let jawaban = state.tangani("{ini bukan json").expect("balasan");
    let nilai: Value = serde_json::from_str(&jawaban).expect("balasan JSON");
    assert_eq!(nilai["id"], Value::Null, "id tidak diketahui, jadi null");
    assert_eq!(nilai["error"]["code"], json!(-32700));
}

#[test]
fn frame_tanpa_method_tidak_dijawab() {
    // Chrome mengabaikan frame seperti ini; menjawabnya akan membuat klien
    // mengira ada perintah yang dieksekusi.
    let state = NativeState::baru();
    assert!(state.tangani(r#"{"id":1}"#).is_none());
}

#[test]
fn notifikasi_tanpa_id_dieksekusi_tanpa_balasan() {
    let state = NativeState::baru();
    // `Target.setDiscoverTargets` tanpa `id` adalah notifikasi CDP yang lazim.
    assert!(
        state
            .tangani(r#"{"method":"Target.setDiscoverTargets","params":{"discover":true}}"#)
            .is_none(),
        "notifikasi tidak boleh dijawab"
    );
}

#[test]
fn browser_get_version_membawa_identitas_mesin_native() {
    let state = NativeState::baru();
    let hasil = ok(&state, 1, "Browser.getVersion", json!({}));
    assert_eq!(hasil["product"], PRODUK);
    assert!(
        hasil["product"]
            .as_str()
            .unwrap_or_default()
            .contains("RustNativeEngine"),
        "AC-013 menuntut string RustNativeEngine: {hasil}"
    );
    assert_eq!(hasil["jsVersion"], "QuickJS");
    assert_eq!(hasil["protocolVersion"], "1.3");
}

#[test]
fn get_targets_dan_json_list_melihat_daftar_yang_sama() {
    let state = NativeState::baru();
    let satu = tab(&state);
    let dua = tab(&state);

    let targets = ok(&state, 3, "Target.getTargets", json!({}));
    let infos = targets["targetInfos"].as_array().expect("targetInfos");
    assert_eq!(infos.len(), 2);
    let ids = infos
        .iter()
        .filter_map(|info| info["targetId"].as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&satu.as_str()) && ids.contains(&dua.as_str()));

    // `/json/list` dan `Target.getTargets` harus sepakat: dua sumber kebenaran
    // yang berbeda berarti klien melihat tab yang tidak bisa ditempeli.
    let daftar = state.target();
    assert_eq!(daftar.len(), 2);
    assert_eq!(daftar[0]["id"].as_str(), Some(satu.as_str()));
}

#[test]
fn sesi_lekat_ditandai_dan_bisa_dilepas() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);

    let targets = ok(&state, 3, "Target.getTargets", json!({}));
    assert_eq!(targets["targetInfos"][0]["attached"], json!(true));

    ok(
        &state,
        4,
        "Target.detachFromTarget",
        json!({"sessionId": sesi}),
    );
    let targets = ok(&state, 5, "Target.getTargets", json!({}));
    assert_eq!(
        targets["targetInfos"][0]["attached"],
        json!(false),
        "setelah dilepas, target tidak boleh terlihat terpasang"
    );

    // Sesi yang sudah dilepas tidak bisa dipakai lagi.
    let gagal = balas_sesi(&state, 6, "Page.getFrameTree", json!({}), &sesi);
    assert_eq!(gagal["error"]["code"], json!(-32001));
}

#[test]
fn perintah_halaman_tanpa_session_ditolak_dengan_pesan_jelas() {
    let state = NativeState::baru();
    let gagal = balas(&state, 1, "Page.getFrameTree", json!({}));
    assert_eq!(gagal["error"]["code"], json!(-32602));
    assert!(
        gagal["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("sessionId"),
        "pesannya harus menyebut apa yang kurang: {gagal}"
    );
}

#[test]
fn frame_tree_membawa_url_tab_yang_sebenarnya() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    let url = "data:text/html,<html><head><title>Keranjang</title></head><body></body></html>";
    ok_sesi(&state, 3, "Page.navigate", json!({"url": url}), &sesi);

    let pohon = ok_sesi(&state, 4, "Page.getFrameTree", json!({}), &sesi);
    assert_eq!(pohon["frameTree"]["frame"]["url"], url);
    assert_eq!(pohon["frameTree"]["childFrames"], json!([]));
}

#[test]
fn tutup_target_menghapus_tab_dan_sesinya() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    assert_eq!(state.jumlah_tab(), 1);

    ok(&state, 3, "Target.closeTarget", json!({"targetId": id}));
    assert_eq!(state.jumlah_tab(), 0);

    let gagal = balas_sesi(&state, 4, "Page.getFrameTree", json!({}), &sesi);
    assert_eq!(
        gagal["error"]["code"],
        json!(-32001),
        "sesi milik tab yang ditutup harus ikut hilang: {gagal}"
    );
}

#[test]
fn target_tidak_dikenal_dijawab_galat_bukan_panic() {
    let state = NativeState::baru();
    for (id, metode, params) in [
        (1, "Target.closeTarget", json!({"targetId": "7"})),
        (
            2,
            "Target.activateTarget",
            json!({"targetId": "bukan-angka"}),
        ),
        (3, "Target.attachToTarget", json!({"targetId": "999"})),
    ] {
        let gagal = balas(&state, id, metode, params);
        assert!(
            gagal.get("error").is_some(),
            "{metode} ke target asing harus jadi galat: {gagal}"
        );
        assert_eq!(gagal["id"], json!(id), "id balasan harus sama");
    }
}

#[test]
fn data_url_bukan_html_ditolak() {
    let state = NativeState::baru();
    let gagal = balas(
        &state,
        1,
        "Target.createTarget",
        json!({"url": "data:text/plain,halo"}),
    );
    assert_eq!(gagal["error"]["code"], json!(-32000));
    assert_eq!(
        state.jumlah_tab(),
        0,
        "tab yang gagal dibuat tidak boleh tertinggal di daftar"
    );
}

#[test]
fn set_document_content_mengganti_isi_tab_yang_sama() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);

    ok_sesi(
        &state,
        3,
        "Page.setDocumentContent",
        json!({"html": "<html><body><p>pertama</p></body></html>"}),
        &sesi,
    );
    ok_sesi(
        &state,
        4,
        "Page.setDocumentContent",
        json!({"html": "<html><body><p>kedua</p></body></html>"}),
        &sesi,
    );

    assert_eq!(
        state.jumlah_tab(),
        1,
        "isi baru tidak boleh membuat tab baru"
    );
    let dok = ok_sesi(&state, 5, "DOM.getDocument", json!({"depth": -1}), &sesi);
    let teks = dok.to_string();
    assert!(teks.contains("kedua"), "dokumen harus berisi isi terakhir");
    assert!(!teks.contains("pertama"), "isi lama harus hilang: {dok}");
}

/// `DOM.querySelector` dan `querySelectorAll` menjawab node sungguhan, bukan
/// galat "belum ada". Node yang dijawab harus bisa dipakai metode DOM lain,
/// jadi tesnya merangkai: cari, baca HTML luar, lalu simpulkan.
#[test]
fn dom_query_selector_menjawab_node_sungguhan() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);

    ok_sesi(
        &state,
        2,
        "Page.setDocumentContent",
        json!({"html": "<div class=\"kartu\"><p>satu</p><p id=\"dua\">dua</p></div>"}),
        &sesi,
    );

    let satu = ok_sesi(
        &state,
        3,
        "DOM.querySelector",
        json!({"selector": "p"}),
        &sesi,
    );
    let node = satu["nodeId"].as_u64().expect("nodeId harus angka");
    let luar = ok_sesi(
        &state,
        4,
        "DOM.getOuterHTML",
        json!({"nodeId": node}),
        &sesi,
    );
    assert_eq!(luar["outerHTML"], "<p>satu</p>");

    let semua = ok_sesi(
        &state,
        5,
        "DOM.querySelectorAll",
        json!({"selector": "div.kartu > p"}),
        &sesi,
    );
    assert_eq!(
        semua["nodeIds"].as_array().map(Vec::len),
        Some(2),
        "dua p anak langsung .kartu: {semua}"
    );

    // Selektor yang tidak cocok menjawab `null`, bukan node dokumen (indeks 0).
    let kosong = ok_sesi(
        &state,
        6,
        "DOM.querySelector",
        json!({"selector": "article"}),
        &sesi,
    );
    assert!(
        kosong["nodeId"].is_null(),
        "tidak ketemu harus null: {kosong}"
    );
}

#[test]
fn dom_query_selector_menolak_selektor_rusak() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    let jawab = balas_sesi(
        &state,
        2,
        "DOM.querySelector",
        json!({"selector": "div >"}),
        &sesi,
    );
    let pesan = jawab["error"]["message"].as_str().unwrap_or_default();
    assert!(
        pesan.contains("selektor tidak sah"),
        "selektor rusak harus ditolak dengan alasan: {jawab}"
    );
}

/// Pencarian dibatasi ke subtree `nodeId`, dan node di luar arena ditolak.
#[test]
fn dom_query_selector_menghormati_node_akar() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);

    ok_sesi(
        &state,
        2,
        "Page.setDocumentContent",
        json!({"html": "<div id=\"a\"><p>x</p></div><div id=\"b\"><p>y</p></div>"}),
        &sesi,
    );
    let akar = ok_sesi(
        &state,
        3,
        "DOM.querySelector",
        json!({"selector": "#b"}),
        &sesi,
    )["nodeId"]
        .as_u64()
        .expect("#b harus ketemu");

    let dalam = ok_sesi(
        &state,
        4,
        "DOM.querySelectorAll",
        json!({"selector": "p", "nodeId": akar}),
        &sesi,
    );
    let ids = dalam["nodeIds"].as_array().cloned().unwrap_or_default();
    assert_eq!(ids.len(), 1, "hanya satu p di dalam #b: {dalam}");
    let luar = ok_sesi(
        &state,
        5,
        "DOM.getOuterHTML",
        json!({"nodeId": ids[0]}),
        &sesi,
    );
    assert_eq!(luar["outerHTML"], "<p>y</p>");

    let salah = balas_sesi(
        &state,
        6,
        "DOM.querySelectorAll",
        json!({"selector": "p", "nodeId": 9999}),
        &sesi,
    );
    assert!(
        salah.get("error").is_some(),
        "nodeId di luar arena harus galat: {salah}"
    );
}

/// `DOM.resolveNode` mengembalikan pegangan yang menyebut elemennya, supaya
/// klien tidak menerima objek tanpa keterangan.
#[test]
fn dom_resolve_node_menyebut_elemennya() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);

    ok_sesi(
        &state,
        2,
        "Page.setDocumentContent",
        json!({"html": "<div id=\"utama\" class=\"kartu besar\">x</div>"}),
        &sesi,
    );
    let node = ok_sesi(
        &state,
        3,
        "DOM.querySelector",
        json!({"selector": "#utama"}),
        &sesi,
    )["nodeId"]
        .as_u64()
        .expect("nodeId");

    let simpul = ok_sesi(&state, 4, "DOM.resolveNode", json!({"nodeId": node}), &sesi);
    assert_eq!(simpul["object"]["subtype"], "node");
    assert_eq!(simpul["object"]["description"], "div#utama.kartu.besar");
    assert_eq!(simpul["object"]["objectId"], format!("node-{node}"));
}

/// `DOM.getBoxModel` tetap jujur: kotak layout belum memisahkan padding,
/// border, dan margin, jadi metodenya menjawab galat yang menyebut itu.
#[test]
fn dom_get_box_model_masih_menyebut_kekurangannya() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    let jawab = balas_sesi(&state, 2, "DOM.getBoxModel", json!({"nodeId": 1}), &sesi);
    assert_eq!(jawab["error"]["code"].as_i64(), Some(-32601));
    let pesan = jawab["error"]["message"].as_str().unwrap_or_default();
    assert!(
        pesan.contains("padding"),
        "galat harus menyebut bagian yang kurang: {pesan}"
    );
}

/// `Accessibility.getFullAXTree` menjawab pohon sungguhan dari DOM yang sudah
/// dimuat, bukan `{}` kosong. Yang diperiksa: peran, nama, dan tautan
/// `backendDOMNodeId` yang bisa dilacak balik ke `DOM.*`.
#[test]
fn accessibility_get_full_ax_tree_membawa_peran_dan_nama() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    ok_sesi(
        &state,
        3,
        "Page.setDocumentContent",
        json!({
            "frameId": "utama",
            "html": "<html><head><title>Halaman</title></head>\
                     <body><h1>Judul</h1><p>isi</p>\
                     <button aria-label=\"Tutup\">X</button></body></html>"
        }),
        &sesi,
    );
    let hasil = ok_sesi(&state, 4, "Accessibility.getFullAXTree", json!({}), &sesi);
    let simpul = hasil["nodes"].as_array().expect("nodes array");
    assert!(!simpul.is_empty(), "pohon AX tidak boleh kosong");

    let peran = |role: &str| -> Vec<&Value> {
        simpul
            .iter()
            .filter(|n| n["role"]["value"].as_str() == Some(role))
            .collect()
    };
    let judul = peran("heading");
    assert_eq!(judul.len(), 1, "satu heading: {simpul:#?}");
    assert_eq!(judul[0]["name"]["value"].as_str(), Some("Judul"));
    assert!(
        judul[0]["backendDOMNodeId"].is_number(),
        "heading harus punya backendDOMNodeId"
    );
    assert_eq!(peran("paragraph")[0]["name"]["value"].as_str(), Some("isi"));
    let tombol = peran("button");
    assert_eq!(
        tombol[0]["name"]["value"].as_str(),
        Some("Tutup"),
        "nama diambil dari aria-label"
    );
    // Akar pohon adalah html, sama seperti Chrome.
    assert_eq!(simpul[0]["role"]["value"].as_str(), Some("RootWebArea"));
    // `head` dan `title` tidak muncul sebagai simpul.
    assert!(
        peran("generic")
            .iter()
            .all(|n| n["backendDOMNodeId"] != json!(0)),
        "dokumen bukan simpul AX"
    );
}

/// Tangkapan layar mengembalikan PNG sungguhan dalam base64, dengan ukuran
/// yang diminta klip, bukan PNG kosong atau `{}`.
#[test]
fn capture_screenshot_mengembalikan_png_base64() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    ok_sesi(
        &state,
        3,
        "Page.setDocumentContent",
        json!({
            "frameId": "utama",
            "html": "<body><div style=\"width:60px;height:40px;background-color:#ff0000\"></div></body>"
        }),
        &sesi,
    );
    let hasil = ok_sesi(
        &state,
        4,
        "Page.captureScreenshot",
        json!({"format": "png", "clip": {"x": 0, "y": 0, "width": 60, "height": 40}}),
        &sesi,
    );
    let data = hasil["data"].as_str().expect("data base64");
    let png = cdp::base64::dekode(data).expect("base64 sah");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "harus PNG sungguhan");
    let lebar = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let tinggi = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    assert_eq!((lebar, tinggi), (60, 40), "ukuran mengikuti klip");
    assert_eq!(hasil["receipt"]["kotakDilukis"].as_u64(), Some(1));
}

/// JPEG ditolak dengan pesan yang menyebut yang didukung, bukan diam-diam
/// mengirim PNG berlabel JPEG.
#[test]
fn capture_screenshot_menolak_format_selain_png() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    let jawab = balas_sesi(
        &state,
        3,
        "Page.captureScreenshot",
        json!({"format": "jpeg", "quality": 80}),
        &sesi,
    );
    assert_eq!(jawab["error"]["code"].as_i64(), Some(-32602));
    let pesan = jawab["error"]["message"].as_str().unwrap_or_default();
    assert!(
        pesan.contains("PNG"),
        "galat harus menyebut format yang didukung: {pesan}"
    );
}

/// Klip dengan skala selain 1 ditolak: merasterisasi pada skala lain butuh
/// penskalaan kanvas, dan mengabaikannya berarti mengirim ukuran yang salah.
#[test]
fn capture_screenshot_menolak_klip_berskala() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    let jawab = balas_sesi(
        &state,
        3,
        "Page.captureScreenshot",
        json!({
            "format": "png",
            "clip": {"x": 0, "y": 0, "width": 10, "height": 10, "scale": 2}
        }),
        &sesi,
    );
    assert_eq!(jawab["error"]["code"].as_i64(), Some(-32602));
    let pesan = jawab["error"]["message"].as_str().unwrap_or_default();
    assert!(
        pesan.contains("scale"),
        "galat harus menyebut scale: {pesan}"
    );
}
