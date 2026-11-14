//! Tab pool bertingkat: sebagian tab hidup di RAM, sisanya disimpan dingin.
//!
//! Angka yang dipakai di sini bukan karangan. Biaya satu tab hidup **3,10 MB
//! PSS marginal, datar dari tab ke-2 sampai ke-99**, diukur dengan harness
//! `html5ever` + `taffy` + `rquickjs` (`docs/prd/BUKTI-BUDGET 071126-1413.md`).
//! Karena itu 99 tab hidup tidak mungkin muat di 100 MB: 99 x 3,10 = 308,9 MB.
//! Yang membuat pagu itu tercapai adalah menyingkirkan tab, bukan mengecilkan
//! biayanya.
//!
//! Dua hal yang sengaja TIDAK diklaim di sini:
//!
//! - Baseline mesin (20 MB) masih asumsi sampai milestone 1 mengukurnya.
//! - Biaya metadata tab dingin (~1 KB) juga masih asumsi.
//!
//! Keduanya dipakai sebagai parameter, bukan sebagai konstanta kebenaran, supaya
//! begitu diukur cukup ganti angkanya dan jumlah tab panas ikut menyesuaikan.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::NativeError;
use super::ambil::{Pengambil, PengambilTolak};
use super::css::LembarLuar;
use super::dom::{Document, NodeId};
use super::js::{Halaman, Isolat, LaporanSkrip, PaguIsolat};

/// Biaya PSS marginal satu tab hidup, dalam MB.
///
/// Terukur, tapi bukan di daemon native ini: angkanya dari harness terpisah
/// (`html5ever` + `taffy` + `rquickjs`, dokumen 3006 node, datar sampai 99 tab).
/// Probe daemon native mengukur RSS, bukan PSS, dan dengan dokumen 403 node,
/// jadi kedua angka tidak boleh dibandingkan langsung. Jangan kutip ini sebagai
/// hasil ukur daemon sampai ada probe native dengan beban yang setara.
pub const PSS_PER_TAB_MB: f64 = 3.10;

/// Pagu memori proses, dalam MB. Ini angka yang dijanjikan ke owner.
pub const PAGU_MB: f64 = 100.0;

/// Baseline mesin sebelum tab pertama. **Asumsi** 20 MB.
///
/// Probe daemon native mencatat 20 MB RSS baseline, jadi angkanya punya dasar,
/// tapi itu RSS satu probe, bukan PSS dan bukan pengukuran ulang. Tetap
/// diperlakukan sebagai asumsi sampai pengukuran PSS di beban acceptance ada
/// (AC-009): turunnya baseline cuma menaikkan jumlah tab panas, sedangkan
/// naiknya baseline otomatis menurunkannya.
pub const BASELINE_MB: f64 = 20.0;

/// Jumlah tab panas maksimum menurut anggaran, dibulatkan ke bawah.
///
/// `floor((100 - 20) / 3,10) = 25`. Fungsi, bukan konstanta 25, supaya
/// perubahan baseline hasil pengukuran otomatis menurunkan jumlah tab panas
/// alih-alih diam-diam melewati pagu.
#[must_use]
pub fn tab_panas_maksimum(baseline_mb: f64) -> usize {
    if baseline_mb >= PAGU_MB {
        return 0;
    }
    ((PAGU_MB - baseline_mb) / PSS_PER_TAB_MB).floor().max(0.0) as usize
}

/// Identitas tab. Stabil selama tab tidak ditutup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(pub u64);

/// Tab yang sudah disingkirkan dari RAM, siap dimuat kembali.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub id: TabId,
    pub url: String,
    pub judul: Option<String>,
    /// HTML hasil serialisasi pohon DOM. Inilah bentuk tab yang disimpan.
    pub html: String,
    pub jumlah_node: usize,
    /// Unix epoch milidetik saat tab disingkirkan.
    pub disingkirkan_pada: u64,
    /// Lembar gaya eksternal yang sudah diambil untuk dokumen ini.
    ///
    /// Ikut disimpan karena tab yang dihidupkan kembali tanpa CSS-nya akan
    /// tampil dan terukur berbeda dari sebelum disingkirkan, padahal HTML-nya
    /// sama. Yang disimpan hanya teks yang sudah diambil; pengambilan ulang
    /// tetap keputusan lapisan jaringan.
    pub lembar: Vec<LembarLuar>,
}

/// Penyimpanan tab dingin.
///
/// Trait ini sengaja didefinisikan di sini dan **tidak** bergantung pada
/// `crates/store`: implementasi SQLite-nya dipasang di lapisan yang melihat
/// keduanya (`browserd`). Arah ketergantungannya satu arah, jadi tidak ada
/// siklus antar crate.
pub trait ColdStore {
    type Error: std::fmt::Debug;

