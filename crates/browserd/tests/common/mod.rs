//! Helper bersama untuk uji yang melahirkan mesin browser sungguhan.
//!
//! # Kenapa uji live harus diserialkan
//!
//! Tiap uji live melahirkan satu sampai dua proses Chromium, dan `cargo test
//! --workspace` menjalankan **setiap binary uji secara paralel** sekaligus
//! paralel di dalam binary-nya sendiri. Di laptop ini artinya belasan Chromium
//! berebut CPU dan RAM, lalu tiga kegagalan muncul bersamaan padahal ketiganya
//! lolos sendirian:
//!
//! - `m4_proof`: `Page.enable` tidak dijawab sampai 90 detik habis,
//! - `mata_tangan`: `Runtime.evaluate` tidak dijawab dalam 10 detik,
//! - `sesi_live`: tangkapan layar handoff belum sempat ada.
//!
//! Semuanya berbentuk "perintah CDP dijawab jauh lebih lambat dari dugaan",
//! bukan bukti produk salah. Kegagalan yang bergantung pada beban host tidak
//! membuktikan apa pun, jadi menaikkan batas waktu cuma memindahkan masalah.
//!
//! Yang diperbaiki di sini adalah sumbernya: satu kunci host-wide yang dipegang
//! selama satu uji live berjalan. Uji lain menunggu, bukan berebut. Kuncinya
//! memakai `flock` lewat `std::fs::File`, jadi kernel yang melepas saat proses
//! mati (uji yang ke-kill tidak meninggalkan kunci basi) dan tidak perlu
//! dependency tambahan.
//!
//! Berkas kunci diletakkan di direktori `target` yang sama dengan binary uji,
//! sehingga semua binary uji dalam satu build berbagi kunci yang sama, sementara
//! checkout lain di host yang sama tidak ikut menunggu. Antrean lintas checkout
//! akan membuat uji saling mengunci tanpa alasan.

#![allow(dead_code)]

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// Batas menunggu kunci. Jauh lebih longgar daripada durasi uji mana pun: kalau
/// batas ini tercapai, artinya ada uji lain yang menggantung, dan pesan
/// galatnya harus menyebut itu, bukan menggantung tanpa penjelasan.
const BATAS_TUNGGU: Duration = Duration::from_secs(900);

/// Berapa lama menunggu sebelum memberi tahu bahwa kita sedang mengantre.
const AMBANG_DIAM: Duration = Duration::from_secs(10);

/// Kunci yang dipegang selama satu uji live berjalan.
///
/// Dilepas saat nilai ini di-`drop`. `flock` juga dilepas kernel kalau proses
/// mati mendadak, jadi kunci tidak bisa basi.
pub struct KunciLive {
    // Berkas harus tetap hidup selama kunci dipegang: menutup berkas melepas
    // kuncinya.
    _berkas: File,
}

/// Path berkas kunci: `<target>/<profil>/.ocb-live.lock`.
///
/// Diturunkan dari lokasi binary uji (`<target>/debug/deps/<binary>`), jadi
/// semua binary uji dalam satu build berbagi berkas yang sama.
fn path_kunci() -> PathBuf {
    // `current_exe` bisa gagal di lingkungan yang aneh; jatuh ke direktori
    // sementara lebih baik daripada melewati kunci diam-diam.
    let exe = std::env::current_exe().expect("lokasi binary uji");
    let deps = exe.parent().expect("direktori binary uji");
    let profil = deps.parent().unwrap_or(deps);
    profil.join(".ocb-live.lock")
}

/// Mengambil kunci uji live, menunggu bila uji lain sedang berjalan.
///
/// `tag` hanya dipakai untuk pesan: berkas kunci menyimpan siapa pemegangnya
/// supaya uji yang menunggu bisa menyebutkan pelakunya.
pub fn kunci_live(tag: &str) -> KunciLive {
    let path = path_kunci();
    let berkas = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap_or_else(|e| panic!("membuka berkas kunci {}: {e}", path.display()));

    let mulai = Instant::now();
    let mut sudah_lapor = false;
    loop {
        match berkas.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {
                if !sudah_lapor && mulai.elapsed() >= AMBANG_DIAM {
                    eprintln!(
                        "[{tag}] menunggu kunci uji live {} (pemegang: {})",
                        path.display(),
                        pemegang(&path)
                    );
                    sudah_lapor = true;
                }
                if mulai.elapsed() >= BATAS_TUNGGU {
                    panic!(
                        "[{tag}] menunggu kunci uji live lebih dari {BATAS_TUNGGU:?}; pembaca terakhir: {}. Ada uji live lain yang menggantung.",
                        pemegang(&path)
                    );
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                panic!("mengunci {}: {e}", path.display());
            }
        }
    }

    // Catat pemegang untuk diagnosa. Gagal menulis tidak boleh menggagalkan uji:
    // kunci sudah dipegang, dan isi berkas cuma keterangan.
    if let Ok(mut tulis) = berkas.try_clone() {
        let _ = tulis.set_len(0);
        let _ = tulis.seek(SeekFrom::Start(0));
        let _ = writeln!(tulis, "{tag} pid {}", std::process::id());
    }

    KunciLive { _berkas: berkas }
}

/// Isi berkas kunci apa adanya, untuk pesan antrean.
fn pemegang(path: &std::path::Path) -> String {
    std::fs::read_to_string(path)
        .map(|isi| isi.trim().to_string())
        .unwrap_or_default()
}

/// Batas menunggu daemon menutup mesinnya sendiri sesudah SIGTERM.
///
/// Daemon mematikan mesin satu per satu, dan tiap mesin punya lima detik
/// sendiri untuk menutup profilnya.
const SABAR_MATI: Duration = Duration::from_secs(20);

/// Menutup daemon lewat SIGTERM, lalu SIGKILL hanya kalau dia membandel.
///
/// `Child::kill()` mengirim SIGKILL, dan daemon yang dibunuh begitu melewati
/// `matikan_semua`: setiap `chrome_shell`-nya tinggal yatim memegang RAM, port,
/// dan profil. Satu run gate pernah meninggalkan 26 mesin seperti itu; RAM
/// habis, lalu test berikutnya gagal boot dalam 15 detik dan gate terbaca merah
/// padahal kodenya tidak berubah.
pub fn buang(child: &mut Child) {
    let _ = kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM);
    let mulai = Instant::now();
    while mulai.elapsed() < SABAR_MATI {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}
