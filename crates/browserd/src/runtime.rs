//! Kumpulan skrip yang disuntikkan ke halaman: mata dan tangan.
//!
//! Dua berkas dipisah karena tanggung jawabnya berbeda (membaca vs menyentuh),
//! tapi dikirim sebagai satu suntikan. Kalau dipisah saat dikirim, ada jendela
//! waktu di mana halaman punya `__oc.mata` tanpa `__oc.klik`, dan pemanggil
//! yang kebetulan jalan di jendela itu akan melihat API yang setengah jadi.
//!
//! Isinya JavaScript, bukan Rust yang dibangkitkan: skrip ini hidup di dalam
//! halaman, dan menuliskannya sebagai string Rust akan menghilangkan seluruh
//! penyorotan sintaks serta membuat satu tanda kutip salah menjadi galat build
//! yang membingungkan.

/// Skrip lengkap, siap dikirim sebagai `source` pada
/// `Page.addScriptToEvaluateOnNewDocument`.
pub const SCRIPT: &str = concat!(
    include_str!("../assets/oc-mata.js"),
    "\n",
    include_str!("../assets/oc-tangan.js"),
);

/// Versi protokol `__oc`. Dipakai halaman untuk mengenali suntikan ganda dan
/// dipakai test untuk memastikan yang terpasang memang versi yang diharapkan.
pub const VERSI: &str = "1";

/// Berkas fixture untuk test, disajikan lewat server lokal di test.
pub mod fixture {
    pub const CONSENT: &str = include_str!("../assets/fixtures/consent.html");
    pub const SHADOW: &str = include_str!("../assets/fixtures/shadow.html");
    pub const IFRAME: &str = include_str!("../assets/fixtures/iframe.html");
    pub const ISI_IFRAME: &str = include_str!("../assets/fixtures/isi-iframe.html");
    pub const OVERLAY: &str = include_str!("../assets/fixtures/overlay.html");
    pub const DIAM: &str = include_str!("../assets/fixtures/diam.html");
    pub const PADAT: &str = include_str!("../assets/fixtures/padat.html");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skrip_memuat_mata_dan_tangan() {
        assert!(SCRIPT.contains("window.__oc"));
        assert!(SCRIPT.contains("OC.klik = klik"));
        assert!(SCRIPT.contains("akarTertutup"));
    }

    #[test]
    fn skrip_tidak_memakai_await_di_tubuh_atas() {
        // Aksi sengaja sinkron: Runtime.evaluate tanpa awaitPromise akan
        // mengembalikan Promise kosong, dan itu laporan palsu.
        assert!(SCRIPT.contains("function klik(target, opsi)"));
        assert!(!SCRIPT.contains("async function klik"));
    }

    #[test]
    fn fixture_lengkap() {
        assert!(fixture::CONSENT.contains("kotak-centang"));
        assert!(fixture::SHADOW.contains("mode:'closed'"));
        assert!(fixture::OVERLAY.contains("lapisan"));
    }
}
