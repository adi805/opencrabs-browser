//! Tes pengambilan dokumen dari jaringan.
//!
//! Servernya lokal dan dijalankan di dalam tes ini: tidak ada situs sungguhan
//! yang dihubungi, jadi tes ini jalan tanpa internet dan tidak bergantung pada
//! apa pun di luar repo.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::Duration;

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
