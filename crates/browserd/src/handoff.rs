//! Menerapkan jawaban owner ke tab yang sedang menunggu.
//!
//! Bagian yang menentukan jawaban ini benar atau salah bukan CDP-nya, tapi
//! perencanaan gerakannya: klik yang mendarat di luar tombol akan diulang
//! owner, dan setiap pengulangan memakan satu kuota handoff. Karena itu
//! perencanaan jalur mouse dan penyusunan skrip DOM ditulis sebagai fungsi
//! murni yang bisa diuji tanpa browser.
//!
//! Semua perintah halaman dikirim **di dalam sesi target** (`Target.attachToTarget`
//! lalu `sessionId`). Tanpa itu mesin menjawab `not attached to an active
//! target`, dan pesan itu tidak menyebut apa yang sebenarnya kurang.

use anyhow::{Context, Result, bail};
use control::{Answer, ApplyOutcome, ApplyRequest, Registry};
use serde_json::{Value, json};

use crate::cdp_client::Client;

/// Jarak jitter pada jalur mouse, dalam piksel CSS. Di bawah 2 px tidak
/// mengubah apa pun, di atas 5 px berisiko keluar dari tombol kecil.
pub const JITTER_MIN: f64 = 2.0;
pub const JITTER_MAX: f64 = 5.0;

/// Jumlah titik antara mouse-down dan mouse-up. Cukup untuk terlihat seperti
/// gerakan manusia, tidak sampai membanjiri websocket.
const PATH_STEPS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// Titik awal gerakan: di luar elemen, supaya tidak ada klik yang lahir tepat
/// di atas sasaran (pola yang paling mudah dikenali skrip anti-bot).
fn titik_awal(target: Point) -> Point {
    Point {
        x: (target.x - 96.0).max(1.0),
        y: (target.y - 48.0).max(1.0),
    }
}

/// Jalur mouse menuju `target`: kurva kubik dengan jitter deterministik.
///
/// Deterministik penting: jalur yang berubah tiap panggilan tidak bisa diuji,
/// dan jalur yang selalu sama persis justru pola yang dicari detektor. Jitter
/// dihitung dari posisi indeks, jadi tetap acak secara visual tapi stabil saat
/// diuji.
#[must_use]
pub fn mouse_path(target: Point) -> Vec<Point> {
    let awal = titik_awal(target);
    let dx = target.x - awal.x;
    let dy = target.y - awal.y;

    // Dua titik kendali membuat kurva melengkung, bukan garis lurus.
    let kendali1 = Point {
        x: awal.x + dx * 0.35,
        y: awal.y + dy * 0.05,
    };
    let kendali2 = Point {
        x: awal.x + dx * 0.75,
        y: awal.y + dy * 0.9,
    };

    let mut titik = Vec::with_capacity(PATH_STEPS + 1);
    for i in 0..=PATH_STEPS {
        let t = i as f64 / PATH_STEPS as f64;
        let (x, y) = bezier(awal, kendali1, kendali2, target, t);
        // Titik terakhir harus tepat di sasaran: jitter di situ berarti klik
        // yang meleset, dan itu satu handoff terbuang.
        if i == PATH_STEPS {
            titik.push(target);
        } else {
            let (jx, jy) = jitter(i);
            titik.push(Point {
                x: (x + jx).max(0.0),
                y: (y + jy).max(0.0),
            });
        }
    }
    titik
}

fn bezier(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    let a = u * u * u;
    let b = 3.0 * u * u * t;
    let c = 3.0 * u * t * t;
    let d = t * t * t;
    (
        a * p0.x + b * p1.x + c * p2.x + d * p3.x,
        a * p0.y + b * p1.y + c * p2.y + d * p3.y,
    )
}

