//! Uji peta identitas. Semua di sini murni: tanpa soket dan tanpa browser.

use std::collections::BTreeMap;
use std::sync::Arc;

use engine::Engine;
use serde_json::{Value, json};

use super::*;
use crate::engines::EnginePool;

fn pool(ids: &[(&str, u16)], bawaan: &str) -> EnginePool {
    let mesin: BTreeMap<String, Arc<dyn Engine>> = ids
        .iter()
        .map(|(id, port)| {
            (
                (*id).to_owned(),
                Arc::new(testing::FakeEngine::at_port(*port)) as Arc<dyn Engine>,
            )
        })
        .collect();
    EnginePool::new(bawaan, mesin).expect("pool sah")
}

fn dua() -> Peta {
    Peta::baru(&pool(&[("a", 40001), ("b", 40002)], "a"), None)
}

/// Satu balasan `Perintah::Satu`, dipastikan bentuknya.
fn satu(perintah: Perintah) -> (String, Value) {
    let Perintah::Satu { mesin, payload } = perintah else {
        panic!("harus satu pesan, dapat: {perintah:?}");
    };
    (mesin, serde_json::from_str(&payload).expect("payload json"))
}

/// Menyiapkan satu target di mesin lewat jalur yang sama dengan klien nyata:
/// createTarget lalu balasan mesin.
fn target_baru(peta: &mut Peta, id: i64, mesin: Option<&str>, upstream: &str) -> String {
    let mut params = json!({"url": "about:blank"});
    if let Some(mesin) = mesin {
        params["openCrabsOptions"] = json!({"engine": mesin});
    }
    let permintaan = json!({"id": id, "method": "Target.createTarget", "params": params});
    let (mesin, _) = satu(
        peta.terjemahkan_klien(&permintaan.to_string())
            .expect("rute"),
    );
    let balasan = json!({"id": id_naik(peta, &mesin, id), "result": {"targetId": upstream}});
    let Balasan::Teruskan(teks) = peta.terjemahkan_mesin(&mesin, &balasan.to_string()) else {
        panic!("balasan harus diteruskan");
    };
    let nilai: Value = serde_json::from_str(&teks).expect("json");
    assert_eq!(nilai["id"], id, "id harus kembali ke ruang klien");
    nilai["result"]["targetId"]
        .as_str()
        .expect("ada")
        .to_owned()
}

/// Id yang kita kirim ke mesin untuk satu permintaan klien. Dibaca dari pesan
/// yang benar-benar keluar, bukan dihitung ulang di test.
fn id_naik(peta: &mut Peta, mesin: &str, klien_id: i64) -> i64 {
    peta.id_masuk(mesin, klien_id)
}

