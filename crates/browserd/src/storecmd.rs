//! Perintah store di `browserd`: kunci, daftar, ekspor, impor, dan bukti restart.
//!
//! Yang dijaga di sini adalah janji M2: cookie yang ditanam hari ini masih ada
//! besok. Karena itu `cookies proof` menjalankan mesin dua kali dengan direktori
//! profil mesin yang **berbeda**, sehingga tidak ada jalur "cookie-nya
//! sebenarnya masih di disk Chromium". Kalau nilainya kembali, yang
//! mengembalikannya adalah store.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use engine::{Engine, LaunchSpec, ShellEngine, discover_shell};
use store::{Cookie, ProfileMeta, Store, StoreKey};

use crate::cdp_client::Client;
use crate::cli::{CookieArgs, KeygenArgs, ProofArgs};

/// Batas menunggu mesin siap saat perintah cookie dipakai.
const READY_MS: u64 = 20_000;

/// Jeda setelah engine dimatikan, supaya port-nya benar-benar lepas sebelum
/// engine kedua menyalakan port baru.
const SETTLE: Duration = Duration::from_millis(500);

pub fn keygen(args: &KeygenArgs) -> Result<()> {
    let kunci = StoreKey::create(&args.key_file).with_context(|| {
        format!(
            "kunci tidak dibuat di {}; kalau berkas itu sudah ada, jangan ditimpa, \
             pakai yang lama atau hapus sadar-sadar",
            args.key_file.display()
        )
    })?;

    let store = Store::open(&args.db, kunci)
        .with_context(|| format!("store tidak bisa dibuka di {}", args.db.display()))?;
    let sweep = store.sweep(now())?;

    println!("kunci   = {}", args.key_file.display());
    println!("store   = {}", args.db.display());
    println!("profil  = {}", store.list_profiles()?.len());
    println!(
        "sweep   = {} sesi, {} captcha_event, {} otp_event, {} engine_run",
        sweep.session, sweep.captcha_event, sweep.otp_event, sweep.engine_run
    );
    Ok(())
}

/// Daftar cookie tanpa nilainya. Nilai tidak pernah dicetak, bahkan ke terminal
/// operator: yang bocor ke scrollback dan log shell itu permanen.
pub fn ls(args: &CookieArgs, host: Option<&str>) -> Result<()> {
    let store = buka_store(&args.db, &args.key_file)?;
    let cookies = store.cookies(&args.profile, host, now())?;

    println!(
        "{} cookie di profil {} (nilai tidak dicetak)",
        cookies.len(),
        args.profile
    );
    for cookie in cookies {
        println!(
            "  {:<28} {:<24} {:<6} {}",
            cookie.host,
            cookie.name,
            cookie.path,
            expired_label(cookie.expires)
        );
    }
    Ok(())
}

/// Mengambil cookie dari mesin yang sedang jalan lalu menyimpannya ke store.
pub fn export(args: &CookieArgs) -> Result<()> {
    let store = buka_store(&args.db, &args.key_file)?;
    siapkan_profil(&store, &args.profile)?;

    let ws = args
        .cdp
        .as_deref()
        .context("--cdp ws://127.0.0.1:<port>/devtools/browser/<id> wajib untuk export")?;

    let cookies = blok(async {
        let mut klien = Client::connect(ws).await?;
        klien.cookies().await
    })?;

    let laporan = store.save_cookies(&args.profile, &cookies, now())?;
    println!(
        "disimpan {} cookie ke profil {} ({} dilewati: terlalu besar, {} tidak sah)",
        laporan.saved, args.profile, laporan.skipped_too_big, laporan.skipped_invalid
    );
    Ok(())
}

/// Menanam cookie dari store ke mesin yang sedang jalan.
pub fn import(args: &CookieArgs) -> Result<()> {
    let store = buka_store(&args.db, &args.key_file)?;
    let cookies = store.cookies(&args.profile, None, now())?;
    if cookies.is_empty() {
        bail!("profil {} tidak punya cookie untuk ditanam", args.profile);
    }

    let ws = args
        .cdp
        .as_deref()
        .context("--cdp ws://127.0.0.1:<port>/devtools/browser/<id> wajib untuk import")?;

    let jumlah = blok(async {
        let mut klien = Client::connect(ws).await?;
        klien.set_cookies(&cookies).await
    })?;

    println!("ditanam {jumlah} cookie ke profil {}", args.profile);
    Ok(())
}

