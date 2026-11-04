//! Artefak bukti kegagalan: satu direktori per percobaan, empat berkas, kuota.
//!
//! Bukti yang tidak bisa dibaca ulang tanpa mengulang percobaan itu bukan
//! bukti, itu cerita. Karena itu setiap kegagalan menulis empat berkas yang
//! cukup untuk memutuskan ulang dari meja: `screenshot.png`, `dom.html`,
//! `mesin.log` (200 baris terakhir), dan `ringkasan.json` yang menyebut
//! kategori beserta sebabnya.
//!
//! Tiga pagar yang menjaga ini tidak berubah jadi cara mengisi disk:
//!
//! 1. **Direktori dasar tidak boleh di dalam working tree repo.** Artefak yang
//!    ditulis ke direktori kerja akan ikut ter-commit, ikut terhapus
//!    `git clean`, dan mengotori `git status` tepat saat orang sedang panik.
//!    Pemeriksaannya murah: jalan ke atas mencari `.git`.
//! 2. **Kuota byte dan jumlah percobaan.** Percobaan lama dihapus, yang baru
//!    tidak pernah dihapus oleh pembersihannya sendiri.
//! 3. **Log dipotong 200 baris.** Log Chromium penuh bisa puluhan MB, dan
//!    baris yang dibutuhkan untuk mendiagnosis selalu ada di ekor.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Berapa baris log mesin terakhir yang disimpan.
pub const BARIS_LOG: usize = 200;

/// Batas bawaan: 20 percobaan terakhir, total di bawah 64 MB.
///
/// Angka ini dari ukuran nyata, bukan tebakan: screenshot halaman tantangan
/// 1,6 MB dan halaman biasa 45 KB, `dom.html` ratusan KB, log 200 baris
/// belasan KB. 20 percobaan berarti puluhan MB, dan itu masih di bawah satu
/// batch build.
#[derive(Debug, Clone, Copy)]
pub struct Kuota {
    pub maks_percobaan: usize,
    pub maks_byte_total: u64,
}

impl Default for Kuota {
    fn default() -> Self {
        Self {
            maks_percobaan: 20,
            maks_byte_total: 64 * 1024 * 1024,
        }
    }
}

/// Isi satu percobaan. Semua opsional karena percobaan yang gagal di tengah
/// jalan tetap berhak punya artefak sebagian, bukan gagal menulis apa pun.
#[derive(Debug, Default, Clone)]
pub struct Isi {
    pub screenshot: Option<Vec<u8>>,
    pub dom: String,
    /// Baris log mesin; yang disimpan hanya [`BARIS_LOG`] terakhir.
    pub log: Vec<String>,
    /// Ringkasan status. Wajib memuat `kategori` dan `sebab`.
    pub ringkasan: BTreeMap<String, String>,
}

impl Isi {
    /// Ringkasan dari bukti klasifikasi, bentuk yang selalu dipakai.
    #[must_use]
    pub fn dari_bukti(bukti: &crate::klasifikasi::Bukti) -> Self {
        let mut ringkasan = BTreeMap::new();
        ringkasan.insert("kategori".to_owned(), bukti.kategori.to_string());
        ringkasan.insert("sebab".to_owned(), bukti.sebab.join("; "));
        if let Some(status) = bukti.status {
            ringkasan.insert("status".to_owned(), status.to_string());
        }
        if let Some(url) = bukti.url_akhir.as_deref() {
            ringkasan.insert("url_akhir".to_owned(), url.to_owned());
        }
        if let Some(judul) = bukti.judul.as_deref() {
            ringkasan.insert("judul".to_owned(), judul.to_owned());
        }
        Self {
            ringkasan,
            ..Self::default()
        }
    }
}

/// Direktori dasar artefak, di luar working tree.
#[derive(Debug, Clone)]
pub struct Penulis {
    dasar: PathBuf,
    kuota: Kuota,
}

impl Penulis {
    /// `Err` kalau `dasar` berada di dalam working tree repo.
    pub fn baru(dasar: impl Into<PathBuf>, kuota: Kuota) -> Result<Self> {
        let dasar = dasar.into();
        if let Some(repo) = di_dalam_work_tree(&dasar) {
            bail!(
                "direktori artefak {} berada di dalam working tree {}",
                dasar.display(),
                repo.display()
            );
        }
        Ok(Self { dasar, kuota })
    }

    #[must_use]
    pub fn dasar(&self) -> &Path {
        &self.dasar
    }

    /// Tulis satu percobaan lalu rapikan kuota. Mengembalikan direktorinya.
    ///
    /// `stempel` dipakai apa adanya sebagai awalan nama direktori supaya waktu
    /// diambil pemanggil (jam sistem), bukan di sini: itu yang membuat aturan
    /// "jam diambil di batas crate" tetap berlaku dan ujinya deterministik.
    pub fn tulis(&self, stempel: &str, nama: &str, isi: &Isi) -> Result<PathBuf> {
        let dir = self.dasar.join(format!("{}-{}", slug(stempel), slug(nama)));
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("membuat direktori artefak {}", dir.display()))?;

