//! Penyesuaian [`ProcessGroup`] ke proses Unix yang benar-benar ada di mesin ini.
//!
//! [`lifecycle::reaper::reap`] memutuskan **kapan** sinyal dikirim; modul ini
//! hanya mengantarkannya. Aturan apa pun tentang urutan atau tenggat tidak
//! boleh hidup di sini.
//!
//! Kenapa grup dan bukan satu pid: anak dari shell (zygote, renderer) tidak
//! ikut mati kalau hanya induknya di-SIGTERM. Yang tersisa itu memori yang
//! tidak pernah kembali ke sistem.

use std::io;
use std::path::Path;

use lifecycle::pgrp_from_stat;
use lifecycle::reaper::{ProcessGroup, Signal as ReapSignal, SignalError};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

/// Satu process group Unix. [`Self::new`] menerima pgid, bukan pid: keduanya
/// kebetulan sama hanya kalau pemanggil memakai `process_group(0)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PgidGroup {
    pgid: u32,
}

impl PgidGroup {
    #[must_use]
    pub const fn new(pgid: u32) -> Self {
        Self { pgid }
    }

    #[must_use]
    pub const fn pgid(self) -> u32 {
        self.pgid
    }
}

impl ProcessGroup for PgidGroup {
    fn signal(&self, signal: ReapSignal) -> Result<(), SignalError> {
        let os_signal = match signal {
            ReapSignal::Term => Signal::SIGTERM,
            ReapSignal::Kill => Signal::SIGKILL,
        };

        // `libc::killpg` yang dipakai nix sudah menegasikan pgid-nya sendiri:
        // argumen di sini **positif**. Menegasikan manual akan membalik artinya
        // jadi "kirim ke satu proses ber-pid ini".
        killpg(
            Pid::from_raw(i32::try_from(self.pgid).unwrap_or(i32::MAX)),
            os_signal,
        )
        .map_err(|err| SignalError::Send {
            signal,
            source: io::Error::from_raw_os_error(err as i32),
        })
    }

    fn alive(&self) -> Result<usize, SignalError> {
        count_alive_in_group(self.pgid).map_err(|source| SignalError::Alive { source })
    }
}

/// Jumlah proses hidup di satu group.
///
/// Zombie **tidak dihitung**. Proses yang sudah mati tetapi belum di-`wait`
/// induknya masih punya direktori `/proc` dan masih terlihat "ada" bagi
/// pembaca yang tidak melihat kolom state. Menghitungnya membuat reaper
/// mengeskalasi ke SIGKILL untuk mayat, lalu melaporkan `Survived` padahal
/// group-nya sudah bersih.
fn count_alive_in_group(pgid: u32) -> io::Result<usize> {
    let mut alive = 0;

    for entry in std::fs::read_dir("/proc")? {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };

        let Some(stat) = read_stat(pid) else { continue };
        if pgrp_from_stat(&stat) != Some(pgid) || is_zombie(&stat) {
            continue;
        }

        alive += 1;
    }

    Ok(alive)
}

fn read_stat(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()
}

/// Kolom pertama setelah kurung tutup terakhir adalah state proses.
fn is_zombie(stat: &str) -> bool {
    stat.rfind(')')
        .and_then(|end| stat.get(end + 1..))
        .and_then(|rest| rest.split_whitespace().next())
        .is_some_and(|state| state == "Z")
}