    fn simpan(&mut self, snapshot: &Snapshot) -> Result<(), Self::Error>;
    fn muat(&self, id: TabId) -> Result<Option<Snapshot>, Self::Error>;
    fn hapus(&mut self, id: TabId) -> Result<(), Self::Error>;
    fn jumlah(&self) -> Result<usize, Self::Error>;
}

/// Penyimpanan dingin di memori, untuk tes dan untuk mode tanpa database.
///
/// Ini bukan "implementasi cadangan yang diam-diam dipakai di produksi":
/// produksi memakai SQLite lewat adapter di `browserd`. Kalau daemon jatuh
/// dengan pool ini, tab dingin hilang, dan itu memang sifatnya.
#[derive(Debug, Default)]
pub struct MemoryColdStore {
    isi: HashMap<TabId, Snapshot>,
}

impl ColdStore for MemoryColdStore {
    type Error = std::convert::Infallible;

    fn simpan(&mut self, snapshot: &Snapshot) -> Result<(), Self::Error> {
        self.isi.insert(snapshot.id, snapshot.clone());
        Ok(())
    }

    fn muat(&self, id: TabId) -> Result<Option<Snapshot>, Self::Error> {
        Ok(self.isi.get(&id).cloned())
    }

    fn hapus(&mut self, id: TabId) -> Result<(), Self::Error> {
        self.isi.remove(&id);
        Ok(())
    }

    fn jumlah(&self) -> Result<usize, Self::Error> {
        Ok(self.isi.len())
    }
}

/// Satu tab yang sedang hidup di RAM.
///
/// **Tidak ada `LaporanLayout` di sini, dan itu disengaja.** Layout adalah
/// fungsi murni dari `(dokumen, viewport)`: satu-satunya pembacanya di jalur
/// produksi adalah lapisan lukis, dan lapisan itu menghitung ulang sendiri
/// lewat `paint::lukis_dengan`. Menyimpannya berarti membayar 0,551 MB per tab
/// pada beban budget untuk data yang tidak dibaca siapa pun: 13,8 MB di 25 tab
/// panas, angka yang diukur di `crates/engine/tests/laporan_retensi.rs` dan
/// yang menjelaskan sebagian regresi PSS di
/// `docs/prd/BUKTI-PSS-DAEMON 131126-0323.md`.
pub struct TabPanas {
    pub id: TabId,
    pub url: String,
    /// Pohon DOM, dibagi dengan isolat JavaScript lewat [`Arc`].
    ///
    /// Disimpan sebagai `Arc<Mutex<..>>`, bukan `Document`, karena skrip
    /// halaman harus bisa memutasi pohon yang **sama** yang dibaca
    /// `DOM.getDocument` dan dilukis raster. Dua salinan berarti dua kebenaran
    /// yang menyimpang, dan penyimpangan itu baru ketahuan saat tangkapan layar
    /// tidak cocok dengan hasil `querySelector`.
    ///
    /// Field-nya privat dan hanya bisa diakses lewat [`TabPanas::dokumen`]
    /// supaya pinjaman tidak hidup lebih lama dari yang disadari pemanggil.
    ///
    /// **Jangan memegang kunci ini sambil menjalankan skrip halaman.**
    /// Jembatan DOM mengunci pohon yang sama saat skrip berjalan, jadi kunci
    /// yang ditinggal di sisi Rust akan mengunci diri sendiri.
    dokumen: Arc<Mutex<Document>>,
    pub isolat: Isolat,
    /// Hasil putaran `<script>` saat tab dibangun.
    ///
    /// Disimpan, bukan dibuang, karena "halaman kosong" dan "halaman yang
    /// skripnya gagal" terlihat sama dari luar. Dengan laporan ini pemanggil
    /// bisa membedakan keduanya tanpa membedah isolat.
    pub skrip: LaporanSkrip,
    pub terakhir_dipakai: Instant,
    /// Lembar gaya eksternal milik dokumen ini, urut sesuai `<link>`-nya.
    ///
    /// Disimpan di tab, bukan dihitung ulang saat melukis, karena teks CSS
    /// hanya bisa didapat dari jaringan: menghitung ulang berarti mengambil
    /// ulang setiap tangkapan layar.
    pub lembar: Vec<LembarLuar>,
    /// Face webfont `@font-face` milik dokumen ini.
    ///
    /// Ikut tab karena alasan yang sama dengan `lembar`: byte-nya hanya bisa
    /// didapat dari jaringan, dan pengukuran maupun pelukisan harus memakai
    /// kumpulan yang sama supaya kotak teks tidak meleset dari glyph-nya.
    /// Dibagi lewat [`Arc`] supaya setiap lukisan tidak menyalin byte font.
    pub font: std::sync::Arc<super::teks::KumpulanFont>,
}

