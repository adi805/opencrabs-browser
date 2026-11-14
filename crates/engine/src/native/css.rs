//! Kaskade stylesheet: dari teks CSS ke deklarasi pemenang per elemen.
//!
//! Yang diurai di sini adalah subset CSS yang benar-benar dipakai halaman
//! biasa: blok aturan `selektor { deklarasi }`, daftar selektor dipisah koma,
//! `!important`, dan shorthand `padding`/`margin`. `@media` **dijalankan**:
//! prelude-nya dievaluasi terhadap [`KonteksMedia`], dan isinya masuk kaskade
//! di posisi blok itu bila cocok. `@font-face` dibaca karena bloknya
//! mendeklarasikan face. At-rule lain (`@supports`, sisa `@import`) tetap
//! **tidak** dijalankan: dicatat di [`Kaskade::diabaikan`], bukan diterapkan
//! dengan arti lain. `@import`
//! diselesaikan lebih dulu oleh lapisan yang mengenal jaringan
//! (`cdp::native_wire::gaya`), jadi yang sampai ke sini sudah berupa teks
//! sisipan; `@import` yang masih tersisa di teks tetap dicatat sebagai at-rule
//! yang tidak dijalankan, bukan ditebak isinya.
//!
//! `<link rel="stylesheet">` **tidak** diambil dari jaringan oleh crate ini:
//! crate mesin sengaja nol pengetahuan jaringan. [`tautan_eksternal`] memberi
//! tahu tautan mana yang layak diambil, dan [`Kaskade::kumpulkan_dengan`]
//! menerima teks CSS-nya dari lapisan yang mengambil. Lembar itu dipasang **di
//! posisi `<link>`-nya** di pohon, bukan ditempel di akhir, supaya `<style>`
//! yang datang sesudahnya tetap menang sesuai urutan sumber. Tautan yang tidak
//! disertai teksnya tetap dicatat di [`Kaskade::diabaikan`], bukan diterapkan
//! dengan arti lain.

use std::collections::HashMap;

use super::dom::{Document, Node, NodeId, NodeKind};
use super::selectors::{Kompleks, urai};

/// Satu lembar gaya dari luar dokumen, mis. hasil pengambilan
/// `<link rel="stylesheet" href="...">`.
///
/// `href` adalah teks atribut `href` **apa adanya**, bukan URL absolut:
/// kaskade mencocokkannya dengan node `<link>` di pohon, sementara penyelesaian
/// URL relatif (termasuk `<base href>`) adalah tugas lapisan yang mengenal
/// jaringan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LembarLuar {
    pub href: String,
    pub teks: String,
}

impl LembarLuar {
    #[must_use]
    pub fn baru(href: impl Into<String>, teks: impl Into<String>) -> Self {
        Self {
            href: href.into(),
            teks: teks.into(),
        }
    }
}

/// Satu tautan stylesheet yang layak diambil dari jaringan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tautan {
    /// Teks atribut `href` apa adanya; kunci yang dipakai [`LembarLuar`].
    pub href: String,
}

/// Keputusan untuk satu node `<link rel="stylesheet">`.
enum StatusTautan {
    Ambil,
    Lewati(String),
}

/// Membaca `rel`, `media`, dan `disabled` satu node `<link>`.
///
/// Aturannya sama dengan yang dipakai [`Kaskade::kumpulkan_dengan`], jadi
/// tautan yang dilewati di sini tidak mungkin tiba-tiba diterapkan di sana.
fn status_tautan(node: &Node) -> StatusTautan {
    let rel = node.attr("rel").unwrap_or_default().to_ascii_lowercase();
    let rel: Vec<&str> = rel.split_whitespace().collect();
    if rel.contains(&"alternate") {
        return StatusTautan::Lewati(
            "`rel=alternate` adalah stylesheet alternatif, bukan yang berlaku".to_owned(),
        );
    }
    if node.attr("disabled").is_some() {
        return StatusTautan::Lewati("atribut `disabled` membuat lembar tidak berlaku".to_owned());
    }
    let media = node.attr("media").unwrap_or("").trim().to_ascii_lowercase();
    // Media yang bisa diputuskan tanpa mesin media query: kosong, `all`, atau
    // `screen`. `print`, `speech`, dan query berparameter (`screen and
    // (min-width: 600px)`) tidak dievaluasi; menerapkannya akan membuat halaman
    // memakai aturan yang seharusnya tidak berlaku, jadi dilewati dan dicatat.
    let layak_media = media.is_empty()
        || media
            .split(',')
            .map(str::trim)
            .all(|m| m == "all" || m == "screen");
    if !layak_media {
        return StatusTautan::Lewati(format!("`media=\"{media}\"` tidak dievaluasi mesin ini"));
    }
    StatusTautan::Ambil
}

/// Mengumpulkan `<link rel="stylesheet">` yang layak diambil dari jaringan.
///
/// Yang dilewati dikembalikan sebagai catatan beserta alasannya, supaya halaman
/// yang tampak tanpa gaya bisa dijelaskan tanpa menebak.
#[must_use]
pub fn tautan_eksternal(doc: &Document) -> (Vec<Tautan>, Vec<String>) {
    let mut ambil = Vec::new();
    let mut lewati = Vec::new();
    for (id, node) in doc.nodes().iter().enumerate() {
        if node.tag() != Some("link") {
            continue;
        }
        let rel = node.attr("rel").unwrap_or_default().to_ascii_lowercase();
        if !rel.split_whitespace().any(|r| r == "stylesheet") {
            continue;
        }
        match status_tautan(node) {
            StatusTautan::Ambil => match node.attr("href") {
                Some(href) if !href.trim().is_empty() => {
                    ambil.push(Tautan {
                        href: href.to_owned(),
                    });
                }
                _ => lewati.push(format!("node {id}: `<link rel=stylesheet>` tanpa `href`")),
            },
            StatusTautan::Lewati(alasan) => {
                let href = node.attr("href").unwrap_or("(tanpa href)");
                lewati.push(format!(
                    "node {id}: `<link rel=stylesheet href=\"{href}\">` dilewati: {alasan}"
                ));
            }
        }
    }
    (ambil, lewati)
}

