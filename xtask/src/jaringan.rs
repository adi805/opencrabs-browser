//! `cargo xtask jaringan`: siapa yang membuka koneksi keluar, dan lewat gerbang apa.
//!
//! # Kenapa ini ada (AC-017)
//!
//! Janji keamanan mesin ini adalah "setiap koneksi keluar melewati
//! `KebijakanJaringan`". Janji yang hanya ditulis di dokumen akan bocor pada
//! perubahan kelima: satu `ureq` baru di berkas baru tidak menabrak apa pun,
//! dan tidak ada yang tahu sampai ada halaman yang menyuruh fetch ke
//! `169.254.169.254`. Pemeriksa ini menggantikan janji itu dengan pemindaian
//! yang jalan di setiap `cargo xtask check`.
//!
//! # Tiga putusan
//!
//! Setiap pemanggil pembuka koneksi diklasifikasikan, dan klasifikasinya
//! dibaca dari teks fungsi yang memuatnya, bukan dari daftar nama berkas yang
//! diizinkan. Daftar izin akan berhenti benar begitu berkasnya di-rename, dan
//! yang lebih buruk: daftar izin tidak pernah menolak pemanggil baru di berkas
//! yang sudah ada di dalamnya.
//!
//! * **gerbang**: fungsi itu sendiri memanggil `putuskan*` pada kebijakan.
//! * **loopback**: fungsi itu membangun alamatnya dari literal
//!   `SocketAddr::from(([127, 0, 0, 1], ..))`, jadi tidak ada host yang bisa
//!   dipengaruhi halaman. Ini kasus probe ke mesin lokal sendiri.
//! * **gerbang-tak-langsung**: fungsi itu tidak memanggil gerbang, tapi semua
//!   pemanggilnya memanggil gerbang. Ini bentuk `ambil_blokir`, yang sengaja
//!   memisahkan "sudah lolos kebijakan" dari "mengambil".
//!
//! Sisanya **TANPA GERBANG** dan membuat perintah ini keluar dengan kode 1.
//!
//! # Batas yang jujur
//!
//! Pemindaian ini berbasis teks, dan teks bisa menipu. Ia membuktikan bahwa
//! sebuah fungsi yang membuka koneksi memuat pemanggilan gerbang atau literal
//! loopback; ia tidak membuktikan bahwa gerbang itu dipanggil **sebelum**
//! koneksinya, dan tidak membaca makro atau pemanggilan tidak langsung.
//! Karena itu ia jadi penjaga kebocoran, bukan bukti kebenaran: yang ia
//! tangkap adalah pemanggil baru yang lupa gerbang, bukan gerbang yang
//! dipasang di urutan yang salah.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Pola yang menandai pembukaan koneksi keluar.
const POLA: &[&str] = &[
    "ureq::AgentBuilder",
    "TcpStream::connect(",
    "TcpStream::connect_timeout(",
    "reqwest::",
    "hyper::client",
];

/// Pemanggilan gerbang kebijakan jaringan.
const GERBANG: &[&str] = &["putuskan_dokumen(", "putuskan_subresource(", "putuskan("];

/// Alamat loopback yang dibangun fungsi itu sendiri.
const LOOPBACK: &[&str] = &["SocketAddr::from(([127, 0, 0, 1]"];

struct Berkas {
    path: PathBuf,
    baris: Vec<String>,
}

struct Temuan {
    lokasi: String,
    fungsi: String,
    putusan: String,
    alasan: String,
}

