//! Tes pengambilan dokumen dari jaringan.
//!
//! Servernya lokal dan dijalankan di dalam tes ini: tidak ada situs sungguhan
//! yang dihubungi, jadi tes ini jalan tanpa internet dan tidak bergantung pada
//! apa pun di luar repo.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::Duration;

use cdp::native_wire::gaya;
use cdp::native_wire::jaringan::{self, Batas};

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

fn respons(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut kepala = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
    for (nama, nilai) in headers {
        kepala.push_str(&format!("{nama}: {nilai}\r\n"));
    }
    kepala.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut out = kepala.into_bytes();
    out.extend_from_slice(body);
    out
}

fn html(body: &str) -> Vec<u8> {
    respons(
        "200 OK",
        &[("content-type", "text/html; charset=utf-8")],
        body.as_bytes(),
    )
}

#[test]
fn butuh_jaringan_hanya_untuk_http_dan_https() {
    assert!(jaringan::butuh_jaringan("http://contoh.test/"));
    assert!(jaringan::butuh_jaringan("HTTPS://contoh.test/"));
    assert!(!jaringan::butuh_jaringan("about:blank"));
    assert!(!jaringan::butuh_jaringan("data:text/html,<p>x</p>"));
    assert!(!jaringan::butuh_jaringan("ftp://contoh.test/"));
}

#[test]
fn muat_mengambil_html_dari_server_lokal() {
    let isi = "<html><head><title>Halo</title></head><body><p>isi</p></body></html>";
    let alamat = server(1, move |_| html(isi));
    let url = format!("http://{alamat}/halaman");
    let ambil = jaringan::muat_blokir(&url, Batas::default()).expect("dokumen terambil");
    assert_eq!(ambil.status, 200);
    assert_eq!(ambil.url, url);
    assert_eq!(ambil.html, isi);
    assert_eq!(ambil.byte, isi.len());
    assert_eq!(ambil.charset, "utf-8");
    assert!(ambil.content_type.starts_with("text/html"));
}

#[tokio::test]
async fn muat_async_mengembalikan_dokumen_yang_sama() {
    let alamat = server(1, |_| html("<p>async</p>"));
    let url = format!("http://{alamat}/");
    let ambil = jaringan::muat(&url, Batas::default())
        .await
        .expect("dokumen terambil");
    assert_eq!(ambil.html, "<p>async</p>");
}

#[test]
fn muat_mengikuti_pengalihan_dan_melaporkan_url_akhir() {
    let alamat = server(2, |jalur| {
        if jalur == "/awal" {
            respons("302 Found", &[("location", "/akhir")], b"")
        } else {
            html("<p>akhir</p>")
        }
    });
    let url = format!("http://{alamat}/awal");
    let ambil = jaringan::muat_blokir(&url, Batas::default()).expect("dokumen terambil");
    assert_eq!(ambil.url, format!("http://{alamat}/akhir"));
    assert_eq!(ambil.html, "<p>akhir</p>");
}

#[test]
fn muat_menolak_skema_di_luar_http() {
    let galat = jaringan::muat_blokir("ftp://contoh.test/berkas", Batas::default())
        .expect_err("ftp harus ditolak");
    assert!(
        galat.pesan.contains("bukan URL http/https"),
        "pesan: {}",
        galat.pesan
    );
}

#[test]
fn muat_menolak_content_type_bukan_html() {
    let alamat = server(1, |_| {
        respons(
            "200 OK",
            &[("content-type", "application/pdf")],
            b"%PDF-1.7 bukan html",
        )
    });
    let url = format!("http://{alamat}/berkas.pdf");
    let galat = jaringan::muat_blokir(&url, Batas::default()).expect_err("pdf harus ditolak");
    assert!(
        galat.pesan.contains("application/pdf"),
        "pesan: {}",
        galat.pesan
    );
}

#[test]
fn muat_membatasi_ukuran_badan() {
    let badan = "x".repeat(4096);
    let alamat = server(1, move |_| html(&badan));
    let url = format!("http://{alamat}/besar");
    let batas = Batas {
        byte: 64,
        ..Batas::default()
    };
    let galat = jaringan::muat_blokir(&url, batas).expect_err("badan besar harus ditolak");
    assert!(
        galat.pesan.contains("melebihi batas 64 byte"),
        "pesan: {}",
        galat.pesan
    );
}

