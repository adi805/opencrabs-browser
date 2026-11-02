//! Pompa multiplex: satu koneksi klien, satu soket per mesin.
//!
//! Bentuknya sengaja bodoh: **satu pipa per mesin**, bukan satu soket gabungan.
//! Klien melihat satu koneksi, tetapi di belakangnya ada soket websocket terpisah
//! ke setiap mesin, karena Chrome tidak punya konsep "beberapa proses dalam satu
//! soket". Yang menyatukan semuanya adalah [`Peta`], yang menerjemahkan
//! `id`/`sessionId`/`targetId` antara ruang klien dan ruang mesin.
//!
//! Konsekuensi yang harus diingat saat membaca kode ini:
//!
//! - **Tidak ada urutan global.** Dua mesin bisa menjawab bersamaan dan balasan
//!   mesin B boleh tiba sebelum balasan mesin A. Protokol CDP tidak menjanjikan
//!   urutan antar-target, jadi ini tidak melanggar apa pun; yang dijaga hanya
//!   urutan per mesin, dan itu otomatis karena tiap mesin punya task sendiri.
//! - **Mesin mati tidak mematikan koneksi.** Sesi milik mesin itu dilepas dan
//!   klien diberi `Target.detachedFromTarget` per sesi, sementara tab di mesin
//!   lain tetap hidup. Koneksi klien baru ditutup kalau semua mesin sudah mati.
//! - **Balasan siaran hanya diteruskan sekali.** `Target.setDiscoverTargets`
//!   disiarkan ke semua mesin, dan klien tidak boleh menerima dua balasan untuk
//!   satu `id` yang sama: itu membuat klien CDP menganggap protokolnya rusak.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::ws::{Message as AxumMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TsMessage;

use crate::engines::EnginePool;
use crate::framing;
use crate::mux::peta::{Balasan, KODE_DITOLAK, KODE_SESI_TAK_DIKENAL, MuxError, Perintah, Peta};
use crate::passthrough::Stats;
use crate::{CdpError, FrameObserver};

/// Cukup dalam untuk menyerap ledakan event saat halaman dimuat, cukup kecil
/// untuk menahan klien yang berhenti membaca sebelum memorinya meledak.
const ANTREAN: usize = 256;

/// Satu siaran yang masih menunggu balasan mesin-mesinnya.
struct Siar {
    /// Berapa balasan yang masih ditunggu.
    sisa: usize,
    /// Balasan pertama sudah diteruskan ke klien.
    sudah: bool,
}

/// Satu potongan `Target.getTargets` yang belum lengkap.
struct Gabung {
    /// Berapa balasan yang masih ditunggu.
    sisa: usize,
    /// `targetInfos` yang sudah terkumpul, urut kedatangan.
    potongan: Vec<Value>,
}

