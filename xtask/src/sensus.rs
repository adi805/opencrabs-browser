//! Sensus proses mesin uji.
//!
//! # Kenapa penanda, bukan nama biner
//!
//! Host pengembangan ini juga menjalankan browser produksi (port 9222) dan
//! daemon pengembangan (9224/9225). Menghitung semua proses `chrome` berarti
//! menuduh mesin milik pemilik host, dan gate yang menuduh orang lain akan
//! dimatikan orang. Setiap mesin uji membuat profil di bawah `/tmp/ocb-*`,
//! jadi penanda itulah yang memisahkan mesin uji dari mesin milik pengguna.
//!
//! Penanda dibangun saat runtime. Kalau ditulis utuh di baris perintah atau di
//! dalam berkas ini sebagai literal yang ikut terbaca, proses `xtask` sendiri
//! bisa ikut terhitung dan gate melaporkan kebocoran yang tidak ada.

use anyhow::{Result, bail};

/// Penanda direktori sementara milik mesin uji, dibangun saat runtime.
fn penanda() -> String {
    format!("/tmp/{}", "ocb-")
}

/// Satu proses yang ikut sensus.
pub struct Proses {
    pub pid: i32,
    pub cmdline: String,
}

/// Hasil satu kali pembacaan `/proc`.
pub struct Sensus {
    pub proses: Vec<Proses>,
}

impl Sensus {
    pub fn jumlah(&self) -> usize {
        self.proses.len()
    }

    fn pids(&self) -> Vec<i32> {
        self.proses.iter().map(|p| p.pid).collect()
    }
}

/// Membaca `/proc` dan mengumpulkan semua proses yang membawa penanda.
///
/// Proses yang `/proc/<pid>/cmdline`-nya tidak terbaca dilewati: itu proses
/// milik pengguna lain atau yang sudah mati di tengah pembacaan, dan keduanya
/// bukan kebocoran.
pub fn ambil() -> Sensus {
    let penanda = penanda();
    let mut proses = Vec::new();

    let Ok(isi) = std::fs::read_dir("/proc") else {
        return Sensus { proses };
    };

    for entri in isi.flatten() {
        let nama = entri.file_name();
        let Some(pid) = nama.to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&bytes).replace('\0', " ");
        if cmdline.contains(&penanda) {
            proses.push(Proses { pid, cmdline });
        }
    }

    proses.sort_by_key(|p| p.pid);
    Sensus { proses }
}

/// Membandingkan sensus awal dan akhir.
///
/// Yang diperiksa adalah **himpunan pid**, bukan hanya jumlahnya. Dua proses di
/// awal dan dua di akhir bisa berarti dua yang lama bocor dan dua yang baru
/// lahir, dan jumlah yang sama menyembunyikan itu.
pub fn bandingkan(awal: &Sensus, akhir: &Sensus) -> Result<()> {
    let lama = awal.pids();
    let baru: Vec<&Proses> = akhir
        .proses
        .iter()
        .filter(|p| !lama.contains(&p.pid))
        .collect();

    if baru.is_empty() {
        println!(
            "-> {:<8}ok ({} -> {})",
            "sensus",
            awal.jumlah(),
            akhir.jumlah()
        );
        return Ok(());
    }

    println!(
        "-> {:<8}GAGAL ({} -> {}, {} proses baru)",
        "sensus",
        awal.jumlah(),
        akhir.jumlah(),
        baru.len()
    );
    for p in &baru {
        let pendek: String = p.cmdline.chars().take(140).collect();
        println!("   {} {}", p.pid, pendek.trim());
    }

    bail!("{} mesin uji tidak kembali ke angka awal", baru.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proses(pid: i32) -> Proses {
        Proses {
            pid,
            cmdline: format!("chrome --user-data-dir={}/x", penanda()),
        }
    }

    /// Dua pid yang sama bukan kebocoran.
    #[test]
    fn pid_yang_sama_lolos() {
        let awal = Sensus {
            proses: vec![proses(1), proses(2)],
        };
        let akhir = Sensus {
            proses: vec![proses(1), proses(2)],
        };
        assert!(bandingkan(&awal, &akhir).is_ok());
    }

    /// Jumlah yang sama tapi pid berbeda tetap kebocoran: dua yang lama mati
    /// dan dua yang baru lahir tidak boleh lolos sebagai "tidak berubah".
    #[test]
    fn jumlah_sama_pid_beda_ditolak() {
        let awal = Sensus {
            proses: vec![proses(1), proses(2)],
        };
        let akhir = Sensus {
            proses: vec![proses(3), proses(4)],
        };
        assert!(bandingkan(&awal, &akhir).is_err());
    }

    /// Satu proses baru sesudah sensus awal sudah cukup untuk gagal.
    #[test]
    fn satu_pid_baru_ditolak() {
        let awal = Sensus {
            proses: vec![proses(1)],
        };
        let akhir = Sensus {
            proses: vec![proses(1), proses(2)],
        };
        assert!(bandingkan(&awal, &akhir).is_err());
    }

    /// Pemindai `/proc` benar-benar menemukan proses yang membawa penanda.
    ///
    /// Tanpa uji ini, penanda yang salah ketik akan membuat sensus selalu
    /// hijau dan gate-nya cuma hiasan.
    #[test]
    fn pemindai_menemukan_proses_berpenanda() {
        let penanda = penanda();
        // Dua perintah, bukan satu: bash mengganti dirinya dengan perintah
        // terakhir (`exec`), dan argv yang memuat penanda ikut hilang. Dengan
        // `sleep 30; :` bash tetap hidup sebagai induk dan argv-nya terbaca.
        let mut anak = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!("sleep 30; : # {penanda}uji-sensus"))
            .stdin(std::process::Stdio::null())
            .spawn()
            .expect("melahirkan proses uji");

        let pid = anak.id() as i32;
        let mut ketemu = false;
        for _ in 0..50 {
            if ambil().proses.iter().any(|p| p.pid == pid) {
                ketemu = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = anak.kill();
        let _ = anak.wait();

        assert!(
            ketemu,
            "penanda {penanda} tidak menemukan proses {pid} di /proc"
        );
    }
}
