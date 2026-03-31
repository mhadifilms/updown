use std::path::Path;

use anyhow::Result;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// Compute BLAKE3 hashes for each block of a file.
/// Returns a vector of block hashes, one per block.
/// Used by the receiver to tell the sender which blocks have changed.
pub async fn compute_block_hashes(
    file_path: &Path,
    block_size: usize,
) -> Result<Vec<[u8; 32]>> {
    let metadata = tokio::fs::metadata(file_path).await?;
    let file_size = metadata.len();
    let total_blocks = ((file_size as usize + block_size - 1) / block_size) as u32;

    let mut hashes = Vec::with_capacity(total_blocks as usize);
    let mut file = File::open(file_path).await?;
    let mut buf = vec![0u8; block_size];

    for block_id in 0..total_blocks {
        let offset = block_id as u64 * block_size as u64;
        let remaining = (file_size - offset) as usize;
        let this_block = remaining.min(block_size);

        file.seek(std::io::SeekFrom::Start(offset)).await?;
        file.read_exact(&mut buf[..this_block]).await?;

        let hash = blake3::hash(&buf[..this_block]);
        hashes.push(*hash.as_bytes());
    }

    Ok(hashes)
}

/// Compare source and destination block hashes to determine which blocks differ.
/// Returns the list of block IDs that need to be re-sent.
pub fn diff_block_hashes(
    source_hashes: &[[u8; 32]],
    dest_hashes: &[[u8; 32]],
) -> Vec<u32> {
    let mut changed = Vec::new();

    for (i, src_hash) in source_hashes.iter().enumerate() {
        if i >= dest_hashes.len() {
            // Destination has fewer blocks — all remaining are new
            changed.push(i as u32);
        } else if src_hash != &dest_hashes[i] {
            changed.push(i as u32);
        }
    }

    changed
}

/// Statistics for a delta sync operation
#[derive(Debug)]
pub struct DeltaSyncStats {
    pub total_blocks: u32,
    pub changed_blocks: u32,
    pub unchanged_blocks: u32,
    pub savings_percent: f64,
}

impl DeltaSyncStats {
    pub fn from_diff(total_blocks: u32, changed: &[u32]) -> Self {
        let changed_blocks = changed.len() as u32;
        let unchanged_blocks = total_blocks - changed_blocks;
        let savings_percent = if total_blocks > 0 {
            (unchanged_blocks as f64 / total_blocks as f64) * 100.0
        } else {
            0.0
        };
        Self {
            total_blocks,
            changed_blocks,
            unchanged_blocks,
            savings_percent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_identical() {
        let a = vec![[1u8; 32], [2u8; 32], [3u8; 32]];
        let b = vec![[1u8; 32], [2u8; 32], [3u8; 32]];
        let diff = diff_block_hashes(&a, &b);
        assert!(diff.is_empty());
    }

    #[test]
    fn test_diff_some_changed() {
        let a = vec![[1u8; 32], [2u8; 32], [3u8; 32]];
        let b = vec![[1u8; 32], [99u8; 32], [3u8; 32]];
        let diff = diff_block_hashes(&a, &b);
        assert_eq!(diff, vec![1]);
    }

    #[test]
    fn test_diff_source_longer() {
        let a = vec![[1u8; 32], [2u8; 32], [3u8; 32]];
        let b = vec![[1u8; 32]];
        let diff = diff_block_hashes(&a, &b);
        assert_eq!(diff, vec![1, 2]); // blocks 1 and 2 are new
    }

    #[test]
    fn test_delta_stats() {
        let diff = vec![1, 5, 9];
        let stats = DeltaSyncStats::from_diff(10, &diff);
        assert_eq!(stats.total_blocks, 10);
        assert_eq!(stats.changed_blocks, 3);
        assert_eq!(stats.unchanged_blocks, 7);
        assert!((stats.savings_percent - 70.0).abs() < 0.1);
    }
}