#[test]
fn muat_mendekode_charset_dari_header() {
    // "café" dalam ISO-8859-1: 0xE9, bukan dua byte UTF-8.
    let badan: &[u8] = b"<p>caf\xe9</p>";
    let alamat = server(1, |_| {
        respons(
            "200 OK",
            &[("content-type", "text/html; charset=ISO-8859-1")],
            badan,
        )
    });
    let url = format!("http://{alamat}/latin");
    let ambil = jaringan::muat_blokir(&url, Batas::default()).expect("dokumen terambil");
    assert_eq!(ambil.charset, "iso-8859-1");
    assert_eq!(ambil.html, "<p>café</p>");
}

#[test]
fn muat_melaporkan_status_galat_tanpa_membuang_badannya() {
    let alamat = server(1, |_| {
        respons(
            "404 Not Found",
            &[("content-type", "text/html")],
            b"<h1>tidak ada</h1>",
        )
    });
    let url = format!("http://{alamat}/hilang");
    let ambil = jaringan::muat_blokir(&url, Batas::default()).expect("badan 404 tetap diambil");
    assert_eq!(ambil.status, 404);
    assert_eq!(ambil.html, "<h1>tidak ada</h1>");
}

#[test]
fn muat_menyebut_url_saat_koneksi_ditolak() {
    // Port yang baru saja dilepas: hampir pasti tidak ada yang mendengarkan.
    let alamat = {
        let l = TcpListener::bind("127.0.0.1:0").expect("port bebas");
        l.local_addr().expect("alamat").to_string()
    };
    let url = format!("http://{alamat}/tidak-ada");
    let galat = jaringan::muat_blokir(&url, Batas::default()).expect_err("koneksi harus gagal");
    assert!(galat.pesan.contains(&url), "pesan: {}", galat.pesan);
}

#[test]
fn muat_berhenti_saat_server_tidak_menjawab() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("port bebas");
    let alamat = listener.local_addr().expect("alamat").to_string();
    std::thread::spawn(move || {
        // Terima koneksi lalu diam: klien harus menyerah karena batas waktunya.
        let _ = listener.accept();
        std::thread::sleep(Duration::from_secs(5));
    });
    let url = format!("http://{alamat}/lambat");
    let batas = Batas {
        waktu: Duration::from_millis(400),
        connect: Duration::from_millis(400),
        ..Batas::default()
    };
    let galat = jaringan::muat_blokir(&url, batas).expect_err("batas waktu harus berlaku");
    assert!(galat.pesan.contains(&url), "pesan: {}", galat.pesan);
}

#[test]
fn charset_dibaca_dari_header_content_type() {
    assert_eq!(
        jaringan::charset_dari("text/html; charset=UTF-8").as_deref(),
        Some("utf-8")
    );
    assert_eq!(
        jaringan::charset_dari("text/html; charset=\"windows-1252\"").as_deref(),
        Some("windows-1252")
    );
    assert_eq!(jaringan::charset_dari("text/html"), None);
}

// --- stylesheet eksternal -------------------------------------------------

fn css(badan: &str) -> Vec<u8> {
    respons(
        "200 OK",
        &[("content-type", "text/css; charset=utf-8")],
        badan.as_bytes(),
    )
}

#[test]
fn muat_css_menerima_text_css_dan_menolak_html() {
    let alamat = server(2, |jalur| {
        if jalur == "/gaya.css" {
            css("body { margin: 0 }")
        } else {
            html("<p>halaman galat</p>")
        }
    });
    let url = format!("http://{alamat}/gaya.css");
    let gaya = jaringan::muat_css_blokir(&url, Batas::default()).expect("css terambil");
    assert_eq!(gaya.teks, "body { margin: 0 }");
    assert_eq!(gaya.status, 200);
    assert_eq!(gaya.url, url);

    // Server yang menjawab `text/html` di URL `.css` mengirim halaman galat,
    // bukan gaya. Mengurainya sebagai CSS akan menghasilkan kaskade kosong
    // tanpa satu pun galat.
    let url = format!("http://{alamat}/salah.css");
    let galat = jaringan::muat_css_blokir(&url, Batas::default()).expect_err("html harus ditolak");
    assert!(
        galat.pesan.contains("text/html"),
        "pesan harus menyebut mime yang diterima: {}",
        galat.pesan
    );
    assert!(
        galat.pesan.contains("bukan 'text/css'"),
        "pesan: {}",
        galat.pesan
    );
}