/// Satu blok `@font-face` yang sudah diurai.
///
/// `sumber` berisi URL `src` **apa adanya**, bukan URL absolut, dengan alasan
/// yang sama seperti [`LembarLuar::href`]: resolusi relatif butuh URL dokumen
/// dan `<base href>`, dan itu pengetahuan lapisan jaringan. Entri `local(...)`
/// dibuang, karena face yang dipasang di mesin pengguna bukan webfont yang
/// boleh dipakai halaman; `format()` dan `tech()` diabaikan tanpa menebak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontFace {
    /// `font-family` apa adanya, tanpa tanda kutip pembungkus.
    pub keluarga: String,
    /// URL `src` urut kemunculan, hanya yang berbentuk `url(...)`.
    pub sumber: Vec<String>,
    /// `font-weight`, angka tunggal. Rentang (`100 900`) memakai ujung
    /// terdekatnya, bukan dirata-rata, karena tebal 500 dan 800 bukan 650.
    pub tebal: u16,
    /// `font-style`: `normal`, `italic`, atau `oblique`.
    pub gaya: String,
}

impl FontFace {
    /// Blok tanpa `font-family` atau tanpa satu pun `url()` tidak bisa dipakai:
    /// tidak ada nama untuk dipanggil dan tidak ada byte untuk dilukis.
    #[must_use]
    pub fn berguna(&self) -> bool {
        !self.keluarga.is_empty() && !self.sumber.is_empty()
    }
}

/// Mengumpulkan seluruh `@font-face` dokumen tanpa menjalankan aturan lain.
///
/// Dipisah dari [`Kaskade`] karena pengunduh webfont bekerja SEBELUM layout:
/// ia butuh daftar face untuk diambil dari jaringan, sementara kaskade baru
/// dibangun saat kotak dihitung. Yang dipakai tetap pengurai yang sama, jadi
/// blok `@font-face` rusak dilaporkan di tempat yang sama pula.
#[must_use]
pub fn font_face_dokumen(doc: &Document, eksternal: &[LembarLuar]) -> Vec<FontFace> {
    Kaskade::kumpulkan_dengan(doc, eksternal)
        .font_face()
        .to_vec()
}

/// Satu deklarasi `nama: nilai` di dalam blok aturan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deklarasi {
    pub nama: String,
    pub nilai: String,
    pub penting: bool,
}

/// Satu aturan dengan **satu** selektor. Daftar selektor dipisah koma
/// dipecah jadi beberapa aturan, karena tiap selektor punya spesifisitasnya
/// sendiri dan pemenangnya ditentukan per selektor, bukan per daftar.
#[derive(Debug, Clone)]
pub struct Aturan {
    pub selektor: Kompleks,
    pub spesifisitas: (u32, u32, u32),
    pub deklarasi: Vec<Deklarasi>,
    /// Urutan kemunculan di seluruh dokumen; dipakai sebagai pemutus seri
    /// terakhir setelah spesifisitas.
    pub urutan: usize,
}

/// Hasil penguraian satu lembar gaya.
#[derive(Debug, Clone, Default)]
pub struct Lembar {
    pub aturan: Vec<Aturan>,
    pub diabaikan: Vec<String>,
}

/// Kondisi media yang dipakai mengevaluasi `@media`.
///
/// Angkanya datang dari viewport yang sedang dipakai layout, bukan tebakan:
/// `@media (max-width: 600px)` yang dievaluasi dengan lebar karangan akan
/// menerapkan aturan yang seharusnya tidak berlaku, dan tidak ada galat yang
/// menandainya.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KonteksMedia {
    pub lebar: f32,
    pub tinggi: f32,
    pub skema_gelap: bool,
    pub gerak_berkurang: bool,
}

impl Default for KonteksMedia {
    fn default() -> Self {
        Self {
            lebar: 1280.0,
            tinggi: 800.0,
            skema_gelap: false,
            gerak_berkurang: false,
        }
    }
}

impl KonteksMedia {
    #[must_use]
    pub fn baru(lebar: f32, tinggi: f32) -> Self {
        Self {
            lebar,
            tinggi,
            ..Self::default()
        }
    }

    /// Mengevaluasi prelude `@media`, mis. `screen and (min-width: 600px)`.
    ///
    /// Daftar dipisah koma berarti ATAU; `and` di dalam satu alternatif
    /// berarti DAN. Alternatif yang memakai fitur tak dikenal dianggap tak
    /// dikenal, bukan cocok: CSS memperlakukan kueri rusak sebagai `not all`,
    /// dan menerapkannya akan mengubah halaman secara salah tanpa gejala.
    #[must_use]
    pub fn cocok(&self, kueri: &str) -> HasilKueri {
        let kueri = kueri.trim();
        if kueri.is_empty() {
            return HasilKueri::Cocok;
        }
        let mut tak_dikenal = None;
        for alternatif in kueri.split(',') {
            match self.satu_alternatif(alternatif.trim()) {
                HasilKueri::Cocok => return HasilKueri::Cocok,
                HasilKueri::Tidak => {}
                HasilKueri::TidakDikenal(a) => {
                    if tak_dikenal.is_none() {
                        tak_dikenal = Some(a);
                    }
                }
            }
        }
        match tak_dikenal {
            Some(a) => HasilKueri::TidakDikenal(a),
            None => HasilKueri::Tidak,
        }
    }

    fn satu_alternatif(&self, alternatif: &str) -> HasilKueri {
        if alternatif.is_empty() {
            return HasilKueri::Tidak;
        }
        let rendah = alternatif.to_ascii_lowercase();
        if let Some(sisa) = rendah.strip_prefix("only ") {
            return self.satu_alternatif(sisa);
        }
        if rendah.starts_with("not ") {
            // `not` membalik seluruh alternatif. Mesin ini belum mengurai
            // pohonnya, jadi menebak hasil balikannya lebih berbahaya
            // daripada melewatinya.
            return HasilKueri::TidakDikenal(format!("`{alternatif}` memakai `not`"));
        }
        let mut hasil = HasilKueri::Cocok;
        for bagian in rendah.split(" and ") {
            let bagian = bagian.trim();
            if bagian.is_empty() {
                continue;
            }
            let ini = if bagian.starts_with('(') {
                self.satu_fitur(bagian)
            } else {
                match bagian {
                    // Mesin ini layar, jadi `all` dan `screen` cocok.
                    "all" | "screen" => HasilKueri::Cocok,
                    _ => HasilKueri::Tidak,
                }
            };
            match ini {
                HasilKueri::Cocok => {}
                HasilKueri::Tidak => return HasilKueri::Tidak,
                HasilKueri::TidakDikenal(a) => {
                    if matches!(hasil, HasilKueri::Cocok) {
                        hasil = HasilKueri::TidakDikenal(a);
                    }
                }
            }
        }
        hasil
    }

