//! Keputusan yang diambil daemon sebelum dan sesudah halaman dimuat.
//!
//! Isinya dua hal yang berbeda sifatnya, dan sengaja dipisah:
//!
//! - [`select`] memilih mesin **sebelum** apa pun dijalankan. Keputusannya
//!   deterministik dan bisa diuji tanpa browser.
//! - [`challenge`] menjawab apakah halaman yang sudah dimuat adalah tantangan.
//!   Masukannya sinyal yang benar-benar diamati, bukan tebakan dari nama host.
//!
//! Aturan yang berlaku di seluruh crate ini: **tidak ada fallback senyap dan
//! tidak ada angka karangan.** Pilihan yang tidak mampu menjawab galat yang
//! menyebut kemampuan yang kurang, dan keputusan challenge tidak pernah
//! diambil dari satu sinyal lemah.

#![forbid(unsafe_code)]

pub mod challenge;
pub mod counters;
pub mod rate;
pub mod select;

pub use challenge::{
    Confidence, DomFinding, Escalation, Evidence, JS_DETECT, Signal, Verdict, verdict,
};
pub use counters::CaptchaCounters;
pub use rate::{Allow, HandoffLimits, RateState};
pub use select::{
    Capabilities, CaptchaSection, Catalog, Decision, EngineKind, PolicySection, Request,
    SelectError, select, select_untuk_host,
};
