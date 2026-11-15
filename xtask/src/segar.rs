//! `cargo xtask segar`: apakah biner rilis dibangun dari pohon yang sekarang?
//!
//! # Kenapa ini ada (AC-009)
//!
//! Angka performa yang dilaporkan untuk mesin ini selalu dari biner rilis, dan
//! biner rilis yang basi menghasilkan angka yang salah tanpa satu pun pesan
//! galat: ia hanya mengukur kode kemarin. Hari ini kekeliruan itu nyata. Satu
//! putaran pengukuran NFR-001 sempat dilaporkan "25,9 detik untuk 1.206
//! simpul" padahal angka itu keluar dari biner debug, dan putaran lain
//! mengukur biner rilis yang belum memuat perbaikan yang sedang diuji.
//!
//! Pemeriksa ini menjawab satu pertanyaan dengan dua pembanding, karena satu
//! pembanding tidak cukup:
//!
//! * **sumber**: mtime biner lawan mtime berkas sumber terbaru. Kalau ada
//!   berkas `.rs` yang lebih baru dari binernya, binernya tidak memuat
//!   perubahan itu.
//! * **commit**: mtime biner lawan waktu commit `HEAD`. Ini menangkap kasus
//!   yang lolos dari pembanding pertama: sumber tidak berubah sejak build,
//!   tapi ada commit yang mendarat sesudahnya.
//!
//! Keluar dengan kode 1 kalau salah satu pembanding gagal, 0 kalau keduanya
//! lolos. Waktu dicetak dalam UTC supaya tidak ada pergeseran zona yang harus
//! ditebak pembacanya.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

/// Berkas di akar ruang kerja yang ikut menentukan isi biner.
const BERKAS_AKAR: &[&str] = &["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"];

/// Direktori yang isinya dikompilasi menjadi biner `browserd`.
///
/// `xtask` sengaja di luar daftar: ia alat pengembangan, tidak pernah ikut
/// ditautkan ke `browserd`, dan memasukkannya membuat pemeriksa ini menuduh
/// biner basi setiap kali skrip pengembangan disentuh. Tuduhan yang salah
/// membuat gerbang dimatikan orang, jadi cakupannya dipersempit ke yang benar.
const DIR_SUMBER: &[&str] = &["crates"];

pub fn run(rest: &[String]) -> Result<()> {
    let mut biner: Option<PathBuf> = None;
    let mut akar: Option<PathBuf> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--bin" => {
                biner = rest.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--akar" => {
                akar = rest.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            lain => bail!("argumen tidak dikenal: '{lain}' (dipakai: --bin <berkas> --akar <dir>)"),
        }
    }

    let akar = match akar {
        Some(a) => a,
        None => akar_ruang_kerja()?,
    };
    let biner = match biner {
        Some(b) => b,
        None => direktori_target()?.join("release").join("browserd"),
    };

    let mtime_biner = mtime(&biner).with_context(|| {
        format!(
            "biner rilis tidak ada di {}: bangun dulu dengan `cargo build --release -p browserd`",
            biner.display()
        )
    })?;

    let (mtime_sumber, berkas_sumber) = sumber_terbaru(&akar)?;
    let (mtime_commit, ringkas_commit) = commit_terakhir(&akar)?;

    println!("biner  : {}  {}", biner.display(), waktu(mtime_biner));
    println!(
        "sumber : {}  {}",
        berkas_sumber.display(),
        waktu(mtime_sumber)
    );
    println!("commit : {ringkas_commit}  {}", waktu(mtime_commit));

    let mut basi = Vec::new();
    if mtime_biner < mtime_sumber {
        basi.push(format!(
            "sumber lebih baru {} detik daripada biner",
            selisih(mtime_sumber, mtime_biner)
        ));
    }
    if mtime_biner < mtime_commit {
        basi.push(format!(
            "commit lebih baru {} detik daripada biner",
            selisih(mtime_commit, mtime_biner)
        ));
    }

    if basi.is_empty() {
        println!("putusan: SEGAR");
        return Ok(());
    }
    for alasan in &basi {
        println!("basi   : {alasan}");
    }
    println!("putusan: BASI");
    bail!("biner rilis basi; bangun ulang dengan `cargo build --release -p browserd`")
}

/// Akar ruang kerja: direktori kerja sekarang kalau ia memuat `Cargo.toml`,
/// kalau tidak naik satu tingkat sampai ketemu.
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