/// Bukti M2: cookie dummy, mesin dimatikan, mesin baru, cookie kembali.
pub fn proof(args: &ProofArgs) -> Result<()> {
    let store = buka_store(&args.umum.db, &args.umum.key_file)?;
    siapkan_profil(&store, &args.umum.profile)?;

    let biner = args
        .engine_binary
        .clone()
        .or_else(discover_shell)
        .context("biner chrome-headless-shell tidak ditemukan; pakai --engine-binary")?;

    let akar = std::env::temp_dir().join(format!("ocb-proof-{}-{}", std::process::id(), now()));
    let profil_a = akar.join("engine-a");
    let profil_b = akar.join("engine-b");

    let nilai = token_acak()?;
    let cookie = Cookie {
        host: "uji.ocb".to_owned(),
        name: "ocb_proof".to_owned(),
        path: "/".to_owned(),
        value: nilai.clone(),
        // Satu jam: cukup lama untuk melewati restart, cukup pendek untuk tidak
        // menumpuk di store operator.
        expires: Some(now() + 3600),
        secure: true,
        http_only: false,
        samesite: Some(1),
    };

    println!("profil   = {}", args.umum.profile);
    println!("cookie   = {}={}", cookie.name, samar(&nilai));
    println!("store    = {}", args.umum.db.display());

    let hasil = jalankan_proof(&store, args, &biner, &profil_a, &profil_b, &cookie);

    if !args.keep_profile {
        crate::reap::remove_profile(&akar);
    }

    let (dibaca, ditanam) = hasil?;
    println!("PASS: {ditanam} cookie ditanam di mesin kedua, {dibaca} terbaca kembali");
    println!("      nilai identik dengan yang disimpan sebelum restart");
    Ok(())
}

/// Inti proof, dipisah supaya pembersihan direktori selalu jalan.
fn jalankan_proof(
    store: &Store,
    args: &ProofArgs,
    biner: &Path,
    profil_a: &Path,
    profil_b: &Path,
    cookie: &Cookie,
) -> Result<(usize, usize)> {
    // --- Mesin pertama: tanam cookie, lalu simpan ke store.
    println!("mesin-1  = menyala");
    let a = ShellEngine::launch(
        LaunchSpec::new(biner.to_path_buf(), profil_a.to_path_buf()),
        READY_MS,
    )
    .context("mesin pertama tidak mau menyala")?;
    let ws_a = a.web_socket_url().to_owned();

    let diamati = blok(async {
        let mut klien = Client::connect(&ws_a).await?;
        klien.set_cookies(std::slice::from_ref(cookie)).await?;
        // Dibaca ulang dari mesin, bukan dipercaya dari apa yang kita kirim.
        klien.cookies().await
    });

    let diamati = match diamati {
        Ok(cookies) => cookies,
        Err(galat) => {
            let _ = a.stop();
            return Err(galat);
        }
    };

    let ada = diamati.iter().find(|c| c.name == cookie.name);
    let Some(ada) = ada else {
        let _ = a.stop();
        bail!(
            "mesin pertama tidak memantulkan cookie {}; yang ada: {:?}",
            cookie.name,
            diamati.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
        );
    };
    if ada.value != cookie.value {
        let _ = a.stop();
        bail!("nilai cookie berubah di mesin pertama");
    }
    println!(
        "         = cookie ditanam dan terbaca kembali ({} cookie di jar)",
        diamati.len()
    );

    let laporan = store.save_cookies(&args.umum.profile, &diamati, now())?;
    println!("         = {} cookie disimpan ke store", laporan.saved);

    // --- Matikan mesin pertama. Direktori profilnya TIDAK dipakai mesin kedua.
    a.stop().context("mesin pertama menolak mati")?;
    std::thread::sleep(SETTLE);
    if a.is_running() {
        bail!("mesin pertama masih hidup setelah stop; proof tidak sah");
    }
    println!("mesin-1  = mati, port lepas");

    // --- Mesin kedua: profil mesin kosong, cookie harus datang dari store.
    println!("mesin-2  = menyala dengan direktori profil baru");
    let b = ShellEngine::launch(
        LaunchSpec::new(biner.to_path_buf(), profil_b.to_path_buf()),
        READY_MS,
    )
    .context("mesin kedua tidak mau menyala")?;
    let ws_b = b.web_socket_url().to_owned();

    let hasil = blok(async {
        let mut klien = Client::connect(&ws_b).await?;
        // Jar mesin kedua harus kosong sebelum ditanam: kalau tidak, proof-nya
        // bisa lulus karena kebetulan, bukan karena store.
        let sebelum = klien.cookies().await?;
        let tersimpan = store.cookies(&args.umum.profile, None, now())?;
        let ditanam = klien.set_cookies(&tersimpan).await?;
        let sesudah = klien.cookies().await?;
        Ok::<_, anyhow::Error>((sebelum.len(), ditanam, sesudah))
    });

    let (sebelum, ditanam, sesudah) = match hasil {
        Ok(nilai) => nilai,
        Err(galat) => {
            let _ = b.stop();
            return Err(galat);
        }
    };
    let _ = b.stop();

    if sebelum != 0 {
        bail!("jar mesin kedua sudah berisi {sebelum} cookie; proof tidak bersih");
    }

    let kembali = sesudah
        .iter()
        .find(|c| c.name == cookie.name)
        .context("cookie tidak kembali setelah restart")?;
    if kembali.value != cookie.value {
        bail!("nilai cookie berbeda setelah restart");
    }

    Ok((sesudah.len(), ditanam))
}