#[test]
fn id_diterjemahkan_bolak_balik_per_mesin() {
    let mut peta = dua();
    let (mesin, pesan) = satu(
        peta.terjemahkan_klien(r#"{"id":5,"method":"Page.navigate","params":{"url":"x"}}"#)
            .expect("rute"),
    );
    assert_eq!(mesin, "a", "tanpa petunjuk, pesan mendarat di mesin bawaan");
    assert_eq!(pesan["id"], 1, "id mesin mulai dari 1, bukan 5");

    let Balasan::Teruskan(teks) = peta.terjemahkan_mesin("a", r#"{"id":1,"result":{}}"#) else {
        panic!("harus diteruskan");
    };
    let nilai: Value = serde_json::from_str(&teks).expect("json");
    assert_eq!(nilai["id"], 5, "klien harus melihat id miliknya sendiri");
}

#[test]
fn id_klien_yang_sama_di_dua_mesin_tidak_tertukar() {
    let mut peta = dua();
    let a = satu(
        peta.terjemahkan_klien(r#"{"id":7,"method":"Browser.getVersion"}"#)
            .expect("rute"),
    );
    let b = satu(
        peta.terjemahkan_klien(
            r#"{"id":7,"method":"Target.createTarget","params":{"url":"about:blank","openCrabsOptions":{"engine":"b"}}}"#,
        )
        .expect("rute"),
    );
    assert_eq!(a.0, "a");
    assert_eq!(b.0, "b");
    assert_eq!(a.1["id"], 1);
    assert_eq!(b.1["id"], 1, "tiap mesin punya penghitung sendiri");

    // Balasan id 1 dari mesin B harus jadi 7, bukan tertukar dengan punya A.
    let Balasan::Teruskan(teks) = peta.terjemahkan_mesin("b", r#"{"id":1,"result":{}}"#) else {
        panic!("harus diteruskan");
    };
    let nilai: Value = serde_json::from_str(&teks).expect("json");
    assert_eq!(nilai["id"], 7);
}

#[test]
fn opencrabs_options_dibuang_dan_memilih_mesin() {
    let mut peta = dua();
    let (mesin, pesan) = satu(
        peta.terjemahkan_klien(
            r#"{"id":1,"method":"Target.createTarget","params":{"url":"about:blank","openCrabsOptions":{"engine":"b","profile":"toko","ttlSecs":120}}}"#,
        )
        .expect("rute"),
    );
    assert_eq!(mesin, "b");
    assert_eq!(pesan["params"]["url"], "about:blank");
    assert!(
        pesan["params"].get("openCrabsOptions").is_none(),
        "opsi internal tidak boleh sampai ke mesin: {pesan}"
    );
}

#[test]
fn mesin_yang_tidak_terpasang_ditolak_dengan_daftar() {
    let mut peta = dua();
    let err = peta
        .terjemahkan_klien(
            r#"{"id":1,"method":"Target.createTarget","params":{"url":"about:blank","openCrabsOptions":{"engine":"oxi"}}}"#,
        )
        .expect_err("harus ditolak");
    assert_eq!(
        err,
        MuxError::MesinTakAda {
            diminta: "oxi".to_owned(),
            tersedia: "a, b".to_owned()
        }
    );
    let teks = err.to_string();
    assert!(
        teks.contains("oxi") && teks.contains("a, b"),
        "pesan: {teks}"
    );
}

#[test]
fn sesi_dipetakan_dan_yang_tak_dikenal_ditolak() {
    let mut peta = dua();
    let tid = target_baru(&mut peta, 1, None, "AAA");

    let (mesin, attach) = satu(
        peta.terjemahkan_klien(
            &json!({"id": 2, "method": "Target.attachToTarget", "params": {"targetId": tid, "flatten": true}})
                .to_string(),
        )
        .expect("attach"),
    );
    assert_eq!(
        attach["params"]["targetId"], "AAA",
        "targetId diterjemahkan ke mesin"
    );

    let Balasan::Teruskan(teks) = peta.terjemahkan_mesin(
        &mesin,
        &json!({"id": attach["id"], "result": {"sessionId": "SESI-MESIN"}}).to_string(),
    ) else {
        panic!("balasan attach harus diteruskan");
    };
    let balasan: Value = serde_json::from_str(&teks).expect("json");
    let sid = balasan["result"]["sessionId"]
        .as_str()
        .expect("ada")
        .to_owned();
    assert_eq!(sid, "s1");
    assert_ne!(
        sid, "SESI-MESIN",
        "sessionId mesin tidak boleh bocor ke klien"
    );

    let (mesin, kirim) = satu(
        peta.terjemahkan_klien(
            &json!({"id": 3, "method": "Page.navigate", "sessionId": sid, "params": {"url": "u"}})
                .to_string(),
        )
        .expect("rute sesi"),
    );
    assert_eq!(mesin, "a");
    assert_eq!(kirim["sessionId"], "SESI-MESIN");

    let err = peta
        .terjemahkan_klien(
            &json!({"id": 4, "method": "Page.navigate", "sessionId": "s99"}).to_string(),
        )
        .expect_err("sesi karangan harus ditolak");
    assert_eq!(err, MuxError::SesiTakDikenal("s99".to_owned()));
}

#[test]
fn target_tak_dikenal_ditolak_tetapi_prefiks_mesin_diterima() {
    let mut peta = dua();
    let err = peta
        .terjemahkan_klien(
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"t99"}}"#,
        )
        .expect_err("harus ditolak");
    assert_eq!(err, MuxError::TargetTakDikenal("t99".to_owned()));

    // Bentuk dari /json/list: klien boleh memakainya tanpa pernah melihat event.
    let (mesin, pesan) = satu(
        peta.terjemahkan_klien(
            r#"{"id":2,"method":"Target.attachToTarget","params":{"targetId":"b~HEX"}}"#,
        )
        .expect("prefiks mesin"),
    );
    assert_eq!(mesin, "b");
    assert_eq!(pesan["params"]["targetId"], "HEX");
}

#[test]
fn event_mesin_dipetakan_ke_ruang_id_klien() {
    let mut peta = dua();
    let Balasan::Teruskan(teks) = peta.terjemahkan_mesin(
        "a",
        &json!({
            "method": "Target.targetCreated",
            "params": {"targetInfo": {"targetId": "HEX1", "type": "page", "url": "about:blank"}}
        })
        .to_string(),
    ) else {
        panic!("event harus diteruskan");
    };
    let nilai: Value = serde_json::from_str(&teks).expect("json");
    assert_eq!(nilai["params"]["targetInfo"]["targetId"], "t1");
    assert_eq!(nilai["params"]["targetInfo"]["url"], "about:blank");

    // Event kedua untuk target yang sama tidak boleh membuat id baru.
    let Balasan::Teruskan(teks) = peta.terjemahkan_mesin(
        "a",
        &json!({"method": "Target.targetInfoChanged", "params": {"targetInfo": {"targetId": "HEX1"}}})
            .to_string(),
    ) else {
        panic!("event harus diteruskan");
    };
    let nilai: Value = serde_json::from_str(&teks).expect("json");
    assert_eq!(nilai["params"]["targetInfo"]["targetId"], "t1");
}