/// Jitter 2-5 px dengan arah yang berganti, dari indeks titik.
fn jitter(i: usize) -> (f64, f64) {
    let rentang = JITTER_MAX - JITTER_MIN;
    // Deret sederhana: cukup untuk menyebar, tidak perlu generator acak.
    let a = ((i * 37 + 11) % 7) as f64 / 6.0;
    let b = ((i * 53 + 5) % 5) as f64 / 4.0;
    let tanda_a = if i.is_multiple_of(2) { 1.0 } else { -1.0 };
    let tanda_b = if i.is_multiple_of(3) { -1.0 } else { 1.0 };
    (
        tanda_a * (JITTER_MIN + a * rentang),
        tanda_b * (JITTER_MIN + b * rentang),
    )
}

/// Satu event keyboard dalam bentuk yang diterima `Input.dispatchKeyEvent`.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyEvent {
    pub kind: &'static str,
    pub key: String,
    pub code: String,
    pub vk: i64,
    pub text: Option<String>,
}

/// Menerjemahkan nama tombol menjadi pasangan key-down/key-up.
///
/// Tombol yang tidak dikenal tetap dikirim dengan `key` apa adanya: mesin
/// menolak yang tidak ia kenal, dan galat itu lebih jujur daripada diam-diam
/// mengirim `Enter` untuk semua jawaban.
#[must_use]
pub fn key_events(value: &str) -> Vec<KeyEvent> {
    let nama = value.trim();
    let (code, vk, text) = match nama.to_ascii_lowercase().as_str() {
        "enter" | "return" => ("Enter".to_owned(), 13, Some("\r".to_owned())),
        "tab" => ("Tab".to_owned(), 9, None),
        "escape" | "esc" => ("Escape".to_owned(), 27, None),
        "space" | " " => ("Space".to_owned(), 32, Some(" ".to_owned())),
        "backspace" => ("Backspace".to_owned(), 8, None),
        "arrowdown" | "down" => ("ArrowDown".to_owned(), 40, None),
        "arrowup" | "up" => ("ArrowUp".to_owned(), 38, None),
        "arrowleft" | "left" => ("ArrowLeft".to_owned(), 37, None),
        "arrowright" | "right" => ("ArrowRight".to_owned(), 39, None),
        lain => {
            let huruf = lain.to_ascii_uppercase();
            let vk = huruf
                .chars()
                .next()
                .map(|c| i64::from(c as u32))
                .unwrap_or(0);
            (format!("Key{huruf}"), vk, Some(nama.to_owned()))
        }
    };

    let teks = text.clone();
    let mut down = KeyEvent {
        kind: "keyDown",
        key: nama.to_owned(),
        code: code.clone(),
        vk,
        text: teks,
    };
    if nama.len() == 1 {
        down.key = nama.to_owned();
    } else if let Some(kanonik) = kanonik(&code) {
        down.key = kanonik;
    }

    let up = KeyEvent {
        kind: "keyUp",
        key: down.key.clone(),
        code,
        vk,
        text: None,
    };
    vec![down, up]
}

fn kanonik(code: &str) -> Option<String> {
    Some(
        match code {
            "Enter" => "Enter",
            "Tab" => "Tab",
            "Escape" => "Escape",
            "Space" => " ",
            "Backspace" => "Backspace",
            "ArrowDown" => "ArrowDown",
            "ArrowUp" => "ArrowUp",
            "ArrowLeft" => "ArrowLeft",
            "ArrowRight" => "ArrowRight",
            _ => return None,
        }
        .to_owned(),
    )
}

/// Skrip yang mengisi satu field dan memberi tahu framework-nya.
///
/// Nilai dikirim sebagai literal JSON, bukan disisipkan ke dalam string: satu
/// tanda kutip di dalam OTP tidak boleh mengubah skripnya jadi skrip lain.
#[must_use]
pub fn isi_field_script(selector: &str, value: &str) -> String {
    let selector = Value::String(selector.to_owned()).to_string();
    let value = Value::String(value.to_owned()).to_string();
    format!(
        r#"(() => {{
  const el = document.querySelector({selector});
  if (!el) {{ return {{ ok: false, reason: "selector_tidak_ada" }}; }}
  const setter = Object.getOwnPropertyDescriptor(
    el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype,
    "value"
  )?.set;
  if (setter) {{ setter.call(el, {value}); }} else {{ el.value = {value}; }}
  el.dispatchEvent(new Event("input", {{ bubbles: true }}));
  el.dispatchEvent(new Event("change", {{ bubbles: true }}));
  return {{ ok: true }};
}})()"#
    )
}

