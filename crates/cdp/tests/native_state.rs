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
fn url_jaringan_tanpa_dokumen_ditolak_bukan_jadi_halaman_kosong() {
    // `NativeState::tangani` tidak pernah menunggu jaringan: dokumennya harus
    // diserahkan pemanggil lewat `tangani_ambil`. Kalau lapisan ini diam-diam
    // menerima URL http dan memasang halaman kosong, klien akan mengira
    // navigasinya berhasil dan membaca halaman kosong sebagai isi situs.
    let state = NativeState::baru();
    let gagal = balas(
        &state,
        1,
        "Target.createTarget",
        json!({"url": "https://contoh.test/halaman"}),
    );
    assert_eq!(gagal["error"]["code"], json!(-32000));
    let pesan = gagal["error"]["message"].as_str().unwrap_or_default();
    assert!(
        pesan.contains("butuh pengambilan jaringan")
            && pesan.contains("https://contoh.test/halaman"),
        "penolakan harus menyebut URL dan alasannya: {gagal}"
    );
    assert_eq!(
        state.jumlah_tab(),
        0,
        "tab yang gagal dibuat tidak boleh tertinggal di daftar"
    );

    // Jalur yang sama untuk `Page.navigate`: tab yang sudah ada harus tetap
    // memegang dokumen lamanya, bukan diganti halaman kosong.
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    ok_sesi(
        &state,
        3,
        "Page.setDocumentContent",
        json!({"html": "<p>lama</p>"}),
        &sesi,
    );
    let gagal = balas_sesi(
        &state,
        4,
        "Page.navigate",
        json!({"url": "https://contoh.test/lain"}),
        &sesi,
    );
    assert!(
        gagal.get("error").is_some(),
        "navigate tanpa dokumen harus gagal: {gagal}"
    );
    let akar = ok_sesi(&state, 5, "DOM.getDocument", json!({"depth": -1}), &sesi);
    let teks = akar.to_string();
    assert!(
        teks.contains("lama"),
        "dokumen lama harus utuh setelah navigasi yang gagal: {akar}"
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

/// Halaman yang mengisi dirinya sendiri lewat `<script>` harus terisi di
/// mesin ini, dan hasilnya harus terlihat oleh `DOM.getDocument`.
///
/// Ini beda dari tes jembatan DOM di `engine`: di sana JavaScript dipanggil
/// klien lewat `Runtime.evaluate`. Di sini tidak ada yang memanggil apa pun
/// selain halaman itu sendiri, persis seperti situs sungguhan.
#[test]
fn skrip_halaman_berjalan_saat_dokumen_dipasang() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);

    let terima = ok_sesi(
        &state,
        3,
        "Page.setDocumentContent",
        json!({"html": "<html><head><title>awal</title></head><body>\
               <div id=\"isi\"></div>\
               <script>document.getElementById('isi').textContent = 'dari skrip';\
               document.title = 'judul dari skrip';</script>\
               </body></html>"}),
        &sesi,
    );
    assert_eq!(
        terima["skrip"]["dijalankan"], 1,
        "satu skrip sebaris harus dilaporkan berjalan: {terima}"
    );
    assert_eq!(
        terima["skrip"]["galat"].as_array().map(|a| a.len()),
        Some(0),
        "skrip ini tidak boleh menghasilkan galat: {terima}"
    );

    // Sisi yang dibaca klien: pohon DOM sesudah skrip berjalan.
    let dok = ok_sesi(&state, 4, "DOM.getDocument", json!({"depth": -1}), &sesi);
    let teks = dok.to_string();
    assert!(
        teks.contains("dari skrip"),
        "isi yang ditulis skrip harus terlihat di pohon: {dok}"
    );
    // Judul juga: itu yang dipakai `Target.targetInfo`, bukan string kosong.
    let target = ok(&state, 5, "Target.getTargets", json!({}));
    let info = target["targetInfos"]
        .as_array()
        .and_then(|a| a.iter().find(|t| t["targetId"] == json!(id)))
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(info["title"], json!("judul dari skrip"), "{target}");
}

