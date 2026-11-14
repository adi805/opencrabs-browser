//! Mengisi store kosong dengan cookie **sintetis** untuk latihan restore.
//!
//! Contoh ini ada supaya drill backup bisa diulang siapa pun. Drill sebelumnya
//! (`BUKTI-RESTORE 071126-2025.md`) berjalan di atas store yang nol baris
//! cookie, jadi yang terbukti cuma paritas byte: hasil pulih memang terbuka,
//! tapi tidak ada satu pun cookie yang dibuktikan selamat. Nol cookie yang
//! dibaca ulang sama saja dengan store rusak yang gagal dengan sopan.
//!
//! Kenapa sintetis dan bukan cookie asli operator: berkas ini menyeberang ke
//! host kedua, dan cookie asli adalah token sesi yang masih hidup. Latihan
//! transport tidak butuh kredensial nyata, jadi yang dikirim adalah nilai acak
//! yang tidak membuka apa pun di dunia luar.
//!
//! Pemakaian:
//!     cargo run -p store --example seed_drill -- <dir> [jumlah]
//!
//! Menulis `<dir>/store.db` dan `<dir>/store.key`, lalu mencetak jumlah cookie
//! yang benar-benar terbaca kembali dari store setelah ditulis.

use std::path::PathBuf;

use store::{Cookie, ProfileMeta, Store, StoreKey};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().ok_or("pakai: seed_drill <dir> [jumlah]")?);
    let jumlah: usize = match args.next() {
        Some(text) => text.parse()?,
        None => 3,
    };
    std::fs::create_dir_all(&dir)?;

    let key_path = dir.join("store.key");
    let db_path = dir.join("store.db");
    let kunci = StoreKey::create(&key_path)?;
    let store = Store::open(&db_path, kunci)?;

    let profil = "drill";
    store.upsert_profile(
        profil,
        &ProfileMeta {
            canvas_seed: ProfileMeta::seed_dari_nama(profil),
            ..ProfileMeta::default()
        },
        now(),
    )?;

    let mut cookies = Vec::with_capacity(jumlah);
    for i in 0..jumlah {
        cookies.push(Cookie {
            host: "drill.ocb".to_owned(),
            name: format!("drill_{i:02}"),
            path: "/".to_owned(),
            value: token(&format!("{profil}-{i}")),
            // Satu jam: cukup untuk menyeberang host, cukup pendek untuk tidak
            // menumpuk kalau direktori drill lupa dibersihkan.
            expires: Some(now() + 3600),
            secure: true,
            http_only: false,
            samesite: Some(1),
        });
    }
    let laporan = store.save_cookies(profil, &cookies, now())?;

    // Dibaca ulang dari store, bukan dipercaya dari apa yang barusan dikirim.
    let terbaca = store.cookies(profil, None, now())?;
    println!(
        "ditulis {} cookie, terbaca kembali {}, dilewati {}",
        laporan.saved,
        terbaca.len(),
        laporan.skipped_too_big + laporan.skipped_invalid
    );
    if terbaca.len() != laporan.saved {
        return Err(format!(
            "store mengembalikan {} cookie padahal {} tersimpan",
            terbaca.len(),
            laporan.saved
        )
        .into());
    }
    println!("db  = {}", db_path.display());
    println!("key = {}", key_path.display());
    Ok(())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Nilai acak yang jelas bukan kredensial: prefiks `drill-` plus hex dari
/// campuran benih dan waktu, cukup unik untuk membuktikan nilai tidak tertukar.
fn token(benih: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in benih.bytes().chain(now().to_le_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("drill-{hash:016x}")
}