        if let Some(png) = isi.screenshot.as_deref() {
            tulis(&dir.join("screenshot.png"), png)?;
        }
        tulis(&dir.join("dom.html"), isi.dom.as_bytes())?;
        let ekor: Vec<&str> = isi
            .log
            .iter()
            .skip(isi.log.len().saturating_sub(BARIS_LOG))
            .map(String::as_str)
            .collect();
        tulis(&dir.join("mesin.log"), ekor.join("\n").as_bytes())?;
        let ringkasan =
            serde_json::to_vec_pretty(&isi.ringkasan).context("membentuk ringkasan artefak")?;
        tulis(&dir.join("ringkasan.json"), &ringkasan)?;

        self.rapikan(&dir)?;
        Ok(dir)
    }

    /// Pembersihan kuota: percobaan terlama dihapus sampai jumlah dan total
    /// byte masuk batas. `baru_saja` tidak pernah dihapus.
    fn rapikan(&self, baru_saja: &Path) -> Result<()> {
        let mut percobaan = self.daftar()?;
        // Terlama lebih dulu. Nama selalu berawalan stempel waktu, tapi
        // pengurutan tetap lewat metadata supaya nama yang tidak berpola pun
        // tetap adil.
        percobaan.sort_by_key(|(_, _, diubah)| *diubah);

        let mut total: u64 = percobaan.iter().map(|(_, byte, _)| *byte).sum();
        let mut sisa = percobaan.len();
        for (dir, byte, _) in percobaan {
            let cukup_jumlah = sisa <= self.kuota.maks_percobaan;
            let cukup_byte = total <= self.kuota.maks_byte_total;
            if cukup_jumlah && cukup_byte {
                break;
            }
            if dir == baru_saja {
                continue;
            }
            std::fs::remove_dir_all(&dir)
                .with_context(|| format!("menghapus artefak lama {}", dir.display()))?;
            total = total.saturating_sub(byte);
            sisa -= 1;
        }
        Ok(())
    }

    /// (direktori, byte, waktu ubah) untuk setiap percobaan di bawah dasar.
    fn daftar(&self) -> Result<Vec<(PathBuf, u64, std::time::SystemTime)>> {
        let mut out = Vec::new();
        let Ok(isi) = std::fs::read_dir(&self.dasar) else {
            return Ok(out);
        };
        for entri in isi {
            let entri = entri.with_context(|| format!("membaca {}", self.dasar.display()))?;
            let path = entri.path();
            if !path.is_dir() {
                continue;
            }
            let meta = entri.metadata()?;
            let diubah = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            out.push((path.clone(), ukuran(&path)?, diubah));
        }
        Ok(out)
    }

    /// Jumlah byte seluruh artefak. Dipakai uji kuota dan laporan.
    pub fn total_byte(&self) -> Result<u64> {
        Ok(self.daftar()?.iter().map(|(_, byte, _)| *byte).sum())
    }
}

fn tulis(path: &Path, isi: &[u8]) -> Result<()> {
    let mut berkas =
        std::fs::File::create(path).with_context(|| format!("membuat {}", path.display()))?;
    berkas
        .write_all(isi)
        .with_context(|| format!("menulis {}", path.display()))
}

/// Ukuran seluruh berkas di dalam `dir`, satu tingkat.
fn ukuran(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entri in std::fs::read_dir(dir).with_context(|| format!("membaca {}", dir.display()))? {
        let entri = entri?;
        let meta = entri.metadata()?;
        if meta.is_file() {
            total += meta.len();
        }
    }
    Ok(total)
}

