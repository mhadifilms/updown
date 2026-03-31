use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use tokio::fs::{self, File};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::info;

use crate::crypto::CryptoContext;
use crate::protocol::*;
use crate::transport::receiver::UdpReceiver;

pub struct RecvEngine {
    output_dir: PathBuf,
    block_size: usize,
}

impl RecvEngine {
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            output_dir,
            block_size: DEFAULT_BLOCK_SIZE,
        }
    }

    pub fn with_block_size(mut self, block_size: usize) -> Self {
        self.block_size = block_size;
        self
    }

    /// Receive a file transfer.
    /// For Phase 1, uses pre-shared key and expects metadata via CLI args.
    pub async fn receive_file(
        &self,
        bind_addr: SocketAddr,
        session_id: u32,
        filename: &str,
        file_size: u64,
        total_blocks: u32,
        shared_key: &[u8; 32],
    ) -> Result<RecvResult> {
        let start = Instant::now();

        // Create output directory if needed
        fs::create_dir_all(&self.output_dir).await.ok();

        let output_path = self.output_dir.join(filename);
        info!("Receiving {} -> {}", filename, output_path.display());

        // Create/truncate output file
        let mut file = File::create(&output_path)
            .await
            .context("failed to create output file")?;

        // Pre-allocate file
        file.set_len(file_size).await?;

        // Create crypto context
        let crypto = CryptoContext::from_key(shared_key)?;

        // Create receiver
        let mut receiver = UdpReceiver::new(
            bind_addr,
            session_id,
            crypto,
            self.block_size as u64,
            total_blocks,
        )
        .await?;

        let actual_bind = receiver.local_addr()?;
        info!("Receiver listening on {}", actual_bind);

        // Progress bar
        let pb = ProgressBar::new(file_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}) [{elapsed_precise}] {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );

        // Channel for completed blocks
        let (block_tx, mut block_rx) = mpsc::channel(32);

        // Spawn receiver loop
        let recv_handle = tokio::spawn(async move {
            receiver.receive_loop(block_tx).await
        });

        // Write completed blocks to file
        let mut blocks_written = 0u32;
        let mut bytes_written = 0u64;

        while let Some(block) = block_rx.recv().await {
            let offset = block.block_id as u64 * self.block_size as u64;

            // Calculate actual data length for this block
            let actual_len = if block.block_id == total_blocks - 1 {
                let remaining = file_size - offset;
                remaining as usize
            } else {
                self.block_size
            };

            // Write block data at correct offset
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            file.write_all(&block.data[..actual_len]).await?;

            blocks_written += 1;
            bytes_written += actual_len as u64;

            pb.set_position(bytes_written);
            pb.set_message(format!(
                "block {}/{} ({} pkts, {:?})",
                blocks_written, total_blocks, block.packets_received, block.elapsed
            ));

            if blocks_written >= total_blocks {
                break;
            }
        }

        file.flush().await?;
        drop(file);

        pb.finish_with_message("transfer complete");

        // Wait for receiver to finish
        let recv_stats = recv_handle.await??;

        // Verify file hash
        let received_data = tokio::fs::read(&output_path).await?;
        let received_hash = blake3::hash(&received_data);

        let elapsed = start.elapsed();
        let rate_mbps = if elapsed.as_secs_f64() > 0.0 {
            (file_size as f64 * 8.0) / (elapsed.as_secs_f64() * 1_000_000.0)
        } else {
            0.0
        };

        info!(
            "Receive complete: {} bytes in {:?} ({:.1} Mbps)",
            bytes_written, elapsed, rate_mbps
        );
        info!("File hash: {}", received_hash.to_hex());

        Ok(RecvResult {
            output_path,
            file_size: bytes_written,
            total_packets: recv_stats.total_packets,
            blocks_received: blocks_written,
            elapsed,
            rate_mbps,
            blake3_hash: *received_hash.as_bytes(),
        })
    }
}

#[derive(Debug)]
pub struct RecvResult {
    pub output_path: PathBuf,
    pub file_size: u64,
    pub total_packets: u64,
    pub blocks_received: u32,
    pub elapsed: std::time::Duration,
    pub rate_mbps: f64,
    pub blake3_hash: [u8; 32],
}
