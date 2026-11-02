//! Penerjemahan tipe pesan antara axum (sisi klien) dan tungstenite (sisi mesin).
//!
//! Ini satu-satunya tempat yang boleh menyentuh bentuk pesan. Yang dijaga:
//! **isi pesan tidak berubah**, hanya tipe pembungkusnya. Tidak ada
//! serialisasi ulang JSON, karena serialisasi ulang mengubah urutan kunci dan
//! membuang `null` yang mungkin dipertahankan klien.

use axum::extract::ws::{CloseFrame as AxumCloseFrame, Message as AxumMessage};
use tokio_tungstenite::tungstenite::Message as TsMessage;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as TsCloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

/// Klien ke mesin.
pub fn axum_to_ts(message: AxumMessage) -> TsMessage {
    match message {
        AxumMessage::Text(text) => TsMessage::Text(text.as_str().to_owned().into()),
        AxumMessage::Binary(bytes) => TsMessage::Binary(bytes.to_vec().into()),
        AxumMessage::Ping(bytes) => TsMessage::Ping(bytes.to_vec().into()),
        AxumMessage::Pong(bytes) => TsMessage::Pong(bytes.to_vec().into()),
        AxumMessage::Close(frame) => TsMessage::Close(frame.map(|frame| TsCloseFrame {
            code: CloseCode::from(frame.code),
            reason: frame.reason.as_str().to_owned().into(),
        })),
    }
}

/// Mesin ke klien. `None` berarti pesan itu tidak punya padanan di sisi klien
/// (varian `Frame` mentah), dan diteruskan sebagai catatan log, bukan ditebak.
pub fn ts_to_axum(message: TsMessage) -> Option<AxumMessage> {
    match message {
        TsMessage::Text(text) => Some(AxumMessage::Text(text.as_str().to_owned().into())),
        TsMessage::Binary(bytes) => Some(AxumMessage::Binary(bytes.to_vec().into())),
        TsMessage::Ping(bytes) => Some(AxumMessage::Ping(bytes.to_vec().into())),
        TsMessage::Pong(bytes) => Some(AxumMessage::Pong(bytes.to_vec().into())),
        TsMessage::Close(frame) => Some(AxumMessage::Close(frame.map(|frame| AxumCloseFrame {
            code: u16::from(frame.code),
            reason: frame.reason.as_str().to_owned().into(),
        }))),
        TsMessage::Frame(_) => None,
    }
}

/// Teks sebuah pesan, kalau memang pesan teks.
///
/// Dipakai pengamat yang hanya membaca. Dipisah dari [`ts_to_axum`] supaya
/// pengamat tidak perlu mengambil alih pesan, dan supaya urutan "baca dulu,
/// teruskan kemudian" terlihat di kode pemanggil.
#[must_use]
pub fn text_of(message: &TsMessage) -> Option<&str> {
    match message {
        TsMessage::Text(teks) => Some(teks.as_str()),
        _ => None,
    }
}

/// Memisahkan header dan body pada respons HTTP mentah.
pub fn split_http_response(buf: &[u8]) -> Option<(&[u8], &[u8])> {
    const SEP: &[u8] = b"\r\n\r\n";
    let pos = buf.windows(SEP.len()).position(|window| window == SEP)?;
    Some((&buf[..pos], &buf[pos + SEP.len()..]))
}

/// Nilai `Content-Length` dari kepala respons. Nama header tidak peka huruf.
pub fn content_length(head: &[u8]) -> Option<usize> {
    let text = String::from_utf8_lossy(head);
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.trim().eq_ignore_ascii_case("content-length") {
            return None;
        }
        value.trim().parse::<usize>().ok()
    })
}

/// `true` kalau kepala utuh dan body sudah sepanjang `Content-Length`.
///
/// Chrome mengabaikan `Connection: close` dan tidak menutup socket, jadi
/// menunggu EOF berarti menunggu sampai timeout di setiap permintaan.
pub fn is_complete(buf: &[u8]) -> bool {
    let Some((head, body)) = split_http_response(buf) else {
        return false;
    };
    content_length(head).is_some_and(|expected| body.len() >= expected)
}

/// Kode status dari baris pertama respons HTTP.
pub fn parse_status(head: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(head).ok()?;
    text.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(message: &AxumMessage) -> String {
        match message {
            AxumMessage::Text(text) => text.as_str().to_owned(),
            other => panic!("bukan pesan teks: {other:?}"),
        }
    }

    #[test]
    fn pesan_teks_bolak_balik_identik_byte_per_byte() {
        let raw = r#"{"id":7,"method":"Target.getTargets","sessionId":"A1B2"}"#;
        let through =
            ts_to_axum(axum_to_ts(AxumMessage::Text(raw.into()))).expect("teks punya padanan");
        assert_eq!(text_of(&through), raw, "isi pesan tidak boleh berubah");
    }

    #[test]
    fn spasi_dan_urutan_kunci_dipertahankan() {
        // Klien boleh mengirim JSON dengan format apa pun. Kita bukan parser
        // yang berhak merapikannya.
        let raw = "{ \"method\" : \"Page.navigate\" ,\n  \"id\": 12 }";
        let through = ts_to_axum(axum_to_ts(AxumMessage::Text(raw.into()))).expect("teks");
        assert_eq!(text_of(&through), raw);
    }

    #[test]
    fn pesan_biner_dipertahankan() {
        let raw = vec![0u8, 1, 2, 250, 255];
        match ts_to_axum(axum_to_ts(AxumMessage::Binary(raw.clone().into()))) {
            Some(AxumMessage::Binary(bytes)) => assert_eq!(bytes.to_vec(), raw),
            other => panic!("bukan pesan biner: {other:?}"),
        }
    }

    #[test]
    fn kode_close_dipertahankan() {
        let frame = AxumCloseFrame {
            code: 1001,
            reason: "pergi".into(),
        };
        match ts_to_axum(axum_to_ts(AxumMessage::Close(Some(frame)))) {
            Some(AxumMessage::Close(Some(back))) => {
                assert_eq!(back.code, 1001);
                assert_eq!(back.reason.as_str(), "pergi");
            }
            other => panic!("close tidak kembali: {other:?}"),
        }
    }

    #[test]
    fn status_dibaca_dari_baris_pertama() {
        assert_eq!(parse_status(b"HTTP/1.1 200 OK"), Some(200));
        assert_eq!(parse_status(b"HTTP/1.1 404 Not Found"), Some(404));
        assert_eq!(parse_status(b"sampah"), None);
    }
}
