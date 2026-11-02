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

/// Memisahkan header dan body pada respons HTTP mentah.
pub fn split_http_response(buf: &[u8]) -> Option<(&[u8], &[u8])> {
    const SEP: &[u8] = b"\r\n\r\n";
    let pos = buf.windows(SEP.len()).position(|window| window == SEP)?;
    Some((&buf[..pos], &buf[pos + SEP.len()..]))
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