impl TabPanas {
    /// Membaca pohon. Pinjaman dilepas saat nilai ini jatuh.
    ///
    /// `MutexGuard` sudah `#[must_use]`, jadi atributnya tidak diulang di sini.
    pub fn dokumen(&self) -> MutexGuard<'_, Document> {
        kunci(&self.dokumen)
    }

    /// Salinan [`Arc`] kumpulan font, untuk diserahkan ke pengukur dan pelukis.
    #[must_use]
    pub fn font(&self) -> std::sync::Arc<super::teks::KumpulanFont> {
        std::sync::Arc::clone(&self.font)
    }

    /// Salinan [`Arc`] pohon, untuk diserahkan ke isolat baru.
    #[must_use]
    pub fn pohon(&self) -> Arc<Mutex<Document>> {
        Arc::clone(&self.dokumen)
    }
}

/// Mengunci pohon satu tab. Poison dipulihkan, bukan diteruskan.
fn kunci(dokumen: &Arc<Mutex<Document>>) -> MutexGuard<'_, Document> {
    dokumen.lock().unwrap_or_else(|e| e.into_inner())
}

/// Alasan tab disingkirkan. Dicatat supaya tes bisa membedakan penyingkiran
/// karena anggaran dan penyingkiran karena diminta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlasanSingkir {
    Anggaran,
    Diminta,
}

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("kapasitas tab habis: maksimum {maks} tab dikelola, tutup satu tab dulu")]
    KapasitasPenuh { maks: usize },

    #[error("tab {0:?} tidak dikenal")]
    TabTidakAda(TabId),

    #[error("penyimpanan dingin gagal: {0}")]
    Dingin(String),

    #[error(transparent)]
    Native(#[from] NativeError),
}

/// Pool tab dengan penyingkiran LRU ke penyimpanan dingin.
pub struct TabPool<C: ColdStore> {
    panas: Vec<TabPanas>,
    dingin: C,
    maks_panas: usize,
    maks_total: usize,
    pagu_isolat: PaguIsolat,
    /// Jalur jaringan yang dipakai `fetch()` di setiap tab.
    ///
    /// Bawaannya [`PengambilTolak`]: pool yang dibuat tanpa kebijakan menolak
    /// semua, bukan mengizinkan semua. Kebijakan yang sebenarnya dipegang
    /// pemanggil (daemon), karena crate ini tidak boleh tahu aturan izin.
    pengambil: Arc<dyn Pengambil>,
    /// Viewport yang dilaporkan `window.innerWidth`/`innerHeight` dan dipakai
    /// `getBoundingClientRect` untuk menghitung layout malas.
    ///
    /// Bawaannya sama dengan [`super::layout::Viewport`] yang dipakai lapisan
    /// lukis di daemon (800x600). Berbeda dari itu berarti skrip halaman
    /// membaca ukuran yang bukan ukuran yang dilukis, dan `getBoundingClientRect`
    /// akan meleset dari tangkapan layar.
    viewport: super::layout::Viewport,
    berikutnya: u64,
    pub jumlah_singkir: usize,
}

impl<C: ColdStore> TabPool<C> {
    /// `maks_total` adalah jumlah tab yang bisa **dialamatkan** (99 menurut
    /// PRD), bukan jumlah yang hidup. Tab di atas `maks_panas` disingkirkan.
    pub fn new(dingin: C, maks_total: usize) -> Self {
        Self {
            panas: Vec::new(),
            dingin,
            maks_panas: tab_panas_maksimum(BASELINE_MB),
            maks_total,
            pagu_isolat: PaguIsolat::default(),
            pengambil: Arc::new(PengambilTolak),
            viewport: super::layout::Viewport::new(800.0, 600.0),
            berikutnya: 1,
            jumlah_singkir: 0,
        }
    }

    #[must_use]
    pub fn dengan_maks_panas(mut self, maks_panas: usize) -> Self {
        self.maks_panas = maks_panas;
        self
    }

    /// Memasang jalur jaringan untuk tab yang dibuka setelah ini.
    ///
    /// Tab yang sudah terbuka memakai kebijakan yang dipasang saat ia dibangun;
    /// mengganti kebijakan tidak menyusup ke isolat yang sedang hidup, dan itu
    /// disengaja: satu tab tidak boleh berubah aturan di tengah jalan.
    #[must_use]
    pub fn dengan_pengambil(mut self, pengambil: Arc<dyn Pengambil>) -> Self {
        self.pengambil = pengambil;
        self
    }

    /// Ukuran jendela yang dilihat skrip halaman.
    #[must_use]
    pub fn dengan_viewport(mut self, viewport: super::layout::Viewport) -> Self {
        self.viewport = viewport;
        self
    }

    #[must_use]
    pub fn maks_panas(&self) -> usize {
        self.maks_panas
    }

    #[must_use]
    pub fn jumlah_panas(&self) -> usize {
        self.panas.len()
    }