/// Skrip yang tidak bisa dijalankan dilaporkan, bukan didiamkan: halaman yang
/// tampak kosong tanpa penjelasan adalah kegagalan yang paling mahal, karena
/// tidak ada yang bisa ditindaklanjuti dari luar.
#[test]
fn skrip_yang_dilewati_terlihat_di_tanda_terima_navigasi() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);

    let terima = ok_sesi(
        &state,
        3,
        "Page.setDocumentContent",
        json!({"html": "<html><body>\
               <script type=\"application/json\">{\"a\":1}</script>\
               </body></html>"}),
        &sesi,
    );
    assert_eq!(terima["skrip"]["dijalankan"], 0, "{terima}");
    let dilewati = terima["skrip"]["dilewati"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(dilewati.len(), 1, "{terima}");
    assert!(
        dilewati[0]
            .as_str()
            .unwrap_or_default()
            .contains("application/json"),
        "alasan pelewatan harus menyebut tipenya: {dilewati:?}"
    );
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

/// Tangkapan layar memakai ulang layout selama pohon tidak berubah, dan
/// menghitungnya ulang begitu pohon berubah.
///
/// Dua arah diuji karena satu arah saja tidak membedakan apa pun: tangkapan
/// yang selalu menata ulang juga lolos "berubah jadi segar", dan tangkapan
/// yang selalu memakai cache juga lolos "dua kali sama". Yang dituntut adalah
/// keduanya sekaligus, pada jalur yang benar-benar dipakai agen.
#[test]
fn capture_screenshot_memakai_ulang_layout_dan_tetap_segar() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    ok_sesi(
        &state,
        3,
        "Page.setDocumentContent",
        json!({
            "html": "<body style=\"margin:0\"><div id=\"k\" style=\"width:60px;height:40px;background-color:#ff0000\"></div></body>"
        }),
        &sesi,
    );
    let tangkap = |id_pesan: i64| {
        let hasil = ok_sesi(
            &state,
            id_pesan,
            "Page.captureScreenshot",
            json!({"format": "png", "clip": {"x": 0, "y": 0, "width": 60, "height": 40}}),
            &sesi,
        );
        hasil["data"].as_str().expect("data base64").to_owned()
    };
    let pertama = tangkap(4);
    let kedua = tangkap(5);
    assert_eq!(
        pertama, kedua,
        "pohon yang tidak berubah harus menghasilkan gambar yang sama persis"
    );

    // Lebar kotak diubah dari halaman. Kalau cache tidak dibuang, gambar
    // berikutnya tetap memakai kotak lama dan tes ini gagal.
    ok_sesi(
        &state,
        6,
        "Runtime.evaluate",
        json!({
            "expression": "document.getElementById('k').setAttribute('style', 'width:20px;height:40px;background-color:#0000ff')",
            "returnByValue": true
        }),
        &sesi,
    );
    let ketiga = tangkap(7);
    assert_ne!(
        pertama, ketiga,
        "sesudah pohon berubah, tangkapan harus memakai layout baru"
    );
}