/// Skrip pemeriksaan: elemen harapan ada setelah jawaban diterapkan.
#[must_use]
pub fn verifikasi_script(selector: &str) -> String {
    let selector = Value::String(selector.to_owned()).to_string();
    format!("document.querySelector({selector}) !== null")
}

/// Menerapkan satu jawaban ke tab. `target_id` diambil dari handoff, bukan
/// dari pemanggil: jawaban yang dipasangkan ke tab lain adalah bug yang tidak
/// boleh mungkin terjadi.
pub async fn apply(client: &mut Client, request: &ApplyRequest) -> Result<ApplyOutcome> {
    if request.answer == Answer::Abort {
        // Tidak ada yang diterapkan: pembatalan sudah dicatat registry.
        return Ok(ApplyOutcome::Failed {
            detail: "dibatalkan owner".to_owned(),
        });
    }

    let session = attach(client, &request.target_id).await?;
    let hasil = match &request.answer {
        Answer::Click { x, y } => klik(client, &session, Point { x: *x, y: *y }).await,
        Answer::Text { value, selector } => {
            ketik(client, &session, selector.as_deref(), value).await
        }
        Answer::Keys { value } => tombol(client, &session, value).await,
        Answer::Abort => unreachable!("abort sudah ditangani di atas"),
    };

    let selesai = match hasil {
        Ok(()) => Ok(ApplyOutcome::Solved),
        Err(err) => Ok(ApplyOutcome::Failed {
            detail: format!("{err:#}"),
        }),
    };
    let _ = detach(client, &session).await;
    selesai
}

/// Melampirkan sesi ke satu target. `flatten: true` membuat semua perintah
/// lanjutan memakai `sessionId` yang sama, tanpa websocket kedua.
async fn attach(client: &mut Client, target_id: &str) -> Result<String> {
    let result = client
        .call(
            "Target.attachToTarget",
            json!({ "targetId": target_id, "flatten": true }),
        )
        .await
        .with_context(|| format!("melampirkan target {target_id}"))?;
    result
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("Target.attachToTarget tidak mengembalikan sessionId")
}

async fn detach(client: &mut Client, session: &str) -> Result<()> {
    client
        .call("Target.detachFromTarget", json!({ "sessionId": session }))
        .await?;
    Ok(())
}

async fn klik(client: &mut Client, session: &str, target: Point) -> Result<()> {
    if target.x <= 0.0 && target.y <= 0.0 {
        bail!(
            "koordinat klik ({}, {}) di luar halaman",
            target.x,
            target.y
        );
    }

    for titik in mouse_path(target) {
        client
            .call_in(
                session,
                "Input.dispatchMouseEvent",
                json!({
                    "type": "mouseMoved",
                    "x": titik.x,
                    "y": titik.y,
                    "button": "none",
                    "buttons": 0,
                }),
            )
            .await
            .context("menggerakkan mouse")?;
    }

    for jenis in ["mousePressed", "mouseReleased"] {
        client
            .call_in(
                session,
                "Input.dispatchMouseEvent",
                json!({
                    "type": jenis,
                    "x": target.x,
                    "y": target.y,
                    "button": "left",
                    "buttons": 1,
                    "clickCount": 1,
                }),
            )
            .await
            .with_context(|| format!("{jenis} di ({}, {})", target.x, target.y))?;
    }
    Ok(())
}

