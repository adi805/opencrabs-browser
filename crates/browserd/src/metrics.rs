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

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use cdp::EnginePool;
use engine::{Keadaan, Slot};
use lifecycle::{Counters, State};
use policy::counters::CaptchaCounters;

/// Gauge HIDUP: PSS dibaca saat permintaan datang, bukan disimpan dari
/// pembacaan terakhir. Angka yang disimpan akan terlihat masuk akal di
/// dashboard sambil salah.
pub fn router(pool: EnginePool, counters: Arc<Counters>, captcha: Arc<CaptchaCounters>) -> Router {
    let handler = move || {
        let pool = pool.clone();
        let counters = Arc::clone(&counters);
        let captcha = Arc::clone(&captcha);

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
            body.push_str(&captcha.render());

            (
                [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
                body,
            )
        }
    };

    Router::new().route("/metrics", get(handler))
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