#[test]
fn muat_css_membatasi_ukuran_lembar() {
    let besar = "a{color:red}".repeat(512);
    let alamat = server(1, move |_| css(&besar));
    let url = format!("http://{alamat}/besar.css");
    let batas = Batas {
        byte: 64,
        ..Batas::default()
    };
    let galat = jaringan::muat_css_blokir(&url, batas).expect_err("lembar besar harus ditolak");
    assert!(
        galat.pesan.contains("melebihi batas 64 byte"),
        "pesan: {}",
        galat.pesan
    );
}

#[tokio::test]
async fn gaya_jaringan_mengambil_tautan_relatif_dan_melewati_media_print() {
    let alamat = server(2, |jalur| {
        if jalur.ends_with("/gaya.css") {
            css("#k { height: 44px }")
        } else {
            html("<p>galat</p>")
        }
    });
    let url = format!("http://{alamat}/artikel/satu");
    let dokumen = r#"<!doctype html><html><head>
        <link rel="stylesheet" href="gaya.css">
        <link rel="stylesheet" href="cetak.css" media="print">
        <link rel="alternate stylesheet" href="https://cdn.test/luar.css">
        </head><body><div id="k">isi</div></body></html>"#;

    let hasil = gaya::ambil(&url, dokumen).await;
    assert_eq!(hasil.lembar.len(), 1, "hanya satu yang layak diambil");
    // `href` disimpan apa adanya: kaskade mencocokkannya dengan node `<link>`
    // di pohon, dan URL absolut tidak akan pernah cocok.
    assert_eq!(hasil.lembar[0].href, "gaya.css");
    assert_eq!(hasil.lembar[0].teks, "#k { height: 44px }");
    let catatan = hasil.catatan.join(" | ");
    assert!(
        catatan.contains("media=\"print\""),
        "alasan media print harus tercatat: {catatan}"
    );
    assert!(
        catatan.contains("rel") || catatan.contains("alternate"),
        "tautan alternate harus tercatat: {catatan}"
    );
}

#[tokio::test]
async fn gaya_jaringan_menyelesaikan_href_relatif_terhadap_url_akhir_pengalihan() {
    // `/awal` dialihkan ke `/artikel/`, jadi `href="gaya.css"` berarti
    // `/artikel/gaya.css`. Memakai URL yang diminta akan meminta
    // `/gaya.css` dan mendapat 404 yang tidak kelihatan sebabnya.
    let alamat = server(3, |jalur| match jalur {
        "/awal" => respons("302 Found", &[("location", "/artikel/")], b""),
        "/artikel/" => html(
            r#"<html><head><link rel="stylesheet" href="gaya.css"></head><body><p>x</p></body></html>"#,
        ),
        "/artikel/gaya.css" => css("p { color: #0000ff }"),
        _ => respons(
            "404 Not Found",
            &[("content-type", "text/plain")],
            b"tidak ada",
        ),
    });

    let url = format!("http://{alamat}/awal");
    let ambil = jaringan::muat(&url, Batas::default())
        .await
        .expect("dokumen");
    assert_eq!(ambil.url, format!("http://{alamat}/artikel/"), "url akhir");

    let hasil = gaya::ambil(&ambil.url, &ambil.html).await;
    assert_eq!(hasil.lembar.len(), 1, "catatan: {:?}", hasil.catatan);
    assert_eq!(hasil.lembar[0].teks, "p { color: #0000ff }");
}