    fn satu_fitur(&self, bagian: &str) -> HasilKueri {
        let dalam = bagian
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(bagian)
            .trim();
        let (nama, nilai) = match dalam.split_once(':') {
            Some((n, v)) => (n.trim(), Some(v.trim())),
            None => (dalam, None),
        };
        match (nama, nilai) {
            ("width", Some(v)) | ("device-width", Some(v)) => {
                banding(panjang(v, self), self.lebar, |w, b| (w - b).abs() < 0.5)
            }
            ("height", Some(v)) | ("device-height", Some(v)) => {
                banding(panjang(v, self), self.tinggi, |h, b| (h - b).abs() < 0.5)
            }
            ("min-width", Some(v)) | ("min-device-width", Some(v)) => {
                banding(panjang(v, self), self.lebar, |w, b| w >= b)
            }
            ("max-width", Some(v)) | ("max-device-width", Some(v)) => {
                banding(panjang(v, self), self.lebar, |w, b| w <= b)
            }
            ("min-height", Some(v)) | ("min-device-height", Some(v)) => {
                banding(panjang(v, self), self.tinggi, |h, b| h >= b)
            }
            ("max-height", Some(v)) | ("max-device-height", Some(v)) => {
                banding(panjang(v, self), self.tinggi, |h, b| h <= b)
            }
            ("orientation", Some(v)) => match v {
                "landscape" => boolean(self.lebar >= self.tinggi),
                "portrait" => boolean(self.lebar < self.tinggi),
                _ => HasilKueri::TidakDikenal(format!("nilai `orientation: {v}`")),
            },
            ("prefers-color-scheme", Some(v)) => match v {
                "dark" => boolean(self.skema_gelap),
                "light" => boolean(!self.skema_gelap),
                "no-preference" => HasilKueri::Cocok,
                _ => HasilKueri::TidakDikenal(format!("nilai `prefers-color-scheme: {v}`")),
            },
            ("prefers-reduced-motion", Some(v)) => match v {
                "reduce" => boolean(self.gerak_berkurang),
                "no-preference" => boolean(!self.gerak_berkurang),
                _ => HasilKueri::TidakDikenal(format!("nilai `prefers-reduced-motion: {v}`")),
            },
            _ => HasilKueri::TidakDikenal(format!("fitur `{dalam}`")),
        }
    }
}

/// Keputusan satu kueri media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HasilKueri {
    Cocok,
    Tidak,
    /// Kueri memakai fitur yang belum dikenal mesin ini. Isinya **tidak**
    /// diterapkan: aturan yang salah berlaku tidak punya gejala, jadi
    /// melewatinya sambil mencatat alasannya jauh lebih jujur.
    TidakDikenal(String),
}

/// Membandingkan panjang kueri terhadap ukuran sekarang.
fn banding(kueri: Option<f32>, sekarang: f32, uji: fn(f32, f32) -> bool) -> HasilKueri {
    match kueri {
        Some(k) if uji(sekarang, k) => HasilKueri::Cocok,
        Some(_) => HasilKueri::Tidak,
        None => HasilKueri::TidakDikenal("panjang tanpa angka".to_string()),
    }
}

fn boolean(b: bool) -> HasilKueri {
    if b {
        HasilKueri::Cocok
    } else {
        HasilKueri::Tidak
    }
}

/// Mengurai panjang kueri media: `600px`, `40em`, `50vw`, atau angka polos.
///
/// `em`/`rem` di kueri media selalu 16px, tidak mengikuti `font-size` akar
/// dokumen: begitulah spesifikasinya, dan memakai font akar akan membuat
/// kueri yang sama punya arti berbeda per halaman.
fn panjang(teks: &str, media: &KonteksMedia) -> Option<f32> {
    let t = teks.trim();
    if let Some(v) = t.strip_suffix("px") {
        return v.trim().parse().ok();
    }
    if let Some(v) = t.strip_suffix("em").or_else(|| t.strip_suffix("rem")) {
        return v.trim().parse::<f32>().ok().map(|n| n * 16.0);
    }
    if let Some(v) = t.strip_suffix("vw") {
        return v
            .trim()
            .parse::<f32>()
            .ok()
            .map(|n| n * media.lebar / 100.0);
    }
    if let Some(v) = t.strip_suffix("vh") {
        return v
            .trim()
            .parse::<f32>()
            .ok()
            .map(|n| n * media.tinggi / 100.0);
    }
    t.parse().ok()
}

/// Seluruh lembar gaya yang berlaku untuk satu dokumen, urut dokumen.
#[derive(Debug, Clone, Default)]
pub struct Kaskade {
    lembar: Vec<Lembar>,
    /// Blok `@font-face` dari seluruh lembar, urut kemunculan. Urutan penting:
    /// deklarasi terakhir untuk keluarga yang sama menang, sama seperti CSS.
    font_face: Vec<FontFace>,
    /// Dinaikkan tiap lembar baru supaya urutan tetap naik lintas lembar.
    urutan_berikut: usize,
    diabaikan: Vec<String>,
    /// Kondisi media yang dipakai mengevaluasi `@media` di seluruh lembar.
    media: KonteksMedia,
}

impl Kaskade {
    /// Mengumpulkan `<style>` dari dokumen, urut kemunculan, dan mencatat
    /// `<link rel="stylesheet">` sebagai tidak diambil.
    #[must_use]
    pub fn kumpulkan(doc: &Document) -> Self {
        Self::kumpulkan_dengan(doc, &[])
    }

