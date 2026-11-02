//! Uji yang menyalakan mesin sungguhan.
//!
//! Test unit di dalam crate membuktikan parser dan aritmetika. Test ini
//! membuktikan hal yang paling mahal kalau salah: **proses benar-benar hidup,
//! lalu benar-benar mati seluruh group-nya**. Klaim "memori kembali" tidak
//! punya arti apa pun kalau zygote renderer masih hidup setelah daemon berhenti.
//!
//! Dilewati dengan pesan jelas kalau `chrome-headless-shell` tidak ada, supaya
//! `cargo test` tetap hijau di mesin yang tidak punya browser. Di mesin ini
//! binernya ada, jadi test-nya benar-benar jalan.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::{Engine, LaunchSpec, ShellEngine, discover_shell};
use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

/// Membunuh group yang masih hidup kalau assertion gagal di tengah jalan.
/// Tanpa ini, satu test merah meninggalkan browser hantu yang memakan RAM
/// sampai mesin di-reboot.
struct Reaper {
    pgid: Option<i32>,
}

impl Reaper {
    fn new() -> Self {
        Self { pgid: None }
    }

    fn arm(&mut self, pid: u32) {
        self.pgid = Some(pid as i32);
    }

    fn disarm(&mut self) {
        self.pgid = None;
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        if let Some(pgid) = self.pgid {
            let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
        }
    }
}

fn group_alive(pgid: i32) -> bool {
    match killpg(Pid::from_raw(pgid), None) {
        Ok(()) => true,
        Err(Errno::ESRCH) => false,
        // EPERM berarti group ada tapi bukan milik kita. Tidak mungkin di sini,
        // tapi salah menganggapnya mati jauh lebih berbahaya daripada panik.
        Err(err) => panic!("killpg gagal dengan {err}, tidak bisa memastikan status group"),
    }
}

fn wait_dead(pgid: i32, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if !group_alive(pgid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !group_alive(pgid)
}

/// Profil sementara yang membersihkan dirinya sendiri, termasuk saat test
/// gagal. Profil yang ditinggalkan bukan cuma sampah: berkas
/// `DevToolsActivePort` di dalamnya bisa terbaca run berikutnya dan menunjuk
/// port yang sudah mati.
struct TempProfile {
    path: PathBuf,
}

impl TempProfile {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ocb-engine-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        Self { path }
    }
}

impl Drop for TempProfile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn mesin_nyala_jawab_discovery_lalu_mati_seluruh_group() {
    let Some(binary) = discover_shell() else {
        eprintln!(
            "LEWATI: chrome-headless-shell tidak ditemukan; \
             test mesin sungguhan tidak dijalankan di mesin ini"
        );
        return;
    };

    let profile = TempProfile::new("live");
    let spec = LaunchSpec::new(binary, profile.path.clone());
    let mut reaper = Reaper::new();

    let engine = ShellEngine::launch(spec, 20_000).expect("mesin harus siap dalam 20 detik");
    let pid = engine.process_id().expect("mesin punya pid");
    reaper.arm(pid);

    // 1. Mesin melaporkan dirinya sendiri, bukan laporan karangan kita.
    let info = engine.info();
    assert!(
        info.browser.to_lowercase().contains("chrome"),
        "browser tak terduga: {}",
        info.browser
    );
    assert!(
        info.web_socket_debugger_url.starts_with("ws://127.0.0.1:"),
        "websocket harus loopback: {}",
        info.web_socket_debugger_url
    );

    // 2. Group hidup sebelum dimatikan. Kalau ini gagal, artinya `process_group(0)`
    //    tidak berlaku dan seluruh uji kebocoran di bawah jadi tidak bermakna.
    assert!(
        group_alive(pid as i32),
        "group {pid} harus hidup saat mesin jalan"
    );
    assert!(engine.is_running());

    // 3. Matikan: SIGTERM ke group, tunggu, lalu SIGKILL kalau bandel.
    engine.stop().expect("stop harus berhasil");
    assert!(
        !engine.is_running(),
        "mesin melaporkan masih jalan setelah stop"
    );

    assert!(
        wait_dead(pid as i32, Duration::from_secs(10)),
        "group {pid} masih hidup 10 detik setelah stop: inilah kebocoran memori yang \
         ingin dicegah, dan test ini menangkapnya"
    );
    reaper.disarm();
}

#[test]
fn biner_yang_tidak_ada_ditolak_tanpa_spawn() {
    let dir = TempProfile::new("missing");
    let spec = LaunchSpec::new("/nonexistent/chrome-headless-shell", dir.path.clone());
    let err = ShellEngine::launch(spec, 2_000).expect_err("biner hilang harus error");
    assert!(
        err.to_string().to_lowercase().contains("biner"),
        "pesan error harus menyebut biner: {err}"
    );
}