/// Direktori target cargo. `CARGO_TARGET_DIR` menang; kalau tidak diset,
/// diturunkan dari letak biner `xtask` ini sendiri (`<target>/debug/xtask`),
/// supaya `target-dir` yang dipindah ke disk lain tetap terpakai.
fn direktori_target() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    let exe = std::env::current_exe().context("tidak bisa membaca letak biner xtask")?;
    let dir = exe
        .parent()
        .and_then(Path::parent)
        .context("letak biner xtask tidak berbentuk <target>/<profil>/xtask")?;
    Ok(dir.to_path_buf())
}

fn mtime(path: &Path) -> Result<SystemTime> {
    let meta = fs::metadata(path)?;
    Ok(meta.modified()?)
}

/// Berkas sumber termuda, beserta waktunya.
fn sumber_terbaru(akar: &Path) -> Result<(SystemTime, PathBuf)> {
    let mut terbaru: Option<(SystemTime, PathBuf)> = None;
    for nama in BERKAS_AKAR {
        let path = akar.join(nama);
        if let Ok(t) = mtime(&path) {
            simpan_terbaru(&mut terbaru, t, path);
        }
    }
    for dir in DIR_SUMBER {
        sisir(&akar.join(dir), &mut terbaru)?;
    }
    terbaru.context("tidak ada satu pun berkas sumber yang terbaca")
}

fn sisir(dir: &Path, terbaru: &mut Option<(SystemTime, PathBuf)>) -> Result<()> {
    let isi = match fs::read_dir(dir) {
        Ok(isi) => isi,
        // Direktori yang tidak ada bukan kegagalan: `xtask` bisa saja tidak
        // ada di ruang kerja lain, dan yang penting berkas yang ada terbaca.
        Err(_) => return Ok(()),
    };
    for entri in isi {
        let entri = entri?;
        let path = entri.path();
        let meta = entri.metadata()?;
        if meta.is_dir() {
            let nama = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // `target` bisa bersarang di dalam anggota ruang kerja, dan
            // isinya bukan sumber: artefak build tidak boleh membuat biner
            // terlihat basi. `tests` juga dilewati: uji integrasi di
            // `crates/*/tests/` tidak ikut ditautkan ke biner, jadi
            // menyentuhnya tidak membuat biner basi.
            if nama == "target" || nama == ".git" || nama == "tests" {
                continue;
            }
            sisir(&path, terbaru)?;
        } else if path.extension().is_some_and(|e| e == "rs")
            && let Ok(t) = meta.modified()
        {
            simpan_terbaru(terbaru, t, path);
        }
    }
    Ok(())
}

fn simpan_terbaru(slot: &mut Option<(SystemTime, PathBuf)>, t: SystemTime, path: PathBuf) {
    if slot.as_ref().is_none_or(|(lama, _)| t > *lama) {
        *slot = Some((t, path));
    }
}

/// Waktu commit `HEAD` dan ringkasannya (`<hash> <subjek>`).
fn commit_terakhir(akar: &Path) -> Result<(SystemTime, String)> {
    let out = Command::new("git")
        .arg("-C")
        .arg(akar)
        .args(["log", "-1", "--format=%ct%x09%h %s"])
        .output()
        .context("tidak bisa menjalankan git")?;
    if !out.status.success() {
        bail!(
            "git log gagal di {}: {}",
            akar.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let teks = String::from_utf8_lossy(&out.stdout);
    let (detik, ringkas) = teks
        .trim()
        .split_once('\t')
        .context("keluaran git log tidak berbentuk <detik>TAB<hash> <subjek>")?;
    let detik: u64 = detik.parse().context("detik commit bukan angka")?;
    Ok((UNIX_EPOCH + Duration::from_secs(detik), ringkas.to_owned()))
}

fn selisih(lebih_baru: SystemTime, lebih_lama: SystemTime) -> u64 {
    lebih_baru
        .duration_since(lebih_lama)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Waktu dalam UTC, `YYYY-MM-DD HH:MM:SS`. Ditulis sendiri karena `xtask`
/// tidak membawa dependensi tanggal, dan menambah satu crate hanya untuk
/// format waktu bukan pertukaran yang sepadan.
fn waktu(t: SystemTime) -> String {
    let detik = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let hari = detik.div_euclid(86_400);
    let sisa = detik.rem_euclid(86_400);
    let (y, m, d) = tanggal_dari_hari(hari);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        sisa / 3600,
        (sisa % 3600) / 60,
        sisa % 60
    )
}

/// Algoritma hari-sipil (Howard Hinnant), berlaku untuk semua tahun proleptik.
fn tanggal_dari_hari(hari: i64) -> (i64, u32, u32) {
    let z = hari + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
