//! Adapter untuk Chromium penuh yang hidup di dalam Xvfb.
//!
//! `chrome-headless-shell` (lihat `shell.rs`) cukup untuk hampir semua
//! pekerjaan: headless, hemat, cepat siap. Yang tidak dia punya adalah bentuk
//! browser biasa. Dua hal yang paling sering dipakai situs untuk membedakan
//! keduanya: `navigator.webdriver` yang bernilai `true`, dan ketiadaan display
//! maupun window. Adapter ini menjawab keduanya dengan cara paling membosankan,
//! yaitu menjalankan Chromium **biasa** di atas display X virtual.
//!
//! Tiga aturan yang dijaga di sini:
//!
//! 1. **Dua proses, satu nyawa.** Xvfb dan Chromium saling bergantung: Chromium
//!    mati tanpa display, Xvfb tidak berguna tanpa klien. Keduanya dimatikan
//!    lewat [`Proc`], jadi tidak ada Xvfb yatim yang memegang memori setelah
//!    mesin di-reap.
//! 2. **Tanpa `--headless`.** Menambahkan flag itu mengubah mesinnya kembali
//!    menjadi shell, dan seluruh alasan adapter ini ada ikut hilang.
//! 3. **X hanya lewat socket lokal.** Xvfb dijalankan dengan `-nolisten tcp`;
//!    server X yang mendengarkan TCP adalah pintu terbuka di mesin ini.
//!
//! Yang **tidak** dilakukan di sini: menambal fingerprint dengan flag
//! `--disable-blink-features=AutomationControlled` dan sejenisnya. Flag seperti
//! itu mengubah perilaku mesin tanpa bukti terukur di repo ini, dan klaim
//! "tidak terdeteksi" yang tidak diukur adalah persis jenis klaim yang dilarang
//! `docs/prd/DECISIONS.md`. Kalau nanti dibutuhkan, ukur dulu.

use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::proses::{Proc, stderr_ke_file};
use crate::shell::{http_get, info_from_json, wait_for_port_file};
use crate::{Engine, EngineError, EngineInfo, LaunchSpec};

pub const DEFAULT_READY_TIMEOUT_MS: u64 = 30_000;

/// Berkas stderr kedua proses, di dalam direktori profil. Dipisah supaya
/// kegagalan "chrome tidak siap" tidak tertukar dengan kegagalan Xvfb: dua
/// sebab berbeda, dua tempat membaca yang berbeda.
pub const STDERR_LOG: &str = "penuh-stderr.log";
pub const XVFB_STDERR_LOG: &str = "xvfb-stderr.log";

/// Rentang nomor display yang dicari. Di bawah 99 biasanya ditempati X milik
/// desktop pengguna, dan menabraknya berarti menyalakan mesin di layar orang.
pub const DISPLAY_MULAI: u32 = 99;
pub const DISPLAY_SAMPAI: u32 = 999;

/// Tempat X menaruh socket (`/tmp/.X11-unix/X<n>`) dan lock (`.X<n>-lock`).
pub const SOCKET_DIR: &str = "/tmp/.X11-unix";
pub const LOCK_DIR: &str = "/tmp";

const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Flag Chromium penuh. Sengaja **tanpa** `--headless`: biner ini dijalankan
/// headed di atas display virtual.
const PENUH_FLAGS: &[&str] = &[
    "--no-sandbox",
    "--disable-dev-shm-usage",
    "--mute-audio",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-background-networking",
    "--disable-sync",
    "--disable-extensions",
    "--window-size=1920,1080",
];

/// Flag Xvfb. `-nolisten tcp` wajib (lihat aturan 3 di atas), dan `-noreset`
/// menjaga server tetap hidup saat klien terakhir putus, sehingga mesin yang
/// di-reap lalu dinyalakan ulang tidak menemukan display yang mendadak hilang.
const XVFB_FLAGS: &[&str] = &[
    "-screen",
    "0",
    "1920x1080x24",
    "-nolisten",
    "tcp",
    "-noreset",
];

pub struct PenuhEngine {
    spec: LaunchSpec,
    info: EngineInfo,
    xvfb: Mutex<Proc>,
    proc: Mutex<Proc>,
}

