//! Router HTTP untuk kontrol daemon.
//!
//! Semua endpoint di bawah `/api/v1` dan semuanya butuh token, termasuk yang
//! hanya membaca. Handoff berisi URL dan path screenshot sesi owner; port ini
//! hidup di loopback, tapi "loopback" di mesin yang dipakai bersama berarti
//! akun lain, dan akun lain itu bukan owner.
//!
//! Router ini sengaja tidak menyentuh browser. Dia menerima jawaban, lalu
//! registry yang menyerahkannya ke antrean daemon. Konsekuensinya jelas: satu
//! test bisa memanggil seluruh HTTP tanpa Chrome, dan jalur CDP hanya ada di
//! satu tempat.

use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::auth::{self, Token};
use crate::registry::{Answer, Registry, RegistryError};

/// Fungsi probe yang disuntikkan pemanggil.
///
/// Didefinisikan di sini, bukan di crate `browserd`, karena router tidak boleh
/// tahu cara menghubungi mesin. Yang penting hanya bentuknya: satu target id
/// masuk, satu temuan DOM keluar. `None` berarti daemon ini memang tidak
/// menyediakan probe, dan itu dilaporkan apa adanya, bukan dipalsukan jadi
/// "tidak ada tanda tantangan".
pub type ProbeFn = Arc<
    dyn Fn(
            String,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<policy::challenge::DomFinding, String>>
                    + Send,
            >,
        > + Send
        + Sync,
>;

/// Keadaan yang dibagi semua handler.
pub struct ControlState {
    pub registry: Arc<Registry>,
    pub token: Token,
    pub started_at: u64,
    pub version: &'static str,
    pub probe: Option<ProbeFn>,
}

/// Galat yang keluar sebagai JSON, bukan halaman HTML.
#[derive(Debug)]
pub enum ApiError {
    Unauthorized(auth::AuthError),
    NotFound { id: String },
    Conflict { id: String, state: &'static str },
    Busy { pending: usize, limit: usize },
    BadRequest(String),
}

impl From<RegistryError> for ApiError {
    fn from(err: RegistryError) -> Self {
        match err {
            RegistryError::NotFound { id } => Self::NotFound { id },
            RegistryError::NotOpen { state, .. } => Self::Conflict {
                id: String::new(),
                state: state.as_str(),
            },
            RegistryError::Busy { pending, limit } => Self::Busy { pending, limit },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, pesan) = match self {
            Self::Unauthorized(err) => {
                // Alasan penolakan tidak dirinci ke klien: "token salah" dan
                // "header kurang" sama-sama membuka jalan tebak-menebak.
                let _ = err;
                (StatusCode::UNAUTHORIZED, "butuh token".to_owned())
            }
            Self::NotFound { id } => (StatusCode::NOT_FOUND, format!("handoff {id} tidak ada")),
            Self::Conflict { id, state } => (
                StatusCode::CONFLICT,
                if id.is_empty() {
                    format!("handoff sudah {state}")
                } else {
                    format!("handoff {id} sudah {state}")
                },
            ),
            Self::Busy { pending, limit } => (
                StatusCode::TOO_MANY_REQUESTS,
                format!("{pending} handoff menunggu, batas {limit}"),
            ),
            Self::BadRequest(detail) => (StatusCode::BAD_REQUEST, detail),
        };
        let body = Json(ErrorBody {
            error: pesan,
            status: status.as_u16(),
        });
        let mut response = (status, body).into_response();
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
        response
    }
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
    status: u16,
}

#[derive(Debug, Serialize)]
struct HealthBody {
    status: &'static str,
    version: &'static str,
    uptime_secs: u64,
    pending: usize,
}

#[derive(Debug, Serialize)]
struct PendingBody {
    handoffs: Vec<crate::registry::View>,
}

#[derive(Debug, Deserialize)]
struct AnswerBody {
    #[serde(flatten)]
    answer: Answer,
}

#[derive(Debug, Deserialize)]
struct ProbeBody {
    target_id: String,
}

#[derive(Debug, Serialize)]
struct ProbeHasil {
    finding: policy::challenge::DomFinding,
    /// `true` kalau ada satu pun tanda tantangan terlihat di DOM.
    ///
    /// Sengaja bukan keputusan penuh: bobot dan ambang tinggal di crate
    /// policy, dan menjawab "tantangan" dari satu probe tanpa bukti status
    /// akan membuat alarm palsu pada halaman normal.
    detected: bool,
}

/// Membangun router lengkap.
pub fn router(state: Arc<ControlState>) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/handoff/pending", get(pending))
        .route("/api/v1/handoff/{id}/answer", post(answer))
        .route("/api/v1/challenge/probe", post(challenge_probe))
        .with_state(state)
}

fn otorisasi(headers: &HeaderMap, state: &ControlState) -> Result<(), ApiError> {
    let raw = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let token = auth::parse_bearer(raw).map_err(ApiError::Unauthorized)?;
    if state.token.matches(token) {
        Ok(())
    } else {
        Err(ApiError::Unauthorized(auth::AuthError::Rejected))
    }
}

fn sekarang() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

async fn challenge_probe(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    body: Result<Json<ProbeBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<ProbeHasil>, ApiError> {
    otorisasi(&headers, &state)?;
    let Json(body) = body.map_err(|err| ApiError::BadRequest(err.body_text()))?;
    let Some(probe) = state.probe.clone() else {
        return Err(ApiError::BadRequest(
            "daemon ini tidak menyediakan probe tantangan".to_owned(),
        ));
    };
    let finding = probe(body.target_id).await.map_err(ApiError::BadRequest)?;
    Ok(Json(ProbeHasil {
        detected: finding != policy::challenge::DomFinding::default(),
        finding,
    }))
}

async fn health(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
) -> Result<Json<HealthBody>, ApiError> {
    otorisasi(&headers, &state)?;
    let now = sekarang();
    Ok(Json(HealthBody {
        status: "ok",
        version: state.version,
        uptime_secs: now.saturating_sub(state.started_at),
        pending: state.registry.pending(now).len(),
    }))
}

async fn pending(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
) -> Result<Json<PendingBody>, ApiError> {
    otorisasi(&headers, &state)?;
    Ok(Json(PendingBody {
        handoffs: state.registry.pending(sekarang()),
    }))
}

async fn answer(
    State(state): State<Arc<ControlState>>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    body: Result<Json<AnswerBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<crate::registry::Ack>, ApiError> {
    otorisasi(&headers, &state)?;
    // Body yang tidak sesuai skema harus gagal di sini, bukan diterapkan
    // setengah. Pesan galat serde tidak memuat body-nya, jadi aman dikirim.
    let Json(body) = body.map_err(|err| ApiError::BadRequest(err.body_text()))?;
    let ack = state.registry.answer(&id, body.answer, sekarang())?;
    Ok(Json(ack))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{Kind, NewHandoff, RegistryLimits};
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Mutex;
    use tower::ServiceExt as _;

    const TOKEN: &str = "token-uji";

    fn state() -> (
        Arc<ControlState>,
        Arc<Mutex<Vec<crate::registry::ApplyRequest>>>,
    ) {
        let antrean = Arc::new(Mutex::new(Vec::new()));
        let q = Arc::clone(&antrean);
        let registry = Arc::new(Registry::new(
            RegistryLimits::default(),
            Box::new(move |request| q.lock().unwrap().push(request)),
        ));
        let state = Arc::new(ControlState {
            registry,
            token: Token::new(TOKEN),
            started_at: sekarang(),
            version: "0.0.0-uji",
            probe: None,
        });
        (state, antrean)
    }

    /// State dengan probe palsu yang mengembalikan temuan yang sudah pasti.
    fn state_dengan_probe(
        temuan: policy::challenge::DomFinding,
    ) -> (
        Arc<ControlState>,
        Arc<Mutex<Vec<crate::registry::ApplyRequest>>>,
    ) {
        let (state, antrean) = state();
        let state = Arc::new(ControlState {
            registry: Arc::clone(&state.registry),
            token: Token::new(TOKEN),
            started_at: state.started_at,
            version: state.version,
            probe: Some(Arc::new(move |target_id: String| {
                assert_eq!(target_id, "T1", "target id harus sampai apa adanya");
                Box::pin(async move { Ok(temuan) })
                    as std::pin::Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = Result<policy::challenge::DomFinding, String>,
                                > + Send,
                        >,
                    >
            })),
        });
        (state, antrean)
    }

    fn handoff_baru(state: &ControlState) -> String {
        state
            .registry
            .create(
                sekarang(),
                NewHandoff {
                    session_id: "s-1".to_owned(),
                    target_id: "T-1".to_owned(),
                    kind: Kind::Captcha,
                    url: "https://situs.test/beranda".to_owned(),
                    title: Some("Tantangan".to_owned()),
                    screenshot: None,
                    expected_selector: Some("#selesai".to_owned()),
                    detail: "s2_challenge_url".to_owned(),
                },
            )
            .expect("handoff dibuat")
            .id
    }

    fn minta(method: &str, uri: &str, token: Option<&str>, body: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        }
    }

    async fn kirim(
        state: Arc<ControlState>,
        request: Request<Body>,
    ) -> (StatusCode, serde_json::Value) {
        let response = router(state).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn tanpa_token_ditolak_di_semua_endpoint() {
        let (state, _) = state();
        for (method, uri) in [
            ("GET", "/api/v1/health"),
            ("GET", "/api/v1/handoff/pending"),
            ("POST", "/api/v1/handoff/abc/answer"),
        ] {
            let (status, body) = kirim(Arc::clone(&state), minta(method, uri, None, None)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
            assert_eq!(body["status"], 401);
            assert!(
                body["error"].as_str().unwrap().contains("token"),
                "pesan: {body}"
            );
        }
    }

    #[tokio::test]
    async fn token_salah_ditolak() {
        let (state, _) = state();
        let (status, _) = kirim(
            Arc::clone(&state),
            minta("GET", "/api/v1/health", Some("bukan-token"), None),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn health_menyebut_jumlah_menunggu() {
        let (state, _) = state();
        handoff_baru(&state);

        let (status, body) = kirim(
            Arc::clone(&state),
            minta("GET", "/api/v1/health", Some(TOKEN), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "ok");
        assert_eq!(body["pending"], 1);
    }

    #[tokio::test]
    async fn pending_mengembalikan_handoff_terbuka() {
        let (state, _) = state();
        let id = handoff_baru(&state);

        let (status, body) = kirim(
            Arc::clone(&state),
            minta("GET", "/api/v1/handoff/pending", Some(TOKEN), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["handoffs"][0]["id"], id.as_str());
        assert_eq!(body["handoffs"][0]["kind"], "captcha");
        assert_eq!(body["handoffs"][0]["state"], "pending");
        assert_eq!(body["handoffs"][0]["expected_selector"], "#selesai");
    }

    #[tokio::test]
    async fn jawaban_diterima_dan_masuk_antrean() {
        let (state, antrean) = state();
        let id = handoff_baru(&state);

        let (status, body) = kirim(
            Arc::clone(&state),
            minta(
                "POST",
                &format!("/api/v1/handoff/{id}/answer"),
                Some(TOKEN),
                Some(r#"{"type":"click","x":12,"y":34}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["state"], "applying");

        let antrean = antrean.lock().unwrap();
        assert_eq!(antrean.len(), 1);
        assert_eq!(antrean[0].id, id);
        assert_eq!(antrean[0].answer, Answer::Click { x: 12.0, y: 34.0 });
    }

    #[tokio::test]
    async fn jawaban_kedua_menghasilkan_409() {
        let (state, _) = state();
        let id = handoff_baru(&state);
        let uri = format!("/api/v1/handoff/{id}/answer");

        let (status, _) = kirim(
            Arc::clone(&state),
            minta("POST", &uri, Some(TOKEN), Some(r#"{"type":"abort"}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = kirim(
            Arc::clone(&state),
            minta("POST", &uri, Some(TOKEN), Some(r#"{"type":"abort"}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("aborted"));
    }

    #[tokio::test]
    async fn id_tidak_dikenal_menghasilkan_404() {
        let (state, _) = state();
        let (status, _) = kirim(
            Arc::clone(&state),
            minta(
                "POST",
                "/api/v1/handoff/tidak-ada/answer",
                Some(TOKEN),
                Some(r#"{"type":"abort"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn body_tidak_sesuai_skema_menghasilkan_400() {
        let (state, antrean) = state();
        let id = handoff_baru(&state);
        let (status, body) = kirim(
            Arc::clone(&state),
            minta(
                "POST",
                &format!("/api/v1/handoff/{id}/answer"),
                Some(TOKEN),
                Some(r#"{"type":"drag"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].is_string());
        assert!(
            antrean.lock().unwrap().is_empty(),
            "body rusak tidak boleh masuk antrean"
        );
    }

    #[tokio::test]
    async fn probe_tanpa_dukungan_ditolak_terang_terangan() {
        let (state, _) = state();
        let (status, body) = kirim(
            Arc::clone(&state),
            minta(
                "POST",
                "/api/v1/challenge/probe",
                Some(TOKEN),
                Some(r#"{"target_id":"T1"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"].as_str().unwrap_or_default().contains("probe"),
            "galat harus menyebut probe: {body}"
        );
    }

    #[tokio::test]
    async fn probe_mengembalikan_temuan_dan_penanda() {
        let temuan = policy::challenge::DomFinding {
            challenge_form: true,
            ..policy::challenge::DomFinding::default()
        };
        let (state, _) = state_dengan_probe(temuan);
        let (status, body) = kirim(
            Arc::clone(&state),
            minta(
                "POST",
                "/api/v1/challenge/probe",
                Some(TOKEN),
                Some(r#"{"target_id":"T1"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["detected"], true);
        assert_eq!(body["finding"]["challenge_form"], true);
    }

    #[tokio::test]
    async fn probe_bersih_tidak_menyebut_tantangan() {
        let (state, _) = state_dengan_probe(policy::challenge::DomFinding::default());
        let (status, body) = kirim(
            Arc::clone(&state),
            minta(
                "POST",
                "/api/v1/challenge/probe",
                Some(TOKEN),
                Some(r#"{"target_id":"T1"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["detected"], false);
    }
}
