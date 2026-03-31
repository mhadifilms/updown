use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::fs;

/// Block-level resume state. Persisted alongside the output file as
/// `<filename>.updown-resume`. Tracks which blocks have been received
/// and verified, allowing interrupted transfers to resume without
/// re-transferring completed blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeState {
    /// Session ID this resume state belongs to
    pub session_id: u32,
    /// Expected file size
    pub file_size: u64,
    /// Block size used for the transfer
    pub block_size: usize,
    /// Total number of blocks
    pub total_blocks: u32,
    /// Bitfield: true = block received and verified
    pub completed_blocks: Vec<bool>,
    /// BLAKE3 hash of each completed block (for verification on resume)
    pub block_hashes: Vec<Option<[u8; 32]>>,
}

impl ResumeState {
    /// Create a new empty resume state
    pub fn new(session_id: u32, file_size: u64, block_size: usize, total_blocks: u32) -> Self {
        Self {
            session_id,
            file_size,
            block_size,
            total_blocks,
            completed_blocks: vec![false; total_blocks as usize],
            block_hashes: vec![None; total_blocks as usize],
        }
    }

    /// Mark a block as completed with its hash
    pub fn mark_complete(&mut self, block_id: u32, hash: [u8; 32]) {
        if (block_id as usize) < self.completed_blocks.len() {
            self.completed_blocks[block_id as usize] = true;
            self.block_hashes[block_id as usize] = Some(hash);
        }
    }

    /// Check if a block has been completed
    pub fn is_complete(&self, block_id: u32) -> bool {
        self.completed_blocks
            .get(block_id as usize)
            .copied()
            .unwrap_or(false)
    }

    /// Count of completed blocks
    pub fn completed_count(&self) -> u32 {
        self.completed_blocks.iter().filter(|&&b| b).count() as u32
    }

    /// Get list of incomplete block IDs (what the sender still needs to send)
    pub fn incomplete_blocks(&self) -> Vec<u32> {
        self.completed_blocks
            .iter()
            .enumerate()
            .filter(|(_, &complete)| !complete)
            .map(|(i, _)| i as u32)
            .collect()
    }

    /// Check if the entire transfer is complete
    pub fn is_transfer_complete(&self) -> bool {
        self.completed_count() >= self.total_blocks
    }

    /// Get the resume file path for a given output file
    pub fn resume_path(output_path: &Path) -> PathBuf {
        let mut p = output_path.to_path_buf();
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        p.set_file_name(format!("{}.updown-resume", name));
        p
    }

    /// Save resume state to disk
    pub async fn save(&self, output_path: &Path) -> Result<()> {
        let resume_path = Self::resume_path(output_path);
        let data = bincode::serialize(self)?;
        fs::write(&resume_path, &data)
            .await
            .context("failed to save resume state")?;
        Ok(())
    }

    /// Load resume state from disk, if it exists and matches the transfer
    pub async fn load(
        output_path: &Path,
        _session_id: u32,
        file_size: u64,
        block_size: usize,
        total_blocks: u32,
    ) -> Result<Option<Self>> {
        let resume_path = Self::resume_path(output_path);

        if !resume_path.exists() {
            return Ok(None);
        }

        let data = fs::read(&resume_path)
            .await
            .context("failed to read resume state")?;

        let state: Self = match bincode::deserialize(&data) {
            Ok(s) => s,
            Err(_) => {
                // Corrupt resume file — start fresh
                fs::remove_file(&resume_path).await.ok();
                return Ok(None);
            }
        };

        // Verify the resume state matches this transfer
        if state.file_size != file_size
            || state.block_size != block_size
            || state.total_blocks != total_blocks
        {
            // Mismatched transfer — start fresh
            fs::remove_file(&resume_path).await.ok();
            return Ok(None);
        }

        Ok(Some(state))
    }

    /// Delete the resume file (called when transfer completes successfully)
    pub async fn cleanup(output_path: &Path) {
        let resume_path = Self::resume_path(output_path);
        fs::remove_file(&resume_path).await.ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resume_state_basic() {
        let mut state = ResumeState::new(1, 1024 * 1024, 4096, 256);

        assert_eq!(state.completed_count(), 0);
        assert!(!state.is_complete(0));
        assert_eq!(state.incomplete_blocks().len(), 256);

        state.mark_complete(0, [1u8; 32]);
        state.mark_complete(5, [2u8; 32]);

        assert!(state.is_complete(0));
        assert!(state.is_complete(5));
        assert!(!state.is_complete(1));
        assert_eq!(state.completed_count(), 2);
        assert_eq!(state.incomplete_blocks().len(), 254);
        assert!(!state.is_transfer_complete());
    }

    #[test]
    fn test_resume_path() {
        let path = Path::new("/tmp/myfile.bin");
        let resume = ResumeState::resume_path(path);
        assert_eq!(resume, PathBuf::from("/tmp/myfile.bin.updown-resume"));
    }
}
