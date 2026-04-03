pub mod db;
pub mod api;
pub mod portal;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::routing::get;
use axum::Router;
use tower_http::cors::CorsLayer;
use tracing::info;

use self::api::AppState;
use self::db::Database;

/// Start the updown web server — the full Faspex replacement.
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
    });

    let app = Router::new()
        // Web portal
        .route("/", get(|| async { portal::portal_html() }))
        // Share link download pages
        .route("/d/{code}", get(|| async { portal::download_page_html() }))
        // Drop box (same as upload portal)
        .route("/dropbox", get(|| async { portal::portal_html() }))
        // API routes
        .merge(api::api_router(state))
        // CORS for agent bridge
        .layer(CorsLayer::permissive());

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
