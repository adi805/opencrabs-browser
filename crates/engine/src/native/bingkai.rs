//! Kebijakan muatan `<iframe>`.
//!
//! Mesin ini tidak membuka soket sendiri. Byte dokumen anak diserahkan
//! pemanggil, sama seperti `fetch()` lewat [`super::ambil::Pengambil`]. Yang
//! diputuskan di modul ini adalah **apakah** muatan itu boleh terjadi dan URL
//! mana yang harus diambil, supaya kebijakan same-origin punya satu rumah dan
//! tidak tersebar di tiap adapter.
//!
//! Batas yang disengaja: `sandbox`, `allow`, dan `csp` tidak dibaca. Iframe
//! yang lolos pemeriksaan asal dimuat dengan hak penuh, dan itu tercatat di
//! [`super::CELAH`], bukan disembunyikan.

use url::Url;

use super::NativeError;
use super::dom::{Document, NodeId};

/// Apa yang harus dilakukan terhadap satu `<iframe>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Muatan {
    /// `srcdoc` berisi HTML-nya langsung, jadi tidak perlu jaringan sama
    /// sekali. Menang atas `src`, sama seperti browser.
    Srcdoc(String),
    /// URL yang boleh diambil pemanggil, sudah diresolusi terhadap dokumen
    /// induk.
    Ambil(String),
    /// URL lintas asal, atau asal induknya tidak diketahui. Isinya tidak
    /// dimuat; alasannya ikut supaya pemanggil bisa melaporkannya, bukan
    /// diam-diam melewati iframe.
    LintasAsal(String),
    /// Tidak ada `src` dan tidak ada `srcdoc`.
    Kosong,
}

/// Rencana muatan untuk satu elemen `<iframe>`.
///
/// `dasar` adalah URL dokumen induk. Tanpa itu, asal anak tidak bisa
/// dibandingkan dengan apa pun, dan menebak "pasti same-origin" berarti
/// membuka batas yang seharusnya dijaga. Jadi tanpa `dasar`, `src` apa pun
/// dianggap lintas asal dan alasannya menyebut sebabnya.
#[must_use]
pub fn rencana(doc: &Document, iframe: NodeId, dasar: Option<&str>) -> Muatan {
    let Some(node) = doc.node(iframe) else {
        return Muatan::Kosong;
    };
    if let Some(srcdoc) = node.attr("srcdoc") {
        return Muatan::Srcdoc(srcdoc.to_owned());
    }
    let Some(src) = node.attr("src").map(str::trim).filter(|s| !s.is_empty()) else {
        return Muatan::Kosong;
    };
    let Some(tujuan) = selesaikan(src, dasar) else {
        return Muatan::LintasAsal(format!("`{src}` tidak bisa diresolusi jadi URL"));
    };
    let Some(asal_tujuan) = asal(&tujuan) else {
        return Muatan::LintasAsal(format!("`{tujuan}` bukan http/https"));
    };
    match dasar.and_then(asal) {
        Some(asal_induk) if asal_induk == asal_tujuan => Muatan::Ambil(tujuan),
        Some(asal_induk) => {
            Muatan::LintasAsal(format!("asal `{asal_tujuan}` berbeda dari `{asal_induk}`"))
        }
        None => Muatan::LintasAsal(format!(
            "asal dokumen induk tidak diketahui, jadi `{asal_tujuan}` tidak bisa dibuktikan same-origin"
        )),
    }
}

/// Memasang dokumen anak dari `srcdoc`, kalau ada.
///
/// `Ok(None)` berarti iframe itu tidak punya `srcdoc`; memuat `src` tetap
/// tugas pemanggil, karena hanya dia yang punya jaringan.
pub fn pasang_srcdoc(doc: &mut Document, iframe: NodeId) -> Result<Option<NodeId>, NativeError> {
    match rencana(doc, iframe, None) {
        Muatan::Srcdoc(html) => doc.pasang_bingkai(iframe, &html).map(Some),
        _ => Ok(None),
    }
}

/// Asal sebuah URL: `skema://host[:port]`, huruf kecil.
///
/// `None` untuk apa pun yang bukan http/https, termasuk `about:blank` dan
/// `data:`. Dua URL dengan asal yang sama boleh saling membaca; yang lain
/// tidak, dan itu aturan yang sama yang dipakai browser.
#[must_use]
pub fn asal(url: &str) -> Option<String> {
    let u = Url::parse(url).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    Some(u.origin().ascii_serialization())
}