/// Membuang direktori profil engine kalau ada. Dipakai jalur pembersihan
/// `selftest` dan jalur gagal boot; kegagalan di sini tidak boleh menutupi
/// kegagalan aslinya, jadi hasilnya `bool`, bukan `Result`.
pub fn remove_profile(path: &Path) -> bool {
    std::fs::remove_dir_all(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lifecycle::reaper::{ReapOutcome, ReapReason, ReaperConfig};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    fn config_cepat() -> ReaperConfig {
        ReaperConfig {
            idle_reap: Duration::from_secs(60),
            term_grace: Duration::from_millis(300),
            kill_grace: Duration::from_millis(500),
            poll: Duration::from_millis(20),
            max_polls: 200,
        }
    }

    /// Anak yang mengabaikan SIGTERM: satu-satunya cara menghentikannya adalah
    /// SIGKILL, dan itu memang yang ingin diuji.
    fn spawn_yang_nolak_term() -> (std::process::Child, u32) {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("trap '' TERM; while :; do sleep 1; done")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);

        let child = cmd.spawn().expect("sh bisa dijalankan");
        let pgid = child.id();
        (child, pgid)
    }

    #[test]
    fn zombie_tidak_dihitung_hidup() {
        assert!(is_zombie("12 (nama) Z 1 1 1"));
        assert!(!is_zombie("12 (nama) S 1 1 1"));
        // Nama proses boleh memuat spasi; kurung tutup terakhir yang dipakai.
        assert!(is_zombie("12 (na ma) Z 1 1 1"));
    }

    #[test]
    fn stat_tanpa_kurung_tutup_tidak_dianggap_zombie() {
        assert!(!is_zombie("12 (belum selesai"));
    }

    #[test]
    fn grup_kosong_terbaca_nol_bukan_galat() {
        // Pgid yang tidak mungkin dipakai proses mana pun.
        let group = PgidGroup::new(0x7fff_fff0);
        assert_eq!(group.alive().expect("scan /proc bisa dibaca"), 0);
    }

    #[test]
    fn proses_hidup_dihitung_lebih_dari_nol() {
        let (mut child, pgid) = spawn_yang_nolak_term();
        let group = PgidGroup::new(pgid);
        let alive = group.alive().expect("scan /proc bisa dibaca");
        assert!(alive >= 1, "group hidup harus terhitung, dapat {alive}");

        let _ = group.signal(ReapSignal::Kill);
        let _ = child.wait();
    }

    #[test]
    fn term_dulu_lalu_kill_dan_group_bersih() {
        let (mut child, pgid) = spawn_yang_nolak_term();
        let group = PgidGroup::new(pgid);

        let report = lifecycle::reaper::reap(&group, &config_cepat(), ReapReason::Manual)
            .expect("reap tidak boleh gagal sinyal");

        assert!(report.sent_term, "SIGTERM wajib dikirim lebih dulu");
        assert!(report.sent_kill, "penolak TERM harus kena SIGKILL");
        assert_eq!(report.outcome, ReapOutcome::Killed, "laporan: {report}");
        assert_eq!(group.alive().expect("scan /proc"), 0, "group harus bersih");

        let _ = child.wait();
    }

    #[test]
    fn proses_yang_patuh_mati_dengan_term_saja() {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("while :; do sleep 1; done")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = cmd.spawn().expect("sh bisa dijalankan");
        let pgid = child.id();
        let group = PgidGroup::new(pgid);

        let report = lifecycle::reaper::reap(&group, &config_cepat(), ReapReason::Shutdown)
            .expect("reap tidak boleh gagal sinyal");

        assert_eq!(report.outcome, ReapOutcome::Terminated, "laporan: {report}");
        assert!(!report.sent_kill, "yang patuh tidak perlu dibunuh paksa");

        let _ = child.wait();
    }

    #[test]
    fn grup_yang_sudah_mati_dilaporkan_sudah_pergi() {
        let (mut child, pgid) = spawn_yang_nolak_term();
        let group = PgidGroup::new(pgid);
        let _ = group.signal(ReapSignal::Kill);
        let _ = child.wait();
        // Setelah wait, anak sudah tidak ada: group kosong tanpa sinyal.
        while group.alive().expect("scan /proc") != 0 {
            std::thread::sleep(Duration::from_millis(20));
        }

        let report = lifecycle::reaper::reap(&group, &config_cepat(), ReapReason::Idle)
            .expect("reap tidak boleh gagal sinyal");

        assert_eq!(report.outcome, ReapOutcome::AlreadyGone);
        assert!(!report.sent_term && !report.sent_kill);
    }
}