fn buka_store(db: &Path, key_file: &Path) -> Result<Store> {
    Store::open_with_key_file(db, key_file).with_context(|| {
        format!(
            "store tidak bisa dibuka ({} + {}); jalankan `browserd keygen` dulu kalau kuncinya belum ada",
            db.display(),
            key_file.display()
        )
    })
}

/// Profil dibuat kalau belum ada, dengan meta default yang deterministik.
fn siapkan_profil(store: &Store, nama: &str) -> Result<()> {
    if store.profile(nama)?.is_some() {
        return Ok(());
    }
    let meta = ProfileMeta {
        canvas_seed: ProfileMeta::seed_dari_nama(nama),
        ..ProfileMeta::default()
    };
    store.upsert_profile(nama, &meta, now())?;
    println!("profil   = {nama} dibuat (identitas beku, seed canvas dari nama)");
    Ok(())
}

fn blok<F, T>(future: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("runtime tokio tidak bisa dibuat")?;
    runtime.block_on(future)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|durasi| durasi.as_secs() as i64)
        .unwrap_or(0)
}

fn expired_label(expires: Option<i64>) -> String {
    match expires {
        None => "sesi".to_owned(),
        Some(detik) => {
            let sisa = detik - now();
            if sisa <= 0 {
                "kedaluwarsa".to_owned()
            } else {
                format!("{}j", sisa / 3600)
            }
        }
    }
}

/// Nilai acak 16 byte dari `/dev/urandom`, ditulis hex.
///
/// Bukan `rand`: proof hanya butuh nilai yang tidak bisa ditebak, dan satu
/// dependensi acak untuk itu tidak sepadan.
fn token_acak() -> Result<String> {
    let mut buf = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .context("/dev/urandom tidak bisa dibuka")?
        .read_exact(&mut buf)
        .context("/dev/urandom tidak memberi 16 byte")?;
    let mut teks = String::with_capacity(buf.len() * 2);
    for byte in buf {
        teks.push_str(&format!("{byte:02x}"));
    }
    Ok(teks)
}

/// Nilai cookie disamarkan di layar: cukup untuk mencocokkan mata, tidak cukup
/// untuk dipakai ulang.
fn samar(nilai: &str) -> String {
    let depan: String = nilai.chars().take(4).collect();
    format!("{depan}…({} byte)", nilai.len())
}

/// Direktori default, dipakai CLI sebagai nilai `default_value`.
#[must_use]
pub fn default_db() -> PathBuf {
    crate::config::home().join(".opencrabs/browser-daemon/store.db")
}

#[must_use]
pub fn default_key() -> PathBuf {
    crate::config::home().join(".opencrabs/browser-daemon/store.key")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_acak_punya_32_hex_dan_berbeda() {
        let a = token_acak().expect("token");
        let b = token_acak().expect("token");
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "dua token harus berbeda");
    }

    #[test]
    fn nilai_disamarkan_tanpa_membocorkan_isi() {
        let teks = samar("0123456789abcdef");
        assert!(teks.starts_with("0123"));
        assert!(!teks.contains("456789"));
        assert!(teks.contains("16 byte"));
    }

    #[test]
    fn label_kedaluwarsa_membedakan_sesi_dan_kedaluwarsa() {
        assert_eq!(expired_label(None), "sesi");
        assert_eq!(expired_label(Some(now() - 10)), "kedaluwarsa");
        assert_eq!(expired_label(Some(now() + 7200)), "2j");
    }
}