#[test]
fn get_targets_digabung_dari_semua_mesin() {
    let mut peta = dua();
    let Perintah::Gabung { klien_id, pesan } = peta
        .terjemahkan_klien(r#"{"id":9,"method":"Target.getTargets"}"#)
        .expect("rute")
    else {
        panic!("getTargets harus digabung");
    };
    assert_eq!(klien_id, 9);
    assert_eq!(pesan.len(), 2, "satu permintaan per mesin");

    // Balasan dari mesin pertama ditandai sebagai potongan gabungan.
    let Balasan::Gabung {
        klien_id: id,
        hasil,
    } = peta.terjemahkan_mesin(
        "a",
        &json!({"id": 1, "result": {"targetInfos": [{"targetId": "HEX-A", "type": "page"}]}})
            .to_string(),
    )
    else {
        panic!("harus ditandai gabungan");
    };
    assert_eq!(id, 9);
    assert_eq!(hasil["targetInfos"][0]["targetId"], "t1");
}

#[test]
fn set_discover_targets_disiarkan_ke_semua_mesin() {
    let mut peta = dua();
    let Perintah::Siar { klien_id, pesan } = peta
        .terjemahkan_klien(
            r#"{"id":10,"method":"Target.setDiscoverTargets","params":{"discover":true}}"#,
        )
        .expect("rute")
    else {
        panic!("setDiscoverTargets harus disiarkan");
    };
    assert_eq!(klien_id, 10);
    assert_eq!(pesan.len(), 2);
    assert!(pesan.iter().all(|(_, p)| p.contains("\"discover\":true")));
    assert_eq!(pesan[0].1.matches("\"id\":1").count(), 1);
}

#[test]
fn lepas_mesin_hanya_melepas_sesi_mesin_itu() {
    let mut peta = dua();
    // Satu sesi di mesin a, satu di mesin b.
    let tid_a = target_baru(&mut peta, 1, Some("a"), "HEX-A");
    let tid_b = target_baru(&mut peta, 2, Some("b"), "HEX-B");
    for (id, tid) in [(3, tid_a.clone()), (4, tid_b.clone())] {
        let (_, attach) = satu(
            peta.terjemahkan_klien(
                &json!({"id": id, "method": "Target.attachToTarget", "params": {"targetId": tid}})
                    .to_string(),
            )
            .expect("attach"),
        );
        let (mesin, _) = if id == 3 { ("a", ()) } else { ("b", ()) };
        peta.terjemahkan_mesin(
            mesin,
            &json!({"id": attach["id"], "result": {"sessionId": format!("UP-{id}")}}).to_string(),
        );
    }
    assert_eq!(peta.sesi_per_mesin().get("a"), Some(&1));
    assert_eq!(peta.sesi_per_mesin().get("b"), Some(&1));

    let lepas = peta.lepas_mesin("a");
    assert_eq!(lepas.len(), 1, "hanya sesi mesin a yang dilepas: {lepas:?}");
    let nilai: Value = serde_json::from_str(&lepas[0]).expect("json");
    assert_eq!(nilai["method"], "Target.detachedFromTarget");
    assert_eq!(nilai["params"]["targetId"], tid_a);
    assert_eq!(peta.sesi_per_mesin().get("a"), None);
    assert_eq!(
        peta.sesi_per_mesin().get("b"),
        Some(&1),
        "sesi mesin b tidak boleh ikut hilang"
    );
}

#[test]
fn pesan_bukan_json_ditolak_bukan_ditelan() {
    let mut peta = dua();
    assert_eq!(
        peta.terjemahkan_klien("bukan json")
            .expect_err("harus ditolak"),
        MuxError::BukanObjek
    );
    assert_eq!(
        peta.terjemahkan_klien("[1,2,3]")
            .expect_err("harus ditolak"),
        MuxError::BukanObjek
    );
}

// ── AC-007: routing host ke mesin ───────────────────────────────────────────

/// Pool dua instance dengan **jenis** berbeda, seperti daemon nyata yang
/// menjalankan `chrome_shell` dan `chrome_full` bersamaan.
fn pool_dua_jenis() -> EnginePool {
    let mut slots = BTreeMap::new();
    for (id, jenis, port) in [
        ("shell", "chrome-shell", 40011_u16),
        ("full", "chrome-full", 40012),
    ] {
        let engine: Arc<dyn Engine> = Arc::new(testing::FakeEngine::at_port(port));
        slots.insert(
            id.to_owned(),
            Arc::new(engine::Slot::siap(
                id,
                jenis,
                engine.spec().clone(),
                Arc::new(engine::PenggerakShell),
                Arc::clone(&engine),
                engine::slot::ms_sekarang(),
            )),
        );
    }
    EnginePool::dari_slot("shell", slots).expect("pool sah")
}

/// Pemilih tetap: mengembalikan satu jenis, atau satu penolakan.
struct PemilihUji {
    jenis: Option<&'static str>,
    tolak: Option<&'static str>,
}

impl PemilihUji {
    fn jenis(jenis: &'static str) -> Arc<dyn crate::PemilihMesin> {
        Arc::new(Self {
            jenis: Some(jenis),
            tolak: None,
        })
    }

    fn tolak(pesan: &'static str) -> Arc<dyn crate::PemilihMesin> {
        Arc::new(Self {
            jenis: None,
            tolak: Some(pesan),
        })
    }

    fn tanpa_aturan() -> Arc<dyn crate::PemilihMesin> {
        Arc::new(Self {
            jenis: None,
            tolak: None,
        })
    }
}

impl crate::PemilihMesin for PemilihUji {
    fn jenis_untuk_host(&self, _host: &str) -> Result<Option<crate::RoutingHost>, String> {
        if let Some(pesan) = self.tolak {
            return Err(pesan.to_owned());
        }
        Ok(self.jenis.map(|jenis| crate::RoutingHost {
            jenis: jenis.to_owned(),
            aturan: "host_rule".to_owned(),
        }))
    }
}

/// `Target.createTarget` seperti yang dikirim klien: url + opsi opsional.
fn buka(peta: &mut Peta, url: &str, mesin: Option<&str>) -> Result<String, MuxError> {
    let mut params = json!({"url": url});
    if let Some(mesin) = mesin {
        params["openCrabsOptions"] = json!({"engine": mesin});
    }
    let pesan = json!({"id": 1, "method": "Target.createTarget", "params": params}).to_string();
    Ok(satu(peta.terjemahkan_klien(&pesan)?).0)
}

#[test]
fn host_beraturan_mendarat_di_instance_jenis_yang_ditunjuk() {
    let pool = pool_dua_jenis();
    let mut peta = Peta::baru(&pool, Some(PemilihUji::jenis("chrome-full")));
    assert_eq!(
        buka(&mut peta, "http://toko.test/halaman", None),
        Ok("full".to_owned()),
        "chrome-full yang dipilih aturan harus mendarat di instance jenis itu"
    );
}

#[test]
fn tanpa_aturan_host_mesin_bawaan_yang_dipakai() {
    let pool = pool_dua_jenis();
    let mut peta = Peta::baru(&pool, Some(PemilihUji::tanpa_aturan()));
    assert_eq!(
        buka(&mut peta, "http://lain.test/", None),
        Ok("shell".to_owned())
    );

    // Url yang tidak punya host (about:blank) tidak boleh mengubah apa pun:
    // tidak ada yang bisa dicocokkan aturan.
    assert_eq!(buka(&mut peta, "about:blank", None), Ok("shell".to_owned()));
}

#[test]
fn permintaan_eksplisit_menang_atas_aturan_host() {
    let pool = pool_dua_jenis();
    let mut peta = Peta::baru(&pool, Some(PemilihUji::jenis("chrome-full")));
    assert_eq!(
        buka(&mut peta, "http://toko.test/", Some("shell")),
        Ok("shell".to_owned()),
        "mesin yang disebut klien tidak boleh ditimpa aturan"
    );
}

#[test]
fn aturan_ke_jenis_yang_tidak_terpasang_ditolak_bukan_diganti() {
    let pool = pool_dua_jenis();
    let mut peta = Peta::baru(&pool, Some(PemilihUji::jenis("oxi")));
    let err = buka(&mut peta, "http://toko.test/", None).expect_err("oxi tidak ada di pool");
    let MuxError::Routing(pesan) = err else {
        panic!("harus penolakan routing, dapat: {err:?}");
    };
    assert!(pesan.contains("oxi"), "sebut jenisnya: {pesan}");
    assert!(pesan.contains("full, shell"), "sebut yang ada: {pesan}");
}

#[test]
fn penolakan_aturan_diteruskan_apa_adanya() {
    let pool = pool_dua_jenis();
    let mut peta = Peta::baru(
        &pool,
        Some(PemilihUji::tolak("host stealth butuh chrome-full")),
    );
    let err =
        buka(&mut peta, "http://challenges.cloudflare.com/", None).expect_err("aturan menolak");
    assert_eq!(
        err,
        MuxError::Routing("host stealth butuh chrome-full".to_owned())
    );
}
