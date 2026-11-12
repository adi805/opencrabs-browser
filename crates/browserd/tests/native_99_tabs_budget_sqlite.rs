//! AC-009 dan NFR-001 di jalur produksi: 99 tab dengan penyimpanan dingin SQLite.
//!
//! Tes anggaran sebelumnya (`native_99_tabs_budget.rs`) memakai
//! `MemoryColdStore`, yaitu kasus **konservatif**: snapshot tab yang disingkirkan
//! tetap tinggal di RAM, jadi PSS-nya batas atas, bukan angka yang dirasakan
//! pengguna. Daemon tidak memakai penyimpanan itu. `daemon.rs` memasang
//! `tabstore::SumberDingin` yang membuat `StoreColdStore`, dan adapter itulah
//! yang menulis snapshot ke tabel `tab_snapshot` di SQLite.
//!
//! Berkas ini mengukur hal yang belum pernah diukur: PSS proses pada 99 tab
//! ketika snapshot benar-benar meninggalkan RAM dan mendarat di disk. Beban dan
//! urutan operasinya sama persis dengan tes anggaran sebelumnya (99 tab, dokumen
//! 3006 node, tab panas dijaga di 25), jadi selisih kedua angka adalah efek
//! penyimpanan dinginnya, bukan efek pekerjaan yang berbeda.
//!
//! Satu fungsi tes saja, alasannya sama dengan berkas anggaran: `cargo test`
//! menjalankan tes dalam satu proses, dan tes kedua di berkas yang sama akan
//! menambah PSS proses sehingga angka yang dilaporkan bukan lagi angka beban.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use browserd::tabstore::StoreColdStore;
use engine::native::tab_pool::{BASELINE_MB, PAGU_MB, PSS_PER_TAB_MB, TabPool, tab_panas_maksimum};
use store::{Store, StoreKey};

/// Jumlah `div` di dokumen beban, sama dengan tes anggaran dan tes rehidrasi.
const JUMLAH_DIV: usize = 1500;

/// Batas tab yang dijanjikan PRD, dipakai apa adanya dari `cdp`.
const MAKS_TAB: usize = cdp::native_wire::MAKS_TAB;

struct Sementara {
    path: PathBuf,
}

impl Sementara {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ocb-pss-sqlite-{}-{label}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("direktori sementara");
        Self { path }
    }

    fn db(&self) -> PathBuf {
        self.path.join("store.db")
    }

    fn kunci(&self) -> PathBuf {
        self.path.join("store.key")
    }
}

impl Drop for Sementara {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn dokumen_beban() -> String {
    let mut s = String::with_capacity(JUMLAH_DIV * 64);
    s.push_str("<!doctype html><html><head><title>beban</title></head><body>");
    for i in 0..JUMLAH_DIV {
        let _ = write!(
            s,
            "<div id=\"k{i}\" class=\"baris\" style=\"height:20px\">isi baris {i}</div>"
        );
    }
    s.push_str("</body></html>");
    s
}

/// PSS proses ini dalam MB, dibaca dari `/proc/self/smaps_rollup`.
fn pss_mb() -> f64 {
    let teks = std::fs::read_to_string("/proc/self/smaps_rollup")
        .expect("smaps_rollup harus terbaca di Linux");
    for baris in teks.lines() {
        if let Some(sisa) = baris.strip_prefix("Pss:") {
            let kb: f64 = sisa
                .split_whitespace()
                .next()
                .expect("nilai Pss")
                .parse()
                .expect("Pss dalam kB");
            return kb / 1024.0;
        }
    }
    panic!("baris Pss tidak ditemukan di smaps_rollup");
}

fn ukuran(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

#[test]
fn sembilan_puluh_sembilan_tab_di_bawah_seratus_mb_pss_dengan_sqlite() {
    let maks_panas = tab_panas_maksimum(BASELINE_MB);
    assert_eq!(maks_panas, 25, "floor((100 - 20) / 3,10) = 25");

    let html = dokumen_beban();
    let node = engine::native::Document::parse_html(&html)
        .expect("dokumen beban harus terparse")
        .jumlah_node();
    assert!(
        node >= 3000,
        "beban harus seukuran harness 3006 node, dapat {node} node"
    );

    let dir = Sementara::new("cold");
    let kunci = StoreKey::create(&dir.kunci()).expect("buat kunci store");

    // Sesi harus ada sebelum tab pertama disingkirkan: foreign key menolak
    // baris `tab_snapshot` tanpa induknya. Sesinya "mesin-uji", id yang sama
    // dengan yang dibawa adapter di bawah.
    {
        let store = Store::open(dir.db(), kunci.clone()).expect("buka store");
        store
            .open_session("mesin-uji", "native", None, 1)
            .expect("sesi");
    }

    // Adapter yang sama dengan yang dipasang daemon, bukan tiruan.
    let dingin = StoreColdStore::dari_berkas(&dir.db(), kunci, "mesin-uji");

    let pss_awal = pss_mb();
    let mut pool = TabPool::new(dingin, MAKS_TAB);
    let mut panas_tertinggi = 0usize;

    for nomor in 0..MAKS_TAB {
        pool.buka(&format!("https://contoh.test/tab-{nomor}"), &html)
            .unwrap_or_else(|err| panic!("tab ke-{} ditolak: {err:?}", nomor + 1));
        panas_tertinggi = panas_tertinggi.max(pool.jumlah_panas());
        assert!(
            pool.jumlah_panas() <= maks_panas,
            "setelah tab ke-{} ada {} tab panas, pagu {}",
            nomor + 1,
            pool.jumlah_panas(),
            maks_panas
        );
    }

    let pss_akhir = pss_mb();
    let dingin_tercatat = pool.jumlah_dingin().expect("jumlah tab dingin");
    let db_bytes = ukuran(&dir.db());

    assert_eq!(pool.jumlah_dialamat(), MAKS_TAB, "99 tab harus terkelola");
    assert_eq!(pool.jumlah_panas(), maks_panas, "tab panas tepat di pagu");
    assert_eq!(
        dingin_tercatat,
        MAKS_TAB - maks_panas,
        "sisanya dingin di SQLite"
    );
    assert_eq!(
        panas_tertinggi, maks_panas,
        "tab panas tidak pernah melewati pagu"
    );
    assert!(
        db_bytes > 0,
        "snapshot harus mendarat di disk, bukan hanya di RAM"
    );

    let laporan = format!(
        "PSS {pss_akhir:.1} MB (awal {pss_awal:.1} MB, selisih {:.1} MB) untuk {} tab, \
         {} panas + {} dingin di SQLite, dokumen {node} node, database {:.1} MB, \
         perkiraan model {:.1} MB",
        pss_akhir - pss_awal,
        pool.jumlah_dialamat(),
        pool.jumlah_panas(),
        dingin_tercatat,
        db_bytes as f64 / (1024.0 * 1024.0),
        BASELINE_MB + pool.jumlah_panas() as f64 * PSS_PER_TAB_MB
    );
    println!("{laporan}");
    assert!(
        pss_akhir <= PAGU_MB,
        "PSS {pss_akhir:.1} MB melewati pagu {PAGU_MB:.0} MB: {laporan}"
    );
}