pub fn run(rest: &[String]) -> Result<()> {
    if !rest.is_empty() {
        bail!(
            "perintah ini tidak memakai argumen (diberikan: {})",
            rest.join(" ")
        );
    }
    let akar = akar_ruang_kerja()?;
    let berkas = muat(&akar)?;
    if berkas.is_empty() {
        bail!("tidak ada berkas sumber yang terbaca di {}", akar.display());
    }

    let mut temuan = Vec::new();
    for (idx, b) in berkas.iter().enumerate() {
        for (no, baris) in b.baris.iter().enumerate() {
            if !POLA.iter().any(|p| baris.contains(p)) {
                continue;
            }
            let (fungsi, badan) = fungsi_dan_badan(&b.baris, no);
            let lokasi = format!("{}:{}", ringkas_path(&akar, &b.path), no + 1);
            let (putusan, alasan) = klasifikasi(&berkas, idx, no, &fungsi, &badan);
            temuan.push(Temuan {
                lokasi,
                fungsi,
                putusan,
                alasan,
            });
        }
    }

    println!("disisir: {} berkas di crates/*/src", berkas.len());
    println!();
    for t in &temuan {
        println!(
            "{:<34} {:<24} {:<20} {}",
            t.lokasi, t.fungsi, t.putusan, t.alasan
        );
    }
    println!();

    let tanpa: Vec<&Temuan> = temuan
        .iter()
        .filter(|t| t.putusan == "TANPA GERBANG")
        .collect();
    if tanpa.is_empty() {
        println!(
            "putusan: BERSIH ({} pemanggil, semuanya bergerbang atau loopback)",
            temuan.len()
        );
        return Ok(());
    }
    for t in &tanpa {
        println!(
            "bocor  : {} di {} tidak memeriksa kebijakan jaringan",
            t.fungsi, t.lokasi
        );
    }
    println!(
        "putusan: BOCOR ({} dari {} pemanggil tanpa gerbang)",
        tanpa.len(),
        temuan.len()
    );
    bail!("ada pemanggil pembuka koneksi yang tidak memeriksa KebijakanJaringan")
}

/// Klasifikasi satu pemanggil.
fn klasifikasi(
    berkas: &[Berkas],
    idx: usize,
    no: usize,
    fungsi: &str,
    badan: &str,
) -> (String, String) {
    if let Some(gerbang) = GERBANG.iter().find(|g| badan.contains(*g)) {
        return ("gerbang".into(), format!("memanggil {gerbang}"));
    }
    if LOOPBACK.iter().any(|l| badan.contains(l)) {
        return ("loopback".into(), "alamat dari literal 127.0.0.1".into());
    }
    // Pemanggil tidak bergerbang: periksa satu tingkat ke atas. Kalau setiap
    // pemanggilnya bergerbang, gerbangnya ada di sana, bukan di sini.
    let pemanggil = pemanggil_dari(berkas, idx, no, fungsi);
    if !pemanggil.is_empty()
        && pemanggil
            .iter()
            .all(|(_, badan)| GERBANG.iter().any(|g| badan.contains(*g)))
    {
        let nama: Vec<String> = pemanggil
            .iter()
            .map(|(fungsi, _)| format!("{fungsi}()"))
            .collect();
        return (
            "gerbang-tak-langsung".into(),
            format!("semua pemanggil bergerbang: {}", nama.join(", ")),
        );
    }
    let nama: Vec<String> = pemanggil
        .iter()
        .map(|(fungsi, _)| format!("{fungsi}()"))
        .collect();
    let alasan = if nama.is_empty() {
        "tidak ada pemanggil bergerbang dan bukan loopback".to_owned()
    } else {
        format!("pemanggil tanpa gerbang: {}", nama.join(", "))
    };
    ("TANPA GERBANG".into(), alasan)
}

/// Semua fungsi yang memanggil `fungsi`, beserta badan masing-masing.
fn pemanggil_dari(berkas: &[Berkas], idx: usize, no: usize, fungsi: &str) -> Vec<(String, String)> {
    let cari = format!("{fungsi}(");
    let mut hasil = Vec::new();
    for (i, b) in berkas.iter().enumerate() {
        for (n, baris) in b.baris.iter().enumerate() {
            if i == idx && n == no {
                continue;
            }
            if !baris.contains(&cari) {
                continue;
            }
            let (nama, badan) = fungsi_dan_badan(&b.baris, n);
            // Baris definisinya sendiri bukan pemanggil.
            if nama == fungsi && baris.trim_start().contains("fn ") {
                continue;
            }
            if nama.is_empty() || hasil.iter().any(|(ada, _): &(String, String)| *ada == nama) {
                continue;
            }
            hasil.push((nama, badan));
        }
    }
    hasil
}

