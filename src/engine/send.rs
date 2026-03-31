use std::net::SocketAddr;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use tokio::fs::File;
use tracing::info;

use crate::crypto::CryptoContext;
use crate::protocol::*;
use crate::transport::rate_control::RateMode;
use crate::transport::sender::UdpSender;

/// Default number of blocks to interleave (send in parallel)
const DEFAULT_INTERLEAVE: usize = 4;

pub struct SendEngine {
    target_rate_mbps: u64,
    rate_mode: RateMode,
    block_size: usize,
    repair_ratio: f32,
    interleave_depth: usize,
}

impl SendEngine {
    pub fn new(target_rate_mbps: u64, rate_mode: RateMode) -> Self {
        Self {
            target_rate_mbps,
            rate_mode,
            block_size: DEFAULT_BLOCK_SIZE,
            repair_ratio: 0.15,
            interleave_depth: DEFAULT_INTERLEAVE,
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

    pub fn with_interleave(mut self, depth: usize) -> Self {
        self.interleave_depth = depth.max(1);
        self
    }

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
            "Sending {} ({} bytes, {} blocks, interleave={})",
            filename, file_size, total_blocks, self.interleave_depth
        );

        let file_hash = {
            let file_bytes = tokio::fs::read(file_path).await?;
            blake3::hash(&file_bytes)
        };

        let crypto = CryptoContext::from_key(shared_key)?;

        let bind_addr: SocketAddr = "0.0.0.0:0".parse()?;
        let mut sender = UdpSender::new(
            bind_addr,
            receiver_addr,
            session_id,
            self.target_rate_mbps,
            self.rate_mode,
            crypto,
            self.repair_ratio,
            self.interleave_depth,
        )
        .await?;

        let pb = ProgressBar::new(file_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}) [{elapsed_precise}] {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );

        let mut total_bytes_sent: u64 = 0;
        let mut total_packets_sent: u64 = 0;

        // Read entire file into memory for interleaved sending
        // (Phase 3 will use io_uring + O_DIRECT for streaming)
        let file_data = tokio::fs::read(file_path).await?;

        // Send blocks in interleaved groups
        let mut block_id = 0u32;
        while block_id < total_blocks {
            // Collect a group of blocks for interleaved sending
            let group_end = (block_id + self.interleave_depth as u32).min(total_blocks);
            let mut block_group: Vec<(u32, &[u8])> = Vec::new();

            for bid in block_id..group_end {
                let offset = bid as usize * self.block_size;
                let end = (offset + self.block_size).min(file_data.len());
                block_group.push((bid, &file_data[offset..end]));
            }

            // Send the interleaved group
            let stats = sender.send_blocks_interleaved(&block_group).await?;

            total_bytes_sent += stats.bytes_sent;
            total_packets_sent += stats.packets_sent;

            let progress_pos = (group_end as u64 * self.block_size as u64).min(file_size);
            pb.set_position(progress_pos);
            pb.set_message(format!(
                "blocks {}-{}/{} @ {:.0} Mbps",
                block_id,
                group_end - 1,
                total_blocks,
                stats.rate_mbps
            ));

            block_id = group_end;
        }

        sender.send_done().await?;
        pb.finish_with_message("transfer complete");

        let elapsed = start.elapsed();
        let overall_rate_mbps = if elapsed.as_secs_f64() > 0.0 {
            (file_size as f64 * 8.0) / (elapsed.as_secs_f64() * 1_000_000.0)
        } else {
            0.0
        };

        info!(
            "Transfer complete: {} bytes in {:?} ({:.1} Mbps), {} packets",
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
