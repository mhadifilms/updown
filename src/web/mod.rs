pub mod db;
pub mod api;
pub mod portal;
pub mod agent;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::http::{HeaderValue, Method};
use axum::routing::get;
use axum::Router;
use tower_http::cors::CorsLayer;
use tracing::info;

use self::api::{AppState, RateLimiter};
use self::db::Database;

/// Start the updown web server -- the full Faspex replacement.
///
/// This serves:
/// - Web portal at /
/// - REST API at /api/*
/// - Share link downloads at /d/:code
/// - Drop box uploads at /dropbox
/// - Desktop agent bridge at /api/agent/*
pub async fn start_server(
    bind: SocketAddr,
    storage_dir: PathBuf,
    data_port: u16,
) -> Result<()> {
    // Initialize database
    let db_path = storage_dir.join("updown.db");
    tokio::fs::create_dir_all(&storage_dir).await.ok();
    let db = Database::open(&db_path)?;

    // Create default admin user if none exists
    if db.get_user_by_api_key("upd_admin").unwrap().is_none() {
        let (_id, key) = db.create_user("admin", "admin@localhost", "admin")?;
        info!("Created admin user: api_key={}", key);
    }

    let state = Arc::new(AppState {
        db,
        storage_dir: storage_dir.clone(),
        data_port,
        host: bind.to_string(),
        rate_limiter: tokio::sync::Mutex::new(RateLimiter::new()),
    });

    // Restrictive CORS: only allow the server's own origin and the local agent
    let cors = CorsLayer::new()
        .allow_origin([
            format!("http://{}", bind).parse::<HeaderValue>().unwrap_or_else(|_| HeaderValue::from_static("http://localhost:8080")),
            "http://127.0.0.1:19876".parse::<HeaderValue>().unwrap(),
            "http://localhost:19876".parse::<HeaderValue>().unwrap(),
        ])
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ])
        .allow_credentials(false);

    let app = Router::new()
        // Web portal (SPA — all app routes serve the same HTML, client-side routing)
        .route("/", get(|| async { portal::portal_html() }))
        .route("/send", get(|| async { portal::portal_html() }))
        .route("/inbox", get(|| async { portal::portal_html() }))
        .route("/sent", get(|| async { portal::portal_html() }))
        .route("/links", get(|| async { portal::portal_html() }))
        .route("/dropboxes", get(|| async { portal::portal_html() }))
        .route("/history", get(|| async { portal::portal_html() }))
        .route("/admin", get(|| async { portal::portal_html() }))
        // Share link download page (separate clean UI)
        .route("/d/{code}", get(|| async { portal::download_page_html() }))
        // Public drop box submission page (separate clean UI)
        .route("/submit/{id}", get(|| async { portal::submit_page_html() }))
        // API routes
        .merge(api::api_router(state))
        // Locked-down CORS
        .layer(cors);

    info!("=== updown server ===");
    info!("Web portal:  http://{}", bind);
    info!("API:         http://{}/api/health", bind);
    info!("Storage:     {}", storage_dir.display());
    info!("Data port:   {}", data_port);

    println!();
    println!("  updown server running at http://{}", bind);
    println!("  Open in browser to upload/download files");
    println!();

    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