/// Nama dan badan fungsi yang memuat baris `dari`.
///
/// Badan fungsi diambil dengan menghitung kurawal dari baris `fn` sampai
/// seimbang. Ini bukan parser Rust: kurawal di dalam string atau komentar
/// harfiah ikut terhitung, dan itu bisa membuat badan terpotong atau
/// memanjang. Untuk pemeriksaan "apakah gerbangnya dipanggil di fungsi ini"
/// pendekatan ini cukup, dan batasnya disebut di kepala berkas.
///
/// Penelusurannya dipilih dari DAFTAR deklarasi, bukan dengan mundur sampai
/// ketemu `fn`. Versi lama mundur ke belakang dan berhenti di kurung tutup
/// pertama, termasuk `}` milik `if` di dalam badan fungsi itu sendiri; akibatnya
/// badan yang terbaca cuma satu baris dan fungsi bergerbang dituduh tanpa
/// gerbang. Itu terjadi pada `ambil_badan` (gerbangnya di baris 378, temuan
/// dilaporkan di baris 394).
fn fungsi_dan_badan(baris: &[String], dari: usize) -> (String, String) {
    let mut kandidat: Option<(usize, String, usize)> = None;
    for (i, line) in baris.iter().enumerate() {
        if i > dari {
            break;
        }
        let Some(nama) = nama_fungsi(line) else {
            continue;
        };
        let akhir = akhir_badan(baris, i);
        if akhir >= dari {
            kandidat = Some((i, nama, akhir));
        }
    }
    let Some((awal, nama, akhir)) = kandidat else {
        return (String::new(), baris[dari].clone());
    };
    (nama, baris[awal..=akhir].join("\n"))
}

/// Baris terakhir badan fungsi yang dibuka di `awal`, dihitung dari kurawal.
fn akhir_badan(baris: &[String], awal: usize) -> usize {
    let mut dalam = 0i32;
    let mut sudah_buka = false;
    for (i, line) in baris.iter().enumerate().skip(awal) {
        dalam += line.chars().filter(|c| *c == '{').count() as i32;
        dalam -= line.chars().filter(|c| *c == '}').count() as i32;
        sudah_buka |= line.contains('{');
        if sudah_buka && dalam <= 0 {
            return i;
        }
    }
    baris.len().saturating_sub(1)
}

fn nama_fungsi(baris: &str) -> Option<String> {
    let t = baris.trim_start();
    let posisi = t.find("fn ")?;
    // Hanya deklarasi: sebelum `fn ` boleh ada `pub`, `pub(crate)`, `async`,
    // atau kombinasi keduanya, tidak boleh ada token lain (mis. `let f = fn`).
    let awalan = t[..posisi].trim();
    let sah = awalan.is_empty()
        || awalan == "pub"
        || awalan == "async"
        || awalan == "pub async"
        || (awalan.starts_with("pub(") && awalan.ends_with(')'))
        || (awalan.starts_with("pub(") && awalan.ends_with(") async"))
        || (awalan.starts_with("pub(") && awalan.ends_with("async"))
        || awalan == "unsafe"
        || awalan == "pub unsafe";
    if !sah {
        return None;
    }
    let sisa = &t[posisi + 3..];
    let nama: String = sisa
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if nama.is_empty() { None } else { Some(nama) }
}

fn akar_ruang_kerja() -> Result<PathBuf> {
    let mut dir = std::env::current_dir().context("tidak bisa membaca direktori kerja")?;
    loop {
        if dir.join("Cargo.toml").is_file() && dir.join("crates").is_dir() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("tidak menemukan akar ruang kerja (Cargo.toml + crates/) dari direktori kerja");
        }
    }
}

/// Memuat semua `crates/*/src/**/*.rs`. Direktori `tests` sengaja di luar
/// cakupan: uji integrasi memang menyambung ke peladen fixture di loopback,
/// dan menuntut gerbang kebijakan di sana akan membuat gerbang ini dimatikan
/// orang, bukan diikuti.
fn muat(akar: &Path) -> Result<Vec<Berkas>> {
    let mut hasil = Vec::new();
    let crates = akar.join("crates");
    for entri in
        fs::read_dir(&crates).with_context(|| format!("tidak bisa membaca {}", crates.display()))?
    {
        let entri = entri?;
        let path = entri.path().join("src");
        if path.is_dir() {
            sisir(&path, &mut hasil)?;
        }
    }
    Ok(hasil)
}

fn sisir(dir: &Path, hasil: &mut Vec<Berkas>) -> Result<()> {
    for entri in fs::read_dir(dir)? {
        let entri = entri?;
        let path = entri.path();
        let meta = entri.metadata()?;
        if meta.is_dir() {
            sisir(&path, hasil)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            let isi = fs::read_to_string(&path)
                .with_context(|| format!("tidak bisa membaca {}", path.display()))?;
            hasil.push(Berkas {
                path,
                baris: isi.lines().map(str::to_owned).collect(),
            });
        }
    }
    Ok(())
}

fn ringkas_path(akar: &Path, path: &Path) -> String {
    path.strip_prefix(akar)
        .unwrap_or(path)
        .display()
        .to_string()
}
