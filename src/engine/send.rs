use std::net::SocketAddr;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tracing::info;

use crate::crypto::CryptoContext;
use crate::protocol::*;
use crate::transport::rate_control::RateMode;
use crate::transport::sender::UdpSender;

pub struct SendEngine {
    target_rate_mbps: u64,
    rate_mode: RateMode,
    block_size: usize,
    repair_ratio: f32,
}

impl SendEngine {
    pub fn new(target_rate_mbps: u64, rate_mode: RateMode) -> Self {
        Self {
            target_rate_mbps,
            rate_mode,
            block_size: DEFAULT_BLOCK_SIZE,
            repair_ratio: 0.15, // 15% FEC overhead by default
        }
    }

    pub fn with_block_size(mut self, block_size: usize) -> Self {
        self.block_size = block_size;
        self
    }

    pub fn with_repair_ratio(mut self, ratio: f32) -> Self {
        self.repair_ratio = ratio;
        self
    }

    /// Send a file to the receiver at the given address.
    /// For Phase 1, this uses a pre-shared key (no QUIC control channel yet).
    pub async fn send_file(
        &self,
        file_path: &Path,
        receiver_addr: SocketAddr,
        shared_key: &[u8; 32],
    ) -> Result<SendResult> {
        self.send_file_with_session(file_path, receiver_addr, shared_key, rand::random())
            .await
    }

    pub async fn send_file_with_session(
        &self,
        file_path: &Path,
        receiver_addr: SocketAddr,
        shared_key: &[u8; 32],
        session_id: u32,
    ) -> Result<SendResult> {
        let start = Instant::now();

        // Open and read file metadata
        let file = File::open(file_path)
            .await
            .context("failed to open file")?;
        let metadata = file.metadata().await?;
        let file_size = metadata.len();
        let filename = file_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let total_blocks = ((file_size as usize + self.block_size - 1) / self.block_size) as u32;

        info!(
            "Sending {} ({} bytes, {} blocks of {} bytes)",
            filename, file_size, total_blocks, self.block_size
        );

        // Compute file hash
        let file_hash = {
            let file_bytes = tokio::fs::read(file_path).await?;
            blake3::hash(&file_bytes)
        };

        // Create crypto context from shared key
        let crypto = CryptoContext::from_key(shared_key)?;

        // Create manifest
        let manifest = TransferManifest {
            session_id,
            filename: filename.clone(),
            file_size,
            block_size: self.block_size,
            total_blocks,
            file_hash: *file_hash.as_bytes(),
        };

        // Bind sender to any available port
        let bind_addr: SocketAddr = "0.0.0.0:0".parse()?;
        let mut sender = UdpSender::new(
            bind_addr,
            receiver_addr,
            manifest.session_id,
            self.target_rate_mbps,
            self.rate_mode,
            crypto,
            self.repair_ratio,
        )
        .await?;

        // Set up progress bar
        let pb = ProgressBar::new(file_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}) [{elapsed_precise}] {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );

        // Send blocks
        let mut total_bytes_sent: u64 = 0;
        let mut total_packets_sent: u64 = 0;
        let mut block_buf = vec![0u8; self.block_size];

        // Re-open file for sequential reading
        let mut file = File::open(file_path).await?;

        for block_id in 0..total_blocks {
            // Read block from file
            let bytes_remaining = file_size - (block_id as u64 * self.block_size as u64);
            let this_block_size = (bytes_remaining as usize).min(self.block_size);
            let buf = &mut block_buf[..this_block_size];
            file.read_exact(buf).await?;

            // Send the block
            let stats = sender.send_block(block_id, buf).await?;

            total_bytes_sent += stats.bytes_sent;
            total_packets_sent += stats.packets_sent;

            pb.set_position((block_id as u64 + 1) * self.block_size as u64);
            pb.set_message(format!(
                "block {}/{} @ {:.1} Mbps",
                block_id + 1,
                total_blocks,
                stats.rate_mbps
            ));
        }

        // Send done signal
        sender.send_done().await?;

        pb.finish_with_message("transfer complete");

        let elapsed = start.elapsed();
        let overall_rate_mbps = if elapsed.as_secs_f64() > 0.0 {
            (file_size as f64 * 8.0) / (elapsed.as_secs_f64() * 1_000_000.0)
        } else {
            0.0
        };

        info!(
            "Transfer complete: {} bytes in {:?} ({:.1} Mbps), {} packets sent",
            file_size, elapsed, overall_rate_mbps, total_packets_sent
        );

        Ok(SendResult {
            file_size,
            total_bytes_sent,
            total_packets_sent,
            elapsed,
            rate_mbps: overall_rate_mbps,
            file_hash: *file_hash.as_bytes(),
        })
    }
}

#[derive(Debug)]
pub struct SendResult {
    pub file_size: u64,
    pub total_bytes_sent: u64,
    pub total_packets_sent: u64,
    pub elapsed: std::time::Duration,
    pub rate_mbps: f64,
    pub file_hash: [u8; 32],
}
