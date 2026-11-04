//! Pembacaan memori yang jujur.
//!
//! PSS, bukan RSS. RSS menghitung halaman bersama sekali per proses, jadi
//! browser dengan 8 proses terlihat dua kali lebih berat padahal memori fisik
//! yang dipakai sama. Keputusan anggaran yang diambil dari RSS akan
//! menghukum arsitektur multi-proses tanpa alasan nyata.
//!
//! Diukur 2 Nov 2026 di mesin ini: 3 tab = 858.8 MB RSS tetapi 356.0 MB PSS,
//! dan `--single-process` = 522.8 MB RSS dengan PSS 355.8 MB. Selisih RSS
//! 336 MB itu bukan penghematan, itu artefak cara menghitung.
//!
//! Sejak M4 daemon bisa menjalankan lebih dari satu mesin. Karena itu blok
//! metrik dipecah dua: bagian **per mesin** (memori, state) dicetak sekali per
//! mesin, dan bagian **milik daemon** (reap, anggaran) dicetak sekali saja.
//! Mencetak seluruh blok per mesin akan menggandakan penghitung yang sebenarnya
//! hanya punya satu nilai, dan dashboard akan melaporkan dua kali kejadian.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::routing::get;
use cdp::EnginePool;
use engine::{Keadaan, Slot};
use lifecycle::{Counters, State};
use policy::counters::CaptchaCounters;

use crate::klasifikasi::{Bukti, Hasil, Kategori, Kenyataan};