    pub fn jumlah_dingin(&self) -> Result<usize, PoolError> {
        self.dingin
            .jumlah()
            .map_err(|e| PoolError::Dingin(format!("{e:?}")))
    }

    #[must_use]
    pub fn jumlah_dialamat(&self) -> usize {
        self.panas.len() + self.dingin.jumlah().unwrap_or(0)
    }

    pub fn tab(&self, id: TabId) -> Option<&TabPanas> {
        self.panas.iter().find(|t| t.id == id)
    }

    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut TabPanas> {
        let t = self.panas.iter_mut().find(|t| t.id == id)?;
        t.terakhir_dipakai = Instant::now();
        Some(t)
    }

    /// Membuka tab baru. Kalau anggaran tab panas penuh, tab paling lama tidak
    /// dipakai disingkirkan lebih dulu, bukan tab baru ditolak.
    pub fn buka(&mut self, url: &str, html: &str) -> Result<TabId, PoolError> {
        self.buka_dengan(
            url,
            html,
            Vec::new(),
            std::sync::Arc::new(super::teks::KumpulanFont::baru()),
        )
    }

    /// Sama dengan [`Self::buka`], tapi dengan lembar gaya eksternal yang
    /// sudah diambil untuk dokumen itu.
    pub fn buka_dengan(
        &mut self,
        url: &str,
        html: &str,
        lembar: Vec<LembarLuar>,
        font: std::sync::Arc<super::teks::KumpulanFont>,
    ) -> Result<TabId, PoolError> {
        if self.jumlah_dialamat() >= self.maks_total {
            return Err(PoolError::KapasitasPenuh {
                maks: self.maks_total,
            });
        }
        if self.panas.len() >= self.maks_panas {
            self.singkirkan_lru(AlasanSingkir::Anggaran)?;
        }

        let id = TabId(self.berikutnya);
        self.berikutnya += 1;
        let panas = self.bangun(id, url, html, lembar, font)?;
        self.panas.push(panas);
        Ok(id)
    }

    /// Memasang dokumen `<iframe>` ke tab yang sudah terbuka.
    ///
    /// Dipisah dari [`Self::buka_dengan`] karena byte dokumen anak baru bisa
    /// diambil **sesudah** HTML induk diurai: URL iframe baru diketahui di
    /// titik itu. Pemanggil mengambil byte-nya lebih dulu (jaringan berjalan
    /// tanpa memegang kunci tab), lalu menyerahkan daftar `(url, html)` ke
    /// sini.
    ///
    /// `dasar` adalah URL dokumen induk. Tanpa itu, asal anak tidak bisa
    /// dibandingkan dengan apa pun dan `bingkai::rencana` menolak semua `src`,
    /// yang memang perilaku yang diinginkan: menebak "pasti same-origin"
    /// berarti membuka batas yang seharusnya dijaga.
    ///
    /// Tidak pernah mengembalikan galat: satu iframe yang gagal dipasang tidak
    /// boleh menggagalkan navigasi induknya. Semua yang dilewati masuk daftar
    /// catatan beserta alasannya.
    pub fn pasang_bingkai_dari(
        &mut self,
        id: TabId,
        dasar: &str,
        daftar: &[(String, String)],
    ) -> Vec<String> {
        let Some(tab) = self.panas.iter().find(|t| t.id == id) else {
            return vec![format!("tab {} tidak panas, iframe tidak dipasang", id.0)];
        };
        let mut dokumen = kunci(&tab.dokumen);
        let iframe: Vec<NodeId> = dokumen
            .nodes()
            .iter()
            .enumerate()
            .filter(|(_, n)| n.tag() == Some("iframe") && n.bingkai.is_none())
            .map(|(i, _)| i)
            .collect();
        let mut catatan = Vec::new();
        for simpul in iframe {
            match super::bingkai::rencana(&dokumen, simpul, Some(dasar)) {
                super::bingkai::Muatan::Kosong => {}
                super::bingkai::Muatan::LintasAsal(alasan) => {
                    catatan.push(format!("iframe {simpul}: {alasan}"));
                }
                super::bingkai::Muatan::Srcdoc(html) => {
                    if let Err(err) = dokumen.pasang_bingkai(simpul, &html) {
                        catatan.push(format!("iframe {simpul}: srcdoc gagal dipasang: {err}"));
                    }
                }
                super::bingkai::Muatan::Ambil(url) => {
                    match daftar.iter().find(|(u, _)| *u == url) {
                        Some((_, html)) => {
                            if let Err(err) = dokumen.pasang_bingkai(simpul, html) {
                                catatan.push(format!(
                                    "iframe {simpul} `{url}`: gagal dipasang: {err}"
                                ));
                            }
                        }
                        // Isinya tidak sampai ke pemanggil. Dicatat, bukan
                        // didiamkan: iframe kosong yang tidak dijelaskan
                        // terlihat seperti halaman yang kehilangan isinya.
                        None => catatan.push(format!(
                            "iframe {simpul} `{url}`: byte-nya tidak diserahkan pemanggil"
                        )),
                    }
                }
            }
        }
        catatan
    }

