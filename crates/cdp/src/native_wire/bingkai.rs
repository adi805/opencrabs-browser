//! Pengambilan dokumen `<iframe>` untuk satu dokumen induk.
//!
//! `engine::native::bingkai::rencana` memutuskan **apakah** satu iframe boleh
//! dimuat dan URL mana yang harus diambil; modul ini yang menjalankan
//! keputusan itu di atas jaringan, lalu menyerahkan hasilnya ke
//! `TabPool::pasang_bingkai_dari`.
//!
//! Dua putaran, sama seperti stylesheet: URL iframe baru diketahui setelah
//! HTML induk diurai, dan `src` relatif harus diselesaikan terhadap URL
//! **setelah** pengalihan. Karena itu modul ini menerima pohon yang sudah ada,
//! bukan teks HTML.
//!
//! Batas yang dijaga di sini:
//!
//! - hanya iframe same-origin yang diambil. Lintas asal dilewati **dengan
//!   alasan yang tercatat**, karena mesin ini tidak punya kebijakan CORS
//!   sendiri dan mengambilnya diam-diam akan melanggar batas yang justru
//!   dijaga browser
//! - jumlah, ukuran per dokumen, dan total byte dibatasi; yang melewati batas
//!   dilewati dengan catatan, bukan dipotong, karena HTML yang terpotong
//!   menghasilkan pohon yang salah tanpa satu pun galat
//! - hanya satu tingkat. Iframe di dalam iframe tidak diambil di sini: tiap
//!   tingkat menambah satu putaran jaringan di jalur navigasi, dan batasnya
//!   lebih jujur dinyatakan di `CELAH` daripada ditebak

use std::time::Duration;

use engine::native::Document;
use engine::native::bingkai::{Muatan, rencana};
use futures_util::future::join_all;

use super::jaringan::{self, Batas};

/// Jumlah iframe yang diambil untuk satu dokumen induk.
pub const MAKS_BINGKAI: usize = 8;

/// Batas byte satu dokumen iframe.
pub const BATAS_BYTE_BINGKAI: usize = 4 * 1024 * 1024;

/// Batas total byte seluruh iframe satu dokumen induk.
pub const BATAS_TOTAL_BINGKAI: usize = 12 * 1024 * 1024;

/// Batas waktu satu pengambilan iframe. Lebih pendek dari dokumen induk:
/// iframe yang lambat tidak boleh menahan navigasi yang sudah selesai.
pub const BATAS_WAKTU_BINGKAI: Duration = Duration::from_secs(8);

/// Hasil pengambilan dokumen anak untuk satu dokumen induk.
#[derive(Debug, Default)]
pub struct Hasil {
    /// `(url final, html)` untuk tiap iframe yang berhasil diambil.
    pub dokumen: Vec<(String, String)>,
    /// Iframe yang dilewati atau gagal, beserta alasannya.
    pub catatan: Vec<String>,
}

/// Mengambil dokumen anak untuk setiap `<iframe>` di `dokumen`.
///
/// Tidak pernah mengembalikan galat: iframe yang gagal diambil tidak boleh
/// menggagalkan navigasi induknya. `srcdoc` tidak melewati jaringan sama
/// sekali, jadi ia tidak muncul di sini: pemasangannya diputuskan
/// `TabPool::pasang_bingkai_dari` dari pohonnya sendiri.
pub async fn ambil(url_dokumen: &str, dokumen: &Document) -> Hasil {
    let mut catatan = Vec::new();
    let mut target: Vec<(usize, String)> = Vec::new();
    for (simpul, node) in dokumen.nodes().iter().enumerate() {
        if node.tag() != Some("iframe") {
            continue;
        }
        match rencana(dokumen, simpul, Some(url_dokumen)) {
            Muatan::Ambil(url) => {
                if target.len() >= MAKS_BINGKAI {
                    catatan.push(format!(
                        "iframe {simpul} `{url}`: melewati batas {MAKS_BINGKAI} iframe"
                    ));
                    continue;
                }
                target.push((simpul, url));
            }
            Muatan::LintasAsal(alasan) => catatan.push(format!("iframe {simpul}: {alasan}")),
            Muatan::Srcdoc(_) | Muatan::Kosong => {}
        }
    }
    if target.is_empty() {
        return Hasil {
            dokumen: Vec::new(),
            catatan,
        };
    }

    let batas = Batas {
        byte: BATAS_BYTE_BINGKAI,
        waktu: BATAS_WAKTU_BINGKAI,
        ..Batas::default()
    };
    let hasil = join_all(target.iter().map(|(_, url)| jaringan::muat(url, batas))).await;

    let mut dokumen_anak = Vec::new();
    let mut total = 0usize;
    for ((simpul, url), ambilan) in target.iter().zip(hasil) {
        match ambilan {
            Ok(a) => {
                if total + a.html.len() > BATAS_TOTAL_BINGKAI {
                    catatan.push(format!(
                        "iframe {simpul} `{url}`: total iframe melewati \
                         {BATAS_TOTAL_BINGKAI} byte"
                    ));
                    continue;
                }
                total += a.html.len();
                dokumen_anak.push((a.url, a.html));
            }
            Err(err) => catatan.push(format!("iframe {simpul} `{url}`: {}", err.pesan)),
        }
    }
    Hasil {
        dokumen: dokumen_anak,
        catatan,
    }
}
