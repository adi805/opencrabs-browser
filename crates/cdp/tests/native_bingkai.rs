//! Tes pengambilan dokumen `<iframe>`.
//!
//! Servernya lokal dan dijalankan di dalam tes ini, sama seperti
//! `tests/jaringan.rs`: tidak ada situs sungguhan yang dihubungi.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use cdp::native_wire::bingkai;
use engine::native::Document;

/// Server HTTP satu pakai. Menerima `n` koneksi lalu berhenti.
fn server(n: usize, jawab: impl Fn(&str) -> Vec<u8> + Send + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("port bebas");
    let alamat = listener.local_addr().expect("alamat lokal").to_string();
    std::thread::spawn(move || {
        for _ in 0..n {
            let Ok((mut soket, _)) = listener.accept() else {
                return;
            };
            let jalur = {
                let pembaca = BufReader::new(&mut soket);
                let mut jalur = String::new();
                for (nomor, baris_masuk) in pembaca.lines().enumerate() {
                    let Ok(baris_masuk) = baris_masuk else { break };
                    if nomor == 0 {
                        jalur = baris_masuk
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or("/")
                            .to_owned();
                    }
                    if baris_masuk.is_empty() {
                        break;
                    }
                }
                jalur
            };
            let isi = jawab(&jalur);
            let _ = soket.write_all(&isi);
            let _ = soket.flush();
        }
    });
    alamat
}

fn html(body: &str) -> Vec<u8> {
    let mut kepala = String::from(
        "HTTP/1.1 200 OK\r\nConnection: close\r\ncontent-type: text/html; charset=utf-8\r\n",
    );
    kepala.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut out = kepala.into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

#[tokio::test]
async fn iframe_same_origin_diambil_dan_lintas_asal_dilewati() {
    let asal = server(1, |jalur| {
        assert_eq!(jalur, "/anak", "hanya iframe same-origin yang diambil");
        html("<p>isi anak</p>")
    });
    let induk = format!("http://{asal}/induk");
    let isi = format!(
        r#"<body>
             <iframe id="sama" src="/anak"></iframe>
             <iframe id="luar" src="http://localhost:{port}/anak"></iframe>
             <iframe id="dok" srcdoc="<b>srcdoc</b>"></iframe>
             <iframe id="kosong"></iframe>
           </body>"#,
        port = asal.rsplit(':').next().unwrap()
    );
    let dokumen = Document::parse_html(&isi).expect("HTML induk harus terurai");
    let hasil = bingkai::ambil(&induk, &dokumen).await;

    // Hanya satu iframe yang butuh jaringan, jadi hanya satu yang diambil.
    assert_eq!(hasil.dokumen.len(), 1, "catatan: {:?}", hasil.catatan);
    assert_eq!(hasil.dokumen[0].0, format!("http://{asal}/anak"));
    assert!(hasil.dokumen[0].1.contains("isi anak"));

    // Lintas asal dicatat beserta alasannya, bukan dilewati diam-diam.
    assert!(
        hasil
            .catatan
            .iter()
            .any(|c| c.contains("lintas asal") || c.contains("asal") || c.contains("localhost")),
        "lintas asal harus tercatat: {:?}",
        hasil.catatan
    );
    // `srcdoc` dan iframe kosong tidak menyentuh jaringan sama sekali.
    assert_eq!(
        hasil.catatan.len(),
        1,
        "hanya iframe lintas asal yang dicatat: {:?}",
        hasil.catatan
    );
}

#[tokio::test]
async fn iframe_tanpa_dasar_ditolak_bukan_ditebak() {
    // Tanpa URL induk, asal anak tidak bisa dibandingkan dengan apa pun, jadi
    // semua `src` dianggap lintas asal. Menebak "pasti same-origin" berarti
    // membuka batas yang seharusnya dijaga.
    let dokumen = Document::parse_html(r#"<iframe src="http://contoh.test/anak"></iframe>"#)
        .expect("HTML harus terurai");
    let hasil = bingkai::ambil("about:blank", &dokumen).await;
    assert!(hasil.dokumen.is_empty());
    assert_eq!(hasil.catatan.len(), 1, "{:?}", hasil.catatan);
    assert!(hasil.catatan[0].contains("asal"), "{:?}", hasil.catatan);
}

#[tokio::test]
async fn batas_jumlah_iframe_dilaporkan() {
    // Sepuluh iframe dengan URL berbeda, tanpa server: semuanya akan gagal
    // diambil, tapi yang lebih penting adalah yang melewati batas dicatat
    // sebagai "melewati batas", bukan sebagai kegagalan jaringan.
    let mut isi = String::from("<body>");
    for i in 0..bingkai::MAKS_BINGKAI + 2 {
        isi.push_str(&format!(r#"<iframe src="/anak-{i}"></iframe>"#));
    }
    isi.push_str("</body>");
    let dokumen = Document::parse_html(&isi).expect("HTML harus terurai");
    let hasil = bingkai::ambil("http://127.0.0.1:1/induk", &dokumen).await;
    assert!(
        hasil.catatan.iter().any(|c| c.contains("melewati batas")),
        "batas iframe harus dilaporkan: {:?}",
        hasil.catatan
    );
}
