//! Multiplex: satu koneksi klien, banyak mesin.
//!
//! Dua bagian, dan pembagiannya disengaja:
//!
//! - [`peta`] memutuskan **ke mesin mana** sebuah pesan pergi dan bagaimana
//!   id-nya diterjemahkan. Seluruhnya sinkron dan tanpa soket, jadi bisa diuji
//!   tanpa browser.
//! - [`pompa`] menjalankan keputusan itu: satu soket ke tiap mesin, satu task
//!   per mesin, dan satu klien yang melihat semuanya sebagai satu koneksi.
//!
//! Kenapa dipisah dari [`crate::passthrough`]: jalur pass-through satu mesin
//! sudah terbukti dipakai produksi, dan menaruh logika peta di jalur itu berarti
//! mempertaruhkan yang sudah jalan untuk fitur yang belum dibutuhkan. Dua jalur,
//! dipilih di router berdasarkan jumlah mesin.

#![forbid(unsafe_code)]

pub mod peta;
pub mod pompa;

pub use peta::{Balasan, KODE_DITOLAK, KODE_SESI_TAK_DIKENAL, MuxError, Opsi, Perintah, Peta};
pub use pompa::proxy_multiplex;
