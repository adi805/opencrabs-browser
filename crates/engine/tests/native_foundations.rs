//! Uji ujung ke ujung fondasi mesin native: HTML masuk, kotak keluar, skrip
//! jalan, dan dokumen bisa disimpan lalu dipulihkan.
//!
//! Tes ini tidak butuh Chrome, tidak butuh jaringan, dan tidak menyentuh port
//! 9222. Semuanya berjalan di dalam proses.

use engine::native::{CELAH, Document, Isolat, LaporanLayout, PaguIsolat, Viewport, layout};

fn halaman(n_kartu: usize) -> String {
    let mut s = String::from(
        "<!doctype html><html><head><title>Halaman Uji</title></head><body>\
         <div id=\"akar\" style=\"display:flex;flex-direction:row;padding:8px\">",
    );
    for i in 0..n_kartu {
        s.push_str(&format!(
            "<div class=\"kartu\" id=\"k{i}\" style=\"width:100px;height:40px\">\
             <h2>Judul {i}</h2><p>Isi {i}</p></div>"
        ));
    }
    s.push_str("</div></body></html>");
    s
}

fn siapkan(n_kartu: usize) -> (Document, LaporanLayout) {
    let doc = Document::parse_html(&halaman(n_kartu)).expect("html terurai");
    let laporan = layout::hitung(&doc, Viewport::new(1280.0, 800.0)).expect("layout terhitung");
    (doc, laporan)
}

#[test]
fn satu_halaman_utuh_terurai_dan_tertata() {
    let (doc, laporan) = siapkan(3);

    assert_eq!(doc.judul().as_deref(), Some("Halaman Uji"));
    assert!(doc.jumlah_node() >= 20, "node: {}", doc.jumlah_node());

    let akar = doc.cari_id("akar").expect("akar ada");
    let kotak_akar = laporan.kotak_untuk(akar).expect("akar punya kotak");
    assert_eq!(kotak_akar.y, 0.0);
    assert!(kotak_akar.height >= 40.0, "tinggi: {}", kotak_akar.height);

    for i in 0..3 {
        let id = doc.cari_id(&format!("k{i}")).expect("kartu ada");
        let k = laporan.kotak_untuk(id).expect("kartu punya kotak");
        assert_eq!(k.width, 100.0, "lebar kartu {i}");
        assert_eq!(k.height, 40.0, "tinggi kartu {i}");
    }
}

#[test]
fn kartu_berderet_ke_samping_bukan_bertumpuk() {
    let (doc, laporan) = siapkan(3);
    let x: Vec<f32> = (0..3)
        .map(|i| {
            laporan
                .kotak_untuk(doc.cari_id(&format!("k{i}")).unwrap())
                .unwrap()
                .x
        })
        .collect();

    assert!(x[0] < x[1] && x[1] < x[2], "x harus naik: {x:?}");
    let delta = x[1] - x[0];
    assert_eq!(delta, x[2] - x[1], "jarak antar kartu harus seragam");
}

#[test]
fn snapshot_tab_cold_bolak_balik_tanpa_kehilangan_struktur() {
    let (doc, _) = siapkan(2);
    let html = doc.to_html();

    let pulih = Document::parse_html(&html).expect("hasil snapshot bisa diurai ulang");
    assert_eq!(pulih.jumlah_node(), doc.jumlah_node());
    assert_eq!(pulih.jumlah_elemen(), doc.jumlah_elemen());
    assert_eq!(pulih.judul(), doc.judul());
    assert_eq!(pulih.cari_id("k1"), doc.cari_id("k1"));

    // Layout hasil pemulihan harus sama persis dengan aslinya.
    let a = layout::hitung(&doc, Viewport::new(1280.0, 800.0)).unwrap();
    let b = layout::hitung(&pulih, Viewport::new(1280.0, 800.0)).unwrap();
    assert_eq!(a.kotak, b.kotak);
}

#[test]
fn skrip_halaman_jalan_di_isolat_terpisah() {
    let (doc, _) = siapkan(1);

    let pertama = Isolat::new(PaguIsolat::default()).unwrap();
    let kedua = Isolat::new(PaguIsolat::default()).unwrap();

    pertama
        .pasang_judul(doc.judul().as_deref().unwrap_or(""))
        .unwrap();
    pertama
        .eval("var rahasia = 'hanya tab satu'; console.log(document.title)")
        .unwrap();

    assert_eq!(pertama.log(), vec!["Halaman Uji".to_string()]);
    assert_eq!(kedua.eval("typeof rahasia").unwrap(), "undefined");
    assert_eq!(kedua.log(), Vec::<String>::new());
}