/// Meresolusi `src` terhadap URL dokumen induk.
#[must_use]
pub fn selesaikan(src: &str, dasar: Option<&str>) -> Option<String> {
    let dasar = dasar.and_then(|d| Url::parse(d).ok());
    match dasar {
        Some(dasar) => dasar.join(src).ok().map(|u| u.to_string()),
        None => Url::parse(src).ok().map(|u| u.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::dom::Document;

    fn dokumen(html: &str) -> Document {
        Document::parse_html(html).unwrap()
    }

    #[test]
    fn asal_menormalkan_huruf_dan_menyimpan_port() {
        assert_eq!(
            asal("HTTPS://Example.COM/a/b").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            asal("http://example.com:8080/x").as_deref(),
            Some("http://example.com:8080")
        );
        // Skema berbeda = asal berbeda, sekalipun host-nya sama.
        assert_ne!(asal("http://example.com"), asal("https://example.com"));
        assert_eq!(asal("about:blank"), None);
        assert_eq!(asal("data:text/html,<p>x</p>"), None);
    }

    #[test]
    fn src_relatif_diresolusi_terhadap_dokumen_induk() {
        let doc = dokumen(r#"<iframe id="f" src="../anak.html"></iframe>"#);
        let f = doc.cari_id("f").unwrap();
        assert_eq!(
            rencana(&doc, f, Some("https://contoh.id/a/b/induk.html")),
            Muatan::Ambil("https://contoh.id/a/anak.html".to_owned())
        );
    }

    #[test]
    fn src_lintas_asal_ditolak_dengan_alasan() {
        let doc = dokumen(r#"<iframe id="f" src="https://lain.id/x"></iframe>"#);
        let f = doc.cari_id("f").unwrap();
        match rencana(&doc, f, Some("https://contoh.id/induk.html")) {
            Muatan::LintasAsal(sebab) => assert!(sebab.contains("lain.id"), "{sebab}"),
            lain => panic!("harusnya lintas asal, dapat {lain:?}"),
        }
    }

    #[test]
    fn asal_induk_tidak_diketahui_bukan_alasan_untuk_memuat() {
        let doc = dokumen(r#"<iframe id="f" src="https://contoh.id/x"></iframe>"#);
        let f = doc.cari_id("f").unwrap();
        match rencana(&doc, f, None) {
            Muatan::LintasAsal(sebab) => assert!(sebab.contains("tidak diketahui"), "{sebab}"),
            lain => panic!("harusnya ditolak, dapat {lain:?}"),
        }
    }

    #[test]
    fn srcdoc_menang_atas_src_dan_langsung_dipasang() {
        let mut doc = dokumen(
            r#"<iframe id="f" src="https://lain.id/x" srcdoc="<p id=&quot;dalam&quot;>isi</p>"></iframe>"#,
        );
        let f = doc.cari_id("f").unwrap();
        assert!(matches!(
            rencana(&doc, f, Some("https://contoh.id/")),
            Muatan::Srcdoc(_)
        ));

        let akar = pasang_srcdoc(&mut doc, f)
            .unwrap()
            .expect("srcdoc terpasang");
        assert_eq!(doc.bingkai(f), Some(akar));
        let dalam = doc.cari_id("dalam").expect("node dari srcdoc");
        assert_eq!(doc.akar_pohon(dalam), akar);
    }

    #[test]
    fn iframe_tanpa_src_maupun_srcdoc_tidak_memuat_apa_pun() {
        let mut doc = dokumen(r#"<iframe id="f"></iframe>"#);
        let f = doc.cari_id("f").unwrap();
        assert_eq!(rencana(&doc, f, Some("https://contoh.id/")), Muatan::Kosong);
        assert_eq!(pasang_srcdoc(&mut doc, f).unwrap(), None);
        assert_eq!(doc.bingkai(f), None);
    }

    #[test]
    fn src_kosong_diperlakukan_seperti_tidak_ada() {
        let doc = dokumen(r#"<iframe id="f" src="   "></iframe>"#);
        let f = doc.cari_id("f").unwrap();
        assert_eq!(rencana(&doc, f, Some("https://contoh.id/")), Muatan::Kosong);
    }
}