impl PenuhEngine {
    /// Menyalakan Xvfb, lalu Chromium headed di atasnya, lalu menunggu sampai
    /// endpoint discovery menjawab.
    ///
    /// Kalau salah satu langkah gagal, **semua** yang sudah telanjur hidup
    /// dimatikan sebelum error dikembalikan. Xvfb yang tertinggal dari boot
    /// gagal tetap memegang display dan memori sampai mesin di-reboot, dan itu
    /// jenis kebocoran yang tidak pernah ketahuan dari log.
    pub fn launch(spec: LaunchSpec, timeout_ms: u64) -> Result<Self, EngineError> {
        if !spec.binary.is_file() {
            return Err(EngineError::BinaryMissing(spec.binary.clone()));
        }
        let xvfb_bin = crate::probe::discover_xvfb().ok_or(EngineError::XvfbMissing)?;
        std::fs::create_dir_all(&spec.user_data_dir)?;

        let socket_dir = Path::new(SOCKET_DIR);
        let nomor = pilih_display(socket_dir, Path::new(LOCK_DIR))
            .ok_or(EngineError::DisplayPenuh(DISPLAY_MULAI, DISPLAY_SAMPAI))?;
        let display = format!(":{nomor}");
        let socket = socket_dir.join(format!("X{nomor}"));

        let mut xvfb_cmd = Command::new(&xvfb_bin);
        xvfb_cmd.arg(&display);
        xvfb_cmd.args(XVFB_FLAGS);
        stderr_ke_file(&mut xvfb_cmd, &spec.user_data_dir.join(XVFB_STDERR_LOG));
        let mut xvfb = Proc::spawn(xvfb_cmd)?;

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        if let Err(err) = tunggu_socket(&socket, deadline, timeout_ms) {
            xvfb.kill_group();
            return Err(err);
        }

        // Berkas dari run sebelumnya membuat kita membaca port yang sudah mati.
        let port_file = spec.user_data_dir.join("DevToolsActivePort");
        let _ = std::fs::remove_file(&port_file);

        let mut cmd = Command::new(&spec.binary);
        cmd.env("DISPLAY", &display);
        cmd.args(argumen(&spec));
        stderr_ke_file(&mut cmd, &spec.user_data_dir.join(STDERR_LOG));
        let mut proc = Proc::spawn(cmd)?;

        let siap = (|| -> Result<EngineInfo, EngineError> {
            let port = if spec.port == 0 {
                wait_for_port_file(&port_file, deadline, timeout_ms)?
            } else {
                spec.port
            };
            let body = tunggu_version(port, deadline, timeout_ms)?;
            info_from_json(&body, port)
        })();

        match siap {
            Ok(info) => Ok(Self {
                spec,
                info,
                xvfb: Mutex::new(xvfb),
                proc: Mutex::new(proc),
            }),
            Err(err) => {
                proc.kill_group();
                xvfb.kill_group();
                Err(err)
            }
        }
    }
}

/// `Debug` ditulis tangan: yang berguna di log adalah resep dan fakta mesin,
/// bukan isi `Mutex<Child>` yang tidak bisa dibaca dari luar.
impl std::fmt::Debug for PenuhEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PenuhEngine")
            .field("spec", &self.spec)
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl Engine for PenuhEngine {
    /// Nama yang sama dengan `policy::EngineKind::Full::as_str()`, supaya katalog
    /// dan metrik memakai satu ejaan untuk satu mesin.
    fn kind(&self) -> &'static str {
        "chrome-full"
    }

    fn info(&self) -> &EngineInfo {
        &self.info
    }

    fn spec(&self) -> &LaunchSpec {
        &self.spec
    }

    fn http_base(&self) -> String {
        format!("http://127.0.0.1:{}", self.info.devtools_port)
    }

    fn web_socket_url(&self) -> &str {
        &self.info.web_socket_debugger_url
    }

    fn process_id(&self) -> Option<u32> {
        self.proc.lock().ok().map(|guard| guard.pid())
    }

    /// Hidup berarti Chromium-nya hidup. Xvfb yang mati selalu ikut membunuh
    /// kliennya, jadi menjawab dari Chromium sudah mencakup keduanya.
    fn is_running(&self) -> bool {
        let Ok(mut guard) = self.proc.lock() else {
            return false;
        };
        guard.is_running()
    }

    /// Chromium dulu, baru Xvfb: klien yang menutup profil dengan rapi, lalu
    /// server yang pergi. Keduanya lewat [`Proc::stop`], jadi masa tenggang dan
    /// SIGKILL-nya sama dengan mesin lain.
    fn stop(&self) -> Result<(), EngineError> {
        let klien = self.proc.lock().map_err(|_| EngineError::Poisoned)?.stop();
        let server = self.xvfb.lock().map_err(|_| EngineError::Poisoned)?.stop();
        klien.and(server)
    }
}

/// Argumen Chromium penuh. Dipisah dari `launch` supaya resep yang dikirim ke
/// biner bisa diperiksa test tanpa menyalakan proses.
fn argumen(spec: &LaunchSpec) -> Vec<String> {
    let mut args = vec![
        format!("--remote-debugging-port={}", spec.port),
        "--remote-debugging-address=127.0.0.1".to_owned(),
        format!("--user-data-dir={}", spec.user_data_dir.display()),
    ];
    args.extend(PENUH_FLAGS.iter().map(|flag| (*flag).to_owned()));
    args.extend(spec.extra_args.iter().cloned());
    args
}

/// Nomor display pertama yang socket maupun lock-nya belum ada.
///
/// Diperiksa keduanya: Xvfb menulis lock sebelum socket, jadi socket saja bisa
/// membuat dua mesin memilih nomor yang sama saat start bersamaan.
#[must_use]
pub fn pilih_display(socket_dir: &Path, lock_dir: &Path) -> Option<u32> {
    (DISPLAY_MULAI..=DISPLAY_SAMPAI).find(|nomor| {
        !socket_dir.join(format!("X{nomor}")).exists()
            && !lock_dir.join(format!(".X{nomor}-lock")).exists()
    })
}

