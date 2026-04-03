use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// SQLite database for transfer history, packages, users, and share links.
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transfer {
    pub id: String,
    pub filename: String,
    pub file_size: i64,
    pub bytes_transferred: i64,
    pub packets: i64,
    pub rate_mbps: f64,
    pub duration_ms: i64,
    pub blake3_hash: String,
    pub status: String, // "pending", "active", "completed", "failed"
    pub direction: String, // "upload" or "download"
    pub peer: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub description: String,
    pub files: Vec<String>, // JSON array of filenames
    pub total_size: i64,
    pub share_link: Option<String>,
    pub created_by: String,
    pub created_at: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub name: String,
    pub email: String,
    pub api_key: String,
    pub role: String, // "admin" or "user"
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareLink {
    pub id: String,
    pub code: String, // Short URL-safe code
    pub package_id: String,
    pub created_by: String,
    pub download_count: i64,
    pub max_downloads: Option<i64>,
    pub expires_at: Option<String>,
    pub created_at: String,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.init_tables()?;
        Ok(db)
    }

    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.init_tables()?;
        Ok(db)
    }

    fn init_tables(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS transfers (
                id TEXT PRIMARY KEY,
                filename TEXT NOT NULL,
                file_size INTEGER NOT NULL,
                bytes_transferred INTEGER DEFAULT 0,
                packets INTEGER DEFAULT 0,
                rate_mbps REAL DEFAULT 0,
                duration_ms INTEGER DEFAULT 0,
                blake3_hash TEXT DEFAULT '',
                status TEXT DEFAULT 'pending',
                direction TEXT NOT NULL,
                peer TEXT DEFAULT '',
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS packages (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT DEFAULT '',
                files TEXT DEFAULT '[]',
                total_size INTEGER DEFAULT 0,
                share_link TEXT,
                created_by TEXT DEFAULT '',
                created_at TEXT NOT NULL,
                expires_at TEXT
            );

            CREATE TABLE IF NOT EXISTS users (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                email TEXT DEFAULT '',
                api_key TEXT UNIQUE NOT NULL,
                role TEXT DEFAULT 'user',
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS share_links (
                id TEXT PRIMARY KEY,
                code TEXT UNIQUE NOT NULL,
                package_id TEXT NOT NULL,
                created_by TEXT DEFAULT '',
                download_count INTEGER DEFAULT 0,
                max_downloads INTEGER,
                expires_at TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (package_id) REFERENCES packages(id)
            );

            CREATE INDEX IF NOT EXISTS idx_share_code ON share_links(code);
            CREATE INDEX IF NOT EXISTS idx_transfers_status ON transfers(status);
            CREATE INDEX IF NOT EXISTS idx_users_api_key ON users(api_key);
            ",
        )?;
        Ok(())
    }

    // --- Transfers ---

    pub fn create_transfer(&self, filename: &str, file_size: i64, direction: &str, peer: &str) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO transfers (id, filename, file_size, direction, peer, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, filename, file_size, direction, peer, now],
        )?;
        Ok(id)
    }

    pub fn complete_transfer(&self, id: &str, bytes: i64, packets: i64, rate: f64, duration_ms: i64, hash: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE transfers SET status='completed', bytes_transferred=?2, packets=?3, rate_mbps=?4, duration_ms=?5, blake3_hash=?6 WHERE id=?1",
            params![id, bytes, packets, rate, duration_ms, hash],
        )?;
        Ok(())
    }

    pub fn list_transfers(&self, limit: i64) -> Result<Vec<Transfer>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, filename, file_size, bytes_transferred, packets, rate_mbps, duration_ms, blake3_hash, status, direction, peer, created_at FROM transfers ORDER BY created_at DESC LIMIT ?1"
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(Transfer {
                id: row.get(0)?,
                filename: row.get(1)?,
                file_size: row.get(2)?,
                bytes_transferred: row.get(3)?,
                packets: row.get(4)?,
                rate_mbps: row.get(5)?,
                duration_ms: row.get(6)?,
                blake3_hash: row.get(7)?,
                status: row.get(8)?,
                direction: row.get(9)?,
                peer: row.get(10)?,
                created_at: row.get(11)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // --- Packages ---

    pub fn create_package(&self, name: &str, description: &str, files: &[String], total_size: i64, created_by: &str) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let files_json = serde_json::to_string(files)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO packages (id, name, description, files, total_size, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, name, description, files_json, total_size, created_by, now],
        )?;
        Ok(id)
    }

    pub fn get_package(&self, id: &str) -> Result<Option<Package>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, description, files, total_size, share_link, created_by, created_at, expires_at FROM packages WHERE id=?1"
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            let files_json: String = row.get(3)?;
            Ok(Package {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                files: serde_json::from_str(&files_json).unwrap_or_default(),
                total_size: row.get(4)?,
                share_link: row.get(5)?,
                created_by: row.get(6)?,
                created_at: row.get(7)?,
                expires_at: row.get(8)?,
            })
        })?;
        Ok(rows.next().and_then(|r| r.ok()))
    }

    pub fn list_packages(&self, limit: i64) -> Result<Vec<Package>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, description, files, total_size, share_link, created_by, created_at, expires_at FROM packages ORDER BY created_at DESC LIMIT ?1"
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            let files_json: String = row.get(3)?;
            Ok(Package {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                files: serde_json::from_str(&files_json).unwrap_or_default(),
                total_size: row.get(4)?,
                share_link: row.get(5)?,
                created_by: row.get(6)?,
                created_at: row.get(7)?,
                expires_at: row.get(8)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // --- Users ---

    pub fn create_user(&self, name: &str, email: &str, role: &str) -> Result<(String, String)> {
        let id = Uuid::new_v4().to_string();
        let api_key = format!("upd_{}", Uuid::new_v4().to_string().replace('-', ""));
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO users (id, name, email, api_key, role, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, name, email, api_key, role, now],
        )?;
        Ok((id, api_key))
    }

    pub fn get_user_by_api_key(&self, api_key: &str) -> Result<Option<User>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, email, api_key, role, created_at FROM users WHERE api_key=?1"
        )?;
        let mut rows = stmt.query_map(params![api_key], |row| {
            Ok(User {
                id: row.get(0)?,
                name: row.get(1)?,
                email: row.get(2)?,
                api_key: row.get(3)?,
                role: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        Ok(rows.next().and_then(|r| r.ok()))
    }

    // --- Share Links ---

    pub fn create_share_link(&self, package_id: &str, created_by: &str, max_downloads: Option<i64>, expires_at: Option<&str>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let code = Uuid::new_v4().to_string()[..8].to_string(); // Short 8-char code
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO share_links (id, code, package_id, created_by, max_downloads, expires_at, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, code, package_id, created_by, max_downloads, expires_at, now],
        )?;
        Ok(code)
    }

    pub fn get_share_link(&self, code: &str) -> Result<Option<ShareLink>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, code, package_id, created_by, download_count, max_downloads, expires_at, created_at FROM share_links WHERE code=?1"
        )?;
        let mut rows = stmt.query_map(params![code], |row| {
            Ok(ShareLink {
                id: row.get(0)?,
                code: row.get(1)?,
                package_id: row.get(2)?,
                created_by: row.get(3)?,
                download_count: row.get(4)?,
                max_downloads: row.get(5)?,
                expires_at: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        Ok(rows.next().and_then(|r| r.ok()))
    }

    pub fn increment_download_count(&self, code: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE share_links SET download_count = download_count + 1 WHERE code=?1",
            params![code],
        )?;
        Ok(())
    }
}
