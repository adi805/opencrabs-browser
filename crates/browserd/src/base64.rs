//! Dekoder base64 seperlunya, untuk satu pemakai: `Page.captureScreenshot`.
//!
//! Chrome mengirim layar sebagai base64 di dalam JSON. Menarik satu crate
//! hanya untuk itu menambah permukaan dependensi di jalur yang menyentuh
//! halaman asing, jadi dekodernya ditulis di sini: tanpa `unsafe`, tanpa
//! tabel statis, dan yang **ditolak** sama pentingnya dengan yang diterima.
//! Data yang tidak sah harus jadi error, bukan byte sampah yang tersimpan
//! sebagai PNG rusak lalu ditunjukkan ke owner sebagai "bukti".

/// Mendekode base64 standar (alfabet `A-Za-z0-9+/`, padding `=` opsional).
///
/// Padding hanya diterima pada panjang kelipatan empat, dan panjang bersisa
/// satu karakter selalu ditolak: base64 tidak pernah menghasilkan bentuk itu,
/// jadi kalau muncul berarti masukannya bukan base64.
pub fn dekode(teks: &str) -> Result<Vec<u8>, String> {
    let bytes = teks.as_bytes();
    let mut ujung = bytes.len();
    let mut pad = 0;
    while ujung > 0 && bytes[ujung - 1] == b'=' && pad < 2 {
        ujung -= 1;
        pad += 1;
    }
    if pad > 0 && !bytes.len().is_multiple_of(4) {
        return Err("padding base64 hanya sah pada panjang kelipatan 4".to_owned());
    }

    let inti = &bytes[..ujung];
    if inti.len() % 4 == 1 {
        return Err("panjang base64 tidak mungkin: sisa 1 karakter".to_owned());
    }

    let mut keluar = Vec::with_capacity(inti.len() / 4 * 3 + 2);
    let mut buf: u32 = 0;
    let mut bit: u32 = 0;
    for (posisi, byte) in inti.iter().enumerate() {
        let nilai =
            nilai(*byte).ok_or_else(|| format!("karakter base64 tidak sah di posisi {posisi}"))?;
        buf = (buf << 6) | u32::from(nilai);
        bit += 6;
        if bit >= 8 {
            bit -= 8;
            #[allow(clippy::cast_possible_truncation)]
            keluar.push(((buf >> bit) & 0xff) as u8);
        }
    }
    Ok(keluar)
}

fn nilai(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vektor_rfc_4648_dibaca_benar() {
        // Vektor resmi: kalau tiga ini benar, pergeseran bitnya benar.
        assert_eq!(dekode("").expect("kosong sah"), Vec::<u8>::new());
        assert_eq!(dekode("TWE=").expect("satu byte"), b"Ma".to_vec());
        assert_eq!(dekode("TWE").expect("tanpa padding"), b"Ma".to_vec());
        assert_eq!(dekode("TWFu").expect("dua byte"), b"Man".to_vec());
        assert_eq!(dekode("TQ==").expect("satu byte pendek"), b"M".to_vec());
    }

    #[test]
    fn tanda_tangan_png_utuh_setelah_didekode() {
        // Delapan byte pertama PNG dalam base64. Dipakai jalur screenshot, jadi
        // yang diuji bentuk masukannya yang sebenarnya, bukan contoh acak.
        let b64 = "iVBORw0KGgo=";
        let bytes = dekode(b64).expect("harus sah");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn masukan_tidak_sah_ditolak_bukan_dipaksakan() {
        assert!(dekode("TWF*").is_err(), "karakter asing harus ditolak");
        assert!(dekode("T").is_err(), "sisa 1 karakter tidak mungkin");
        assert!(
            dekode("TW=").is_err(),
            "padding pada panjang bukan kelipatan 4 harus ditolak"
        );
        assert!(dekode("TWFu\n").is_err(), "baris baru bukan alfabet base64");
    }
}