    /// Sama dengan [`Self::kumpulkan`], tapi lembar eksternal yang sudah
    /// diambil dari jaringan ikut dipasang **di posisi `<link>`-nya**.
    ///
    /// Urutan itu yang membuat kaskade benar: lembar eksternal yang muncul
    /// sebelum `<style>` kalah oleh `<style>` itu, dan yang muncul sesudahnya
    /// menang. Menambahkan semua lembar di akhir akan membalik prioritas
    /// halaman yang menaruh `<link>` di atas blok `<style>`.
    #[must_use]
    pub fn kumpulkan_dengan(doc: &Document, eksternal: &[LembarLuar]) -> Self {
        Self::kumpulkan_dengan_media(doc, eksternal, KonteksMedia::default())
    }

    /// Sama dengan [`Self::kumpulkan_dengan`], tapi `@media` dievaluasi
    /// terhadap viewport yang benar-benar dipakai layout.
    #[must_use]
    pub fn kumpulkan_dengan_media(
        doc: &Document,
        eksternal: &[LembarLuar],
        media: KonteksMedia,
    ) -> Self {
        let mut k = Self {
            media,
            ..Self::default()
        };
        for (id, node) in doc.nodes().iter().enumerate() {
            let Some(tag) = node.tag() else {
                continue;
            };
            match tag {
                "style" => {
                    let teks = teks_di_dalam(doc, id);
                    k.tambah_lembar(&teks, &format!("<style> node {id}"));
                }
                "link" => {
                    let rel = node.attr("rel").unwrap_or_default().to_ascii_lowercase();
                    if !rel.split_whitespace().any(|r| r == "stylesheet") {
                        continue;
                    }
                    match status_tautan(node) {
                        StatusTautan::Lewati(alasan) => k.diabaikan.push(format!(
                            "node {id}: `<link rel=stylesheet>` dilewati: {alasan}"
                        )),
                        StatusTautan::Ambil => {
                            let href = node.attr("href").unwrap_or("(tanpa href)");
                            match eksternal.iter().find(|l| l.href == href) {
                                Some(lembar) => k.tambah_lembar(
                                    &lembar.teks,
                                    &format!("<link> node {id}: {href}"),
                                ),
                                None => k.diabaikan.push(format!(
                                    "node {id}: `<link rel=stylesheet>` tidak diambil dari \
                                     jaringan: {href}"
                                )),
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        k
    }

    /// Menambah satu lembar gaya dari teks CSS. Dipakai `kumpulkan` untuk
    /// `<style>`, dan tersedia untuk lapisan lain yang sudah punya teks
    /// stylesheet eksternal.
    pub fn tambah_lembar(&mut self, teks: &str, asal: &str) {
        let mut lembar = Lembar::default();
        lembar.aturan = urai_lembar(
            teks,
            &mut lembar.diabaikan,
            &mut self.font_face,
            &self.media,
        );
        for aturan in &mut lembar.aturan {
            aturan.urutan += self.urutan_berikut;
        }
        self.urutan_berikut += lembar.aturan.len();
        self.diabaikan
            .extend(lembar.diabaikan.iter().map(|d| format!("{asal}: {d}")));
        self.lembar.push(lembar);
    }

    /// Blok `@font-face` dari seluruh lembar, urut kemunculan.
    ///
    /// Lapisan jaringan memakai ini untuk tahu byte apa yang harus diambil;
    /// kaskade sendiri tidak pernah memasang face, karena memasang face butuh
    /// soket dan mesin ini tidak membuka soket di dalam lapisan gaya.
    #[must_use]
    pub fn font_face(&self) -> &[FontFace] {
        &self.font_face
    }

    /// Deklarasi yang menang per properti untuk satu elemen, sudah termasuk
    /// atribut `style` sebaris.
    ///
    /// Kuncinya `(penting, sebaris, spesifisitas, urutan)`: deklarasi
    /// `!important` mengalahkan deklarasi normal, `style` sebaris mengalahkan
    /// aturan selektor, lalu spesifisitas, lalu urutan sumber.
    #[must_use]
    pub fn menang(&self, doc: &Document, id: NodeId) -> Vec<(String, String)> {
        let mut pemenang = Pemenang::new();

        for lembar in &self.lembar {
            for aturan in &lembar.aturan {
                if !doc.cocok_terurai(id, &aturan.selektor) {
                    continue;
                }
                let dasar = (false, false, aturan.spesifisitas, aturan.urutan);
                for d in &aturan.deklarasi {
                    let kunci = (d.penting, dasar.1, dasar.2, dasar.3);
                    masukkan(&mut pemenang, &d.nama, &d.nilai, kunci);
                }
            }
        }

        if let Some(node) = doc.node(id)
            && let Some(style) = node.attr("style")
        {
            let mut diabaikan = Vec::new();
            for d in urai_deklarasi(style, &mut diabaikan) {
                let kunci = (d.penting, true, (0, 0, 0), usize::MAX);
                masukkan(&mut pemenang, &d.nama, &d.nilai, kunci);
            }
        }

        let mut out: Vec<(String, String)> = pemenang
            .into_iter()
            .map(|(nama, (_, nilai))| (nama, nilai))
            .collect();
        out.sort();
        out
    }

    /// Catatan hal yang sengaja tidak dijalankan: at-rule, stylesheet
    /// eksternal, dan deklarasi yang bentuknya tidak dikenali.
    #[must_use]
    pub fn diabaikan(&self) -> &[String] {
        &self.diabaikan
    }
}

/// Kunci pembanding kaskade: `(penting, sebaris, spesifisitas, urutan)`.
///
/// Diurutkan sebagai tuple, jadi urutannya persis prioritas CSS: `!important`
/// dulu, lalu deklarasi sebaris, lalu spesifisitas, lalu urutan sumber.
type Kunci = (bool, bool, (u32, u32, u32), usize);

/// Pemenang sementara per properti: kunci pembanding dan nilai yang menang.
type Pemenang = HashMap<String, (Kunci, String)>;

fn masukkan(peta: &mut Pemenang, nama: &str, nilai: &str, kunci: Kunci) {
    match peta.get(nama) {
        Some((kunci_lama, _)) if *kunci_lama >= kunci => {}
        _ => {
            peta.insert(nama.to_string(), (kunci, nilai.to_string()));
        }
    }
}

/// Teks dari seluruh node teks di dalam satu elemen, digabung tanpa pemisah.
fn teks_di_dalam(doc: &Document, id: NodeId) -> String {
    let mut out = String::new();
    let Some(node) = doc.node(id) else {
        return out;
    };
    if let NodeKind::Text(t) = &node.kind {
        out.push_str(t);
    }
    for &anak in &node.children {
        out.push_str(&teks_di_dalam(doc, anak));
    }
    out
}

/// Mengurai satu blok `@font-face` menjadi [`FontFace`].
///
/// Mengembalikan `None` kalau bloknya tidak menyebut `font-family` atau tidak
/// punya satu pun `url()`: blok seperti itu tidak bisa dipanggil maupun
/// dilukis, jadi mencatatnya sebagai face hanya akan menyesatkan lapisan
/// berikutnya.
fn urai_font_face(isi: &str, diabaikan: &mut Vec<String>) -> Option<FontFace> {
    let mut keluarga = String::new();
    let mut sumber = Vec::new();
    let mut tebal = 400u16;
    let mut gaya = "normal".to_string();
    for d in urai_deklarasi(isi, diabaikan) {
        match d.nama.as_str() {
            "font-family" => keluarga = tanpa_kutip(&d.nilai),
            "src" => sumber = url_di_src(&d.nilai),
            "font-weight" => tebal = bobot_pertama(&d.nilai).unwrap_or(400),
            "font-style" => {
                let v = d.nilai.trim().to_ascii_lowercase();
                if matches!(v.as_str(), "normal" | "italic" | "oblique") {
                    gaya = v;
                }
            }
            _ => {}
        }
    }
    let face = FontFace {
        keluarga,
        sumber,
        tebal,
        gaya,
    };
    face.berguna().then_some(face)
}

/// Membuang tanda kutip pembungkus nama keluarga, tanpa mengubah isinya.
fn tanpa_kutip(nilai: &str) -> String {
    let t = nilai.trim();
    let t = t
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| t.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(t);
    t.trim().to_string()
}

/// Mengambil setiap `url(...)` dari nilai `src`, urut kemunculan.
///
/// `local(...)` sengaja dibuang: face yang terpasang di mesin pengguna bukan
/// aset yang dikirim halaman, dan memakainya berarti hasil lukis bergantung
/// pada apa yang kebetulan terpasang di host itu. `format(...)` diabaikan
/// karena byte-nya tetap diperiksa sendiri saat dipasang.
fn url_di_src(nilai: &str) -> Vec<String> {
    let mut out = Vec::new();
    let rendah = nilai.to_ascii_lowercase();
    let bytes = rendah.as_bytes();
    let mut i = 0usize;
    while let Some(p) = rendah[i..].find("url(") {
        let mulai = i + p + 4;
        let Some(tutup) = rendah[mulai..].find(')') else {
            break;
        };
        let mentah = nilai[mulai..mulai + tutup].trim();
        let bersih = tanpa_kutip(mentah);
        if !bersih.is_empty() {
            out.push(bersih);
        }
        i = mulai + tutup + 1;
        if i >= bytes.len() {
            break;
        }
    }
    out
}

/// Angka pertama pada nilai `font-weight`.
///
/// Rentang seperti `100 900` memakai angka pertamanya, bukan titik tengah:
/// mesin ini memilih face per nama, dan memilih ujung rentang lebih jujur
/// daripada mengarang bobot yang tidak dideklarasikan.
fn bobot_pertama(nilai: &str) -> Option<u16> {
    let v = nilai.trim();
    match v.to_ascii_lowercase().as_str() {
        "normal" => return Some(400),
        "bold" => return Some(700),
        _ => {}
    }
    let angka: String = v.chars().take_while(char::is_ascii_digit).collect();
    angka.parse().ok()
}

/// Mengurai teks CSS menjadi aturan. Kesalahan sintaks tidak menggagalkan
/// seluruh lembar: bagian yang tidak bisa diurai dicatat di `diabaikan` dan
/// penguraian lanjut, karena satu aturan rusak di halaman nyata tidak boleh
/// membatalkan sisa stylesheet.
pub fn urai_lembar(
    teks: &str,
    diabaikan: &mut Vec<String>,
    font: &mut Vec<FontFace>,
    media: &KonteksMedia,
) -> Vec<Aturan> {
    let chars: Vec<char> = teks.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    let mut aturan = Vec::new();

    while i < n {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        if chars[i] == '/' && i + 1 < n && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < n && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        if chars[i] == '@' {
            let mulai = i;
            i += 1;
            while i < n && (chars[i].is_alphanumeric() || chars[i] == '-') {
                i += 1;
            }
            let nama: String = chars[mulai + 1..i].iter().collect();
            let setelah_nama = i;
            // At-rule berakhir di `;` (mis. `@import`) atau di blok `{}`
            // (mis. `@media`). Isinya TIDAK dijalankan: menjalankan
            // `@media` tanpa informasi media yang benar akan menerapkan
            // aturan yang seharusnya tidak berlaku. Satu pengecualian:
            // `@font-face` memang harus dibaca, karena bloknya mendeklarasikan
            // face yang kemudian dipakai `font-family`.
            let mut kedalaman = 0i32;
            let mut buka: Option<usize> = None;
            let mut badan: Option<String> = None;
            while i < n {
                match chars[i] {
                    '{' => {
                        kedalaman += 1;
                        if buka.is_none() {
                            buka = Some(i);
                        }
                    }
                    '}' => {
                        kedalaman -= 1;
                        if kedalaman <= 0 {
                            if let Some(b) = buka {
                                badan = Some(chars[b + 1..i].iter().collect());
                            }
                            i += 1;
                            break;
                        }
                    }
                    ';' if kedalaman == 0 => {
                        i += 1;
                        break;
                    }
                    _ => {}
                }
                i += 1;
            }
            if nama.eq_ignore_ascii_case("font-face") {
                match badan {
                    Some(isi) => match urai_font_face(&isi, diabaikan) {
                        Some(face) => font.push(face),
                        None => diabaikan
                            .push("at-rule `@font-face` tanpa `font-family`/`url()`".to_string()),
                    },
                    None => diabaikan.push("at-rule `@font-face` tanpa blok".to_string()),
                }
                continue;
            }
            if nama.eq_ignore_ascii_case("media") {
                let Some(isi) = badan else {
                    diabaikan.push("at-rule `@media` tanpa blok".to_string());
                    continue;
                };
                let prelude: String = match buka {
                    Some(b) => chars[setelah_nama..b].iter().collect(),
                    None => String::new(),
                };
                let prelude = prelude.trim();
                match media.cocok(prelude) {
                    HasilKueri::Cocok => {
                        // Isi `@media` masuk kaskade di POSISI bloknya, bukan
                        // di akhir lembar: urutan sumber masih menentukan
                        // pemenang saat spesifisitas sama.
                        aturan.extend(urai_lembar(&isi, diabaikan, font, media));
                    }
                    HasilKueri::Tidak => diabaikan.push(format!(
                        "at-rule `@media {prelude}` tidak cocok dengan viewport {}x{}",
                        media.lebar, media.tinggi
                    )),
                    HasilKueri::TidakDikenal(alasan) => diabaikan.push(format!(
                        "at-rule `@media {prelude}` tidak dievaluasi: {alasan}"
                    )),
                }
                continue;
            }
            diabaikan.push(format!("at-rule `@{nama}` tidak dijalankan"));
            continue;
        }

        let mulai = i;
        let mut kedalaman = 0i32;
        let mut kutip: Option<char> = None;
        while i < n {
            let c = chars[i];
            if let Some(k) = kutip {
                if c == k {
                    kutip = None;
                }
                i += 1;
                continue;
            }
            match c {
                '"' | '\'' => kutip = Some(c),
                '(' | '[' => kedalaman += 1,
                ')' | ']' => kedalaman -= 1,
                '{' | '}' if kedalaman == 0 => break,
                _ => {}
            }
            i += 1;
        }
        let prelude: String = chars[mulai..i].iter().collect();
        if i >= n || chars[i] != '{' {
            if !prelude.trim().is_empty() {
                diabaikan.push(format!("aturan tanpa blok `{}`", prelude.trim()));
            }
            i = (i + 1).min(n);
            continue;
        }

        i += 1;
        let isi_mulai = i;
        let mut kedalaman_blok = 1i32;
        let mut kutip_isi: Option<char> = None;
        while i < n {
            let c = chars[i];
            if let Some(k) = kutip_isi {
                if c == k {
                    kutip_isi = None;
                }
                i += 1;
                continue;
            }
            match c {
                '"' | '\'' => kutip_isi = Some(c),
                '{' => kedalaman_blok += 1,
                '}' => {
                    kedalaman_blok -= 1;
                    if kedalaman_blok == 0 {
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let isi: String = chars[isi_mulai..i.min(n)].iter().collect();
        i = (i + 1).min(n);

        let deklarasi = urai_deklarasi(&isi, diabaikan);
        if deklarasi.is_empty() {
            continue;
        }
        match urai(prelude.trim()) {
            Ok(daftar) => {
                for k in daftar {
                    aturan.push(Aturan {
                        spesifisitas: k.spesifisitas(),
                        selektor: k,
                        deklarasi: deklarasi.clone(),
                        urutan: 0,
                    });
                }
            }
            Err(e) => diabaikan.push(format!("selektor `{}` ditolak: {e}", prelude.trim())),
        }
    }
    aturan
}

/// Mengurai isi atribut `style` atau blok deklarasi menjadi daftar deklarasi.
///
/// Shorthand `padding`/`margin` diperluas jadi empat longhand di sini, supaya
/// kaskade tinggal membandingkan nama properti yang sama, bukan menebak mana
/// yang lebih spesifik antara `padding` dan `padding-left`.
pub fn urai_deklarasi(teks: &str, diabaikan: &mut Vec<String>) -> Vec<Deklarasi> {
    let mut out = Vec::new();
    for potong in pisah_deklarasi(teks) {
        let bersih = potong.trim();
        if bersih.is_empty() {
            continue;
        }
        let Some((nama, nilai)) = bersih.split_once(':') else {
            diabaikan.push(format!("deklarasi tanpa titik dua `{bersih}`"));
            continue;
        };
        let nama = nama.trim().to_ascii_lowercase();
        let (nilai, penting) = pisah_penting(nilai.trim());
        if nama.is_empty() || nilai.is_empty() {
            diabaikan.push(format!("deklarasi kosong `{bersih}`"));
            continue;
        }

        match nama.as_str() {
            "padding" | "margin" => {
                let bagian: Vec<&str> = nilai.split_whitespace().collect();
                let Some(empat) = empat_sisi(&bagian) else {
                    diabaikan.push(format!("`{nama}: {nilai}` tidak bisa diperluas"));
                    continue;
                };
                for (sisi, v) in ["top", "right", "bottom", "left"].iter().zip(empat.iter()) {
                    out.push(Deklarasi {
                        nama: format!("{nama}-{sisi}"),
                        nilai: v.clone(),
                        penting,
                    });
                }
            }
            _ => out.push(Deklarasi {
                nama,
                nilai,
                penting,
            }),
        }
    }
    out
}

/// Memisah deklarasi pada `;` yang berada di luar kurung dan di luar kutip.
fn pisah_deklarasi(teks: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut kedalaman = 0i32;
    let mut kutip: Option<char> = None;
    for c in teks.chars() {
        if let Some(k) = kutip {
            if c == k {
                kutip = None;
            }
            buf.push(c);
            continue;
        }
        match c {
            '"' | '\'' => {
                kutip = Some(c);
                buf.push(c);
            }
            '(' => {
                kedalaman += 1;
                buf.push(c);
            }
            ')' => {
                kedalaman -= 1;
                buf.push(c);
            }
            ';' if kedalaman == 0 => {
                out.push(std::mem::take(&mut buf));
            }
            _ => buf.push(c),
        }
    }
    if !buf.trim().is_empty() {
        out.push(buf);
    }
    out
}

/// Melepas penanda `!important` dari nilai, mengembalikan nilai dan tandanya.
///
/// Pencocokan memakai huruf kecil, tapi nilai yang dikembalikan tetap huruf
/// aslinya: nama font dan `url()` tidak boleh diubah besar kecilnya.
fn pisah_penting(nilai: &str) -> (String, bool) {
    let v = nilai.trim_end();
    let rendah = v.to_ascii_lowercase();
    let Some(idx) = rendah.rfind("important") else {
        return (v.to_string(), false);
    };
    if idx + "important".len() != rendah.len() {
        return (v.to_string(), false);
    }
    let sisa = v[..idx].trim_end();
    let Some(sisa) = sisa.strip_suffix('!') else {
        return (v.to_string(), false);
    };
    (sisa.trim_end().to_string(), true)
}

/// Memperluas 1 sampai 4 nilai shorthand jadi urutan atas, kanan, bawah, kiri.
fn empat_sisi(bagian: &[&str]) -> Option<Vec<String>> {
    let v: Vec<String> = bagian.iter().map(|s| (*s).to_string()).collect();
    Some(match v.len() {
        1 => vec![v[0].clone(); 4],
        2 => vec![v[0].clone(), v[1].clone(), v[0].clone(), v[1].clone()],
        3 => vec![v[0].clone(), v[1].clone(), v[2].clone(), v[1].clone()],
        4 => v,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dokumen(html: &str) -> Document {
        Document::parse_html(html).expect("html harus terurai")
    }

    fn peta(k: &Kaskade, doc: &Document, id: NodeId) -> HashMap<String, String> {
        k.menang(doc, id).into_iter().collect()
    }

    fn satu(html: &str, selektor: &str) -> HashMap<String, String> {
        let doc = dokumen(html);
        let k = Kaskade::kumpulkan(&doc);
        let id = doc
            .pilih_satu(selektor)
            .expect("selektor sah")
            .expect("elemen ada");
        peta(&k, &doc, id)
    }

    #[test]
    fn test_kaskade_style_block_menang_atas_bawaan() {
        let g = satu(
            "<html><head><style>div{width:120px}</style></head>\
             <body><div id=\"a\"></div></body></html>",
            "#a",
        );
        assert_eq!(g.get("width").map(String::as_str), Some("120px"));
    }

    #[test]
    fn test_kaskade_spesifisitas_lalu_urutan() {
        let g = satu(
            "<html><head><style>\
             div{width:10px;height:10px}\
             .k{width:20px;height:20px}\
             #a{width:30px}\
             </style></head><body><div id=\"a\" class=\"k\"></div></body></html>",
            "#a",
        );
        // id menang atas kelas dan tag
        assert_eq!(g.get("width").map(String::as_str), Some("30px"));
        // dua deklarasi tanpa id: yang muncul belakangan menang
        assert_eq!(g.get("height").map(String::as_str), Some("20px"));
    }

    #[test]
    fn test_kaskade_sebaris_menang_atas_aturan_biasa() {
        let g = satu(
            "<html><head><style>#a{width:30px}</style></head>\
             <body><div id=\"a\" style=\"width:40px\"></div></body></html>",
            "#a",
        );
        assert_eq!(g.get("width").map(String::as_str), Some("40px"));
    }

    #[test]
    fn test_kaskade_penting_menang_atas_sebaris_biasa() {
        let g = satu(
            "<html><head><style>#a{width:30px !important}</style></head>\
             <body><div id=\"a\" style=\"width:40px\"></div></body></html>",
            "#a",
        );
        assert_eq!(g.get("width").map(String::as_str), Some("30px"));
    }

    #[test]
    fn test_kaskade_sebaris_penting_menang_atas_aturan_penting() {
        let g = satu(
            "<html><head><style>#a{width:30px !important}</style></head>\
             <body><div id=\"a\" style=\"width:40px !important\"></div></body></html>",
            "#a",
        );
        assert_eq!(g.get("width").map(String::as_str), Some("40px"));
    }

    #[test]
    fn test_kaskade_shorthand_padding_dan_margin() {
        let g = satu(
            "<html><head><style>#a{padding:1px 2px 3px 4px;margin:0 auto}</style></head>\
             <body><div id=\"a\"></div></body></html>",
            "#a",
        );
        assert_eq!(g.get("padding-top").map(String::as_str), Some("1px"));
        assert_eq!(g.get("padding-right").map(String::as_str), Some("2px"));
        assert_eq!(g.get("padding-bottom").map(String::as_str), Some("3px"));
        assert_eq!(g.get("padding-left").map(String::as_str), Some("4px"));
        assert_eq!(g.get("margin-top").map(String::as_str), Some("0"));
        assert_eq!(g.get("margin-right").map(String::as_str), Some("auto"));
        assert_eq!(g.get("margin-bottom").map(String::as_str), Some("0"));
        assert_eq!(g.get("margin-left").map(String::as_str), Some("auto"));
    }

    #[test]
    fn test_kaskade_at_rule_dan_link_dicatat_bukan_dijalankan() {
        let doc = dokumen(
            "<html><head>\
             <link rel=\"stylesheet\" href=\"https://contoh.test/x.css\">\
             <style>@supports (display:grid) { div{width:1px} } div{height:2px}</style>\
             </head><body><div id=\"a\"></div></body></html>",
        );
        let k = Kaskade::kumpulkan(&doc);
        let id = doc.pilih_satu("#a").unwrap().unwrap();
        let g = peta(&k, &doc, id);
        // aturan di dalam @supports tidak dijalankan
        assert!(
            !g.contains_key("width"),
            "aturan @supports tidak boleh jalan"
        );
        // aturan sesudahnya tetap jalan
        assert_eq!(g.get("height").map(String::as_str), Some("2px"));
        let catatan = k.diabaikan().join(" | ");
        assert!(catatan.contains("x.css"), "href harus tercatat: {catatan}");
        assert!(
            catatan.contains("@supports"),
            "at-rule harus tercatat: {catatan}"
        );
    }

    #[test]
    fn test_media_query_dievaluasi_terhadap_viewport() {
        let html = "<html><head><style>\
             @media (min-width: 600px) { #a{width:11px} }\
             @media (max-width: 599px) { #a{width:99px} }\
             @media screen and (min-width: 400px) { #a{height:12px} }\
             @media print { #a{height:98px} }\
             </style></head><body><div id=\"a\"></div></body></html>";
        let doc = dokumen(html);
        let id = doc.pilih_satu("#a").unwrap().unwrap();

        let lapang = Kaskade::kumpulkan_dengan_media(&doc, &[], KonteksMedia::baru(800.0, 600.0));
        let g = peta(&lapang, &doc, id);
        assert_eq!(
            g.get("width").map(String::as_str),
            Some("11px"),
            "min-width harus jalan"
        );
        assert_eq!(
            g.get("height").map(String::as_str),
            Some("12px"),
            "screen and min-width harus jalan"
        );

        let sempit = Kaskade::kumpulkan_dengan_media(&doc, &[], KonteksMedia::baru(300.0, 600.0));
        let g2 = peta(&sempit, &doc, id);
        assert_eq!(
            g2.get("width").map(String::as_str),
            Some("99px"),
            "max-width harus jalan"
        );
        assert!(
            !g2.contains_key("height"),
            "min-width 400 tidak cocok di lebar 300"
        );
    }

    #[test]
    fn test_media_query_prefers_dan_fitur_tak_dikenal() {
        let html = "<html><head><style>\
             @media (prefers-color-scheme: dark) { #a{color:#ffffff} }\
             @media (min-resolution: 2dppx) { #a{width:50px} }\
             @media not screen { #a{width:77px} }\
             </style></head><body><div id=\"a\"></div></body></html>";
        let doc = dokumen(html);
        let id = doc.pilih_satu("#a").unwrap().unwrap();

        let terang = Kaskade::kumpulkan(&doc);
        let g = peta(&terang, &doc, id);
        assert!(
            !g.contains_key("color"),
            "skema terang tidak boleh kena aturan gelap"
        );
        assert!(
            !g.contains_key("width"),
            "fitur tak dikenal tidak boleh diterapkan"
        );
        let catatan = terang.diabaikan().join(" | ");
        assert!(
            catatan.contains("min-resolution"),
            "fitur tak dikenal harus tercatat: {catatan}"
        );
        assert!(
            catatan.contains("not screen"),
            "`not` harus tercatat: {catatan}"
        );

        let gelap = Kaskade::kumpulkan_dengan_media(
            &doc,
            &[],
            KonteksMedia {
                skema_gelap: true,
                ..KonteksMedia::default()
            },
        );
        let g2 = peta(&gelap, &doc, id);
        assert_eq!(g2.get("color").map(String::as_str), Some("#ffffff"));
    }

    fn dengan_lembar(html: &str, selektor: &str, lembar: &[LembarLuar]) -> HashMap<String, String> {
        let doc = dokumen(html);
        let k = Kaskade::kumpulkan_dengan(&doc, lembar);
        let id = doc
            .pilih_satu(selektor)
            .expect("selektor sah")
            .expect("elemen ada");
        peta(&k, &doc, id)
    }

    #[test]
    fn lembar_eksternal_dipasang_di_posisi_linknya() {
        let lembar = [LembarLuar::baru("a.css", "#a{width:10px;height:10px}")];
        let g = dengan_lembar(
            "<html><head>\
             <link rel=\"stylesheet\" href=\"a.css\">\
             <style>#a{width:20px}</style>\
             </head><body><div id=\"a\"></div></body></html>",
            "#a",
            &lembar,
        );
        assert_eq!(
            g.get("height").map(String::as_str),
            Some("10px"),
            "lembar eksternal harus dipakai, bukan dilewati"
        );
        assert_eq!(
            g.get("width").map(String::as_str),
            Some("20px"),
            "`<style>` sesudah `<link>` menang atas lembar eksternal"
        );
    }

    #[test]
    fn lembar_eksternal_sesudah_style_menang() {
        let lembar = [LembarLuar::baru("a.css", "#a{width:10px}")];
        let g = dengan_lembar(
            "<html><head><style>#a{width:20px}</style>\
             <link rel=\"stylesheet\" href=\"a.css\"></head>\
             <body><div id=\"a\"></div></body></html>",
            "#a",
            &lembar,
        );
        assert_eq!(
            g.get("width").map(String::as_str),
            Some("10px"),
            "lembar eksternal sesudah `<style>` harus menang"
        );
    }

    #[test]
    fn tautan_media_print_dan_alternate_dilewati_dengan_alasan() {
        let doc = dokumen(
            "<html><head>\
             <link rel=\"stylesheet\" href=\"cetak.css\" media=\"print\">\
             <link rel=\"alternate stylesheet\" href=\"lain.css\">\
             <link rel=\"stylesheet\" href=\"nonaktif.css\" disabled>\
             <link rel=\"stylesheet\" href=\"layar.css\" media=\"screen\">\
             </head><body></body></html>",
        );
        let (ambil, lewati) = tautan_eksternal(&doc);
        assert_eq!(
            ambil,
            vec![Tautan {
                href: "layar.css".to_owned()
            }],
            "hanya stylesheet layar yang layak diambil"
        );
        let catatan = lewati.join(" | ");
        for nama in ["cetak.css", "lain.css", "nonaktif.css"] {
            assert!(catatan.contains(nama), "{nama} harus tercatat: {catatan}");
        }
        assert!(catatan.contains("media"), "alasan media hilang: {catatan}");
        assert!(
            catatan.contains("alternate"),
            "alasan alternate hilang: {catatan}"
        );
    }

    #[test]
    fn tautan_tanpa_teks_lembar_tetap_dicatat() {
        let doc = dokumen(
            "<html><head><link rel=\"stylesheet\" href=\"belum.css\"></head>\
             <body><div id=\"a\"></div></body></html>",
        );
        let k = Kaskade::kumpulkan(&doc);
        let catatan = k.diabaikan().join(" | ");
        assert!(
            catatan.contains("belum.css"),
            "tautan yang tidak diambil harus tercatat: {catatan}"
        );
        assert!(
            catatan.contains("tidak diambil"),
            "catatan harus menyebut tidak diambil: {catatan}"
        );
    }
}