/// Nama direktori yang aman: huruf kecil, angka, tanda hubung.
#[must_use]
pub fn slug(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(48));
    for ch in raw.chars() {
        if out.len() >= 48 {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

/// Jalan ke atas mencari `.git`. `Some(root)` berarti `dasar` ada di dalam
/// working tree dan artefak harus ditolak.
#[must_use]
pub fn di_dalam_work_tree(dasar: &Path) -> Option<PathBuf> {
    let mut sekarang = if dasar.is_absolute() {
        dasar.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(dasar)
    };
    loop {
        if sekarang.join(".git").exists() {
            return Some(sekarang);
        }
        if !sekarang.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::klasifikasi::{Bukti, Kategori, Kenyataan};

    fn tmp(nama: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ocb-artefak-{nama}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn bukti() -> Bukti {
        let ken = Kenyataan {
            status_dokumen: Some(429),
            url_akhir: Some("https://www.google.com/sorry/index".to_owned()),
            judul: Some(String::new()),
            detik: Some(3.5),
            ..Kenyataan::default()
        };
        Bukti::baru(Kategori::BlokAntiBot, vec!["status=429".to_owned()], &ken)
            .expect("sebab tidak kosong")
    }

    #[test]
    fn satu_percobaan_empat_berkas() {
        // AC-005.
        let dasar = tmp("empat");
        let penulis = Penulis::baru(&dasar, Kuota::default()).expect("di luar repo");
        let mut isi = Isi::dari_bukti(&bukti());
        isi.screenshot = Some(vec![0x89, b'P', b'N', b'G']);
        isi.dom = "<html><body>tantangan</body></html>".to_owned();
        isi.log = vec!["baris log".to_owned()];

        let dir = penulis
            .tulis("20261104-085700", "google.com", &isi)
            .unwrap();
        for berkas in ["screenshot.png", "dom.html", "mesin.log", "ringkasan.json"] {
            let path = dir.join(berkas);
            assert!(path.is_file(), "{berkas} tidak ada di {}", dir.display());
            assert!(path.metadata().unwrap().len() > 0, "{berkas} kosong");
        }
        let ringkasan = std::fs::read_to_string(dir.join("ringkasan.json")).unwrap();
        assert!(ringkasan.contains("blok_anti_bot"), "{ringkasan}");
        assert!(ringkasan.contains("status=429"), "{ringkasan}");
        let _ = std::fs::remove_dir_all(&dasar);
    }

    #[test]
    fn percobaan_gagal_separuh_tetap_menulis() {
        // Tanpa screenshot pun direktori artefaknya berguna.
        let dasar = tmp("separuh");
        let penulis = Penulis::baru(&dasar, Kuota::default()).unwrap();
        let dir = penulis
            .tulis(
                "20261104-085800",
                "tanpa-screenshot",
                &Isi::dari_bukti(&bukti()),
            )
            .unwrap();
        assert!(!dir.join("screenshot.png").exists());
        assert!(dir.join("ringkasan.json").is_file());
        let _ = std::fs::remove_dir_all(&dasar);
    }

    #[test]
    fn log_dipotong_dua_ratus_baris() {
        let dasar = tmp("log");
        let penulis = Penulis::baru(&dasar, Kuota::default()).unwrap();
        let log: Vec<String> = (0..500).map(|i| format!("baris-{i}")).collect();
        let isi = Isi {
            log,
            ..Isi::default()
        };
        let dir = penulis.tulis("20261104-085900", "log", &isi).unwrap();
        let tersimpan = std::fs::read_to_string(dir.join("mesin.log")).unwrap();
        assert_eq!(tersimpan.lines().count(), BARIS_LOG);
        assert!(tersimpan.starts_with("baris-300"));
        assert!(tersimpan.ends_with("baris-499"));
        let _ = std::fs::remove_dir_all(&dasar);
    }

    #[test]
    fn kuota_menahan_seratus_kegagalan() {
        // AC-006: 100 kegagalan tetap di bawah batas byte dan jumlah.
        let dasar = tmp("kuota");
        let kuota = Kuota {
            maks_percobaan: 5,
            maks_byte_total: 64 * 1024,
        };
        let penulis = Penulis::baru(&dasar, kuota).unwrap();
        let isi = Isi {
            dom: "x".repeat(2048),
            ..Isi::default()
        };
        for i in 0..100 {
            penulis
                .tulis(&format!("20261104-09{:04}", i), "situs", &isi)
                .unwrap();
        }
        let jumlah = std::fs::read_dir(&dasar).unwrap().count();
        assert!(jumlah <= 5, "jumlah percobaan {jumlah} melebihi kuota");
        let total = penulis.total_byte().unwrap();
        assert!(total <= 64 * 1024, "total byte {total} melebihi kuota byte");
        let _ = std::fs::remove_dir_all(&dasar);
    }

    #[test]
    fn direktori_di_dalam_repo_ditolak() {
        // AC-006: direktori kerja repo tidak pernah dipakai.
        let dasar = tmp("repo");
        std::fs::create_dir_all(dasar.join(".git")).unwrap();
        let salah = Penulis::baru(&dasar, Kuota::default());
        assert!(salah.is_err(), "dasar di dalam repo harus ditolak");
        let pesan = salah.err().unwrap().to_string();
        assert!(pesan.contains("working tree"), "{pesan}");
        let _ = std::fs::remove_dir_all(&dasar);
    }

    #[test]
    fn slug_aman_untuk_nama_direktori() {
        assert_eq!(
            slug("https://www.google.com/sorry/index?x=1"),
            "https-www-google-com-sorry-index-x-1"
        );
        assert_eq!(slug("../../etc/passwd"), "etc-passwd");
        assert_eq!(slug("NOWSECURE.NL"), "nowsecure-nl");
    }
}