#[tokio::test]
async fn gaya_jaringan_ikut_sampai_ke_layar() {
    // Rantai penuh: HTML + CSS dari server lokal, lalu tangkapan layar lewat
    // state. Kalau salah satu sambungan lupa meneruskan lembar, halaman akan
    // terlukis tanpa gaya dan tes ini gagal pada perbandingan piksel.
    use cdp::native_wire::{LembarJaringan, NativeState};
    use serde_json::json;

    let alamat = server(2, |jalur| match jalur {
        "/gaya.css" => css("#k { background-color: #ff0000 }"),
        _ => html(
            r#"<html><head><link rel="stylesheet" href="/gaya.css"></head><body><div id="k" style="height:40px;width:40px">isi</div></body></html>"#,
        ),
    });
    let url = format!("http://{alamat}/halaman");
    let ambil = jaringan::muat(&url, Batas::default())
        .await
        .expect("dokumen");
    let hasil = gaya::ambil(&ambil.url, &ambil.html).await;
    assert_eq!(hasil.lembar.len(), 1, "catatan: {:?}", hasil.catatan);

    let potret = |lembar: Vec<engine::native::LembarLuar>| {
        let state = NativeState::baru();
        let buka =
            json!({"id": 1, "method": "Target.createTarget", "params": {"url": "about:blank"}});
        let target = state.tangani(&buka.to_string()).expect("balasan");
        let target: serde_json::Value = serde_json::from_str(&target).expect("json");
        let id = target["result"]["targetId"]
            .as_str()
            .expect("targetId")
            .to_owned();
        let lekat = json!({"id": 2, "method": "Target.attachToTarget", "params": {"targetId": id}});
        let sesi = state.tangani(&lekat.to_string()).expect("balasan");
        let sesi: serde_json::Value = serde_json::from_str(&sesi).expect("json");
        let sesi = sesi["result"]["sessionId"]
            .as_str()
            .expect("sessionId")
            .to_owned();

        let navigasi = json!({
            "id": 3, "method": "Page.navigate",
            "params": {"url": url}, "sessionId": sesi,
        });
        let jawab = state
            .tangani_ambil(
                &navigasi.to_string(),
                Some(&sesi),
                Some(Ok(ambil.clone())),
                LembarJaringan {
                    lembar,
                    catatan: hasil.catatan.clone(),
                    font: std::sync::Arc::new(engine::native::teks::KumpulanFont::baru()),
                },
            )
            .expect("balasan navigasi");
        let jawab: serde_json::Value = serde_json::from_str(&jawab).expect("json");
        assert!(jawab.get("error").is_none(), "navigasi gagal: {jawab}");

        let potret = json!({
            "id": 4, "method": "Page.captureScreenshot",
            "params": {"format": "png", "clip": {"x": 0, "y": 0, "width": 80, "height": 60, "scale": 1}},
            "sessionId": sesi,
        });
        let jawab = state.tangani(&potret.to_string()).expect("balasan potret");
        let jawab: serde_json::Value = serde_json::from_str(&jawab).expect("json");
        jawab["result"]["data"].as_str().expect("data").to_owned()
    };

    let dengan_gaya = potret(hasil.lembar.clone());
    let tanpa_gaya = potret(Vec::new());
    assert_ne!(
        dengan_gaya, tanpa_gaya,
        "lembar eksternal tidak sampai ke pelukis: raster identik dengan halaman tanpa gaya"
    );
}

// --- @import --------------------------------------------------------------

#[test]
fn impor_urai_bentuk_url_kutip_dan_media() {
    let (impor, sisa) = gaya::impor_dari(
        "@import \"a.css\";\n@import url(b.css) screen;\n@import url('c.css') print;\np { color: red }",
    );
    assert_eq!(impor.len(), 3, "tiga impor terbaca: {impor:?}");
    assert_eq!(impor[0].href, "a.css");
    assert_eq!(impor[0].media, None);
    assert_eq!(impor[1].href, "b.css");
    assert_eq!(impor[1].media.as_deref(), Some("screen"));
    assert_eq!(impor[2].href, "c.css");
    assert_eq!(impor[2].media.as_deref(), Some("print"));
    // Pernyataan `@import` dibuang dari teks yang disisakan: setelah isinya
    // benar-benar disisipkan, catatan "at-rule tidak dijalankan" jadi bohong.
    assert!(!sisa.contains("@import"), "sisa: {sisa:?}");
    assert!(sisa.contains("p { color: red }"), "sisa: {sisa:?}");
}

#[tokio::test]
async fn impor_relatif_diambil_dan_ditaruh_di_depan_aturan_induk() {
    let alamat = server(2, |jalur| match jalur {
        "/dasar.css" => css("@import \"anak.css\";\np { color: #ff0000 }"),
        "/anak.css" => css("p { height: 7px }"),
        _ => respons("404 Not Found", &[("content-type", "text/plain")], b"-"),
    });
    let dokumen = r#"<html><head><link rel="stylesheet" href="dasar.css"></head><body><p>x</p></body></html>"#;
    let hasil = gaya::ambil(&format!("http://{alamat}/"), dokumen).await;
    assert_eq!(hasil.lembar.len(), 1, "catatan: {:?}", hasil.catatan);
    let teks = &hasil.lembar[0].teks;
    let posisi_anak = teks.find("height: 7px").expect("isi impor ikut");
    let posisi_induk = teks.find("color: #ff0000").expect("aturan induk utuh");
    assert!(
        posisi_anak < posisi_induk,
        "isi @import harus mendahului aturan induk: {teks:?}"
    );
    assert!(!teks.contains("@import"), "sisa @import: {teks:?}");
}

