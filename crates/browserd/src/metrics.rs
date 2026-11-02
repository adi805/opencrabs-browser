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