#[test]
fn celah_dinyatakan_bukan_disembunyikan() {
    assert!(!CELAH.is_empty());
    let semua = CELAH.join("\n");
    // Daftar ini bergerak bersama [`CELAH`]: celah yang sudah ditutup wajib
    // DICABUT dari sini, bukan dipertahankan supaya tes tetap hijau.
    // "snapshot tab" dicabut di milis 4 karena `ColdStore` (simpan/muat/hapus)
    // dan skema SQLite `tab_snapshot` sudah dikirim beserta 10 tes.
    // "selektor CSS" dicabut setelah `native::selectors` mencocokkan tag,
    // kelas, id, atribut, kombinator, dan pseudo-kelas; tesnya ada di modul
    // itu sendiri dan di `tests/native_state.rs` lewat `DOM.querySelector`.
    // "rasterisasi bitmap" dicabut setelah `native::paint` melukis kotak
    // layout ke PNG sungguhan lewat `tiny-skia`; yang tersisa bukan lagi
    // "belum ada raster", tapi batas raster yang sebenarnya.
    // "pengambilan dokumen dari jaringan" dicabut setelah
    // `cdp::native_wire::jaringan` mengambil dokumen `http`/`https` sungguhan
    // (diuji di `crates/cdp/tests/jaringan.rs`); yang tersisa adalah batas
    // yang lebih sempit, yaitu skrip halaman belum punya `fetch`.
    // "pengukuran teks" dicabut setelah `native::teks` mengukur lebar teks
    // dari face yang ditanam dan node teks menghasilkan kotak layout
    // (diuji di `native::layout`); yang menggantikannya adalah batas yang
    // lebih sempit: satu face, tanpa webfont, tanpa shaping.
    // "pengukuran teks hanya satu face" dicabut setelah `native::teks`
    // memakai rantai dua face dengan pemilihan per code point dan shaping
    // `rustybuzz` (diuji di modul itu: aksara Devanagari berganti face dan
    // matra `i` mendahului konsonannya); yang menggantikannya adalah batas
    // yang lebih sempit: tanpa `@font-face`, tanpa face per bobot.
    // "raster tidak melukis teks" juga dicabut setelah `native::paint`
    // melukis glyph dari outline TrueType, lalu klausa "tanpa gambar/gradien/
    // border, tanpa tebal sintetis, `justify` rata kiri, tidak ada pemotongan"
    // dicabut setelah keempatnya dilukis; yang tersisa adalah batas yang lebih
    // sempit: tanpa bayangan, `border` per sisi, dan `object-fit` selain
    // `fill`.
    // "mesin tidak mengirim peristiwa CDP" dicabut setelah
    // `cdp::native_wire::peristiwa` mengantre `Runtime.consoleAPICalled`,
    // `Runtime.executionContextCreated`, `Page.loadEventFired`, dan
    // `Network.requestWillBeSent`/`responseReceived`, lalu `layani` mengurasnya
    // sesudah tiap frame; yang menggantikannya adalah batas yang lebih sempit:
    // tidak ada jalur dorong, dan domain lain belum ada.
    // "stylesheet eksternal tidak diambil" dicabut setelah
    // `cdp::native_wire::gaya` mengambil `<link rel=stylesheet>` beserta
    // `@import` rekursif dan memasangnya di posisi `<link>`-nya; yang
    // menggantikannya adalah batas at-rule yang lebih sempit: `@media` dan
    // `@font-face` masih dicatat lalu tidak dijalankan.
    // Yang tersisa di daftar adalah celah yang masih benar-benar terbuka.
    for wajib in [
        // Batas teks yang masih terbuka. Kalau baris ini hilang, daftar
        // mengklaim webfont sudah diunduh padahal `@font-face` masih dicatat
        // lalu dilewati, dan halaman yang bergantung pada `font-family` akan
        // dilaporkan tampil sesuai desainnya.
        "`@font-face` tidak diunduh",
        "raster melukis latar kotak dan glyph teks",
        "`fetch()` di dalam isolat",
        // Batas peristiwa CDP yang masih terbuka. Kalau baris ini hilang,
        // daftar mengklaim mesin mendorong peristiwa ke klien tanpa ditanya,
        // padahal antreannya baru dikuras sesudah klien mengirim frame.
        "antreannya baru dikuras sesudah klien mengirim satu frame",
        // Batas at-rule yang masih terbuka. Kalau baris ini hilang, daftar
        // mengklaim `@media` sudah dijalankan padahal isinya masih dilewati,
        // dan halaman yang menaruh tata letaknya di dalam `@media` akan
        // dilaporkan tampil benar.
        "at-rule selain `@import` belum dijalankan",
        // Batas screenshot yang masih benar-benar terbuka: encoder `webp`
        // belum ada. Kalau baris ini hilang, daftar mengklaim screenshot
        // melayani semua format yang diminta Chrome.
        "`webp` (dan",
        // Batas AX yang masih terbuka: isi `<iframe>`. Kalau baris ini hilang,
        // daftar mengklaim pohon aksesibilitas sudah mencakup dokumen anak,
        // padahal iframe belum dimuat sama sekali.
        "pohon aksesibilitas belum mencakup konten di dalam `<iframe>`",
    ] {
        assert!(
            semua.contains(wajib),
            "celah `{wajib}` hilang dari daftar: {semua}"
        );
    }
    // Penjaga arah sebaliknya: celah yang sudah ditutup tidak boleh muncul
    // lagi di daftar. Kalau selektor dikembalikan ke `CELAH`, kemampuan yang
    // sudah ada jadi diklaim belum ada, dan pembaca berikutnya salah paham.
    assert!(
        !semua.contains("selektor CSS"),
        "selektor CSS sudah ditutup, jangan dikembalikan ke CELAH: {semua}"
    );
    assert!(
        !semua.contains("rasterisasi bitmap"),
        "rasterisasi sudah ditutup, jangan dikembalikan ke CELAH: {semua}"
    );
    // Kalimat lama "teks ... tidak dilukis" sudah tidak benar: glyph dilukis
    // dari outline face yang ditanam. Kalau kalimat itu kembali, pembaca
    // mengira halaman teks masih menghasilkan kanvas kosong.
    assert!(
        !semua.contains("kanvas kosong"),
        "teks sudah dilukis, jangan kembalikan kalimat lamanya: {semua}"
    );
    assert!(
        !semua.contains("pengambilan dokumen dari jaringan"),
        "pengambilan dokumen dari jaringan sudah ditutup, jangan dikembalikan ke CELAH: {semua}"
    );
    // Versi lama dari celah teks: lebar teks sudah diukur dan kotaknya sudah
    // ada. Kalau kalimat ini kembali, pembaca mengira node teks masih tanpa
    // kotak padahal tes `native::layout` membuktikan sebaliknya.
    assert!(
        !semua.contains("pengukuran teks belum ada"),
        "pengukuran teks sudah ada, jangan kembalikan kalimat lamanya: {semua}"
    );
    // `Page.captureScreenshot` sekarang melayani `jpeg`, `quality`,
    // `optimizeForSpeed`, dan `clip.scale` sungguhan (dirender ulang pada
    // kerapatan itu), jadi kalimat "hanya PNG pada skala 1" tidak boleh
    // kembali: pembaca akan mengira format dan skala masih ditolak.
    // Klausa lama "belum melukis gambar, gradien, border, maupun radius"
    // sudah tidak benar sejak `native::paint` melukis keempatnya (diuji di
    // modul itu: gradien, border, radius, PNG, dan JPEG). Kalau klausa itu
    // kembali, pembaca mengira halaman bergambar masih keluar sebagai kotak
    // kosong.
    for usang in [
        // Kalimat lama sebelum rantai face dan shaping: kalau salah satunya
        // kembali, pembaca mengira teks Arab dan Devanagari masih diukur
        // selebar `.notdef`.
        "pengukuran teks hanya satu face",
        "tidak ada shaping",
        "belum melukis gambar",
        "belum melukis gradien",
        "justify` diperlakukan seperti rata kiri",
        "tidak ada tebal sintetis",
        "glyph tidak dipotong",
    ] {
        assert!(
            !semua.contains(usang),
            "kemampuan raster sudah ditutup, jangan kembalikan `{usang}`: {semua}"
        );
    }
    assert!(
        !semua.contains("hanya PNG pada skala 1"),
        "screenshot sudah multi-format dan multi-skala: {semua}"
    );
    // Klausa lama "`<link rel=stylesheet>` tidak diambil" sudah tidak benar:
    // `cdp::native_wire::gaya` mengambilnya beserta `@import`, dan raster
    // memakainya. Kalau klausa itu kembali, pembaca mengira halaman bertaut
    // gaya masih tampil tanpa CSS.
    assert!(
        !semua.contains("<link rel=stylesheet>` tidak diambil"),
        "stylesheet eksternal sudah diambil, jangan kembalikan kalimat lamanya: {semua}"
    );
    // Klausa lama "`aria-labelledby` berantai, status focusable/selected/
    // expanded belum dihitung" sudah tidak benar sejak W7: ketiganya dibaca
    // dari atribut dokumen di `native::axtree`. Kalau klausa itu kembali,
    // pembaca mengira pohon AX masih mengirim konstanta.
    assert!(
        !semua.contains("belum menghitung `aria-labelledby`"),
        "detail AX sudah dihitung, jangan kembalikan kalimat lamanya: {semua}"
    );
}