#[tokio::test]
async fn impor_siklus_berhenti_dan_dicatat() {
    // A -> B -> A. Tanpa pemutus siklus ini berputar sampai anggaran habis.
    let alamat = server(2, |jalur| match jalur {
        "/a.css" => css("@import \"b.css\";\na { height: 1px }"),
        "/b.css" => css("@import \"a.css\";\nb { height: 2px }"),
        _ => respons("404 Not Found", &[("content-type", "text/plain")], b"-"),
    });
    let dokumen = r#"<html><head><link rel="stylesheet" href="a.css"></head><body></body></html>"#;
    let hasil = gaya::ambil(&format!("http://{alamat}/"), dokumen).await;
    assert_eq!(hasil.lembar.len(), 1);
    let catatan = hasil.catatan.join(" | ");
    assert!(
        catatan.contains("siklus"),
        "siklus harus dicatat dengan namanya: {catatan}"
    );
    assert!(hasil.lembar[0].teks.contains("height: 2px"), "b ikut");
}

#[tokio::test]
async fn impor_kedalaman_berlebih_dicatat_bukan_diambil() {
    // Rantai lima berkas: lembar akar (bukan impor) -> impor 1 -> 2 -> 3 -> 4.
    // `KEDALAMAN_IMPOR` = 3 berarti tiga tingkat impor boleh, tingkat keempat
    // dilewati. Akar sengaja tidak dihitung: halaman yang menautkan langsung
    // lima berkas bertingkat itu bukan impor sedalam lima.
    let alamat = server(4, |jalur| match jalur {
        "/1.css" => css("@import \"2.css\";\na{height:1px}"),
        "/2.css" => css("@import \"3.css\";\nb{height:2px}"),
        "/3.css" => css("@import \"4.css\";\nc{height:3px}"),
        "/4.css" => css("@import \"5.css\";\nd{height:4px}"),
        "/5.css" => css("e{height:5px}"),
        _ => respons("404 Not Found", &[("content-type", "text/plain")], b"-"),
    });
    let dokumen = r#"<html><head><link rel="stylesheet" href="1.css"></head><body></body></html>"#;
    let hasil = gaya::ambil(&format!("http://{alamat}/"), dokumen).await;
    let teks = &hasil.lembar[0].teks;
    assert!(teks.contains("height:4px"), "impor ketiga ikut: {teks:?}");
    assert!(
        !teks.contains("height:5px"),
        "impor keempat harus dilewati: {teks:?}"
    );
    let catatan = hasil.catatan.join(" | ");
    assert!(
        catatan.contains("kedalaman"),
        "alasan kedalaman harus tercatat: {catatan}"
    );
}

#[tokio::test]
async fn impor_berkondisi_media_dicatat_tidak_dijalankan() {
    let alamat = server(1, |jalur| match jalur {
        "/dasar.css" => css("@import \"cetak.css\" print;\np{height:5px}"),
        _ => respons("404 Not Found", &[("content-type", "text/plain")], b"-"),
    });
    let dokumen =
        r#"<html><head><link rel="stylesheet" href="dasar.css"></head><body></body></html>"#;
    let hasil = gaya::ambil(&format!("http://{alamat}/"), dokumen).await;
    let catatan = hasil.catatan.join(" | ");
    assert!(
        catatan.contains("print"),
        "kondisi media harus disebut: {catatan}"
    );
    assert_eq!(
        hasil.lembar[0].teks.trim(),
        "p{height:5px}",
        "isi impor berkondisi tidak boleh disisipkan"
    );
}

// --- @font-face -----------------------------------------------------------

