//! State mesin engine beserta aturan transisinya.
//!
//! Semua fungsi di sini murni: tidak ada async, tidak ada I/O, tidak ada jam.
//! Alasannya bukan gaya: keputusan "boleh matikan proses sekarang atau tidak"
//! adalah tempat bug paling mahal di project ini (satu `kill` ke process group
//! yang salah mematikan semua tab). Karena itu aturannya diuji sebagai tabel,
//! dan kode yang benar-benar menyentuh proses (`reaper`) hanya boleh mengikuti
//! hasil dari sini.
//!
//! Bentuk siklus hidup, sesuai `04-OPERASIONAL-ROADMAP` §3.3:
//!
//! ```text
//! Cold -> Booting -> Ready -> Draining -> Reaping -> Stopped
//!                      |         |
//!                      +---------+--> Reaping   (reap darurat, tanpa drain)
//! ```
//!
//! `Stopped -> Booting` juga sah: daemon hidup terus, engine-nya yang lahir dan
//! mati berkali-kali. `Cold` hanya ada sebelum boot pertama.

use std::error::Error;
use std::fmt;

/// Kondisi satu engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum State {
    /// Belum pernah ada proses. Kondisi awal daemon, dan satu-satunya cara
    /// membedakan "belum pernah dinyalakan" dari "sudah dimatikan".
    Cold,
    /// Proses diminta lahir, belum menjawab `/json/version`.
    Booting,
    /// Endpoint CDP siap. Satu-satunya state yang menerima sesi baru.
    Ready,
    /// Sesi baru ditolak, sesi yang masih jalan diberi waktu menyelesaikan
    /// perintahnya sendiri. Ini bukan titik kembali: drain yang dibatalkan akan
    /// membuat hitungan sesi tidak bisa dipercaya.
    Draining,
    /// Sinyal mati sudah dikirim ke seluruh process group, menunggu prosesnya
    /// benar-benar hilang.
    Reaping,
    /// Proses hilang dan memori sudah kembali ke sistem. Siap diboot ulang.
    Stopped,
}

impl State {
    /// Semua state, dipakai test supaya tidak ada kombinasi yang terlewat.
    pub const ALL: [Self; 6] = [
        Self::Cold,
        Self::Booting,
        Self::Ready,
        Self::Draining,
        Self::Reaping,
        Self::Stopped,
    ];

    /// Nama state untuk label metrik `browser_engine_state{state="..."}`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Booting => "booting",
            Self::Ready => "ready",
            Self::Draining => "draining",
            Self::Reaping => "reaping",
            Self::Stopped => "stopped",
        }
    }

    /// Apakah ada (atau sedang lahir, atau sedang mati) proses yang harus
    /// diperhitungkan anggaran memorinya.
    #[must_use]
    pub const fn has_process(self) -> bool {
        matches!(
            self,
            Self::Booting | Self::Ready | Self::Draining | Self::Reaping
        )
    }

    /// Apakah sesi baru boleh masuk. Hanya `Ready`.
    ///
    /// `Draining` sengaja menolak: kalau sesi baru diterima saat drain, engine
    /// tidak pernah benar-benar menganggur, dan reap idle jadi mustahil.
    #[must_use]
    pub const fn accepts_sessions(self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Tabel transisi. Satu-satunya sumber kebenaran; `next` dan test membaca
    /// dari sini, jadi tabel di kode dan tabel di test tidak bisa berbeda.
    #[must_use]
    pub const fn next(self, event: Event) -> Option<Self> {
        match (self, event) {
            (Self::Cold, Event::Boot) | (Self::Stopped, Event::Boot) => Some(Self::Booting),
            (Self::Booting, Event::Booted) => Some(Self::Ready),
            // Boot gagal sebelum proses lahir (biner tidak ada, port ditolak
            // kernel): tidak ada yang perlu dimatikan.
            (Self::Booting, Event::BootFailedWithoutProcess) => Some(Self::Stopped),
            // Boot gagal setelah proses lahir (tidak menjawab dalam batas
            // waktu): prosesnya harus dimatikan lewat jalur reap biasa.
            (Self::Booting, Event::BootFailedWithProcess) => Some(Self::Reaping),
            (Self::Ready, Event::Drain) => Some(Self::Draining),
            // Reap darurat: anggaran memori menembus hard cap, tidak ada waktu
            // menunggu sesi selesai.
            (Self::Ready | Self::Draining, Event::Reap) => Some(Self::Reaping),
            (Self::Reaping, Event::Reaped) => Some(Self::Stopped),
            _ => None,
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Kejadian yang membuat state bergerak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    /// Ada permintaan yang butuh engine.
    Boot,
    /// Engine menjawab `/json/version`.
    Booted,
    /// Boot gagal dan tidak ada proses yang tertinggal.
    BootFailedWithoutProcess,
    /// Boot gagal tapi proses masih hidup dan harus direap.
    BootFailedWithProcess,
    /// Mulai menolak sesi baru, tunggu yang lama selesai.
    Drain,
    /// Kirim sinyal mati sekarang.
    Reap,
    /// Proses sudah hilang, memori sudah kembali.
    Reaped,
}

impl Event {
    /// Semua event, dipakai test transisi terlarang.
    pub const ALL: [Self; 7] = [
        Self::Boot,
        Self::Booted,
        Self::BootFailedWithoutProcess,
        Self::BootFailedWithProcess,
        Self::Drain,
        Self::Reap,
        Self::Reaped,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Boot => "boot",
            Self::Booted => "booted",
            Self::BootFailedWithoutProcess => "boot_failed_without_process",
            Self::BootFailedWithProcess => "boot_failed_with_process",
            Self::Drain => "drain",
            Self::Reap => "reap",
            Self::Reaped => "reaped",
        }
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Transisi yang berhasil, cukup lengkap untuk satu baris log JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub from: State,
    pub to: State,
    pub event: Event,
    /// Nomor boot saat transisi terjadi. Naik hanya pada `Boot` yang sah.
    pub generation: u64,
}

impl fmt::Display for Transition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} -> {} ({}, gen {})",
            self.from, self.to, self.event, self.generation
        )
    }
}