/// Mengisi teks. Dengan selector, nilai ditulis lewat DOM supaya framework
/// yang mengendalikan input ikut tahu; tanpa selector, teks disisipkan ke
/// elemen yang sedang fokus.
async fn ketik(
    client: &mut Client,
    session: &str,
    selector: Option<&str>,
    value: &str,
) -> Result<()> {
    if let Some(selector) = selector {
        let hasil = client
            .call_in(
                session,
                "Runtime.evaluate",
                json!({
                    "expression": isi_field_script(selector, value),
                    "returnByValue": true,
                    "awaitPromise": false,
                }),
            )
            .await
            .context("mengisi field")?;

        let ok = hasil
            .get("result")
            .and_then(|result| result.get("value"))
            .and_then(|value| value.get("ok"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !ok {
            let sebab = hasil
                .get("result")
                .and_then(|result| result.get("value"))
                .and_then(|value| value.get("reason"))
                .and_then(Value::as_str)
                .unwrap_or("tidak diketahui");
            bail!("field {selector} tidak bisa diisi: {sebab}");
        }
        return Ok(());
    }

    client
        .call_in(session, "Input.insertText", json!({ "text": value }))
        .await
        .context("menyisipkan teks")?;
    Ok(())
}

async fn tombol(client: &mut Client, session: &str, value: &str) -> Result<()> {
    for event in key_events(value) {
        let mut params = json!({
            "type": event.kind,
            "key": event.key,
            "code": event.code,
            "windowsVirtualKeyCode": event.vk,
            "nativeVirtualKeyCode": event.vk,
        });
        if let Some(text) = event.text {
            params["text"] = json!(text);
        }
        client
            .call_in(session, "Input.dispatchKeyEvent", params)
            .await
            .with_context(|| format!("mengirim tombol {value}"))?;
    }
    Ok(())
}

/// Hasil satu sapuan: handoff yang baru kedaluwarsa, dan target yang masih
/// ditahan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepReport {
    pub expired: Vec<String>,
    pub masih_ditahan: Vec<String>,
}

/// Menyapu handoff dan melaporkan target yang masih perlu ditahan.
///
/// Dipanggil berkala oleh daemon. Tanpa ini, handoff yang ditinggalkan akan
/// menahan tab selamanya, dan daemon pelan-pelan kehabisan tab tanpa satu pun
/// pesan galat.
#[must_use]
pub fn sweep(registry: &Registry, now: u64) -> SweepReport {
    SweepReport {
        expired: registry.expire(now),
        masih_ditahan: registry.pinned_targets(now),
    }
}

/// Memeriksa apakah elemen harapan sudah muncul. `Ok(false)` berarti halaman
/// belum berubah, dan itu bukan galat: tantangan bisa butuh beberapa detik.
pub async fn terverifikasi(client: &mut Client, target_id: &str, selector: &str) -> Result<bool> {
    let session = attach(client, target_id).await?;
    let hasil = client
        .call_in(
            &session,
            "Runtime.evaluate",
            json!({
                "expression": verifikasi_script(selector),
                "returnByValue": true,
            }),
        )
        .await;
    let _ = detach(client, &session).await;

    let hasil = hasil.with_context(|| format!("memeriksa {selector}"))?;
    Ok(hasil
        .get("result")
        .and_then(|result| result.get("value"))
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jalur_mouse_berakhir_tepat_di_sasaran() {
        let target = Point { x: 842.0, y: 517.0 };
        let jalur = mouse_path(target);
        assert_eq!(jalur.len(), PATH_STEPS + 1);
        assert_eq!(*jalur.last().unwrap(), target, "titik terakhir harus tepat");
    }

    #[test]
    fn jalur_mouse_tidak_lurus_dan_berjitter() {
        let target = Point { x: 400.0, y: 300.0 };
        let jalur = mouse_path(target);

        let awal = jalur[0];
        assert!(
            (awal.x - target.x).abs() > 10.0 || (awal.y - target.y).abs() > 10.0,
            "gerakan harus mulai di luar sasaran"
        );

        // Setiap titik antara harus bergeser dari garis lurus.
        let mut bergeser = 0;
        for (i, titik) in jalur.iter().enumerate().take(PATH_STEPS).skip(1) {
            let t = i as f64 / PATH_STEPS as f64;
            let lurus_x = awal.x + (target.x - awal.x) * t;
            let lurus_y = awal.y + (target.y - awal.y) * t;
            if (titik.x - lurus_x).abs() > 1.0 || (titik.y - lurus_y).abs() > 1.0 {
                bergeser += 1;
            }
        }
        assert!(bergeser >= PATH_STEPS / 2, "jalur terlalu lurus");
    }

    #[test]
    fn jitter_di_dalam_batas_dua_sampai_lima_piksel() {
        for i in 0..32 {
            let (x, y) = jitter(i);
            for nilai in [x.abs(), y.abs()] {
                assert!(
                    (JITTER_MIN..=JITTER_MAX).contains(&nilai),
                    "jitter {nilai} di luar batas pada i={i}"
                );
            }
        }
    }

    #[test]
    fn jalur_deterministik_untuk_sasaran_yang_sama() {
        let target = Point { x: 120.0, y: 90.0 };
        assert_eq!(mouse_path(target), mouse_path(target));
    }

    #[test]
    fn koordinat_negatif_tidak_dihasilkan() {
        let jalur = mouse_path(Point { x: 3.0, y: 2.0 });
        assert!(jalur.iter().all(|titik| titik.x >= 0.0 && titik.y >= 0.0));
    }

    #[test]
    fn enter_menghasilkan_keydown_dan_keyup() {
        let event = key_events("Enter");
        assert_eq!(event.len(), 2);
        assert_eq!(event[0].kind, "keyDown");
        assert_eq!(event[0].key, "Enter");
        assert_eq!(event[0].vk, 13);
        assert_eq!(event[0].text.as_deref(), Some("\r"));
        assert_eq!(event[1].kind, "keyUp");
        assert_eq!(event[1].text, None);
    }

    #[test]
    fn nama_tombol_tidak_peka_huruf_besar() {
        assert_eq!(key_events("enter")[0].code, "Enter");
        assert_eq!(key_events("TAB")[0].vk, 9);
        assert_eq!(key_events("esc")[0].vk, 27);
    }

    #[test]
    fn huruf_tunggal_dikirim_sebagai_teks() {
        let event = key_events("a");
        assert_eq!(event[0].code, "KeyA");
        assert_eq!(event[0].text.as_deref(), Some("a"));
    }

    #[test]
    fn skrip_isi_field_melarikan_kutip() {
        let skrip = isi_field_script("#otp", r#"a") ; alert(1)//"#);
        assert!(skrip.contains(r#""a\") ; alert(1)//""#), "nilai: {skrip}");
        assert!(skrip.contains("dispatchEvent(new Event(\"input\""));
        assert!(skrip.contains("new Event(\"change\""));
    }

    #[test]
    fn skrip_verifikasi_hanya_menanyakan_keberadaan() {
        let skrip = verifikasi_script("#selesai");
        assert_eq!(skrip, "document.querySelector(\"#selesai\") !== null");
    }

    #[test]
    fn sapuan_melaporkan_yang_kedaluwarsa_dan_masih_ditahan() {
        use control::{Kind, NewHandoff, RegistryLimits};

        let registry = Registry::new(RegistryLimits::default(), Box::new(|_| {}));
        let handoff = registry
            .create(
                1_000,
                NewHandoff {
                    session_id: "s-1".to_owned(),
                    target_id: "T-1".to_owned(),
                    kind: Kind::Captcha,
                    url: "https://situs.test/".to_owned(),
                    title: None,
                    screenshot: None,
                    expected_selector: None,
                    detail: "s2_challenge_url".to_owned(),
                },
            )
            .expect("handoff dibuat");

        let awal = sweep(&registry, 1_001);
        assert!(awal.expired.is_empty());
        assert_eq!(awal.masih_ditahan, vec!["T-1".to_owned()]);

        let lewat = sweep(&registry, 1_000 + control::registry::HOLD_SECS + 1);
        assert_eq!(lewat.expired, vec![handoff.id.clone()]);
        assert_eq!(
            lewat.masih_ditahan,
            vec!["T-1".to_owned()],
            "tab masih ditahan sampai menit ke-30"
        );

        let lepas = sweep(&registry, 1_000 + control::registry::RELEASE_SECS);
        assert!(lepas.masih_ditahan.is_empty(), "pin dilepas di menit ke-30");
    }
}
