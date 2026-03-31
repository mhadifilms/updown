use std::path::{Path, PathBuf};

use anyhow::Result;
use tokio::fs;

use crate::protocol::FileEntry;

/// Walk a directory and collect all files with their relative paths.
pub async fn walk_directory(root: &Path) -> Result<Vec<(PathBuf, FileEntry)>> {
    let mut entries = Vec::new();
    let root = root.canonicalize()?;
    walk_recursive(&root, &root, &mut entries).await?;
    // Sort by path for deterministic ordering
    entries.sort_by(|a, b| a.1.relative_path.cmp(&b.1.relative_path));
    Ok(entries)
}

#[async_recursion::async_recursion]
async fn walk_recursive(
    root: &Path,
    current: &Path,
    entries: &mut Vec<(PathBuf, FileEntry)>,
) -> Result<()> {
    let mut dir = fs::read_dir(current).await?;
    while let Some(entry) = dir.next_entry().await? {
        let path = entry.path();
        let file_type = entry.file_type().await?;

        if file_type.is_dir() {
            walk_recursive(root, &path, entries).await?;
        } else if file_type.is_file() {
            let metadata = entry.metadata().await?;
            let file_size = metadata.len();

            // Skip empty files
            if file_size == 0 {
                continue;
            }

            // Compute relative path
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();

            // Hash the file
            let data = tokio::fs::read(&path).await?;
            let hash = blake3::hash(&data);

            entries.push((
                path.clone(),
                FileEntry {
                    relative_path: relative,
                    file_size,
                    blake3_hash: *hash.as_bytes(),
                },
            ));
        }
    }
    Ok(())
}

/// Walk a single file and return it as a manifest entry
pub async fn single_file_entry(path: &Path) -> Result<(PathBuf, FileEntry)> {
    let metadata = tokio::fs::metadata(path).await?;
    let filename = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // Stream hash for large files
    use tokio::io::AsyncReadExt;
    let mut hasher = blake3::Hasher::new();
    let mut file = fs::File::open(path).await?;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok((
        path.to_path_buf(),
        FileEntry {
            relative_path: filename,
            file_size: metadata.len(),
            blake3_hash: *hasher.finalize().as_bytes(),
        },
    ))
}

/// Format a byte count as human-readable
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}
