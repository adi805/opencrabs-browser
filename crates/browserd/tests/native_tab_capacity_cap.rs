//! AC-010: tab ke-100 ditolak dengan galat kapasitas, dan tab yang sudah ada
//! tidak ikut rusak.
//!
//! Batas 99 ditegakkan di `TabPool`, bukan di lapisan protokol. Tes ini
//! memanggil protokolnya seperti klien sungguhan, lalu memeriksa bahwa
//! penolakannya **tidak** menghapus atau menyingkirkan tab yang sudah ada:
//! penolakan yang membuang tab lain lebih buruk daripada penolakan yang jelas.

use cdp::native_wire::{MAKS_TAB, NativeState};
use serde_json::{Value, json};

fn frame(id: i64, metode: &str, params: Value) -> String {
    json!({"id": id, "method": metode, "params": params}).to_string()
}

/// Mengirim satu frame dan mengembalikan seluruh balasan, termasuk galat.
fn balas(state: &NativeState, teks: &str) -> Value {
    let jawaban = state.tangani(teks).expect("balasan");
    serde_json::from_str(&jawaban).expect("balasan JSON")
}

#[tokio::test]
async fn tab_ke_seratus_ditolak_dan_sembilan_puluh_sembilan_tetap_ada() {
    let state = NativeState::baru();
    assert_eq!(MAKS_TAB, 99, "batas yang dijanjikan PRD adalah 99");

    let mut id_tab = Vec::with_capacity(MAKS_TAB);
    for nomor in 0..MAKS_TAB {
        let jawab = balas(
            &state,
            &frame(
                (nomor + 1) as i64,
                "Target.createTarget",
                json!({"url": "about:blank"}),
            ),
        );
        if let Some(galat) = jawab.get("error") {
            panic!("tab ke-{} ditolak terlalu awal: {galat}", nomor + 1);
        }
        id_tab.push(
            jawab["result"]["targetId"]
                .as_str()
                .expect("targetId")
                .to_owned(),
        );
    }
    assert_eq!(state.jumlah_tab(), MAKS_TAB);

    let lebih = balas(
        &state,
        &frame(100, "Target.createTarget", json!({"url": "about:blank"})),
    );
    let pesan = lebih["error"]["message"].as_str().unwrap_or_default();
    assert!(
        pesan.contains(&MAKS_TAB.to_string()),
        "galat kapasitas harus menyebut angka batasnya, dapat: {lebih}"
    );
    assert_eq!(
        lebih["error"]["code"].as_i64(),
        Some(-32000),
        "penolakan kapasitas adalah galat server, bukan method-not-found"
    );

    // Tab ke-100 tidak meninggalkan jejak, dan yang lama tidak ikut hilang.
    assert_eq!(state.jumlah_tab(), MAKS_TAB, "tidak boleh ada tab ke-100");
    let daftar = state.target();
    assert_eq!(daftar.len(), MAKS_TAB);
    let id_di_state = daftar
        .iter()
        .filter_map(|tab| tab["id"].as_str().map(str::to_owned))
        .collect::<std::collections::BTreeSet<_>>();
    for id in &id_tab {
        assert!(
            id_di_state.contains(id),
            "tab {id} hilang setelah penolakan kapasitas"
        );
    }
}

#[tokio::test]
async fn setelah_satu_tab_ditutup_slotnya_boleh_dipakai_lagi() {
    let state = NativeState::baru();
    let mut terakhir = String::new();
    for nomor in 0..MAKS_TAB {
        let jawab = balas(
            &state,
            &frame(
                (nomor + 1) as i64,
                "Target.createTarget",
                json!({"url": "about:blank"}),
            ),
        );
        terakhir = jawab["result"]["targetId"]
            .as_str()
            .expect("targetId")
            .to_owned();
    }
    assert_eq!(state.jumlah_tab(), MAKS_TAB);

    let tutup = balas(
        &state,
        &frame(100, "Target.closeTarget", json!({"targetId": terakhir})),
    );
    assert!(
        tutup.get("error").is_none(),
        "menutup tab harus berhasil: {tutup}"
    );
    assert_eq!(state.jumlah_tab(), MAKS_TAB - 1);

    let lagi = balas(
        &state,
        &frame(101, "Target.createTarget", json!({"url": "about:blank"})),
    );
    assert!(
        lagi.get("error").is_none(),
        "setelah satu tab ditutup, tab baru harus diterima: {lagi}"
    );
    assert_eq!(state.jumlah_tab(), MAKS_TAB);
}