/// Transisi yang tidak ada di tabel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub from: State,
    pub event: Event,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "transisi terlarang: state {} tidak menerima event {}",
            self.from, self.event
        )
    }
}

impl Error for IllegalTransition {}

/// Pemegang state sekarang, plus nomor generasi.
///
/// Generasi dipakai pemanggil untuk membuang pekerjaan yang sudah basi: timer
/// reap yang dijadwalkan untuk generasi 3 tidak boleh membunuh engine generasi 4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    state: State,
    generation: u64,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    /// Daemon baru: belum ada proses apa pun.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::Cold,
            generation: 0,
        }
    }

    #[must_use]
    pub const fn state(&self) -> State {
        self.state
    }

    /// Jumlah boot yang pernah berhasil dimulai. `0` berarti engine belum
    /// pernah dinyalakan sama sekali.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub const fn accepts_sessions(&self) -> bool {
        self.state.accepts_sessions()
    }

    /// Jalankan satu event.
    ///
    /// Gagal berarti state **tidak berubah**: tidak ada transisi setengah jadi,
    /// karena satu-satunya cara mengubah state adalah lewat tabel.
    pub fn apply(&mut self, event: Event) -> Result<Transition, IllegalTransition> {
        let Some(to) = self.state.next(event) else {
            return Err(IllegalTransition {
                from: self.state,
                event,
            });
        };

        let from = self.state;
        if event == Event::Boot {
            self.generation = self.generation.saturating_add(1);
        }
        self.state = to;

        Ok(Transition {
            from,
            to,
            event,
            generation: self.generation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tabel yang dijanjikan ke pembaca: setiap baris sah, dan selain baris ini
    /// tidak ada yang sah. Test di bawah memeriksa kedua arah.
    const ALLOWED: &[(State, Event, State)] = &[
        (State::Cold, Event::Boot, State::Booting),
        (State::Stopped, Event::Boot, State::Booting),
        (State::Booting, Event::Booted, State::Ready),
        (
            State::Booting,
            Event::BootFailedWithoutProcess,
            State::Stopped,
        ),
        (State::Booting, Event::BootFailedWithProcess, State::Reaping),
        (State::Ready, Event::Drain, State::Draining),
        (State::Ready, Event::Reap, State::Reaping),
        (State::Draining, Event::Reap, State::Reaping),
        (State::Reaping, Event::Reaped, State::Stopped),
    ];

    #[test]
    fn transisi_yang_diizinkan_sesuai_tabel() {
        for &(from, event, expected) in ALLOWED {
            assert_eq!(
                from.next(event),
                Some(expected),
                "tabel kode tidak cocok dengan tabel test untuk {from} --{event}-->"
            );

            let mut machine = Machine {
                state: from,
                generation: 0,
            };
            let transition = machine
                .apply(event)
                .unwrap_or_else(|err| panic!("{from} --{event}--> harus sah, tapi: {err}"));

            assert_eq!(transition.from, from);
            assert_eq!(transition.to, expected);
            assert_eq!(transition.event, event);
            assert_eq!(machine.state(), expected);
        }
    }

    #[test]
    fn transisi_terlarang_ditolak_dan_state_tidak_berubah() {
        let mut diperiksa = 0usize;

        for from in State::ALL {
            for event in Event::ALL {
                if ALLOWED.iter().any(|&(f, e, _)| f == from && e == event) {
                    continue;
                }

                let mut machine = Machine {
                    state: from,
                    generation: 7,
                };
                let err = machine
                    .apply(event)
                    .expect_err(&format!("{from} --{event}--> seharusnya ditolak"));

                assert_eq!(err.from, from);
                assert_eq!(err.event, event);
                assert_eq!(machine.state(), from, "state berubah padahal ditolak");
                assert_eq!(machine.generation(), 7, "generasi berubah padahal ditolak");
                diperiksa += 1;
            }
        }

        let semua = State::ALL.len() * Event::ALL.len();
        assert_eq!(diperiksa, semua - ALLOWED.len(), "ada kombinasi yang lolos");
    }

    #[test]
    fn hanya_ready_yang_menerima_sesi() {
        for state in State::ALL {
            assert_eq!(
                state.accepts_sessions(),
                state == State::Ready,
                "state {state} salah menjawab soal sesi baru"
            );
        }
    }

    #[test]
    fn siklus_penuh_dari_cold_sampai_stopped_lalu_boot_lagi() {
        let mut machine = Machine::new();
        assert_eq!(machine.state(), State::Cold);
        assert_eq!(machine.generation(), 0);

        for event in [
            Event::Boot,
            Event::Booted,
            Event::Drain,
            Event::Reap,
            Event::Reaped,
        ] {
            machine.apply(event).expect("siklus utama harus lengkap");
        }
        assert_eq!(machine.state(), State::Stopped);
        assert_eq!(machine.generation(), 1, "satu boot tercatat");
        assert!(
            !machine.state().has_process(),
            "stopped tidak boleh punya proses"
        );

        let transition = machine.apply(Event::Boot).expect("boot ulang sah");
        assert_eq!(transition.from, State::Stopped);
        assert_eq!(transition.to, State::Booting);
        assert_eq!(machine.generation(), 2);
    }

    #[test]
    fn drain_tidak_bisa_dibatalkan() {
        let mut machine = Machine::new();
        machine.apply(Event::Boot).unwrap();
        machine.apply(Event::Booted).unwrap();
        machine.apply(Event::Drain).unwrap();

        assert_eq!(
            machine.apply(Event::Booted),
            Err(IllegalTransition {
                from: State::Draining,
                event: Event::Booted,
            })
        );
        assert_eq!(machine.state(), State::Draining);
    }

    #[test]
    fn boot_gagal_membedakan_ada_proses_atau_tidak() {
        let mut tanpa_proses = Machine::new();
        tanpa_proses.apply(Event::Boot).unwrap();
        tanpa_proses
            .apply(Event::BootFailedWithoutProcess)
            .expect("boot gagal tanpa proses boleh langsung stopped");
        assert_eq!(tanpa_proses.state(), State::Stopped);

        let mut dengan_proses = Machine::new();
        dengan_proses.apply(Event::Boot).unwrap();
        dengan_proses
            .apply(Event::BootFailedWithProcess)
            .expect("boot gagal dengan proses harus lewat reap");
        assert_eq!(dengan_proses.state(), State::Reaping);
        assert!(dengan_proses.state().has_process());
    }

    #[test]
    fn nama_state_unik_dan_huruf_kecil_untuk_label_metrik() {
        let mut terlihat: Vec<&str> = Vec::new();
        for state in State::ALL {
            let nama = state.as_str();
            assert_eq!(nama, nama.to_ascii_lowercase(), "{nama} bukan huruf kecil");
            assert!(!terlihat.contains(&nama), "nama {nama} dipakai dua kali");
            terlihat.push(nama);
        }
        assert_eq!(terlihat.len(), State::ALL.len());
    }
}
