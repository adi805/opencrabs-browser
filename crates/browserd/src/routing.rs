//! Keputusan mesin per host, versi daemon.
//!
//! Ini satu-satunya tempat yang menyambungkan `policy` ke jalur permintaan.
//! `policy` tahu aturannya tetapi tidak tahu mesin apa yang benar-benar
//! terpasang di pool; `cdp` tahu pool-nya tetapi tidak boleh menyimpan satu
//! aturan pun (crate protokol tidak membaca config). Yang menjembatani keduanya
//! ada di sini, dan hanya di sini.
//!
//! Galat aturan **tidak** diturunkan jadi mesin bawaan. Aturan yang menolak
//! hampir selalu berarti config menunjuk mesin yang tidak terpasang; menggantinya
//! diam-diam akan membuat tab rendering muncul di mesin yang salah, jauh dari
//! penyebabnya.

use std::sync::Arc;

use cdp::{PemilihMesin, RoutingHost};
use policy::{Catalog, PolicySection};

/// Pemilih mesin dari `[policy]`, dibungkus bersama katalog mesin terpasang.
///
/// Katalog bisa basi kalau daemon menyalakan atau mematikan mesin di tengah
/// hidupnya. Untuk sekarang isinya dibangun sekali saat boot, dari jenis mesin
/// yang benar-benar terpasang, dan itu sama dengan yang dipakai `validasi_policy`
/// saat start.
pub struct PemilihPolicy {
    aturan: PolicySection,
    katalog: Catalog,
}

impl PemilihPolicy {
    #[must_use]
    pub fn baru(aturan: PolicySection, katalog: Catalog) -> Self {
        Self { aturan, katalog }
    }

    /// Pemilih dari jenis mesin yang terpasang, bentuk yang dipakai daemon.
    #[must_use]
    pub fn dari_jenis(aturan: PolicySection, jenis: &[String]) -> Arc<Self> {
        Arc::new(Self::baru(
            aturan,
            Catalog::dari_mesin(jenis.iter().map(String::as_str)),
        ))
    }
}

impl PemilihMesin for PemilihPolicy {
    fn jenis_untuk_host(&self, host: &str) -> Result<Option<RoutingHost>, String> {
        match policy::select_untuk_host(host, &self.aturan, &self.katalog) {
            // Tidak ada aturan yang berlaku. Bukan galat: itu memang artinya
            // "pakai mesin bawaan", dan pemanggil yang memutuskannya.
            Ok(None) => Ok(None),
            Ok(Some(keputusan)) => {
                let jenis = keputusan.kind.as_str().to_owned();
                // Nama aturan ikut dicatat: "kenapa host ini dapat mesin itu"
                // adalah pertanyaan pertama saat ada bug rendering, dan menebak
                // jawabannya dari kode itu mahal.
                tracing::info!(
                    host,
                    jenis = %jenis,
                    aturan = keputusan.rule,
                    "aturan host memilih mesin"
                );
                Ok(Some(RoutingHost {
                    jenis,
                    aturan: keputusan.rule.to_owned(),
                }))
            }
            Err(err) => {
                tracing::warn!(host, error = %err, "aturan host menolak permintaan");
                Err(err.to_string())
            }
        }
    }
}

#[cfg(test)]
mod uji {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    /// Penampung log: menulis ke memori supaya uji bisa memeriksa isinya.
    #[derive(Clone, Default)]
    struct Catatan(Arc<Mutex<Vec<u8>>>);

    impl Catatan {
        fn isi(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("kunci catatan")).into_owned()
        }
    }

    impl Write for Catatan {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("kunci catatan").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn pemilih(rules: &[(&str, &str)], terpasang: &[&str]) -> PemilihPolicy {
        PemilihPolicy::baru(
            PolicySection {
                rules: rules
                    .iter()
                    .map(|(host, jenis)| ((*host).to_owned(), (*jenis).to_owned()))
                    .collect(),
                ..PolicySection::default()
            },
            Catalog::dari_mesin(terpasang.iter().copied()),
        )
    }

    const TERPASANG_DUA: &[&str] = &["chrome-shell", "chrome-full"];
    const ATURAN_TOKO: &[(&str, &str)] = &[("toko.test", "chrome-full")];

    #[test]
    fn keputusan_dicatat_dengan_nama_aturan_yang_menang() {
        let pemilih = pemilih(ATURAN_TOKO, TERPASANG_DUA);
        let catatan = Catatan::default();
        let penulis = catatan.clone();
        let langganan = tracing_subscriber::fmt()
            .with_writer(move || penulis.clone())
            .with_ansi(false)
            .finish();

        let hasil =
            tracing::subscriber::with_default(langganan, || pemilih.jenis_untuk_host("toko.test"));
        let keputusan = hasil.expect("aturan sah").expect("ada aturan");
        assert_eq!(keputusan.jenis, "chrome-full");
        assert_eq!(keputusan.aturan, "host_rule");

        let log = catatan.isi();
        assert!(
            log.contains("host_rule"),
            "log harus menyebut aturan yang menang: {log}"
        );
        assert!(
            log.contains("toko.test"),
            "log harus menyebut host yang diatur: {log}"
        );
    }

    #[test]
    fn host_tanpa_aturan_tidak_memutuskan_apa_apa() {
        let pemilih = pemilih(ATURAN_TOKO, TERPASANG_DUA);
        assert_eq!(
            pemilih.jenis_untuk_host("lain.test"),
            Ok(None),
            "tanpa aturan, pemanggil yang memakai mesin bawaan"
        );
    }

    #[test]
    fn aturan_ke_jenis_yang_tidak_terpasang_ditolak_menyebut_jenisnya() {
        // Aturan ada, tapi `chrome-full` tidak dipasang. Ini yang paling sering
        // terjadi di config nyata, dan pesannya harus bisa dicari di config.
        let pemilih = pemilih(ATURAN_TOKO, &["chrome-shell"]);
        let err = pemilih
            .jenis_untuk_host("toko.test")
            .expect_err("chrome-full tidak terpasang");
        assert!(err.contains("chrome-full"), "sebut jenisnya: {err}");
    }
}
