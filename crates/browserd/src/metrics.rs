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

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::get;
use engine::Engine;
use lifecycle::{Counters, Machine, State};
use policy::counters::CaptchaCounters;

/// Gauge HIDUP: PSS dibaca saat permintaan datang, bukan disimpan dari
/// pembacaan terakhir. Angka yang disimpan akan terlihat masuk akal di
/// dashboard sambil salah.
pub fn router(
    engine: Arc<dyn Engine>,
    counters: Arc<Counters>,
    captcha: Arc<CaptchaCounters>,
    machine: Arc<Mutex<Machine>>,
) -> Router {
    let handler = move || {
        let engine = Arc::clone(&engine);
        let counters = Arc::clone(&counters);
        let captcha = Arc::clone(&captcha);
        let machine = Arc::clone(&machine);

        async move {
            let pss = engine.process_id().and_then(read_pss_bytes);
            let state = machine.lock().map_or(State::Stopped, |guard| guard.state());
            // Dua keluarga metrik digabung di satu endpoint supaya scraper
            // tidak perlu dua target. Tanpa penggabungan ini, handoff yang
            // berjalan akan tidak terlihat di dashboard sama sekali.
            let mut body = counters.render(engine.kind(), state, pss);
            body.push_str(&captcha.render());

            (
                [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
                body,
            )
        }
    };

    Router::new().route("/metrics", get(handler))
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
    use super::read_pss_bytes;

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