/// JPEG dan WebP dilayani oleh encoder sendiri, dan `quality` benar-benar
/// dipakai pada JPEG: dua mutu berbeda menghasilkan ukuran byte berbeda. Yang
/// tidak punya encoder (`avif`) ditolak dengan nama, bukan diam-diam dikirim
/// PNG berlabel AVIF.
#[test]
fn capture_screenshot_melayani_jpeg_dan_webp_dan_menolak_format_lain() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    ok_sesi(
        &state,
        3,
        "Page.setDocumentContent",
        json!({
            "html": "<body><div style=\"width:60px;height:40px;background:linear-gradient(#f00,#00f)\"></div></body>"
        }),
        &sesi,
    );
    let tinggi = |mutu: u8| {
        let hasil = ok_sesi(
            &state,
            4,
            "Page.captureScreenshot",
            json!({
                "format": "jpeg",
                "quality": mutu,
                "clip": {"x": 0, "y": 0, "width": 60, "height": 40}
            }),
            &sesi,
        );
        let data = hasil["data"].as_str().expect("data base64");
        assert_eq!(
            hasil["receipt"]["format"].as_str(),
            Some("image/jpeg"),
            "tanda terima harus menyebut JPEG"
        );
        let jpeg = cdp::base64::dekode(data).expect("base64 sah");
        assert_eq!(&jpeg[..3], b"\xff\xd8\xff", "harus JPEG sungguhan");
        jpeg.len()
    };
    let mutu_rendah = tinggi(10);
    let mutu_tinggi = tinggi(95);
    assert!(
        mutu_rendah < mutu_tinggi,
        "quality 10 harus lebih kecil dari 95: {mutu_rendah} vs {mutu_tinggi}"
    );

    let webp = ok_sesi(
        &state,
        5,
        "Page.captureScreenshot",
        json!({"format": "webp"}),
        &sesi,
    );
    assert_eq!(
        webp["receipt"]["format"].as_str(),
        Some("image/webp"),
        "tanda terima harus menyebut WebP: {webp}"
    );
    let data = webp["data"].as_str().expect("data base64");
    let bytes = cdp::base64::dekode(data).expect("base64 sah");
    assert_eq!(&bytes[..4], b"RIFF", "harus container RIFF");
    assert_eq!(&bytes[8..12], b"WEBP", "container harus WebP");

    let jawab = balas_sesi(
        &state,
        6,
        "Page.captureScreenshot",
        json!({"format": "avif"}),
        &sesi,
    );
    assert_eq!(jawab["error"]["code"].as_i64(), Some(-32602));
    let pesan = jawab["error"]["message"].as_str().unwrap_or_default();
    assert!(
        pesan.contains("avif") && pesan.contains("webp") && pesan.contains("jpeg"),
        "galat harus menyebut yang diminta dan yang didukung: {pesan}"
    );
}

/// `clip.scale` merender ulang pada kerapatan lain: 10 px CSS pada skala 2
/// menghasilkan gambar 20x20, bukan 10x10 yang diperbesar setelahnya.
#[test]
fn capture_screenshot_klip_berskala_memperbesar_keluaran() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    let jawab = ok_sesi(
        &state,
        3,
        "Page.captureScreenshot",
        json!({
            "format": "png",
            "clip": {"x": 0, "y": 0, "width": 10, "height": 10, "scale": 2}
        }),
        &sesi,
    );
    assert_eq!(
        jawab["receipt"]["scale"].as_f64(),
        Some(2.0),
        "tanda terima harus menyebut skala yang dipakai: {jawab}"
    );
    let data = jawab["data"].as_str().expect("data base64");
    let png = cdp::base64::dekode(data).expect("base64 sah");
    let lebar = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let tinggi = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    assert_eq!((lebar, tinggi), (20, 20), "10 px CSS pada skala 2");
}

/// `quality` pada PNG tidak mengubah apa pun sejak awal, jadi nilainya dicatat
/// sebagai diabaikan alih-alih diterima diam-diam sebagai parameter bermakna.
#[test]
fn capture_screenshot_mencatat_quality_png_sebagai_diabaikan() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    let jawab = ok_sesi(
        &state,
        3,
        "Page.captureScreenshot",
        json!({"format": "png", "quality": 50}),
        &sesi,
    );
    let diabaikan = jawab["receipt"]["diabaikan"]
        .as_array()
        .expect("daftar diabaikan");
    assert!(
        diabaikan
            .iter()
            .any(|n| n.as_str().unwrap_or_default().contains("quality 50")),
        "quality pada PNG harus tercatat: {diabaikan:#?}"
    );
}

