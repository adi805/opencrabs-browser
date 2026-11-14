//! `fetch()` dari skrip halaman, ujung ke ujung lewat gerbang jaringan.
//!
//! Yang diuji di sini rantai penuhnya, bukan satu lapisan: `Runtime.evaluate`
//! → isolat → `fetch` → [`PengambilJaringan`] → kebijakan → `ureq` → server
//! HTTP sungguhan di `127.0.0.1`. Kalau salah satu sambungan lepas (isolat
//! tidak dipasangi hook, pool lupa meneruskan gerbang, atau kebijakan tidak
//! dipanggil), tes ini gagal.
//!
//! Server-nya sengaja ditulis dengan `std::net`, bukan dependensi baru: yang
//! perlu dibuktikan hanya "badan jawaban sampai ke skrip", dan satu server
//! mainan lebih mudah dibaca daripada satu framework di dev-dependencies.
//!
//! `fetch` di mesin ini **sinkron**: tidak ada antrean microtask di isolat,
//! jadi promise tidak akan pernah selesai. Karena itu `text()` mengembalikan
//! string langsung dan tesnya memakai bentuk itu.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

use cdp::native_wire::{KebijakanJaringan, NativeState, PengambilJaringan, pabrik_memori};
use engine::native::ambil::Pengambil;
use serde_json::{Value, json};

/// Server yang menjawab `jumlah` permintaan dengan badan yang sama, lalu
/// thread-nya selesai.
///
/// `Connection: close` dipakai supaya pembacaan di sisi klien berakhir di EOF,
/// bukan menunggu koneksi yang tidak pernah ditutup.
fn server(badan: &'static str, jumlah: usize) -> u16 {
    let pendengar = TcpListener::bind("127.0.0.1:0").expect("bind server uji");
    let port = pendengar.local_addr().expect("alamat server").port();
    thread::spawn(move || {
        for _ in 0..jumlah {
            let Ok((mut soket, _)) = pendengar.accept() else {
                return;
            };
            let mut buf = [0u8; 2048];
            let _ = soket.read(&mut buf);
            let jawaban = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{badan}",
                badan.len()
            );
            let _ = soket.write_all(jawaban.as_bytes());
            let _ = soket.flush();
        }
    });
    port
}

fn state_dengan(kebijakan: KebijakanJaringan) -> NativeState {
    let pengambil: Arc<dyn Pengambil> = Arc::new(PengambilJaringan::baru(kebijakan));
    NativeState::dengan_ambil(pabrik_memori(), pengambil)
}

fn balas(state: &NativeState, id: i64, metode: &str, params: Value, sesi: Option<&str>) -> Value {
    let mut frame = json!({"id": id, "method": metode, "params": params});
    if let Some(sesi) = sesi {
        frame["sessionId"] = json!(sesi);
    }
    let jawaban = state.tangani(&frame.to_string()).expect("balasan");
    serde_json::from_str(&jawaban).expect("balasan JSON")
}

fn ok(state: &NativeState, id: i64, metode: &str, params: Value, sesi: Option<&str>) -> Value {
    let nilai = balas(state, id, metode, params, sesi);
    assert!(
        nilai.get("error").is_none(),
        "{metode} harus berhasil, dapat: {nilai}"
    );
    nilai["result"].clone()
}

/// Membuka tab, melengkapinya, dan mengembalikan id sesinya.
fn buka_tab(state: &NativeState) -> String {
    let target = ok(
        state,
        1,
        "Target.createTarget",
        json!({"url": "about:blank"}),
        None,
    )["targetId"]
        .as_str()
        .expect("targetId")
        .to_owned();
    ok(
        state,
        2,
        "Target.attachToTarget",
        json!({"targetId": target}),
        None,
    )["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_owned()
}

/// Mengevaluasi ekspresi di tab dan mengembalikan `RemoteObject`-nya.
fn evaluasi(state: &NativeState, sesi: &str, ekspresi: &str) -> Value {
    ok(
        state,
        3,
        "Runtime.evaluate",
        json!({"expression": ekspresi}),
        Some(sesi),
    )["result"]
        .clone()
}

#[test]
fn fetch_yang_diizinkan_mengembalikan_badan_dari_server_sungguhan() {
    let port = server("isi dari server uji", 2);
    let kebijakan = KebijakanJaringan::baru(vec!["127.0.0.1".to_owned()], Vec::new(), Vec::new());
    let state = state_dengan(kebijakan);
    let sesi = buka_tab(&state);

    let badan = evaluasi(
        &state,
        &sesi,
        &format!("fetch('http://127.0.0.1:{port}/isi').text()"),
    );
    assert_eq!(badan["type"], json!("string"), "dapat: {badan}");
    assert_eq!(badan["value"], json!("isi dari server uji"));

    let status = evaluasi(
        &state,
        &sesi,
        &format!("fetch('http://127.0.0.1:{port}/isi').status"),
    );
    // JSON tidak membedakan 200 dari 200.0, tapi `json!` membedakannya:
    // `json!(200)` adalah integer dan `json!(200.0)` adalah float. Nilai yang
    // datang dari isolat selalu float karena JS hanya punya satu tipe angka,
    // jadi yang dibandingkan harus bentuk yang sama.
    assert_eq!(status["value"], json!(200.0), "dapat: {status}");
}

#[test]
fn fetch_ke_host_di_luar_daftar_izin_ditolak_dengan_alasannya() {
    let state = state_dengan(KebijakanJaringan::default());
    let sesi = buka_tab(&state);

    let hasil = evaluasi(
        &state,
        &sesi,
        "try { fetch('https://contoh.test/'); 'tidak menolak' } \
         catch (e) { e.name + ': ' + e.message }",
    );
    let pesan = hasil["value"].as_str().unwrap_or_default().to_owned();
    assert!(
        pesan.starts_with("TypeError: "),
        "penolakan harus berupa galat yang bisa ditangkap skrip: {pesan}"
    );
    assert!(
        pesan.contains("daftar izin"),
        "alasan harus menyebut daftar izin: {pesan}"
    );
    assert!(
        pesan.contains("contoh.test"),
        "alasan harus menyebut host yang ditolak: {pesan}"
    );
}

#[test]
fn daftar_tolak_menang_lewat_runtime_evaluate() {
    let port = server("tidak boleh terbaca", 1);
    let kebijakan = KebijakanJaringan::baru(
        vec!["127.0.0.1".to_owned()],
        vec!["127.0.0.1".to_owned()],
        Vec::new(),
    );
    let state = state_dengan(kebijakan);
    let sesi = buka_tab(&state);

    let hasil = evaluasi(
        &state,
        &sesi,
        &format!(
            "try {{ fetch('http://127.0.0.1:{port}/'); 'tidak menolak' }} \
             catch (e) {{ e.message }}"
        ),
    );
    let pesan = hasil["value"].as_str().unwrap_or_default().to_owned();
    assert!(
        pesan.contains("daftar tolak"),
        "alasan harus menyebut daftar tolak: {pesan}"
    );
}
