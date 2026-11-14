//! Mengukur satu halaman sungguhan: berapa lembar gaya yang diambil, berapa
//! byte, dan berapa lama.
//!
//! Alat ini ada karena dokumen bukti tidak boleh berisi angka karangan. Yang
//! dicetak di sini adalah hasil jalur kode yang sama dengan `Page.navigate`,
//! bukan perkiraan: `jaringan::muat` mengambil dokumen, `gaya::ambil`
//! mengumpulkan `<link rel=stylesheet>` beserta `@import`-nya.
//!
//! Jalankan: `cargo run -p cdp --example ukur_gaya -- <url>`

use std::time::Instant;

use cdp::native_wire::{KebijakanJaringan, gaya, jaringan};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let Some(url) = std::env::args().nth(1) else {
        eprintln!("pakai: ukur_gaya <url>");
        std::process::exit(2);
    };

    // Alat ukur ini dijalankan operator dengan URL di baris perintah, dan yang
    // diukur justru sering server lokal. Gerbang jaringan tetap berlaku: host
    // yang diminta dibuka secara eksplisit di sini, bukan dimatikan diam-diam.
    // Loopback dan jaringan privat tetap harus disebut dengan nama, dan
    // link-local tetap ditolak gerbang walaupun disebut.
    let Some(host) = url::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
    else {
        eprintln!("'{url}' bukan URL yang bisa diurai");
        std::process::exit(2);
    };
    let kebijakan = KebijakanJaringan::baru(vec![host], Vec::new(), Vec::new());

    let mulai = Instant::now();
    let dokumen = match jaringan::muat(&url, jaringan::Batas::default(), &kebijakan).await {
        Ok(dokumen) => dokumen,
        Err(galat) => {
            eprintln!("dokumen gagal: {} ({})", galat.pesan, galat.kode);
            std::process::exit(1);
        }
    };
    let waktu_dokumen = mulai.elapsed();

    let mulai = Instant::now();
    let hasil = gaya::ambil(&dokumen.url, &dokumen.html, &kebijakan).await;
    let waktu_gaya = mulai.elapsed();

    let total_byte: usize = hasil.lembar.iter().map(|l| l.teks.len()).sum();

    println!("url        : {}", dokumen.url);
    println!("status     : {}", dokumen.status);
    println!(
        "html       : {} byte, {} ms",
        dokumen.byte,
        waktu_dokumen.as_millis()
    );
    println!("lembar     : {}", hasil.lembar.len());
    println!(
        "css        : {total_byte} byte, {} ms",
        waktu_gaya.as_millis()
    );
    println!(
        "total      : {} ms",
        (waktu_dokumen + waktu_gaya).as_millis()
    );
    println!("--- lembar ---");
    for (i, lembar) in hasil.lembar.iter().enumerate() {
        println!("{i:>2}. {:>7} byte  {}", lembar.teks.len(), lembar.href);
    }
    println!("--- catatan ({}) ---", hasil.catatan.len());
    for catatan in &hasil.catatan {
        println!("  - {catatan}");
    }
}