/// Gauge HIDUP: PSS dibaca saat permintaan datang, bukan disimpan dari
/// pembacaan terakhir. Angka yang disimpan akan terlihat masuk akal di
/// dashboard sambil salah.
pub fn router(
    pool: EnginePool,
    counters: Arc<Counters>,
    captcha: Arc<CaptchaCounters>,
    kegagalan: Arc<Kegagalan>,
) -> Router {
    let handler = move || {
        let pool = pool.clone();
        let counters = Arc::clone(&counters);
        let captcha = Arc::clone(&captcha);
        let kegagalan = Arc::clone(&kegagalan);

        async move {
            let mut body = String::with_capacity(2048);

            if pool.is_multi() {
                // Label = id mesin, bukan `kind`: dua mesin bisa berjenis sama
                // dan metrik yang mencampurnya tidak bisa dipakai memutuskan
                // mesin mana yang harus dimatikan.
                //
                // State per mesin diambil dari hidup/matinya proses, bukan dari
                // tabel transisi daemon: tabel itu satu untuk seluruh daemon
                // (idle reap per mesin belum ada), jadi memakainya di sini
                // akan mengklaim semua mesin berada di fase yang sama.
                for (id, slot) in pool.slots() {
                    let state = state_slot(slot);
                    let pss = slot
                        .hidup()
                        .and_then(|engine| engine.process_id())
                        .and_then(read_pss_bytes);
                    body.push_str(&counters.render_engine(id, state, pss));
                }
            } else {
                // Satu mesin: label tetap `kind` seperti sebelum M4, supaya
                // dashboard yang sudah ada tidak pecah karena rename label.
                let slot = pool.default_slot();
                let state = state_slot(slot);
                let pss = slot
                    .hidup()
                    .and_then(|engine| engine.process_id())
                    .and_then(read_pss_bytes);
                body.push_str(&counters.render_engine(slot.kind(), state, pss));
            }

            body.push_str(&counters.render_global());
            body.push_str(&kegagalan.render());
            body.push_str(&captcha.render());

            (
                [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
                body,
            )
        }
    };

    Router::new().route("/metrics", get(handler))
}

/// Penghitung kegagalan buka-web, satu seri per kategori.
///
/// Aturan yang dijaga di sini: **kategori yang belum pernah terukur tidak
/// dirender.** Menulis `browser_open_failures_total{kategori="jaringan"} 0`
/// untuk kategori yang belum pernah dihitung bukan laporan keadaan, itu klaim
/// bahwa jaringan sudah diuji dan baik-baik saja. Dashboard yang menerima nol
/// karangan akan menyimpulkan sehat tanpa ada yang pernah memeriksa.
///
/// Karena itu penghitung ini dimulai kosong dan setiap kategori baru muncul
/// setelah kegagalan pertamanya benar-benar tercatat.
#[derive(Debug, Default)]
pub struct Kegagalan {
    hitung: [AtomicU64; KATEGORI],
}

const KATEGORI: usize = 6;

impl Kegagalan {
    #[must_use]
    pub fn baru() -> Self {
        Self::default()
    }

    /// Catat satu kegagalan untuk kategori ini.
    pub fn catat(&self, kategori: Kategori) {
        self.hitung[indeks(kategori)].fetch_add(1, Ordering::Relaxed);
    }

    /// Catat hasil satu percobaan buka-web.
    ///
    /// Halaman yang **terbuka** tidak dihitung: penghitung ini mengukur
    /// kegagalan, dan menambah angka untuk halaman sehat akan membuat judul
    /// metriknya bohong. Bukti tanpa kategori juga tidak dihitung, karena
    /// tidak ada label yang bisa dipakai.
    pub fn catat_bukti(&self, bukti: &Bukti) {
        self.catat(bukti.kategori);
    }

    /// Catat hasil klasifikasi satu percobaan, kalau memang gagal.
    pub fn catat_kenyataan(&self, kenyataan: &Kenyataan) {
        if let Hasil::Gagal(bukti) = crate::klasifikasi::nilai(kenyataan) {
            self.catat(bukti.kategori);
        }
    }

    /// Berapa kali kategori ini tercatat. `0` berarti belum pernah terukur,
    /// bukan "tidak pernah gagal".
    #[must_use]
    pub fn jumlah(&self, kategori: Kategori) -> u64 {
        self.hitung[indeks(kategori)].load(Ordering::Relaxed)
    }

    /// Blok metrik. Kategori tanpa catatan tidak muncul sama sekali.
    #[must_use]
    pub fn render(&self) -> String {
        let terukur = Kategori::SEMUA
            .into_iter()
            .filter(|kategori| self.jumlah(*kategori) > 0)
            .collect::<Vec<_>>();
        if terukur.is_empty() {
            return String::new();
        }

        let mut out = String::with_capacity(256);
        out.push_str("# TYPE browser_open_failures_total counter\n");
        for kategori in terukur {
            let _ = writeln!(
                out,
                "browser_open_failures_total{{kategori=\"{}\"}} {}",
                kategori.as_str(),
                self.jumlah(kategori)
            );
        }
        out
    }
}

const fn indeks(kategori: Kategori) -> usize {
    match kategori {
        Kategori::MesinMati => 0,
        Kategori::MesinPenuh => 1,
        Kategori::Jaringan => 2,
        Kategori::ButuhManusia => 3,
        Kategori::BlokAntiBot => 4,
        Kategori::RenderLambat => 5,
    }
}

/// State metrik satu slot.
///
/// Diambil dari slot, bukan dari tabel transisi daemon: tabel itu satu untuk
/// seluruh daemon, jadi memakainya per mesin akan mengklaim semua mesin berada
/// di fase yang sama. Yang bisa dibedakan slot: hidup (`Ready`), sengaja
/// ditidurkan (`Stopped`), dan belum pernah dinyalakan (`Cold`).
///
/// Mesin yang mati di luar kendali juga dilaporkan `Stopped`: prosesnya memang
/// tidak ada, dan itu satu-satunya pernyataan yang benar di sini. Yang **tidak**
/// diklaim adalah siapa yang mematikannya, karena `browser_engine_state` tidak
/// punya label untuk itu.
fn state_slot(slot: &Slot) -> State {
    match slot.keadaan() {
        Keadaan::Hidup => State::Ready,
        Keadaan::Tidur | Keadaan::Mati => State::Stopped,
        Keadaan::BelumPernah => State::Cold,
    }
}

/// PSS total satu proses, dibaca dari `/proc/<pid>/smaps_rollup`.
///
/// `None` kalau proses sudah mati atau berkasnya tidak bisa dibaca. Sengaja
/// tidak mengembalikan `0`: nol berarti "tidak memakai memori", dan itu klaim
/// yang berbeda dari "tidak tahu".
#[must_use]
pub fn read_pss_bytes(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    text.lines().find_map(|line| {
        let rest = line.strip_prefix("Pss:")?;
        let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
        Some(kb * 1024)
    })
}

#[cfg(test)]
mod tests {
    use super::{Kegagalan, read_pss_bytes};
    use crate::klasifikasi::{Bukti, Kategori, Kenyataan};

    fn bukti(kategori: Kategori) -> Bukti {
        let kenyataan = Kenyataan {
            status_dokumen: Some(429),
            ..Kenyataan::default()
        };
        Bukti::baru(kategori, vec!["status=429".to_owned()], &kenyataan)
            .expect("sebab ada berarti bukti terbentuk")
    }

    #[test]
    fn kategori_tak_terukur_tidak_dirender() {
        // AC-018: hanya kategori yang pernah terukur yang muncul. Nol karangan
        // untuk kategori yang belum pernah diperiksa bukan laporan keadaan.
        let kegagalan = Kegagalan::baru();
        assert_eq!(
            kegagalan.render(),
            "",
            "belum ada yang terukur, blok kosong"
        );

        kegagalan.catat(Kategori::BlokAntiBot);
        let blok = kegagalan.render();
        assert!(blok.contains("# TYPE browser_open_failures_total counter"));
        assert!(blok.contains("kategori=\"blok_anti_bot\"} 1"), "{blok}");
        assert!(!blok.contains("kategori=\"jaringan\""), "{blok}");
        assert_eq!(kegagalan.jumlah(Kategori::Jaringan), 0);
    }

    #[test]
    fn catat_bukti_menambah_satu_per_kejadian() {
        let kegagalan = Kegagalan::baru();
        kegagalan.catat_bukti(&bukti(Kategori::Jaringan));
        kegagalan.catat_bukti(&bukti(Kategori::Jaringan));
        kegagalan.catat_bukti(&bukti(Kategori::RenderLambat));
        assert_eq!(kegagalan.jumlah(Kategori::Jaringan), 2);
        assert_eq!(kegagalan.jumlah(Kategori::RenderLambat), 1);

        let blok = kegagalan.render();
        let jaringan = blok
            .lines()
            .find(|baris| baris.contains("kategori=\"jaringan\""))
            .expect("baris jaringan ada");
        assert!(jaringan.ends_with(" 2"), "{jaringan}");
    }

    #[test]
    fn halaman_terbuka_tidak_dihitung_sebagai_kegagalan() {
        let kegagalan = Kegagalan::baru();
        let kenyataan = Kenyataan {
            status_dokumen: Some(200),
            url_akhir: Some("https://contoh.test/".to_owned()),
            dokumen_selesai: Some(true),
            mesin_hidup: Some(true),
            ..Kenyataan::default()
        };
        kegagalan.catat_kenyataan(&kenyataan);
        assert_eq!(kegagalan.render(), "", "halaman terbuka bukan kegagalan");
    }

    #[test]
    fn kenyataan_yang_gagal_tercatat_dengan_kategori_benar() {
        let kegagalan = Kegagalan::baru();
        let kenyataan = Kenyataan {
            galat_jaringan: Some("net::ERR_NAME_NOT_RESOLVED".to_owned()),
            mesin_hidup: Some(true),
            ..Kenyataan::default()
        };
        kegagalan.catat_kenyataan(&kenyataan);
        assert_eq!(kegagalan.jumlah(Kategori::Jaringan), 1);
    }

    #[test]
    fn pss_proses_sendiri_terbaca() {
        let pid = std::process::id();
        let pss = read_pss_bytes(pid).expect("smaps_rollup proses sendiri harus terbaca");
        assert!(pss > 0, "proses yang hidup memakai memori, dapat {pss}");
    }

    #[test]
    fn pid_tidak_ada_balik_none_bukan_nol() {
        assert_eq!(read_pss_bytes(u32::MAX), None);
    }
}