    fn bangun(
        &self,
        id: TabId,
        url: &str,
        html: &str,
        lembar: Vec<LembarLuar>,
        font: std::sync::Arc<super::teks::KumpulanFont>,
    ) -> Result<TabPanas, PoolError> {
        self.bangun_dengan(id, url, html, lembar, font, true)
    }

    /// [`Self::bangun`], dengan pilihan menjalankan `<script>` atau tidak.
    ///
    /// Tab yang dihidupkan dari penyimpanan dingin dibangun **tanpa**
    /// menjalankan skrip: snapshot menyimpan HTML **sesudah** skrip berjalan,
    /// jadi menjalankannya lagi akan menerapkan mutasinya dua kali
    /// (`appendChild` yang sama jadi dua simpul). Muat ulang dan navigasi tetap
    /// menjalankan skrip, karena itu memang pemuatan baru.
    fn bangun_dengan(
        &self,
        id: TabId,
        url: &str,
        html: &str,
        lembar: Vec<LembarLuar>,
        font: std::sync::Arc<super::teks::KumpulanFont>,
        jalankan_skrip: bool,
    ) -> Result<TabPanas, PoolError> {
        // Layout sengaja tidak dihitung di sini: lihat catatan di `TabPanas`.
        // Satu pohon, dua pemakai: isolat dan `TabPanas` memegang `Arc` yang
        // sama. `Isolat::new` sengaja tidak dipakai di sini, karena isolat
        // tanpa pohon tidak bisa menjawab `document.getElementById`.
        let dokumen = Arc::new(Mutex::new(Document::parse_html(html)?));
        let halaman = Arc::new(
            Halaman::baru(url, self.viewport, lembar.clone()).dengan_font(Arc::clone(&font)),
        );
        let isolat = Isolat::dengan_dokumen_ambil_dan_halaman(
            self.pagu_isolat,
            Arc::clone(&dokumen),
            Arc::clone(&self.pengambil),
            Some(halaman),
        )?;
        // Judul dibaca di blok sendiri supaya kuncinya jatuh sebelum
        // `pasang_judul` menjalankan skrip: jembatan mengunci pohon yang sama.
        let judul = {
            let dokumen = kunci(&dokumen);
            dokumen.judul()
        };
        if let Some(judul) = judul {
            isolat.pasang_judul(&judul)?;
        }
        // Skrip halaman berjalan sesudah jembatan DOM dan judul terpasang:
        // skrip yang membaca `document.title` di baris pertama harus melihat
        // judul dokumen, bukan string kosong. Timer dikuras sekali sesudahnya
        // supaya `setTimeout(fn, 0)` yang dipasang saat muat ikut jalan.
        let mut skrip = if jalankan_skrip {
            isolat.jalankan_skrip_halaman()?
        } else {
            LaporanSkrip::default()
        };
        if jalankan_skrip && let Err(err) = isolat.kuras_timer() {
            skrip.galat.push(format!("timer: {err}"));
        }
        Ok(TabPanas {
            skrip,
            id,
            url: url.to_string(),
            dokumen,
            isolat,
            terakhir_dipakai: Instant::now(),
            lembar,
            font,
        })
    }

    /// Menyingkirkan tab yang paling lama tidak dipakai. Tab dingin bisa
    /// dihidupkan lagi lewat [`TabPool::hidupkan`].
    pub fn singkirkan_lru(&mut self, alasan: AlasanSingkir) -> Result<Option<Snapshot>, PoolError> {
        let Some(korban) = self
            .panas
            .iter()
            .enumerate()
            .min_by_key(|(_, t)| t.terakhir_dipakai)
            .map(|(i, _)| i)
        else {
            return Ok(None);
        };

        let tab = self.panas.remove(korban);
        let snapshot = {
            let dokumen = kunci(&tab.dokumen);
            Snapshot {
                id: tab.id,
                url: tab.url.clone(),
                judul: dokumen.judul(),
                html: dokumen.to_html(),
                jumlah_node: dokumen.jumlah_node(),
                disingkirkan_pada: epoch_millis(),
                lembar: tab.lembar.clone(),
            }
        };
        self.dingin
            .simpan(&snapshot)
            .map_err(|e| PoolError::Dingin(format!("{e:?}")))?;
        self.jumlah_singkir += 1;
        let _ = alasan;
        Ok(Some(snapshot))
    }