/// Byte font Devanagari yang tertanam di crate engine, dipakai sebagai isi
/// berkas webfont di server lokal tes ini.
fn byte_font() -> Vec<u8> {
    let jalur = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../engine/assets/NotoSansDevanagari-Regular.ttf"
    );
    std::fs::read(jalur).expect("fixture NotoSansDevanagari-Regular.ttf")
}

fn font_ok(badan: Vec<u8>) -> Vec<u8> {
    respons("200 OK", &[("content-type", "font/ttf")], badan.as_slice())
}

#[tokio::test]
async fn font_face_diunduh_dan_bisa_dipasang() {
    let byte = byte_font();
    let panjang = byte.len();
    let alamat = server(1, move |jalur| match jalur {
        "/web.ttf" => font_ok(byte.clone()),
        _ => respons("404 Not Found", &[("content-type", "text/plain")], b"-"),
    });

    let dokumen = r#"<html><head><style>
        @font-face { font-family: "WebDeva"; src: url(web.ttf) format("truetype"); }
        p { font-family: "WebDeva"; }
    </style></head><body><p>x</p></body></html>"#;
    let doc = engine::native::Document::parse_html(dokumen).expect("dokumen");
    let face = engine::native::css::font_face_dokumen(&doc, &[]);
    assert_eq!(face.len(), 1, "satu blok @font-face: {face:?}");
    assert_eq!(face[0].keluarga, "WebDeva", "nama keluarga tanpa kutip");
    assert_eq!(face[0].sumber, vec!["web.ttf".to_string()]);

    let hasil = gaya::ambil_font(&format!("http://{alamat}/"), &face).await;
    assert!(
        hasil.catatan.is_empty(),
        "tidak ada face yang dilewati: {:?}",
        hasil.catatan
    );
    assert_eq!(hasil.face.len(), 1, "satu face terunduh");
    assert_eq!(
        hasil.face[0].1.len(),
        panjang,
        "byte yang sampai harus utuh, bukan terpotong"
    );

    // Yang menentukan face itu benar-benar bisa dipakai adalah `ttf_parser`,
    // bukan status HTTP. Pemasangan diuji di sini supaya tes ini gagal kalau
    // unduhannya sukses tapi byte-nya tidak bisa diurai.
    let mut kumpulan = engine::native::teks::KumpulanFont::baru();
    assert!(
        kumpulan.pasang(&hasil.face[0].0, hasil.face[0].1.clone()),
        "byte yang diunduh harus terpasang sebagai face"
    );
    assert_eq!(kumpulan.byte(), panjang, "anggaran byte terhitung");
}

#[tokio::test]
async fn font_face_yang_menjawab_html_dilewati_dengan_alasan() {
    // Server yang menyajikan halaman galat di URL `.woff2` mengirim HTML, bukan
    // font. Memasangnya berarti `font-family` yang menyebut face itu
    // menghasilkan teks kosong, dan pesan "font rusak" menutupi masalah
    // sebenarnya.
    let alamat = server(1, |_| {
        respons(
            "200 OK",
            &[("content-type", "text/html; charset=utf-8")],
            b"<html>404</html>",
        )
    });
    let dokumen = r#"<html><head><style>
        @font-face { font-family: "Salah"; src: url(galat.woff2); }
    </style></head><body></body></html>"#;
    let doc = engine::native::Document::parse_html(dokumen).expect("dokumen");
    let face = engine::native::css::font_face_dokumen(&doc, &[]);
    let hasil = gaya::ambil_font(&format!("http://{alamat}/"), &face).await;
    assert!(hasil.face.is_empty(), "HTML tidak boleh jadi face");
    let catatan = hasil.catatan.join(" | ");
    assert!(
        catatan.contains("text/html") && catatan.contains("Salah"),
        "alasan harus menyebut mime dan nama face: {catatan}"
    );
}

#[tokio::test]
async fn font_face_tanpa_url_tidak_diunduh() {
    // `local("DejaVu Sans")` bukan aset yang dikirim halaman: memakainya
    // membuat hasil lukis bergantung pada apa yang kebetulan terpasang di host.
    let dokumen = r#"<html><head><style>
        @font-face { font-family: "Lokal"; src: local("DejaVu Sans"); }
    </style></head><body></body></html>"#;
    let doc = engine::native::Document::parse_html(dokumen).expect("dokumen");
    assert!(
        engine::native::css::font_face_dokumen(&doc, &[]).is_empty(),
        "blok tanpa url() tidak jadi face"
    );
}
