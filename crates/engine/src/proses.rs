//! Proses anak milik satu mesin, selalu dimatikan sebagai satu group.
//!
//! Ini satu-satunya tempat aturan itu ditulis. Adapter `chrome-headless-shell`
//! dan `oxibrowser` punya cara menyalakan yang berbeda, tapi cara mematikannya
//! tidak boleh berbeda: SIGTERM ke **group** (bukan pid induk), tunggu lima
//! detik, lalu SIGKILL. Anak dari mesin (zygote, renderer, task tokio) tidak
//! ikut mati kalau hanya pid induk yang di-SIGTERM, dan itulah sumber memori
//! yang tidak pernah kembali.
//!
//! Modul ini sengaja tidak tahu apa pun tentang CDP maupun jenis mesin.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use crate::EngineError;

const TERM_GRACE: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Proses mesin beserta group-nya.
pub(crate) struct Proc {
    child: Child,
    pgid: i32,
    stopped: bool,
}

impl Proc {
    /// Menjalankan perintah di group proses sendiri, lalu membungkusnya.
    ///
    /// `process_group(0)` wajib: tanpa itu anak-anak mesin lahir di group milik
    /// pemanggil, dan SIGTERM ke group pemanggil akan ikut membunuh daemon.
    pub(crate) fn spawn(mut cmd: Command) -> std::io::Result<Self> {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        let child = cmd.spawn()?;
        Ok(Self {
            pgid: child.id() as i32,
            child,
            stopped: false,
        })
    }

    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Group id proses ini. Dipakai uji untuk membuktikan group-nya benar-benar
    /// hilang (`killpg` mengembalikan `ESRCH`), jadi hanya ada saat test.
    #[cfg(test)]
    pub(crate) fn pgid(&self) -> i32 {
        self.pgid
    }

    /// Hidup berarti prosesnya masih ada **dan** kita belum mematikannya.
    /// Mesin yang sudah kita tidurkan bukan kegagalan, jadi `false` di sini
    /// bukan sinyal rusak.
    pub(crate) fn is_running(&mut self) -> bool {
        if self.stopped {
            return false;
        }
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Membunuh seluruh group tanpa menunggu rapi. Dipakai jalur gagal-boot:
    /// mesin yang tidak pernah siap tidak perlu diberi kesempatan menutup
    /// profil, dan menunggunya hanya menambah waktu gagal.
    pub(crate) fn kill_group(&mut self) {
        let _ = killpg(Pid::from_raw(self.pgid), Signal::SIGKILL);
        let _ = self.child.wait();
        self.stopped = true;
    }

    /// SIGTERM dulu supaya mesin sempat menutup profil dan menyimpan cookie,
    /// lalu SIGKILL kalau lewat lima detik. Aman dipanggil berkali-kali.
    pub(crate) fn stop(&mut self) -> Result<(), EngineError> {
        if self.stopped {
            return Ok(());
        }
        let pgid = Pid::from_raw(self.pgid);
        let _ = killpg(pgid, Signal::SIGTERM);
        let deadline = Instant::now() + TERM_GRACE;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.stopped = true;
                    return Ok(());
                }
                Ok(None) => std::thread::sleep(POLL_INTERVAL),
                Err(err) => return Err(EngineError::Io(err)),
            }
        }
        self.kill_group();
        Ok(())
    }
}

/// Jaring terakhir: `Proc` yang di-drop tanpa `stop()` tetap membunuh group-nya.
///
/// Tanpa ini, panic di tengah uji meninggalkan Chrome dan Xvfb yatim yang
/// memegang port, display, dan memori sampai mesin reboot. Kejadian nyata:
/// satu probe gagal di `Runtime.enable`, seluruh pohon browser bertahan, dan
/// run berikutnya kehabisan waktu karena sisa run sebelumnya masih berjalan.
/// Group yang sudah dihentikan rapi tidak disentuh lagi, jadi `drop` tidak
/// mengubah hasil `stop()`.
impl Drop for Proc {
    fn drop(&mut self) {
        if !self.stopped {
            self.kill_group();
        }
    }
}

/// `stderr` mesin ditulis ke berkas di dalam direktori profil, bukan dibuang.
/// Kegagalan boot yang bisu memaksa orang menebak, dan tebakan biasanya salah.
pub(crate) fn stderr_ke_file(cmd: &mut Command, path: &std::path::Path) {
    match std::fs::File::create(path) {
        Ok(file) => {
            cmd.stderr(Stdio::from(file));
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "tidak bisa menulis log stderr mesin");
            cmd.stderr(Stdio::null());
        }
    }
    cmd.stdout(Stdio::null());
    cmd.stdin(Stdio::null());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Proses tidur yang lahir di group sendiri harus bisa dimatikan lewat
    /// `stop()`, termasuk anaknya: itu inti modul ini.
    #[test]
    fn stop_mematikan_group_termasuk_anak() {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("sleep 30 & wait");
        let mut proc = Proc::spawn(cmd).expect("proses uji bisa dinyalakan");
        let pgid = proc.pgid();
        assert!(proc.is_running(), "proses baru harus hidup");

        let mulai = Instant::now();
        proc.stop().expect("stop harus sukses");
        assert!(!proc.is_running(), "sesudah stop proses harus mati");
        assert!(
            mulai.elapsed() < Duration::from_secs(6),
            "stop tidak boleh menunggu lebih dari masa tenggang"
        );
        assert!(
            matches!(
                killpg(Pid::from_raw(pgid), None),
                Err(nix::errno::Errno::ESRCH)
            ),
            "group proses harus benar-benar hilang, bukan cuma pid induknya"
        );
        // Aman dipanggil dua kali.
        proc.stop().expect("stop kedua juga sukses");
    }

    #[test]
    fn kill_group_langsung_mematikan() {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("sleep 30");
        let mut proc = Proc::spawn(cmd).expect("proses uji");
        proc.kill_group();
        assert!(!proc.is_running());
    }

    /// Jalur panic: tidak ada yang memanggil `stop()`, hanya `drop`. Group tetap
    /// harus hilang, karena itulah satu-satunya yang menjaga test gagal tidak
    /// mewariskan browser hidup ke run berikutnya.
    ///
    /// `killpg` tidak boleh diperiksa sekali saja. Anak yang dibunuh SIGKILL
    /// menjadi zombie sampai ada yang mereapnya; zombie masih dihitung sebagai
    /// anggota group, jadi `ESRCH` baru muncul beberapa milidetik kemudian.
    /// Ditemukan sebagai kegagalan sekali di suite penuh lalu hijau lima kali
    /// beruntun saat dijalankan sendiri, dan yang benar adalah menunggu
    /// hilangnya group dalam batas waktu, bukan menghapus pemeriksaannya.
    #[test]
    fn drop_tanpa_stop_tetap_mematikan_group() {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("sleep 30 & wait");
        let proc = Proc::spawn(cmd).expect("proses uji");
        let pgid = proc.pgid();

        drop(proc);

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if matches!(
                killpg(Pid::from_raw(pgid), None),
                Err(nix::errno::Errno::ESRCH)
            ) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "group proses masih hidup 2 detik sesudah drop"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
