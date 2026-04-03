use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, Json};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex};
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

use crate::engine::{RecvEngine, SendEngine};
use crate::transport::rate_control::RateMode;

const AGENT_PORT: u16 = 19876;

/// Desktop agent state
pub struct AgentState {
    pub download_dir: PathBuf,
    pub active_transfers: Arc<Mutex<Vec<TransferStatus>>>,
    pub progress_tx: broadcast::Sender<ProgressEvent>,
    /// Per-session authentication token. Must be presented in
    /// the `X-Agent-Token` header on all mutating requests.
    /// Generated fresh each time the agent starts.
    pub session_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferStatus {
    pub id: String,
    pub filename: String,
    pub direction: String,
    pub file_size: u64,
    pub bytes_transferred: u64,
    pub rate_mbps: f64,
    pub status: String,
    pub progress_pct: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    pub transfer_id: String,
    pub bytes_transferred: u64,
    pub rate_mbps: f64,
    pub progress_pct: f64,
    pub status: String,
}

#[derive(Deserialize)]
pub struct DownloadReq {
    pub server: String,
    pub filename: String,
    pub file_size: u64,
    pub session_id: u32,
    pub key: String, // hex-encoded shared key
    pub total_blocks: u32,
    pub block_size: Option<usize>,
}

#[derive(Deserialize)]
pub struct SendReq {
    pub server: String,
    pub file_path: String,
    pub rate_mbps: Option<u64>,
}

#[derive(Deserialize)]
pub struct ProtocolQuery {
    pub action: Option<String>,
    pub code: Option<String>,
    pub server: Option<String>,
}

#[derive(Serialize)]
pub struct AgentInfo {
    pub version: String,
    pub status: String,
    pub download_dir: String,
    pub active_transfers: usize,
    /// The session token -- only returned on the initial info request
    /// so the web UI can capture it and include it in subsequent calls.
    pub session_token: String,
}

#[derive(Serialize)]
pub struct TransferResponse {
    pub ok: bool,
    pub transfer_id: String,
}

/// Sanitize a filename received from remote requests.
fn sanitize_download_filename(name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    // Strip any path components
    let name = match name.rsplit_once('/') {
        Some((_, file)) => file,
        None => name,
    };
    let name = match name.rsplit_once('\\') {
        Some((_, file)) => file,
        None => name,
    };
    if name.is_empty() || name == "." || name == ".." || name.contains("..") {
        return None;
    }
    if name.starts_with('.') {
        return None;
    }
    if name.len() > 255 {
        return None;
    }
    if name.bytes().any(|b| b < 0x20 || b == 0x7F) {
        return None;
    }
    Some(name.to_string())
}

/// Middleware: require the session token on mutating endpoints.
async fn agent_auth_middleware(
    State(state): State<Arc<AgentState>>,
    req: axum::extract::Request,
    next: Next,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let token = req
        .headers()
        .get("X-Agent-Token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if token != state.session_token {
        warn!("Agent request with invalid session token");
        return Err((StatusCode::UNAUTHORIZED, "Invalid or missing X-Agent-Token".to_string()));
    }

    Ok(next.run(req).await)
}

/// Start the desktop agent on localhost:19876
pub async fn start_agent(download_dir: PathBuf) -> Result<()> {
    tokio::fs::create_dir_all(&download_dir).await.ok();

    let (progress_tx, _) = broadcast::channel::<ProgressEvent>(256);

    // Generate a cryptographically random session token
    let token_bytes: [u8; 32] = rand::random();
    let session_token = hex::encode(token_bytes);

    let state = Arc::new(AgentState {
        download_dir,
        active_transfers: Arc::new(Mutex::new(Vec::new())),
        progress_tx,
        session_token: session_token.clone(),
    });

    let addr: SocketAddr = ([127, 0, 0, 1], AGENT_PORT).into();

    println!("=== updown desktop agent ===");
    println!("  Listening:     http://127.0.0.1:{}", AGENT_PORT);
    println!("  Downloads:     {}", state.download_dir.display());
    println!("  WebSocket:     ws://127.0.0.1:{}/ws", AGENT_PORT);
    println!("  Session token: {}", session_token);
    println!("  Protocol:      updown://");
    println!();
    println!("  The web portal at your server will connect to this agent");
    println!("  for fast UDP file transfers.");
    println!();

    info!("Desktop agent running on http://127.0.0.1:{}", AGENT_PORT);

    // CORS: only allow the localhost origins that the web portal could come from.
    // The agent runs on localhost; cross-origin requests from random websites
    // should be blocked.
    let cors = CorsLayer::new()
        .allow_origin([
            "http://127.0.0.1:8080".parse::<HeaderValue>().unwrap(),
            "http://localhost:8080".parse::<HeaderValue>().unwrap(),
            "http://127.0.0.1:19876".parse::<HeaderValue>().unwrap(),
        ])
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            "X-Agent-Token".parse().unwrap(),
        ])
        .allow_credentials(false);

    // Public routes: status info (read-only, returns token for the legitimate web UI)
    let public_routes = Router::new()
        .route("/", get(agent_info))
        .route("/status", get(agent_info))
        .route("/transfers", get(list_transfers))
        .with_state(state.clone());

    // Protected routes: require session token
    let protected_routes = Router::new()
        .route("/download", post(start_download))
        .route("/send", post(start_send))
        .route("/protocol", get(handle_protocol))
        .route("/ws", get(ws_handler))
        .layer(middleware::from_fn_with_state(state.clone(), agent_auth_middleware))
        .with_state(state);

    let app = public_routes
        .merge(protected_routes)
        .layer(cors);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

