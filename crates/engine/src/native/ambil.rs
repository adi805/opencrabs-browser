//! Jalur keluar ke jaringan untuk skrip halaman, sebagai *hook* yang dipasang
//! pemanggil.
//!
//! Mesin ini sengaja tidak membuka soket sendiri dari dalam isolat. Kalau ia
//! bisa, `fetch()` di halaman menjadi jalan pintas yang melewati kebijakan
//! daemon: satu skrip halaman bisa menghubungi alamat apa pun yang bisa
//! dijangkau proses, termasuk alamat internal host. Yang ada di sini hanya
//! bentuk permintaan dan jawabannya; keputusan "boleh atau tidak" dan
//! pengambilan sebenarnya milik pemanggil, yang memegang kebijakan jaringan.
//!
//! Bawaan untuk isolat tanpa hook adalah [`PengambilTolak`]: menolak semua
//! dengan alasan yang bisa dibaca, bukan gagal diam-diam. Isolat yang dibuat
//! tanpa sengaja memanggil `fetch()` karena itu tetap jujur: skrip menerima
//! galat yang menyebut tidak ada jalur jaringan, bukan jawaban kosong yang
//! terlihat seperti halaman tanpa isi.

/// Satu permintaan `fetch()` dari skrip halaman.
///
/// `metode` sudah dinormalkan pemanggil menjadi huruf besar. Yang diterima
/// hanya `GET`, `HEAD` dan `POST`; sisanya ditolak di lapisan kebijakan,
/// karena `fetch` dari halaman tidak butuh `PUT`/`DELETE` dan setiap metode
/// tambahan adalah permukaan yang harus diuji.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermintaanAmbil {
    pub url: String,
    pub metode: String,
    /// Badan permintaan, kalau ada. `None` untuk `GET`/`HEAD`.
    pub badan: Option<String>,
}

/// Jawaban yang sudah diizinkan dan sudah diambil pemanggil.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JawabanAmbil {
    pub status: u16,
    /// URL akhir setelah pengalihan, supaya skrip tahu ke mana ia mendarat.
    pub url: String,
    pub content_type: Option<String>,
    /// Badan sebagai teks. Byte yang bukan UTF-8 sudah diganti U+FFFD oleh
    /// pemanggil, bukan dipotong di tengah karakter.
    pub badan: String,
}

/// Sumber jawaban untuk isolat.
///
/// Implementasinya hidup di daemon, yang memegang kebijakan jaringan. Crate
/// ini tidak boleh tahu apa itu allowlist: kalau ia tahu, aturan izin akan
/// bercabang dua dan yang satu pasti tertinggal.
pub trait Pengambil: Send + Sync {
    /// Mengambil satu URL, atau menolak dengan alasan yang bisa dibaca skrip.
    ///
    /// Alasan penolakan dikirim apa adanya ke halaman. "tidak diizinkan" saja
    /// tidak cukup: penulis skrip perlu tahu apakah yang salah host-nya,
    /// skemanya, atau metodenya.
    fn ambil(&self, permintaan: &PermintaanAmbil) -> Result<JawabanAmbil, String>;
}

/// Pengambil bawaan: menolak semuanya.
///
/// Dipakai isolat yang dibuat tanpa hook jaringan, termasuk semua tes mesin
/// yang tidak menguji `fetch`. Menolak, bukan mengizinkan, karena isolat yang
/// lupa dipasangi kebijakan tidak boleh berubah menjadi jalur keluar bebas.
#[derive(Debug, Default, Clone, Copy)]
pub struct PengambilTolak;

impl Pengambil for PengambilTolak {
    fn ambil(&self, _permintaan: &PermintaanAmbil) -> Result<JawabanAmbil, String> {
        Err("tidak ada jalur jaringan yang dipasang di mesin ini".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tanpa hook, `fetch` ditolak dengan alasan, bukan dengan jawaban kosong.
    #[test]
    fn pengambil_tolak_menyebut_alasannya() {
        let permintaan = PermintaanAmbil {
            url: "https://contoh.test/".to_owned(),
            metode: "GET".to_owned(),
            badan: None,
        };
        let galat = PengambilTolak
            .ambil(&permintaan)
            .expect_err("pengambil bawaan harus menolak");
        assert!(
            galat.contains("tidak ada jalur jaringan"),
            "alasan harus menyebut sebabnya, bukan kosong: {galat}"
        );
    }
}
