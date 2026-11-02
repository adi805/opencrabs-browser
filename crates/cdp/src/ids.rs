//! Membaca id dan `sessionId` dari pesan, untuk log dan metrik.
//!
//! Membaca **tidak sama dengan menulis**. Di v0.1 nilai-nilai ini tidak pernah
//! diubah; fungsinya ada dua: memberi nomor urut yang bisa dilacak di log, dan
//! menjadi dasar multiplex di v0.5 tanpa mengubah perilaku hari ini.

use serde_json::Value;

/// `id` numerik pada pesan permintaan atau respons.
#[must_use]
pub fn extract_id(text: &str) -> Option<i64> {
    serde_json::from_str::<Value>(text)
        .ok()?
        .get("id")?
        .as_i64()
}

/// `sessionId` yang menandai pesan milik target tertentu.
#[must_use]
pub fn extract_session_id(text: &str) -> Option<String> {
    serde_json::from_str::<Value>(text)
        .ok()?
        .get("sessionId")?
        .as_str()
        .map(str::to_owned)
}

/// Nama method CDP, mis. `Page.navigate`.
#[must_use]
pub fn extract_method(text: &str) -> Option<String> {
    serde_json::from_str::<Value>(text)
        .ok()?
        .get("method")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::{axum_to_ts, ts_to_axum};
    use axum::extract::ws::Message as AxumMessage;

    #[test]
    fn id_dibaca_dari_permintaan() {
        assert_eq!(
            extract_id(r#"{"id":7,"method":"Target.getTargets"}"#),
            Some(7)
        );
        assert_eq!(extract_id(r#"{"id":-3}"#), Some(-3));
        assert_eq!(
            extract_id(r#"{"method":"x"}"#),
            None,
            "notifikasi tidak punya id"
        );
        assert_eq!(extract_id("bukan json"), None);
    }

    #[test]
    fn session_id_dan_method_dibaca() {
        let raw = r#"{"id":1,"method":"Page.navigate","sessionId":"ABC"}"#;
        assert_eq!(extract_session_id(raw).as_deref(), Some("ABC"));
        assert_eq!(extract_method(raw).as_deref(), Some("Page.navigate"));
    }

    /// Bukti bahwa penerusan tidak mengubah id: satu-satunya cara id berubah
    /// adalah kalau kita menulis ulang pesannya, dan kita tidak.
    #[test]
    fn id_tetap_sama_setelah_diteruskan() {
        let raw = r#"{"id":42,"method":"Runtime.evaluate","params":{"expression":"1+1"}}"#;
        let forwarded = ts_to_axum(axum_to_ts(AxumMessage::Text(raw.into()))).expect("teks");
        let AxumMessage::Text(text) = forwarded else {
            panic!("harus tetap teks");
        };
        assert_eq!(extract_id(text.as_str()), Some(42));
        assert_eq!(text.as_str(), raw);
    }
}