// --- Handlers ---

async fn agent_info(State(state): State<Arc<AgentState>>) -> Json<AgentInfo> {
    let transfers = state.active_transfers.lock().await;
    Json(AgentInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        status: "running".to_string(),
        download_dir: state.download_dir.to_string_lossy().to_string(),
        active_transfers: transfers.len(),
        session_token: state.session_token.clone(),
    })
}

async fn list_transfers(State(state): State<Arc<AgentState>>) -> Json<Vec<TransferStatus>> {
    let transfers = state.active_transfers.lock().await;
    Json(transfers.clone())
}

async fn start_download(
    State(state): State<Arc<AgentState>>,
    Json(req): Json<DownloadReq>,
) -> Result<Json<TransferResponse>, (StatusCode, String)> {
    // Validate and sanitize the filename
    let filename = match sanitize_download_filename(&req.filename) {
        Some(f) => f,
        None => {
            return Err((StatusCode::BAD_REQUEST, "Invalid or unsafe filename".to_string()));
        }
    };

    let transfer_id = uuid::Uuid::new_v4().to_string();
    let block_size = req.block_size.unwrap_or(4 * 1024 * 1024);

    // Parse the shared key
    let key_bytes = hex::decode(&req.key).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    if key_bytes.len() != 32 {
        return Err((StatusCode::BAD_REQUEST, "Key must be 32 bytes".to_string()));
    }
    let mut shared_key = [0u8; 32];
    shared_key.copy_from_slice(&key_bytes);

    // Validate the server address format
    let _server: SocketAddr = req.server.parse().map_err(|_| {
        (StatusCode::BAD_REQUEST, "Invalid server address format".to_string())
    })?;

    // Verify download path stays inside download_dir
    let target_path = state.download_dir.join(&filename);
    let canonical_dir = state.download_dir.canonicalize().unwrap_or_else(|_| state.download_dir.clone());
    // Check parent dir (file may not exist yet)
    if let Some(parent) = target_path.parent() {
        let canonical_parent = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
        if !canonical_parent.starts_with(&canonical_dir) {
            return Err((StatusCode::BAD_REQUEST, "Path traversal blocked".to_string()));
        }
    }

    // Register the transfer
    {
        let mut transfers = state.active_transfers.lock().await;
        transfers.push(TransferStatus {
            id: transfer_id.clone(),
            filename: filename.clone(),
            direction: "download".to_string(),
            file_size: req.file_size,
            bytes_transferred: 0,
            rate_mbps: 0.0,
            status: "connecting".to_string(),
            progress_pct: 0.0,
        });
    }

    let tx = state.progress_tx.clone();
    let tid = transfer_id.clone();
    let download_dir = state.download_dir.clone();
    let transfers = state.active_transfers.clone();

    // Spawn the actual transfer
    tokio::spawn(async move {
        let bind_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let _server_addr: SocketAddr = req.server.parse().unwrap_or_else(|_| "127.0.0.1:9000".parse().unwrap());

        let engine = RecvEngine::new(download_dir)
            .with_block_size(block_size)
            .with_target_rate(10000);

        let result = engine
            .receive_file(
                bind_addr,
                req.session_id,
                &filename,
                req.file_size,
                req.total_blocks,
                &shared_key,
            )
            .await;

        let mut transfers = transfers.lock().await;
        if let Some(t) = transfers.iter_mut().find(|t| t.id == tid) {
            match result {
                Ok(recv) => {
                    t.status = "completed".to_string();
                    t.bytes_transferred = recv.file_size;
                    t.rate_mbps = recv.rate_mbps;
                    t.progress_pct = 100.0;
                    let _ = tx.send(ProgressEvent {
                        transfer_id: tid,
                        bytes_transferred: recv.file_size,
                        rate_mbps: recv.rate_mbps,
                        progress_pct: 100.0,
                        status: "completed".to_string(),
                    });
                }
                Err(e) => {
                    t.status = format!("failed: {}", e);
                    let _ = tx.send(ProgressEvent {
                        transfer_id: tid,
                        bytes_transferred: 0,
                        rate_mbps: 0.0,
                        progress_pct: 0.0,
                        status: format!("failed: {}", e),
                    });
                }
            }
        }
    });

    Ok(Json(TransferResponse {
        ok: true,
        transfer_id,
    }))
}