/// `captureBeyondViewport` benar-benar mengubah hasil: klip 900 px pada
/// viewport bawaan 800 px dipotong tanpa parameter itu, dan dilayani penuh
/// dengan parameter itu. Tanda terima menyebut mana yang terjadi.
#[test]
fn capture_screenshot_capture_beyond_viewport_mengubah_pemotongan() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    let klip = json!({"x": 0, "y": 0, "width": 900, "height": 100});

    let dalam = ok_sesi(
        &state,
        3,
        "Page.captureScreenshot",
        json!({"format": "png", "clip": klip}),
        &sesi,
    );
    assert_eq!(
        dalam["receipt"]["clipDipotong"], true,
        "klip lebih lebar dari viewport harus dipotong: {dalam}"
    );
    let png = cdp::base64::dekode(dalam["data"].as_str().expect("data base64")).expect("base64");
    let lebar = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    assert_eq!(
        lebar, 800,
        "tanpa captureBeyondViewport lebar dibatasi viewport"
    );

    let luar = ok_sesi(
        &state,
        4,
        "Page.captureScreenshot",
        json!({"format": "png", "clip": klip, "captureBeyondViewport": true}),
        &sesi,
    );
    assert_eq!(
        luar["receipt"]["clipDipotong"], false,
        "captureBeyondViewport melayani klip utuh: {luar}"
    );
    assert_eq!(luar["receipt"]["captureBeyondViewport"], true);
    let png = cdp::base64::dekode(luar["data"].as_str().expect("data base64")).expect("base64");
    let lebar = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    assert_eq!(
        lebar, 900,
        "dengan captureBeyondViewport lebar mengikuti klip"
    );
}

/// `fromSurface` dicatat di tanda terima: mesin software hanya punya satu
/// permukaan, jadi `false` tidak mengubah raster dan itu dinyatakan, bukan
/// diterima sebagai parameter yang seolah-olah berpengaruh.
#[test]
fn capture_screenshot_mencatat_from_surface_sebagai_diabaikan() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    let jawab = ok_sesi(
        &state,
        3,
        "Page.captureScreenshot",
        json!({"format": "png", "fromSurface": false}),
        &sesi,
    );
    assert_eq!(jawab["receipt"]["fromSurface"], false);
    let diabaikan = jawab["receipt"]["diabaikan"].as_array().expect("daftar");
    assert!(
        diabaikan
            .iter()
            .any(|n| n.as_str().unwrap_or_default().contains("fromSurface")),
        "fromSurface=false harus tercatat: {diabaikan:#?}"
    );
}