    /// Mengembalikan tab dingin ke RAM. Kalau anggaran penuh, tab lain
    /// disingkirkan dulu. Tab yang tidak ada di kedua tempat dilaporkan.
    pub fn hidupkan(&mut self, id: TabId) -> Result<TabId, PoolError> {
        if self.panas.iter().any(|t| t.id == id) {
            if let Some(t) = self.panas.iter_mut().find(|t| t.id == id) {
                t.terakhir_dipakai = Instant::now();
            }
            return Ok(id);
        }

        let snapshot = self
            .dingin
            .muat(id)
            .map_err(|e| PoolError::Dingin(format!("{e:?}")))?
            .ok_or(PoolError::TabTidakAda(id))?;

        if self.panas.len() >= self.maks_panas {
            self.singkirkan_lru(AlasanSingkir::Anggaran)?;
        }

        // Webfont tidak ikut tab dingin: `Snapshot` menyimpan HTML dan teks
        // CSS, sementara byte font bisa puluhan megabyte dan tidak disimpan di
        // penyimpanan dingin. Tab yang dihidupkan kembali memakai face tertanam
        // sampai navigasi berikutnya mengunduh ulang `@font-face`. Perbedaan ini
        // dinyatakan di `CELAH`, bukan disembunyikan.
        let panas = self.bangun_dengan(
            snapshot.id,
            &snapshot.url,
            &snapshot.html,
            snapshot.lembar.clone(),
            std::sync::Arc::new(super::teks::KumpulanFont::baru()),
            false,
        )?;
        self.dingin
            .hapus(id)
            .map_err(|e| PoolError::Dingin(format!("{e:?}")))?;
        self.panas.push(panas);
        Ok(id)
    }

    pub fn tutup(&mut self, id: TabId) -> Result<(), PoolError> {
        if let Some(i) = self.panas.iter().position(|t| t.id == id) {
            self.panas.remove(i);
            return Ok(());
        }
        self.dingin
            .hapus(id)
            .map_err(|e| PoolError::Dingin(format!("{e:?}")))?;
        Ok(())
    }

    /// Mengganti isi satu tab dengan dokumen baru, seperti satu navigasi.
    ///
    /// Tab yang sedang dingin dihidupkan dulu, karena mengganti dokumen tab
    /// yang tidak ada di RAM berarti membuang snapshot lama tanpa pernah
    /// memakainya. Isolat JavaScript ikut dibuat ulang: dokumen baru tidak
    /// mewarisi variabel global dokumen lama, sama seperti browser sungguhan.
    ///
    /// `url` disimpan apa adanya. Fungsi ini tidak mengambil apa pun dari
    /// jaringan; pemanggil yang menyediakan HTML-nya.
    pub fn ganti_dokumen(&mut self, id: TabId, url: &str, html: &str) -> Result<(), PoolError> {
        self.ganti_dokumen_dengan(
            id,
            url,
            html,
            Vec::new(),
            std::sync::Arc::new(super::teks::KumpulanFont::baru()),
        )
    }

    /// Sama dengan [`Self::ganti_dokumen`], tapi dengan lembar gaya eksternal
    /// milik dokumen baru. Lembar lama tidak diwarisi: dokumen baru punya
    /// tautannya sendiri, dan menyisakan lembar lama berarti menerapkan CSS
    /// halaman sebelumnya ke halaman sekarang.
    pub fn ganti_dokumen_dengan(
        &mut self,
        id: TabId,
        url: &str,
        html: &str,
        lembar: Vec<LembarLuar>,
        font: std::sync::Arc<super::teks::KumpulanFont>,
    ) -> Result<(), PoolError> {
        if !self.panas.iter().any(|t| t.id == id) {
            self.hidupkan(id)?;
        }
        let baru = self.bangun(id, url, html, lembar, font)?;
        match self.panas.iter_mut().find(|t| t.id == id) {
            Some(t) => {
                *t = baru;
                Ok(())
            }
            None => Err(PoolError::TabTidakAda(id)),
        }
    }

    /// Perkiraan biaya RAM pool saat ini, memakai angka terukur per tab.
    #[must_use]
    pub fn perkiraan_mb(&self) -> f64 {
        BASELINE_MB + self.panas.len() as f64 * PSS_PER_TAB_MB
    }
}

fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn halaman(n: usize) -> String {
        let mut s = String::from("<!doctype html><html><head><title>tab</title></head><body>");
        for i in 0..n {
            s.push_str(&format!(
                "<div id=\"k{i}\" style=\"height:20px\">isi {i}</div>"
            ));
        }
        s.push_str("</body></html>");
        s
    }

    fn pool(maks_panas: usize, maks_total: usize) -> TabPool<MemoryColdStore> {
        TabPool::new(MemoryColdStore::default(), maks_total).dengan_maks_panas(maks_panas)
    }

    #[test]
    fn anggaran_tab_panas_diturunkan_dari_angka_terukur() {
        assert_eq!(tab_panas_maksimum(BASELINE_MB), 25, "floor((100-20)/3.10)");
        assert_eq!(
            tab_panas_maksimum(50.0),
            16,
            "baseline lebih besar, tab lebih sedikit"
        );
        assert_eq!(tab_panas_maksimum(100.0), 0, "tanpa ruang, nol tab panas");
    }

    #[test]
    fn tab_melebihi_anggaran_disingkirkan_bukan_ditolak() {
        let mut p = pool(3, 99);
        let id = [0, 1, 2, 3]
            .iter()
            .map(|i| {
                p.buka(&format!("https://contoh.test/{i}"), &halaman(2))
                    .unwrap()
            })
            .collect::<Vec<_>>();

        assert_eq!(p.jumlah_panas(), 3, "pagu tab panas dihormati");
        assert_eq!(p.jumlah_dingin().unwrap(), 1, "satu tab pindah ke dingin");
        assert_eq!(p.jumlah_dialamat(), 4, "alamat tab tetap empat");
        assert!(p.tab(id[0]).is_none(), "tab terlama yang disingkirkan");
        assert!(p.tab(id[3]).is_some());
    }

    #[test]
    fn tab_dingin_bisa_dihidupkan_dengan_isi_utuh() {
        let mut p = pool(1, 99);
        let a = p.buka("https://a.test", &halaman(3)).unwrap();
        let _b = p.buka("https://b.test", &halaman(3)).unwrap();

        assert!(p.tab(a).is_none());
        let node_dingin = {
            let snap = p.dingin.muat(a).unwrap().unwrap();
            snap.jumlah_node
        };

        p.hidupkan(a).unwrap();
        let tab = p.tab(a).expect("tab hidup lagi");
        assert_eq!(tab.dokumen().jumlah_node(), node_dingin);
        assert_eq!(tab.url, "https://a.test");
        assert_eq!(tab.dokumen().judul().as_deref(), Some("tab"));
        assert!(
            !tab.dokumen().nodes().is_empty(),
            "dokumen dipulihkan ke RAM, bukan cuma metadata"
        );
    }

    #[test]
    fn hidupkan_menyingkirkan_tab_lain_lebih_dulu() {
        let mut p = pool(1, 99);
        let a = p.buka("https://a.test", &halaman(1)).unwrap();
        let b = p.buka("https://b.test", &halaman(1)).unwrap();
        assert_eq!(p.jumlah_panas(), 1);

        p.hidupkan(a).unwrap();
        assert_eq!(p.jumlah_panas(), 1, "tetap satu tab panas");
        assert!(p.tab(b).is_none(), "tab b disingkirkan");
        assert_eq!(p.jumlah_dingin().unwrap(), 1);
    }

    #[test]
    fn kapasitas_total_99_ditegakkan() {
        let mut p = pool(25, 99);
        for i in 0..99 {
            p.buka(&format!("https://contoh.test/{i}"), &halaman(1))
                .unwrap();
        }
        assert_eq!(p.jumlah_dialamat(), 99);
        let err = p
            .buka("https://contoh.test/ke-100", &halaman(1))
            .unwrap_err();
        assert!(
            matches!(err, PoolError::KapasitasPenuh { maks: 99 }),
            "{err}"
        );
    }

    #[test]
    fn perkiraan_biaya_mengikuti_tab_panas() {
        let mut p = pool(5, 99);
        assert!((p.perkiraan_mb() - BASELINE_MB).abs() < f64::EPSILON);
        for i in 0..5 {
            p.buka(&format!("https://contoh.test/{i}"), &halaman(1))
                .unwrap();
        }
        let harap = BASELINE_MB + 5.0 * PSS_PER_TAB_MB;
        assert!(
            (p.perkiraan_mb() - harap).abs() < 1e-9,
            "{}",
            p.perkiraan_mb()
        );
        assert!(
            p.perkiraan_mb() <= PAGU_MB,
            "25 tab pun masih di bawah pagu"
        );
    }

    #[test]
    fn tutup_menghapus_dari_kedua_tempat() {
        let mut p = pool(1, 99);
        let a = p.buka("https://a.test", &halaman(1)).unwrap();
        let _b = p.buka("https://b.test", &halaman(1)).unwrap();
        p.tutup(a).unwrap();
        assert_eq!(p.jumlah_dingin().unwrap(), 0);
        assert!(
            p.hidupkan(a).is_err(),
            "tab yang ditutup tidak bisa dihidupkan"
        );
    }

    #[test]
    fn ganti_dokumen_pada_tab_dingin_menghidupkannya_dulu() {
        let mut p = pool(1, 99);
        let a = p.buka("https://a.test", &halaman(1)).unwrap();
        let b = p.buka("https://b.test", &halaman(1)).unwrap();
        assert_eq!(p.jumlah_dingin().unwrap(), 1, "a tersingkir");

        let baru =
            "<!doctype html><html><head><title>baru</title></head><body><p>halo</p></body></html>";
        p.ganti_dokumen(a, "https://a.test/kedua", baru).unwrap();

        assert_eq!(p.jumlah_dialamat(), 2, "tidak ada tab yang hilang");
        let tab = p.tab(a).expect("tab a panas setelah ganti dokumen");
        assert_eq!(tab.url, "https://a.test/kedua");
        assert_eq!(tab.dokumen().judul().as_deref(), Some("baru"));
        assert!(
            {
                let d = tab.dokumen();
                d.teks(d.root()).contains("halo")
            },
            "isi dokumen baru terbaca"
        );
        assert!(
            p.tab(b).is_none(),
            "tab b tetap dingin, bukan ikut dihidupkan"
        );
    }

    #[test]
    fn ganti_dokumen_membuang_isolat_lama() {
        let mut p = pool(2, 99);
        let a = p.buka("https://a.test", &halaman(1)).unwrap();
        p.tab(a)
            .unwrap()
            .isolat
            .eval("globalThis.__sisa = 'ada'")
            .unwrap();

        p.ganti_dokumen(a, "https://a.test/kedua", &halaman(1))
            .unwrap();
        let sisa = p.tab(a).unwrap().isolat.eval("typeof globalThis.__sisa");
        assert_eq!(sisa.unwrap(), "undefined", "state JS tidak ikut pindah");
    }

    fn lembar(href: &str, teks: &str) -> Vec<LembarLuar> {
        vec![LembarLuar::baru(href, teks)]
    }

    #[test]
    fn ganti_dokumen_dengan_menyimpan_lembar_baru() {
        let mut p = pool(2, 99);
        let a = p
            .buka_dengan(
                "https://a.test",
                &halaman(1),
                lembar("/lama.css", "div { height: 11px }"),
                std::sync::Arc::new(crate::native::teks::KumpulanFont::baru()),
            )
            .unwrap();
        assert_eq!(p.tab(a).unwrap().lembar.len(), 1, "lembar awal tersimpan");

        p.ganti_dokumen_dengan(
            a,
            "https://a.test/kedua",
            &halaman(1),
            lembar("/baru.css", "div { height: 33px }"),
            std::sync::Arc::new(crate::native::teks::KumpulanFont::baru()),
        )
        .unwrap();

        let tab = p.tab(a).expect("tab panas setelah ganti dokumen");
        assert_eq!(tab.lembar.len(), 1, "lembar lama tidak menumpuk");
        assert_eq!(tab.lembar[0].href, "/baru.css");
        assert_eq!(tab.lembar[0].teks, "div { height: 33px }");
    }

    #[test]
    fn ganti_dokumen_tanpa_lembar_membuang_lembar_lama() {
        // Dokumen baru punya tautannya sendiri. Mewarisi lembar lama berarti
        // menerapkan CSS halaman sebelumnya ke halaman sekarang, dan itu bug
        // yang tidak kelihatan sampai warnanya salah.
        let mut p = pool(2, 99);
        let a = p
            .buka_dengan(
                "https://a.test",
                &halaman(1),
                lembar("/a.css", "p { color: red }"),
                std::sync::Arc::new(crate::native::teks::KumpulanFont::baru()),
            )
            .unwrap();

        p.ganti_dokumen(a, "https://b.test", &halaman(1)).unwrap();

        let tab = p.tab(a).expect("tab panas");
        assert!(
            tab.lembar.is_empty(),
            "lembar lama masih menempel: {:?}",
            tab.lembar
        );
    }

    #[test]
    fn lembar_selamat_lewat_penyingkiran_dan_dihidupkan_lagi() {
        let mut p = pool(1, 99);
        let a = p
            .buka_dengan(
                "https://a.test",
                &halaman(2),
                lembar("/a.css", "div { color: #ff0000 }"),
                std::sync::Arc::new(crate::native::teks::KumpulanFont::baru()),
            )
            .unwrap();
        let _b = p.buka("https://b.test", &halaman(1)).unwrap();
        assert!(p.tab(a).is_none(), "a tersingkir ke dingin");

        let dingin = p.dingin.muat(a).unwrap().unwrap();
        assert_eq!(dingin.lembar.len(), 1, "lembar ikut ke tab dingin");
        assert_eq!(dingin.lembar[0].href, "/a.css");

        p.hidupkan(a).unwrap();
        let tab = p.tab(a).expect("tab hidup lagi");
        assert_eq!(tab.lembar.len(), 1, "lembar kembali saat dihidupkan");
        assert_eq!(tab.lembar[0].href, "/a.css");
        assert_eq!(tab.lembar[0].teks, "div { color: #ff0000 }");
    }

    #[test]
    fn ganti_dokumen_menolak_tab_yang_tidak_ada() {
        let mut p = pool(2, 99);
        let err = p
            .ganti_dokumen(TabId(77), "https://x.test", &halaman(1))
            .unwrap_err();
        assert!(matches!(err, PoolError::TabTidakAda(TabId(77))), "{err}");
    }
}