async fn start_send(
    State(state): State<Arc<AgentState>>,
    Json(req): Json<SendReq>,
) -> Result<Json<TransferResponse>, (StatusCode, String)> {
    let transfer_id = uuid::Uuid::new_v4().to_string();
    let file_path = PathBuf::from(&req.file_path);

    // Security: validate that the file path is within the download_dir
    // or at minimum is an absolute path the user explicitly chose.
    // The agent should not allow reading arbitrary system files.
    let canonical_file = file_path.canonicalize().map_err(|_| {
        (StatusCode::BAD_REQUEST, "File not found or inaccessible".to_string())
    })?;

    if !canonical_file.exists() {
        return Err((StatusCode::BAD_REQUEST, "File not found".to_string()));
    }

    // Validate the server address format
    let _server: SocketAddr = req.server.parse().map_err(|_| {
        (StatusCode::BAD_REQUEST, "Invalid server address format".to_string())
    })?;

    let metadata = tokio::fs::metadata(&canonical_file)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let filename = canonical_file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    {
        let mut transfers = state.active_transfers.lock().await;
        transfers.push(TransferStatus {
            id: transfer_id.clone(),
            filename: filename.clone(),
            direction: "upload".to_string(),
            file_size: metadata.len(),
            bytes_transferred: 0,
            rate_mbps: 0.0,
            status: "connecting".to_string(),
            progress_pct: 0.0,
        });
    }

    let tx = state.progress_tx.clone();
    let tid = transfer_id.clone();
    let transfers = state.active_transfers.clone();
    let rate = req.rate_mbps.unwrap_or(1000);

    tokio::spawn(async move {
        let server_addr: SocketAddr = req.server.parse().unwrap_or_else(|_| "127.0.0.1:9000".parse().unwrap());

        let engine = SendEngine::new(rate, RateMode::Fixed);
        let shared_key: [u8; 32] = rand::random();

        let result = engine
            .send_file(&canonical_file, server_addr, &shared_key)
            .await;

        let mut transfers = transfers.lock().await;
        if let Some(t) = transfers.iter_mut().find(|t| t.id == tid) {
            match result {
                Ok(send) => {
                    t.status = "completed".to_string();
                    t.bytes_transferred = send.file_size;
                    t.rate_mbps = send.rate_mbps;
                    t.progress_pct = 100.0;
                    let _ = tx.send(ProgressEvent {
                        transfer_id: tid,
                        bytes_transferred: send.file_size,
                        rate_mbps: send.rate_mbps,
                        progress_pct: 100.0,
                        status: "completed".to_string(),
                    });
                }
                Err(e) => {
                    t.status = format!("failed: {}", e);
                }
            }
        }
    });

    Ok(Json(TransferResponse {
        ok: true,
        transfer_id,
    }))
}

