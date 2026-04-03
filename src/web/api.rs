use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Multipart, Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::db::Database;

/// Maximum upload size: 10 GiB
const MAX_UPLOAD_SIZE: i64 = 10 * 1024 * 1024 * 1024;

/// Maximum individual file size in a single upload: 5 GiB
const MAX_FILE_SIZE: usize = 5 * 1024 * 1024 * 1024;

/// Maximum filename length
const MAX_FILENAME_LEN: usize = 255;

/// Rate limit: max requests per window
const RATE_LIMIT_UPLOAD_MAX: u32 = 30;
const RATE_LIMIT_SHARE_MAX: u32 = 60;
/// Rate limit window in seconds
const RATE_LIMIT_WINDOW_SECS: u64 = 60;

/// Shared application state
pub struct AppState {
    pub db: Database,
    pub storage_dir: PathBuf,
    pub data_port: u16,
    pub host: String,
    pub rate_limiter: Mutex<RateLimiter>,
}

/// Simple in-memory rate limiter keyed by IP/API-key
pub struct RateLimiter {
    /// Maps (action, key) -> (count, window_start)
    buckets: HashMap<(String, String), (u32, Instant)>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            buckets: HashMap::new(),
        }
    }

    /// Check if an action is allowed. Returns true if allowed, false if rate limited.
    pub fn check(&mut self, action: &str, key: &str, max: u32) -> bool {
        let bucket_key = (action.to_string(), key.to_string());
        let now = Instant::now();

        let entry = self.buckets.entry(bucket_key).or_insert((0, now));

        // Reset window if expired
        if now.duration_since(entry.1).as_secs() >= RATE_LIMIT_WINDOW_SECS {
            entry.0 = 0;
            entry.1 = now;
        }

        if entry.0 >= max {
            return false;
        }

        entry.0 += 1;
        true
    }

    /// Periodic cleanup of expired entries (call occasionally)
    pub fn cleanup(&mut self) {
        let now = Instant::now();
        self.buckets.retain(|_, (_, start)| {
            now.duration_since(*start).as_secs() < RATE_LIMIT_WINDOW_SECS * 2
        });
    }
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

fn err_status(status: StatusCode, msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            ok: false,
            error: msg.to_string(),
        }),
    )
}

/// Sanitize a filename to prevent directory traversal and other attacks.
/// Returns None if the filename is invalid/dangerous.
fn sanitize_filename(name: &str) -> Option<String> {
    // Reject empty filenames
    if name.is_empty() {
        return None;
    }

    // Get just the filename component (strip any directory path)
    let name = match name.rsplit_once('/') {
        Some((_, file)) => file,
        None => name,
    };
    let name = match name.rsplit_once('\\') {
        Some((_, file)) => file,
        None => name,
    };

    // Reject empty after stripping path
    if name.is_empty() {
        return None;
    }

    // Reject path traversal components
    if name == "." || name == ".." || name.contains("../") || name.contains("..\\") {
        return None;
    }

    // Reject hidden files (starting with .)
    if name.starts_with('.') {
        return None;
    }

    // Reject names that are too long
    if name.len() > MAX_FILENAME_LEN {
        return None;
    }

    // Reject null bytes and other control characters
    if name.bytes().any(|b| b < 0x20 || b == 0x7F) {
        return None;
    }

    // Reject absolute paths (Windows drive letters)
    if name.len() >= 2 && name.as_bytes()[1] == b':' {
        return None;
    }

    Some(name.to_string())
}

/// API key authentication middleware.
/// Extracts the Bearer token from the Authorization header and validates it.
async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let api_key = match auth_header {
        Some(h) if h.starts_with("Bearer ") => &h[7..],
        _ => {
            warn!("API request without valid Authorization header");
            return Err(err_status(
                StatusCode::UNAUTHORIZED,
                "Missing or invalid Authorization header. Use: Authorization: Bearer upd_xxx",
            ));
        }
    };

    match state.db.validate_api_key(api_key) {
        Ok(Some(_user)) => Ok(next.run(req).await),
        Ok(None) => {
            warn!("Invalid API key attempted");
            Err(err_status(StatusCode::UNAUTHORIZED, "Invalid API key"))
        }
        Err(_) => Err(err_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Authentication error",
        )),
    }
}

