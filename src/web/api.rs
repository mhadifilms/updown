use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use tracing::info;

use super::db::Database;

/// Shared application state
pub struct AppState {
    pub db: Database,
    pub storage_dir: PathBuf,
    pub data_port: u16,
    pub host: String,
}

// --- Request/Response types ---

#[derive(Deserialize)]
pub struct CreatePackageReq {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateShareReq {
    pub package_id: String,
    pub max_downloads: Option<i64>,
    pub expires_hours: Option<i64>,
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
}

#[derive(Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub ok: bool,
    pub data: T,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub ok: bool,
    pub error: String,
}

#[derive(Serialize)]
pub struct ShareLinkResponse {
    pub code: String,
    pub url: String,
    pub package_id: String,
}

#[derive(Serialize)]
pub struct UploadResponse {
    pub package_id: String,
    pub files: Vec<String>,
    pub total_size: i64,
}

#[derive(Serialize, Deserialize)]
pub struct AgentTransferReq {
    pub action: String, // "download" or "upload"
    pub package_id: String,
    pub files: Vec<FileInfo>,
    pub server_host: String,
    pub server_port: u16,
    pub session_key: String,
}

#[derive(Serialize, Deserialize)]
pub struct FileInfo {
    pub name: String,
    pub size: i64,
    pub path: String,
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub transfers_completed: i64,
    pub uptime_seconds: u64,
}

fn ok_json<T: Serialize>(data: T) -> Json<ApiResponse<T>> {
    Json(ApiResponse { ok: true, data })
}

fn err_json(msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            ok: false,
            error: msg.to_string(),
        }),
    )
}

/// Build the API router
pub fn api_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/transfers", get(list_transfers))
        .route("/api/packages", get(list_packages))
        .route("/api/packages", post(create_package))
        .route("/api/packages/{id}", get(get_package))
        .route("/api/upload", post(upload_files))
        .route("/api/share", post(create_share_link))
        .route("/api/share/{code}", get(get_share_info))
        .route("/api/agent/transfer", post(trigger_agent_transfer))
        .with_state(state)
}

// --- Handlers ---

async fn health(State(state): State<Arc<AppState>>) -> Json<ApiResponse<HealthResponse>> {
    let transfers = state.db.list_transfers(10000).unwrap_or_default();
    let completed = transfers.iter().filter(|t| t.status == "completed").count() as i64;
    ok_json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        transfers_completed: completed,
        uptime_seconds: 0,
    })
}

async fn list_transfers(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Json<ApiResponse<Vec<super::db::Transfer>>> {
    let limit = q.limit.unwrap_or(50);
    let transfers = state.db.list_transfers(limit).unwrap_or_default();
    ok_json(transfers)
}

async fn list_packages(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Json<ApiResponse<Vec<super::db::Package>>> {
    let limit = q.limit.unwrap_or(50);
    let packages = state.db.list_packages(limit).unwrap_or_default();
    ok_json(packages)
}

async fn create_package(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreatePackageReq>,
) -> Json<ApiResponse<super::db::Package>> {
    let desc = req.description.unwrap_or_default();
    let id = state
        .db
        .create_package(&req.name, &desc, &[], 0, "api")
        .unwrap();
    let pkg = state.db.get_package(&id).unwrap().unwrap();
    ok_json(pkg)
}

async fn get_package(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<super::db::Package>>, (StatusCode, Json<ErrorResponse>)> {
    match state.db.get_package(&id).unwrap() {
        Some(pkg) => Ok(ok_json(pkg)),
        None => Err(err_json("Package not found")),
    }
}

async fn upload_files(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<ApiResponse<UploadResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let mut files: Vec<String> = Vec::new();
    let mut total_size: i64 = 0;

    // Create package directory
    let pkg_id = uuid::Uuid::new_v4().to_string();
    let pkg_dir = state.storage_dir.join(&pkg_id);
    tokio::fs::create_dir_all(&pkg_dir).await.ok();

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field
            .file_name()
            .unwrap_or("unknown")
            .to_string();
        let data = field.bytes().await.map_err(|_| err_json("Failed to read upload"))?;

        let file_path = pkg_dir.join(&name);
        tokio::fs::write(&file_path, &data)
            .await
            .map_err(|_| err_json("Failed to save file"))?;

        total_size += data.len() as i64;
        files.push(name);

        info!("Uploaded: {} ({} bytes)", files.last().unwrap(), data.len());
    }

    // Create package record
    state
        .db
        .create_package(&pkg_id, "", &files, total_size, "upload")
        .unwrap();

    // Record transfer
    for f in &files {
        state
            .db
            .create_transfer(f, total_size, "upload", "web")
            .unwrap();
    }

    Ok(ok_json(UploadResponse {
        package_id: pkg_id,
        files,
        total_size,
    }))
}

async fn create_share_link(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateShareReq>,
) -> Result<Json<ApiResponse<ShareLinkResponse>>, (StatusCode, Json<ErrorResponse>)> {
    // Verify package exists
    if state.db.get_package(&req.package_id).unwrap().is_none() {
        return Err(err_json("Package not found"));
    }

    let expires = req.expires_hours.map(|h| {
        (Utc::now() + chrono::Duration::hours(h)).to_rfc3339()
    });
    use chrono::Utc;

    let code = state
        .db
        .create_share_link(
            &req.package_id,
            "api",
            req.max_downloads,
            expires.as_deref(),
        )
        .unwrap();

    let url = format!("http://{}/d/{}", state.host, code);

    Ok(ok_json(ShareLinkResponse {
        code: code.clone(),
        url,
        package_id: req.package_id,
    }))
}

async fn get_share_info(
    State(state): State<Arc<AppState>>,
    Path(code): Path<String>,
) -> Result<Json<ApiResponse<super::db::ShareLink>>, (StatusCode, Json<ErrorResponse>)> {
    match state.db.get_share_link(&code).unwrap() {
        Some(link) => Ok(ok_json(link)),
        None => Err(err_json("Share link not found")),
    }
}

/// Trigger a fast transfer via the desktop agent.
/// The web UI calls this, which returns connection info for the agent to use.
async fn trigger_agent_transfer(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<AgentTransferReq>,
) -> Json<ApiResponse<AgentTransferReq>> {
    info!(
        "Agent transfer triggered: {} package {} ({} files)",
        req.action,
        req.package_id,
        req.files.len()
    );
    // Return the transfer request — the web UI passes this to the local agent
    ok_json(req)
}
