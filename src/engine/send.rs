use std::net::SocketAddr;
use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tracing::info;

use crate::crypto::CryptoContext;
use crate::protocol::*;
use crate::transport::rate_control::RateMode;
use crate::transport::sender::UdpSender;

const DEFAULT_INTERLEAVE: usize = 4;

pub struct SendEngine {
    target_rate_mbps: u64,
    rate_mode: RateMode,
    block_size: usize,
    repair_ratio: f32,
    interleave_depth: usize,
    compress: bool,
}

impl SendEngine {
    pub fn new(target_rate_mbps: u64, rate_mode: RateMode) -> Self {
        Self {
            target_rate_mbps,
            rate_mode,
            block_size: DEFAULT_BLOCK_SIZE,
            repair_ratio: 0.15,
            interleave_depth: DEFAULT_INTERLEAVE,
            compress: false,
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

    pub fn with_compression(mut self, enabled: bool) -> Self {
        self.compress = enabled;
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

        let metadata = tokio::fs::metadata(file_path).await?;
        let file_size = metadata.len();
        let filename = file_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let total_blocks = ((file_size as usize + self.block_size - 1) / self.block_size) as u32;

        // Stream-hash the file
        let file_hash = {
            let mut hasher = blake3::Hasher::new();
            let mut f = File::open(file_path).await?;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = f.read(&mut buf).await?;
                if n == 0 { break; }
                hasher.update(&buf[..n]);
            }
            hasher.finalize()
        };

        info!(
            "Sending {} ({} bytes, {} blocks, interleave={}, compress={})",
            filename, file_size, total_blocks, self.interleave_depth, self.compress
        );

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

        // Pipeline: read+encode on a background task, send on this task.
        // Channel depth of 2 = double-buffering (group N sends while group N+1 encodes).
        let (group_tx, mut group_rx) = mpsc::channel::<Vec<(u32, Vec<u8>)>>(2);

        let block_size = self.block_size;
        let interleave = self.interleave_depth;
        let compress = self.compress;
        let file_path_owned = file_path.to_path_buf();

        // Spawn reader/encoder pipeline
        let reader_handle = tokio::spawn(async move {
            let mut file = File::open(&file_path_owned).await?;
            let mut block_bufs: Vec<Vec<u8>> = (0..interleave)
                .map(|_| vec![0u8; block_size])
                .collect();

            let mut bid = 0u32;
            while bid < total_blocks {
                let group_end = (bid + interleave as u32).min(total_blocks);
                let group_size = (group_end - bid) as usize;

                let mut group: Vec<(u32, Vec<u8>)> = Vec::with_capacity(group_size);

                for i in 0..group_size {
                    let current_bid = bid + i as u32;
                    let offset = current_bid as u64 * block_size as u64;
                    let remaining = (file_size - offset) as usize;
                    let this_block_size = remaining.min(block_size);

                    file.read_exact(&mut block_bufs[i][..this_block_size]).await?;

                    let data = if compress {
                        let compressed =
                            zstd::bulk::compress(&block_bufs[i][..this_block_size], 1)?;
                        if compressed.len() < this_block_size {
                            compressed
                        } else {
                            block_bufs[i][..this_block_size].to_vec()
                        }
                    } else {
                        block_bufs[i][..this_block_size].to_vec()
                    };

                    group.push((current_bid, data));
                }

                if group_tx.send(group).await.is_err() {
                    break; // Receiver dropped
                }

                bid = group_end;
            }

            Ok::<(), anyhow::Error>(())
        });

        // Send loop: receives prepared groups from the pipeline
        while let Some(group) = group_rx.recv().await {
            let block_refs: Vec<(u32, &[u8])> =
                group.iter().map(|(id, data)| (*id, data.as_slice())).collect();

            let group_end = group.last().map(|(id, _)| id + 1).unwrap_or(0);

            let stats = sender.send_blocks_interleaved(&block_refs).await?;

            total_bytes_sent += stats.bytes_sent;
            total_packets_sent += stats.packets_sent;

            let progress_pos = (group_end as u64 * self.block_size as u64).min(file_size);
            pb.set_position(progress_pos);
            pb.set_message(format!(
                "blk {}/{} @ {:.0} Mbps",
                group_end,
                total_blocks,
                stats.rate_mbps
            ));
        }

        // Wait for reader to finish
        reader_handle.await??;

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