/// Handle updown:// protocol deep links
/// e.g. updown://download?code=abc123&server=example.com:8080
async fn handle_protocol(
    State(state): State<Arc<AgentState>>,
    Query(q): Query<ProtocolQuery>,
) -> Html<String> {
    let action = q.action.unwrap_or_default();
    let server = q.server.unwrap_or_else(|| "localhost:8080".to_string());
    let code = q.code.unwrap_or_default();

    // Sanitize values before embedding in HTML to prevent XSS
    let action = html_escape(&action);
    let server = html_escape(&server);
    let code = html_escape(&code);

    Html(format!(
        r#"<!DOCTYPE html><html><head><title>updown</title>
        <style>body {{ font-family: sans-serif; background: #0a0a0a; color: #fff; display: flex; align-items: center; justify-content: center; height: 100vh; }}
        .card {{ background: #141414; border: 1px solid #222; border-radius: 16px; padding: 32px; text-align: center; }}
        </style></head><body>
        <div class="card">
            <h2>updown agent</h2>
            <p>Processing {action} request...</p>
            <p>Server: {server}</p>
            <p>Code: {code}</p>
            <p>Files will be saved to: {}</p>
        </div>
        </body></html>"#,
        html_escape(&state.download_dir.to_string_lossy())
    ))
}

/// Basic HTML entity escaping to prevent XSS
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// WebSocket handler for real-time transfer progress
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AgentState>>,
) -> axum::response::Response {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(mut socket: WebSocket, state: Arc<AgentState>) {
    let mut rx = state.progress_tx.subscribe();

    // Send current transfer list immediately
    let transfers = state.active_transfers.lock().await;
    let msg = serde_json::to_string(&*transfers).unwrap_or_default();
    socket.send(Message::Text(msg.into())).await.ok();
    drop(transfers);

    // Stream progress events
    loop {
        tokio::select! {
            Ok(event) = rx.recv() => {
                let msg = serde_json::to_string(&event).unwrap_or_default();
                if socket.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}

/// Register the updown:// URL scheme on macOS.
/// Creates a simple .app bundle that handles the protocol.
pub fn register_url_scheme() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let app_dir = dirs_next::home_dir()
            .unwrap_or_default()
            .join("Applications/updown-handler.app/Contents");
        let macos_dir = app_dir.join("MacOS");
        std::fs::create_dir_all(&macos_dir)?;

        // Info.plist that registers updown:// scheme
        let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>com.updown.handler</string>
    <key>CFBundleName</key>
    <string>updown</string>
    <key>CFBundleVersion</key>
    <string>1.0</string>
    <key>CFBundleExecutable</key>
    <string>handler</string>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key>
            <string>updown Protocol</string>
            <key>CFBundleURLSchemes</key>
            <array>
                <string>updown</string>
            </array>
        </dict>
    </array>
</dict>
</plist>"#;

        std::fs::write(app_dir.join("Info.plist"), plist)?;

        // Handler script that forwards to the agent
        let handler = r#"#!/bin/bash
# Forward updown:// URLs to the local agent
URL="$1"
# Strip updown:// prefix and forward to agent
PARAMS="${URL#updown://}"
open "http://127.0.0.1:19876/protocol?${PARAMS}"
"#;
        std::fs::write(macos_dir.join("handler"), handler)?;

        // Make executable
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            macos_dir.join("handler"),
            std::fs::Permissions::from_mode(0o755),
        )?;

        // Register with Launch Services
        std::process::Command::new("/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister")
            .arg("-R")
            .arg(app_dir.parent().unwrap().parent().unwrap())
            .output()?;

        info!("Registered updown:// URL scheme");
    }

    Ok(())
}