/// Menjalankan satu koneksi klien di atas seluruh mesin di pool.
pub async fn proxy_multiplex(
    client: WebSocket,
    pool: EnginePool,
    observer: Option<Arc<dyn FrameObserver>>,
) -> Result<Stats, CdpError> {
    let (mut klien_tx, mut klien_rx) = client.split();
    let mut peta = Peta::baru(&pool);
    let mut stats = Stats::default();

    // Satu kanal masuk per mesin, plus satu kanal keluar bersama.
    let (ke_klien_tx, mut ke_klien_rx) = mpsc::channel::<(String, TsMessage)>(ANTREAN);
    let (mati_tx, mut mati_rx) = mpsc::channel::<String>(8);

    let mut kanal: BTreeMap<String, mpsc::Sender<TsMessage>> = BTreeMap::new();
    let mut tugas: Vec<JoinHandle<()>> = Vec::new();
    for (id, engine) in pool.engines() {
        let (tx, rx) = mpsc::channel::<TsMessage>(ANTREAN);
        kanal.insert(id.clone(), tx);
        tugas.push(tokio::spawn(pipa_mesin(
            id.clone(),
            engine.web_socket_url().to_owned(),
            rx,
            ke_klien_tx.clone(),
            mati_tx.clone(),
            observer.clone(),
        )));
    }
    drop(ke_klien_tx);
    drop(mati_tx);

    let mut gabung: BTreeMap<i64, Gabung> = BTreeMap::new();
    let mut siar: BTreeMap<i64, Siar> = BTreeMap::new();

    loop {
        // Semua mesin pergi berarti tidak ada lagi yang bisa dilayani. Klien
        // diberi close yang rapi supaya pustaka CDP-nya tidak menunggu selamanya.
        if kanal.is_empty() {
            tracing::debug!("semua mesin sudah pergi, koneksi klien ditutup");
            break;
        }

        tokio::select! {
            masuk = klien_rx.next() => {
                let Some(masuk) = masuk else { break };
                let Ok(pesan) = masuk else {
                    tracing::debug!("klien memutus koneksi");
                    break;
                };
                if matches!(pesan, AxumMessage::Close(_)) {
                    break;
                }
                let terjemah = framing::axum_to_ts(pesan.clone());
                let Some(teks) = framing::text_of(&terjemah) else {
                    // Biner dan ping diteruskan ke mesin bawaan: protokolnya
                    // tidak butuh penerjemahan, tetapi tetap harus sampai.
                    if let Some(tx) = kanal.get(peta.bawaan()) {
                        let _ = tx.send(framing::axum_to_ts(pesan)).await;
                    }
                    continue;
                };
                amati(&observer, teks);
                stats.to_engine += 1;

                // Id dipakai ulang berarti permintaan sebelumnya sudah tidak
                // relevan. Sisa tunggu yang tertinggal akan menelan balasan
                // yang sah, jadi dibersihkan di sini.
                if let Some(id) = id_klien(teks) {
                    gabung.remove(&id);
                    siar.remove(&id);
                }

                match peta.terjemahkan_klien(teks) {
                    Ok(perintah) => {
                        kirim_perintah(&perintah, &kanal, &mut gabung, &mut siar).await;
                    }
                    Err(err) => {
                        // Ditolak di sini, bukan di mesin: mesin tidak tahu
                        // apa-apa soal id klien, jadi pesan ini tidak akan
                        // pernah punya jawaban yang benar kalau diteruskan.
                        let balasan = tolak(teks, &err);
                        tracing::debug!(%err, "pesan klien ditolak");
                        if klien_tx.send(AxumMessage::Text(balasan.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
            keluar = ke_klien_rx.recv() => {
                let Some((mesin, pesan)) = keluar else { break };
                let Some(teks) = framing::text_of(&pesan) else {
                    if let Some(kirim) = framing::ts_to_axum(pesan)
                        && klien_tx.send(kirim).await.is_err()
                    {
                        break;
                    }
                    continue;
                };
                amati(&observer, teks);
                stats.from_engine += 1;

                match peta.terjemahkan_mesin(&mesin, teks) {
                    Balasan::Teruskan(teks) => {
                        if siar_lewat(&teks, &mut siar) {
                            continue;
                        }
                        if klien_tx.send(AxumMessage::Text(teks.into())).await.is_err() {
                            break;
                        }
                    }
                    Balasan::Gabung { klien_id, hasil } => {
                        if let Some(gabungan) = gabung.get_mut(&klien_id) {
                            if let Some(Value::Array(items)) =
                                hasil.get("targetInfos").cloned()
                            {
                                gabungan.potongan.extend(items);
                            }
                            gabungan.sisa = gabungan.sisa.saturating_sub(1);
                            if gabungan.sisa > 0 {
                                continue;
                            }
                        }
                        let Some(gabungan) = gabung.remove(&klien_id) else {
                            continue;
                        };
                        let balasan = json!({
                            "id": klien_id,
                            "result": {"targetInfos": gabungan.potongan},
                        });
                        if klien_tx
                            .send(AxumMessage::Text(balasan.to_string().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Balasan::Telan => {}
                }
            }
            pergi = mati_rx.recv() => {
                let Some(mesin) = pergi else { break };
                kanal.remove(&mesin);
                // Sesi mesin ini dilepas satu per satu, masing-masing dengan
                // targetId-nya, supaya klien tahu persis tab mana yang hilang.
                for pesan in peta.lepas_mesin(&mesin) {
                    if klien_tx.send(AxumMessage::Text(pesan.into())).await.is_err() {
                        break;
                    }
                }
                tracing::warn!(mesin = %mesin, sisa = kanal.len(), "mesin berhenti");
            }
        }
    }

    for tugas in &tugas {
        tugas.abort();
    }
    let _ = klien_tx.send(AxumMessage::Close(None)).await;
    Ok(stats)
}

/// Mengirim satu perintah klien ke mesin yang berhak menerimanya.
async fn kirim_perintah(
    perintah: &Perintah,
    kanal: &BTreeMap<String, mpsc::Sender<TsMessage>>,
    gabung: &mut BTreeMap<i64, Gabung>,
    siar: &mut BTreeMap<i64, Siar>,
) {
    match perintah {
        Perintah::Satu { mesin, payload } => {
            if let Some(tx) = kanal.get(mesin) {
                let _ = tx.send(TsMessage::Text(payload.clone().into())).await;
            }
        }
        Perintah::Gabung { klien_id, pesan } => {
            let hidup: Vec<&(String, String)> = pesan
                .iter()
                .filter(|(mesin, _)| kanal.contains_key(mesin))
                .collect();
            if hidup.is_empty() {
                return;
            }
            // Jumlah yang diharapkan dihitung dari mesin yang benar-benar
            // hidup. Menghitung dari pool membuat satu mesin mati menahan
            // balasan `getTargets` selamanya.
            gabung.insert(
                *klien_id,
                Gabung {
                    sisa: hidup.len(),
                    potongan: Vec::new(),
                },
            );
            for (mesin, payload) in hidup {
                if let Some(tx) = kanal.get(mesin) {
                    let _ = tx.send(TsMessage::Text(payload.clone().into())).await;
                }
            }
        }
        Perintah::Siar { klien_id, pesan } => {
            let hidup = pesan.iter().filter(|(m, _)| kanal.contains_key(m)).count();
            if hidup == 0 {
                return;
            }
            siar.insert(
                *klien_id,
                Siar {
                    sisa: hidup,
                    sudah: false,
                },
            );
            for (mesin, payload) in pesan {
                if let Some(tx) = kanal.get(mesin) {
                    let _ = tx.send(TsMessage::Text(payload.clone().into())).await;
                }
            }
        }
    }
}

/// `true` kalau balasan ini adalah balasan siaran yang bukan pertama.
///
/// Klien mengirim satu `id`, jadi dia hanya boleh menerima satu balasan untuk
/// id itu. Balasan mesin kedua dan seterusnya dibuang di sini, bukan di peta:
/// peta tidak tahu berapa mesin yang hidup saat balasan tiba.
fn siar_lewat(teks: &str, siar: &mut BTreeMap<i64, Siar>) -> bool {
    let Some(id) = id_klien(teks) else {
        return false;
    };
    let (lewat, habis) = match siar.get_mut(&id) {
        Some(tunggu) => {
            let lewat = tunggu.sudah;
            tunggu.sudah = true;
            tunggu.sisa = tunggu.sisa.saturating_sub(1);
            (lewat, tunggu.sisa == 0)
        }
        None => return false,
    };
    if habis {
        siar.remove(&id);
    }
    lewat
}

/// `id` sebuah pesan, kalau ada. Pesan tanpa `id` adalah notifikasi atau event,
/// bukan bagian dari percakapan ber-id.
fn id_klien(teks: &str) -> Option<i64> {
    serde_json::from_str::<Value>(teks)
        .ok()?
        .get("id")
        .and_then(Value::as_i64)
}

/// Membalas pesan klien yang tidak bisa dirutekan.
///
/// Dijawab, bukan didiamkan: klien yang menunggu balasan `id` yang tidak akan
/// pernah datang akan menggantung sampai timeout-nya sendiri, dan pesan
/// timeout itu menutupi penyebab sebenarnya.
fn tolak(payload: &str, err: &MuxError) -> String {
    let id = id_klien(payload);
    let kode = match err {
        MuxError::SesiTakDikenal(_) => KODE_SESI_TAK_DIKENAL,
        _ => KODE_DITOLAK,
    };
    let mut balasan = json!({"error": {"code": kode, "message": err.to_string()}});
    if let (Some(id), Some(obj)) = (id, balasan.as_object_mut()) {
        obj.insert("id".into(), Value::from(id));
    }
    balasan.to_string()
}

fn amati(observer: &Option<Arc<dyn FrameObserver>>, teks: &str) {
    if let Some(observer) = observer.as_ref() {
        // Pengamat menerima teks **asli** dari kedua arah, termasuk id milik
        // mesin. Dia menjelaskan keadaan mesin, bukan keadaan klien, jadi
        // menerjemahkan id untuknya justru menyembunyikan mesin asalnya.
        observer.observe(teks);
    }
}

/// Satu pipa ke satu mesin: sambung, pompa dua arah, lapor kalau putus.
async fn pipa_mesin(
    mesin: String,
    url: String,
    mut dari_klien: mpsc::Receiver<TsMessage>,
    ke_klien: mpsc::Sender<(String, TsMessage)>,
    mati: mpsc::Sender<String>,
    observer: Option<Arc<dyn FrameObserver>>,
) {
    let soket = match connect_async(url.as_str()).await {
        Ok((soket, _)) => soket,
        Err(err) => {
            tracing::warn!(mesin = %mesin, %err, "tidak bisa menyambung ke mesin");
            let _ = mati.send(mesin).await;
            return;
        }
    };
    let (mut keluar, mut masuk) = soket.split();

    let penulis = tokio::spawn(async move {
        while let Some(pesan) = dari_klien.recv().await {
            if keluar.send(pesan).await.is_err() {
                break;
            }
        }
        let _ = keluar.send(TsMessage::Close(None)).await;
    });

    while let Some(pesan) = masuk.next().await {
        let Ok(pesan) = pesan else { break };
        if let Some(teks) = framing::text_of(&pesan) {
            amati(&observer, teks);
        }
        if ke_klien.send((mesin.clone(), pesan)).await.is_err() {
            break;
        }
    }

    penulis.abort();
    let _ = mati.send(mesin).await;
}