/// FR-010 / AC-011: parameter yang tidak dikenal ditolak dengan galat yang
/// **menyebut namanya**, bukan diabaikan senyap.
///
/// Yang diuji bukan cuma "ada galat": klien yang mengirim salah nama harus
/// bisa membaca nama salahnya dari pesan, karena itulah satu-satunya cara ia
/// memperbaiki panggilannya. Parameter sah di perintah yang sama tetap harus
/// lolos, supaya penolakan ini tidak berubah jadi "semua parameter ditolak".
#[test]
fn parameter_tak_dikenal_ditolak_dengan_namanya() {
    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    ok_sesi(
        &state,
        3,
        "Page.setDocumentContent",
        json!({"html": "<body><div style=\"width:20px;height:20px;background:#0f0\"></div></body>"}),
        &sesi,
    );

    // 1. Satu parameter karangan pada perintah yang biasanya berhasil.
    let jawab = balas_sesi(
        &state,
        4,
        "Page.captureScreenshot",
        json!({"format": "png", "karangan": true}),
        &sesi,
    );
    assert_eq!(
        jawab["error"]["code"].as_i64(),
        Some(-32602),
        "parameter asing harus ditolak sebagai invalid params: {jawab}"
    );
    let pesan = jawab["error"]["message"].as_str().unwrap_or_default();
    assert!(
        pesan.contains("karangan"),
        "galat harus menyebut nama parameternya, dapat: {pesan}"
    );
    assert!(
        pesan.contains("format") && pesan.contains("clip"),
        "galat harus menyebut yang didukung, dapat: {pesan}"
    );

    // 2. Parameter sah di perintah yang sama tetap dilayani.
    let lolos = ok_sesi(
        &state,
        5,
        "Page.captureScreenshot",
        json!({"format": "png", "clip": {"x": 0, "y": 0, "width": 20, "height": 20}}),
        &sesi,
    );
    assert!(
        lolos["data"].as_str().is_some_and(|d| !d.is_empty()),
        "parameter sah harus tetap menghasilkan gambar: {lolos}"
    );

    // 3. `Runtime.evaluate` menolak nama asing, bukan mengeksekusi ekspresinya.
    let jawab = balas_sesi(
        &state,
        6,
        "Runtime.evaluate",
        json!({"expression": "1+1", "returnByValue": true, "karangan": 1}),
        &sesi,
    );
    assert_eq!(jawab["error"]["code"].as_i64(), Some(-32602));
    assert!(
        jawab["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("karangan"),
        "evaluate harus menyebut nama parameternya: {jawab}"
    );

    // 4. `Page.navigate` juga, dan `frameId` yang menunjuk bingkai lain ditolak
    //    dengan nama parameternya: satu tab ini hanya punya satu bingkai.
    let jawab = balas_sesi(
        &state,
        7,
        "Page.navigate",
        json!({"url": "about:blank", "karangan": "x"}),
        &sesi,
    );
    assert_eq!(jawab["error"]["code"].as_i64(), Some(-32602));
    assert!(
        jawab["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("karangan"),
        "navigate harus menyebut nama parameternya: {jawab}"
    );
    let jawab = balas_sesi(
        &state,
        8,
        "Page.navigate",
        json!({"url": "about:blank", "frameId": "BUKAN-BINGKAI"}),
        &sesi,
    );
    assert_eq!(jawab["error"]["code"].as_i64(), Some(-32602));
    assert!(
        jawab["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("frameId"),
        "frameId asing harus ditolak dengan namanya: {jawab}"
    );

    // 5. Bingkai yang benar-benar ada tetap dilayani.
    let pohon = ok_sesi(&state, 9, "Page.getFrameTree", json!({}), &sesi);
    let bingkai = pohon["frameTree"]["frame"]["id"]
        .as_str()
        .expect("frame.id")
        .to_owned();
    let ok_nav = ok_sesi(
        &state,
        10,
        "Page.navigate",
        json!({"url": "about:blank", "frameId": bingkai}),
        &sesi,
    );
    assert_eq!(
        ok_nav["frameId"].as_str(),
        Some(bingkai.as_str()),
        "navigasi ke bingkai utama harus berhasil: {ok_nav}"
    );
}

/// NFR-002: `/json/list` tidak boleh menunggu raster.
///
/// Raster memegang kunci state besar selama ia berjalan: detik, bukan
/// milidetik, pada biner debug. Selama daftar target dibaca lewat kunci yang
/// sama, setiap polling `/json/list` ikut menunggu, dan itu terukur p95
/// 200,3 ms pada biner rilis sebelum cermin dipasang (15 Nov 2026).
///
/// Tes ini mengunci janjinya dan membuktikan dirinya sendiri: ia menuntut
/// raster yang diukur benar-benar berjalan lebih lama dari ambang, lalu
/// menuntut setiap pembacaan daftar target di bawah 50 ms. Kalau rasternya
/// kebetulan selesai terlalu cepat, tes gagal di penjaga pertama, bukan lolos
/// diam-diam tanpa pernah menguji tumpang tindih.
#[test]
fn daftar_target_tidak_menunggu_raster() {
    use std::time::{Duration, Instant};

    let state = NativeState::baru();
    let id = tab(&state);
    let sesi = lekat(&state, &id);
    // Dokumen sengaja berat: raster pada satu kotak 60x40 selesai dalam 10 ms
    // di biner debug, terlalu cepat untuk membuktikan tumpang tindih. Tiga
    // ratus kotak berbingkai membuat rasternya berjalan ratusan milidetik,
    // cukup lama bagi pembaca daftar target untuk benar-benar bertabrakan.
    let mut html = String::from("<body style=\"margin:0\">");
    for i in 0..300 {
        html.push_str(&format!(
            "<div style=\"width:120px;height:40px;background-color:#{:06x};border:1px solid #000\">{i}</div>",
            (i * 7919) % 0xffffff
        ));
    }
    html.push_str("</body>");
    ok_sesi(
        &state,
        3,
        "Page.setDocumentContent",
        json!({ "html": html }),
        &sesi,
    );
    // Cermin baru terisi saat kunci dilepas, jadi tab di atas harus sudah
    // terlihat sebelum raster dimulai. Kalau tidak, tes ini mengukur daftar
    // kosong dan tidak membuktikan apa pun.
    assert_eq!(
        state.target().len(),
        1,
        "tab yang baru dibuka harus langsung terlihat di daftar target"
    );

    let mesin = state.clone();
    let sesi_raster = sesi.clone();
    let raster = std::thread::spawn(move || {
        ok_sesi(
            &mesin,
            4,
            "Page.captureScreenshot",
            json!({"format": "png", "clip": {"x": 0, "y": 0, "width": 600, "height": 400}}),
            &sesi_raster,
        );
    });

    let mulai = Instant::now();
    let mut sampel: Vec<Duration> = Vec::new();
    let mut sampel_health: Vec<Duration> = Vec::new();
    while !raster.is_finished() {
        let t0 = Instant::now();
        let daftar = state.target();
        sampel.push(t0.elapsed());
        // `/healthz` membaca jumlah tab, dan jalur itu dulu ikut menunggu
        // raster juga (p50 110,4 ms terukur 15 Nov 2026). Diukur di sini
        // supaya perbaikannya tidak bisa diam-diam dicabut lagi.
        let t1 = Instant::now();
        let jumlah = state.jumlah_tab();
        sampel_health.push(t1.elapsed());
        assert_eq!(jumlah, 1, "jumlah tab harus tetap terbaca selama raster");
        assert_eq!(
            daftar.len(),
            1,
            "selama raster berjalan, tab tetap harus terdaftar"
        );
        if mulai.elapsed() > Duration::from_secs(60) {
            panic!("raster tidak selesai dalam 60 detik; tes ini tidak bisa menilai apa pun");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let durasi_raster = mulai.elapsed();
    raster.join().expect("utas raster");

    assert!(
        durasi_raster > Duration::from_millis(200),
        "raster hanya {durasi_raster:?}, terlalu cepat untuk membuktikan tumpang tindih; \
         naikkan beban tangkapan atau turunkan ambangnya"
    );
    let maks = sampel.iter().max().copied().unwrap_or_default();
    assert!(
        maks < Duration::from_millis(50),
        "daftar target terlambat {maks:?} ({} sampel) selama raster {durasi_raster:?}; \
         NFR-002 menuntut di bawah 50 ms",
        sampel.len()
    );
    let maks_health = sampel_health.iter().max().copied().unwrap_or_default();
    assert!(
        maks_health < Duration::from_millis(50),
        "jumlah tab terlambat {maks_health:?} ({} sampel) selama raster {durasi_raster:?}; \
         /healthz tidak boleh ikut menunggu raster",
        sampel_health.len()
    );

    // Cermin juga harus mengikuti perubahan, bukan hanya tidak memblokir.
    state.tutup(&id).expect("tutup tab");
    assert!(
        state.target().is_empty(),
        "tab yang sudah ditutup tidak boleh tertinggal di daftar target"
    );
}
