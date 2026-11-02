//! Memutuskan kapan tab dan proses mati, dan memaksa memori kembali ke sistem.
//!
//! Crate ini sengaja dipisah dari I/O sebanyak mungkin, dan **nol dependensi**
//! di luar `std`. Aturan "kapan boleh pindah state" dan "kapan anggaran memori
//! harus bertindak" adalah fungsi murni yang bisa diuji tanpa menyalakan
//! Chromium. Yang benar-benar menyentuh proses (`kill`, `smaps_rollup`) hidup di
//! modul yang menerjemahkan keputusan dari sini, dan tidak boleh punya aturan
//! sendiri.
//!
//! Latar angkanya ada di `00-RFC` §1: Chromium **tidak** mengembalikan memori
//! setelah tab ditutup (249.5 MB PSS bertahan dari baseline 72.0 MB). Karena
//! itu mematikan engine saat menganggur adalah bagian dari desain, bukan
//! kegagalan yang ditoleransi.

#![forbid(unsafe_code)]

pub mod budget;
pub mod metrics;
pub mod reaper;
pub mod state;
pub mod tabs;

pub use budget::{
    Action, Budget, BudgetConfig, BudgetError, BudgetViolation, GroupReading, Level, MB,
    SmapsRollup, parse_smaps_rollup, pgrp_from_stat, processes_in_group, pss_of_process,
    read_process_group,
};
pub use metrics::Counters;
pub use reaper::{
    ProcessGroup, ReapOutcome, ReapReason, ReapReport, ReaperConfig, Signal, SignalError, Tab, reap,
};
pub use state::{Event, IllegalTransition, Machine, State, Transition};