/// Build the API router
pub fn api_router(state: Arc<AppState>) -> Router {
    // Public routes (no auth required)
    let public_routes = Router::new()
        .route("/api/health", get(health))
        .route("/api/share/{code}", get(get_share_info))
        .with_state(state.clone());

    // Protected routes (require API key)
    let protected_routes = Router::new()
        .route("/api/transfers", get(list_transfers))
        .route("/api/packages", get(list_packages))
        .route("/api/packages", post(create_package))
        .route("/api/packages/{id}", get(get_package))
        .route("/api/upload", post(upload_files))
        .route("/api/share", post(create_share_link))
        .route("/api/agent/transfer", post(trigger_agent_transfer))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state);

    public_routes.merge(protected_routes)
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
    // Clamp limit to prevent excessive queries
    let limit = q.limit.unwrap_or(50).min(1000).max(1);
    let transfers = state.db.list_transfers(limit).unwrap_or_default();
    ok_json(transfers)
}

async fn list_packages(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Json<ApiResponse<Vec<super::db::Package>>> {
    // Clamp limit to prevent excessive queries
    let limit = q.limit.unwrap_or(50).min(1000).max(1);
    let packages = state.db.list_packages(limit).unwrap_or_default();
    ok_json(packages)
}

async fn create_package(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreatePackageReq>,
) -> Result<Json<ApiResponse<super::db::Package>>, (StatusCode, Json<ErrorResponse>)> {
    // Validate input lengths
    if req.name.len() > 500 {
        return Err(err_json("Package name too long (max 500 chars)"));
    }
    if req.description.as_deref().unwrap_or("").len() > 5000 {
        return Err(err_json("Description too long (max 5000 chars)"));
    }

    let desc = req.description.unwrap_or_default();
    let id = state
        .db
        .create_package(&req.name, &desc, &[], 0, "api")
        .map_err(|_| err_json("Failed to create package"))?;
    let pkg = state
        .db
        .get_package(&id)
        .map_err(|_| err_json("Failed to read package"))?
        .ok_or_else(|| err_json("Package not found after creation"))?;
    Ok(ok_json(pkg))
}

async fn get_package(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<super::db::Package>>, (StatusCode, Json<ErrorResponse>)> {
    // Validate that the ID looks like a UUID to prevent injection in logs
    if id.len() > 50 || id.chars().any(|c| !c.is_ascii_alphanumeric() && c != '-') {
        return Err(err_json("Invalid package ID format"));
    }
    match state.db.get_package(&id) {
        Ok(Some(pkg)) => Ok(ok_json(pkg)),
        Ok(None) => Err(err_json("Package not found")),
        Err(_) => Err(err_json("Database error")),
    }
}

async fn upload_files(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<ApiResponse<UploadResponse>>, (StatusCode, Json<ErrorResponse>)> {
    // Rate limit uploads
    {
        let mut limiter = state.rate_limiter.lock().await;
        if !limiter.check("upload", "global", RATE_LIMIT_UPLOAD_MAX) {
            return Err(err_status(
                StatusCode::TOO_MANY_REQUESTS,
                "Upload rate limit exceeded. Try again later.",
            ));
        }
    }

    let mut files: Vec<String> = Vec::new();
    let mut total_size: i64 = 0;

    // Create package directory
    let pkg_id = uuid::Uuid::new_v4().to_string();
    let pkg_dir = state.storage_dir.join(&pkg_id);

    // Verify the package dir resolves inside the storage dir (defense in depth)
    let canonical_storage = state.storage_dir.canonicalize().unwrap_or_else(|_| state.storage_dir.clone());
    tokio::fs::create_dir_all(&pkg_dir).await.ok();
    let canonical_pkg = pkg_dir.canonicalize().unwrap_or_else(|_| pkg_dir.clone());
    if !canonical_pkg.starts_with(&canonical_storage) {
        return Err(err_json("Invalid storage path"));
    }

    while let Ok(Some(field)) = multipart.next_field().await {
        let raw_name = field
            .file_name()
            .unwrap_or("unknown")
            .to_string();

        // Sanitize the filename
        let name = match sanitize_filename(&raw_name) {
            Some(n) => n,
            None => {
                warn!("Rejected unsafe filename: {:?}", raw_name);
                return Err(err_json("Invalid filename. Filenames must not contain path separators or special characters."));
            }
        };

        let data = field.bytes().await.map_err(|_| err_json("Failed to read upload"))?;

        // Check individual file size
        if data.len() > MAX_FILE_SIZE {
            return Err(err_json("File too large. Maximum file size is 5 GiB."));
        }

        // Check total upload size
        total_size += data.len() as i64;
        if total_size > MAX_UPLOAD_SIZE {
            return Err(err_json("Total upload size exceeds 10 GiB limit."));
        }

        let file_path = pkg_dir.join(&name);

        // Double-check the resolved path is inside the package directory
        // (defense-in-depth against any sanitization bypass)
        if let Ok(canonical_file) = file_path.canonicalize() {
            if !canonical_file.starts_with(&canonical_pkg) {
                warn!("Path traversal attempt blocked: {:?}", file_path);
                return Err(err_json("Invalid filename"));
            }
        }
        // For new files, canonicalize won't work yet, so check the parent
        if let Some(parent) = file_path.parent() {
            let canonical_parent = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
            if !canonical_parent.starts_with(&canonical_pkg) {
                warn!("Path traversal attempt blocked: {:?}", file_path);
                return Err(err_json("Invalid filename"));
            }
        }

        tokio::fs::write(&file_path, &data)
            .await
            .map_err(|_| err_json("Failed to save file"))?;

        files.push(name.clone());

        info!("Uploaded: {} ({} bytes)", name, data.len());
    }

    if files.is_empty() {
        return Err(err_json("No files uploaded"));
    }

    // Create package record
    state
        .db
        .create_package(&pkg_id, "", &files, total_size, "upload")
        .map_err(|_| err_json("Failed to create package record"))?;

    // Record transfer
    for f in &files {
        state
            .db
            .create_transfer(f, total_size, "upload", "web")
            .ok(); // Non-critical, don't fail the upload
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
    // Rate limit share link creation
    {
        let mut limiter = state.rate_limiter.lock().await;
        if !limiter.check("share", "global", RATE_LIMIT_SHARE_MAX) {
            return Err(err_status(
                StatusCode::TOO_MANY_REQUESTS,
                "Share link creation rate limit exceeded. Try again later.",
            ));
        }
    }

    // Validate package_id format
    if req.package_id.len() > 50 || req.package_id.chars().any(|c| !c.is_ascii_alphanumeric() && c != '-') {
        return Err(err_json("Invalid package ID format"));
    }

    // Validate expires_hours is reasonable (max 1 year)
    if let Some(h) = req.expires_hours {
        if h <= 0 || h > 8760 {
            return Err(err_json("expires_hours must be between 1 and 8760 (1 year)"));
        }
    }

    // Validate max_downloads is positive
    if let Some(m) = req.max_downloads {
        if m <= 0 {
            return Err(err_json("max_downloads must be positive"));
        }
    }

    // Verify package exists
    if state.db.get_package(&req.package_id).unwrap_or(None).is_none() {
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
        .map_err(|_| err_json("Failed to create share link"))?;

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
    // Validate code format (should be URL-safe base64, 24 chars)
    if code.len() > 50 || code.chars().any(|c| !c.is_ascii_alphanumeric() && c != '-' && c != '_') {
        return Err(err_json("Invalid share code format"));
    }

    match state.db.get_share_link(&code) {
        Ok(Some(link)) => {
            // Check if link has expired
            if let Some(ref expires) = link.expires_at {
                if let Ok(exp) = chrono::DateTime::parse_from_rfc3339(expires) {
                    if exp < chrono::Utc::now() {
                        return Err(err_json("Share link has expired"));
                    }
                }
            }
            // Check if download limit exceeded
            if let Some(max) = link.max_downloads {
                if link.download_count >= max {
                    return Err(err_json("Share link download limit reached"));
                }
            }
            Ok(ok_json(link))
        }
        Ok(None) => Err(err_json("Share link not found")),
        Err(_) => Err(err_json("Database error")),
    }
}

/// Trigger a fast transfer via the desktop agent.
/// The web UI calls this, which returns connection info for the agent to use.
async fn trigger_agent_transfer(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<AgentTransferReq>,
) -> Result<Json<ApiResponse<AgentTransferReq>>, (StatusCode, Json<ErrorResponse>)> {
    // Validate action
    if req.action != "download" && req.action != "upload" {
        return Err(err_json("action must be 'download' or 'upload'"));
    }

    // Validate package_id format
    if req.package_id.len() > 50 || req.package_id.chars().any(|c| !c.is_ascii_alphanumeric() && c != '-') {
        return Err(err_json("Invalid package ID format"));
    }

    // Validate file names to prevent injection
    for f in &req.files {
        if sanitize_filename(&f.name).is_none() {
            return Err(err_json("Invalid filename in file list"));
        }
    }

    info!(
        "Agent transfer triggered: {} package {} ({} files)",
        req.action,
        req.package_id,
        req.files.len()
    );
    // Return the transfer request -- the web UI passes this to the local agent
    Ok(ok_json(req))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_filename_normal() {
        assert_eq!(sanitize_filename("hello.txt"), Some("hello.txt".to_string()));
        assert_eq!(sanitize_filename("my file (1).pdf"), Some("my file (1).pdf".to_string()));
    }

    #[test]
    fn test_sanitize_filename_traversal() {
        // Path components are stripped, so ../../../etc/passwd -> passwd (safe)
        assert_eq!(sanitize_filename("../../../etc/passwd"), Some("passwd".to_string()));
        // Windows traversal: strips to "system32" (safe)
        assert_eq!(sanitize_filename("..\\..\\windows\\system32"), Some("system32".to_string()));
        // Bare traversal components are rejected
        assert_eq!(sanitize_filename(".."), None);
        assert_eq!(sanitize_filename("."), None);
        // Pure traversal with trailing slash
        assert_eq!(sanitize_filename("../"), None);
        assert_eq!(sanitize_filename("../../"), None);
    }

    #[test]
    fn test_sanitize_filename_strips_path() {
        assert_eq!(sanitize_filename("/etc/passwd"), Some("passwd".to_string()));
        assert_eq!(sanitize_filename("C:\\Windows\\file.txt"), Some("file.txt".to_string()));
        assert_eq!(sanitize_filename("path/to/file.txt"), Some("file.txt".to_string()));
    }

    #[test]
    fn test_sanitize_filename_hidden() {
        assert_eq!(sanitize_filename(".htaccess"), None);
        assert_eq!(sanitize_filename(".env"), None);
    }

    #[test]
    fn test_sanitize_filename_empty() {
        assert_eq!(sanitize_filename(""), None);
        assert_eq!(sanitize_filename("/"), None);
    }

    #[test]
    fn test_sanitize_filename_null_bytes() {
        assert_eq!(sanitize_filename("file\0.txt"), None);
        assert_eq!(sanitize_filename("file\x01.txt"), None);
    }

    #[test]
    fn test_sanitize_filename_too_long() {
        let long_name = "a".repeat(256);
        assert_eq!(sanitize_filename(&long_name), None);
        let ok_name = "a".repeat(255);
        assert!(sanitize_filename(&ok_name).is_some());
    }

    #[test]
    fn test_rate_limiter() {
        let mut rl = RateLimiter::new();
        // Should allow up to max
        for _ in 0..5 {
            assert!(rl.check("test", "key1", 5));
        }
        // Should reject after max
        assert!(!rl.check("test", "key1", 5));
        // Different key should still be allowed
        assert!(rl.check("test", "key2", 5));
    }
}