fn tunggu_socket(path: &Path, deadline: Instant, timeout_ms: u64) -> Result<(), EngineError> {
    loop {
        if path.exists() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(EngineError::XvfbTimeout(timeout_ms));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn tunggu_version(port: u16, deadline: Instant, timeout_ms: u64) -> Result<String, EngineError> {
    loop {
        let terakhir = match http_get(port, "/json/version") {
            Ok(body) => return Ok(body),
            Err(err) => err.to_string(),
        };
        if Instant::now() >= deadline {
            // Kegagalan terakhir ikut dilaporkan: "timeout" tanpa sebab
            // memaksa orang menebak, dan tebakan biasanya salah.
            tracing::warn!(hint = %terakhir, "chrome-full tidak menjawab discovery");
            return Err(EngineError::ReadyTimeout(timeout_ms));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ocb-penuh-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("direktori uji bisa dibuat");
        dir
    }

    fn spec_uji() -> LaunchSpec {
        LaunchSpec::new("/usr/bin/google-chrome", "/tmp/profil-uji")
    }

    #[test]
    fn display_bebas_melewati_socket_dan_lock() {
        let socket_dir = temp_dir("socket");
        let lock_dir = temp_dir("lock");
        fs::write(socket_dir.join(format!("X{DISPLAY_MULAI}")), b"").expect("socket palsu");
        fs::write(lock_dir.join(format!(".X{}-lock", DISPLAY_MULAI + 1)), b"").expect("lock palsu");

        let nomor = pilih_display(&socket_dir, &lock_dir).expect("harus ada yang bebas");
        assert_eq!(
            nomor,
            DISPLAY_MULAI + 2,
            "socket dan lock yang sudah ada tidak boleh dipilih"
        );
        fs::remove_dir_all(&socket_dir).ok();
        fs::remove_dir_all(&lock_dir).ok();
    }

    #[test]
    fn display_pertama_dipilih_saat_semuanya_kosong() {
        let socket_dir = temp_dir("kosong-socket");
        let lock_dir = temp_dir("kosong-lock");
        assert_eq!(pilih_display(&socket_dir, &lock_dir), Some(DISPLAY_MULAI));
        fs::remove_dir_all(&socket_dir).ok();
        fs::remove_dir_all(&lock_dir).ok();
    }

    /// Alasan adapter ini ada adalah menjalankan browser **headed**. Satu flag
    /// `--headless` yang menyelinap ke daftar argumen mengubah mesinnya menjadi
    /// shell, dan test ini yang menahannya.
    #[test]
    fn argumen_tidak_pernah_menyebut_headless() {
        let args = argumen(&spec_uji());
        assert!(
            !args.iter().any(|arg| arg.contains("headless")),
            "chrome-full tidak boleh headless: {args:?}"
        );
        assert!(
            args.iter().any(|arg| arg == "--remote-debugging-port=0"),
            "port 0 wajib supaya mesin memilih port sendiri: {args:?}"
        );
        assert!(
            args.iter()
                .any(|arg| arg == "--user-data-dir=/tmp/profil-uji"),
            "profil harus datang dari LaunchSpec: {args:?}"
        );
        assert!(
            args.iter().any(|arg| arg == "--window-size=1920,1080"),
            "tanpa ukuran window, render bisa keluar kecil: {args:?}"
        );
    }

    #[test]
    fn argumen_tambahan_diteruskan_apa_adanya() {
        let spec = spec_uji().with_args(["--lang=en-US".to_owned()]);
        let args = argumen(&spec);
        assert!(
            args.iter().any(|arg| arg == "--lang=en-US"),
            "argumen pemanggil tidak boleh dibuang: {args:?}"
        );
        assert_eq!(
            args.last().map(String::as_str),
            Some("--lang=en-US"),
            "argumen pemanggil dipasang paling akhir supaya bisa menimpa bawaan"
        );
    }

    /// Server X yang mendengarkan TCP adalah pintu terbuka di mesin ini. Ini
    /// bukan preferensi gaya, jadi ia diuji.
    #[test]
    fn xvfb_tidak_mendengarkan_tcp() {
        let pasangan = XVFB_FLAGS.windows(2).any(|dua| dua == ["-nolisten", "tcp"]);
        assert!(pasangan, "Xvfb wajib dijalankan dengan -nolisten tcp");
        assert!(
            XVFB_FLAGS.contains(&"-noreset"),
            "tanpa -noreset, Xvfb bisa mati saat klien terakhir putus"
        );
    }

    #[test]
    fn socket_yang_tidak_muncul_dilaporkan_sebagai_timeout() {
        let dir = temp_dir("tanpa-socket");
        let socket = dir.join("X99");
        let mulai = Instant::now();
        let hasil = tunggu_socket(&socket, Instant::now() + Duration::from_millis(120), 120);
        assert!(
            matches!(hasil, Err(EngineError::XvfbTimeout(120))),
            "dapat: {hasil:?}"
        );
        assert!(
            mulai.elapsed() < Duration::from_secs(2),
            "menunggu socket tidak boleh lebih lama dari tenggatnya"
        );
        fs::remove_dir_all(&dir).ok();
    }
}
