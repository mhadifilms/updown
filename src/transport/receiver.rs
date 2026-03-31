use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use raptorq::EncodingPacket;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::crypto::CryptoContext;
use crate::fec::FecDecoder;
use crate::protocol::*;

/// A block being received and decoded
struct PendingBlock {
    decoder: FecDecoder,
    packets_received: u32,
    first_packet_time: Instant,
}

/// Result of receiving a complete block
#[derive(Debug)]
pub struct ReceivedBlock {
    pub block_id: u32,
    pub data: Vec<u8>,
    pub packets_received: u32,
    pub elapsed: Duration,
}

/// UDP receiver that collects FEC-encoded packets and reconstructs blocks.
pub struct UdpReceiver {
    socket: Arc<UdpSocket>,
    session_id: u32,
    crypto: CryptoContext,
    pending_blocks: HashMap<u32, PendingBlock>,
    completed_blocks: Vec<u32>,
    block_data_len: u64,
    total_blocks: u32,
    /// Stats
    total_packets_received: u64,
    total_bytes_received: u64,
}

impl UdpReceiver {
    pub async fn new(
        bind_addr: SocketAddr,
        session_id: u32,
        crypto: CryptoContext,
        block_data_len: u64,
        total_blocks: u32,
    ) -> Result<Self> {
        let socket = UdpSocket::bind(bind_addr)
            .await
            .context("failed to bind UDP receiver socket")?;

        // Set large receive buffer (8 MiB)
        let sock_ref = socket2::SockRef::from(&socket);
        sock_ref.set_recv_buffer_size(8 * 1024 * 1024).ok();

        info!("UDP receiver listening on {}", socket.local_addr()?);

        Ok(Self {
            socket: Arc::new(socket),
            session_id,
            crypto,
            pending_blocks: HashMap::new(),
            completed_blocks: Vec::new(),
            block_data_len,
            total_blocks,
            total_packets_received: 0,
            total_bytes_received: 0,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    /// Main receive loop. Returns completed blocks via the channel.
    /// Runs until all blocks are received or cancelled.
    pub async fn receive_loop(
        &mut self,
        block_tx: mpsc::Sender<ReceivedBlock>,
    ) -> Result<ReceiveStats> {
        let start = Instant::now();
        let mut buf = vec![0u8; 65536];

        loop {
            // Check if we've received all blocks
            if self.completed_blocks.len() as u32 >= self.total_blocks {
                info!("All {} blocks received", self.total_blocks);
                break;
            }

            let (len, _peer) = match tokio::time::timeout(
                Duration::from_secs(10),
                self.socket.recv_from(&mut buf),
            )
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(e)) => {
                    warn!("recv error: {}", e);
                    continue;
                }
                Err(_) => {
                    warn!("receive timeout after 10s");
                    break;
                }
            };

            self.total_packets_received += 1;
            self.total_bytes_received += len as u64;

            // Parse packet
            match self.process_packet(&buf[..len]).await {
                Ok(Some(block)) => {
                    self.completed_blocks.push(block.block_id);
                    if block_tx.send(block).await.is_err() {
                        break; // Channel closed
                    }
                }
                Ok(None) => {} // Packet processed but block not yet complete
                Err(e) => {
                    debug!("packet processing error: {}", e);
                }
            }
        }

        Ok(ReceiveStats {
            total_packets: self.total_packets_received,
            total_bytes: self.total_bytes_received,
            blocks_completed: self.completed_blocks.len() as u32,
            elapsed: start.elapsed(),
        })
    }

    async fn process_packet(&mut self, data: &[u8]) -> Result<Option<ReceivedBlock>> {
        if data.len() < 2 {
            anyhow::bail!("packet too short");
        }

        // Parse: [header_len(2)][header][encrypted_payload]
        let header_len = u16::from_le_bytes([data[0], data[1]]) as usize;
        if data.len() < 2 + header_len {
            anyhow::bail!("packet too short for header");
        }

        let header: PacketHeader = bincode::deserialize(&data[2..2 + header_len])?;

        // Verify magic
        if header.magic != MAGIC {
            anyhow::bail!("bad magic");
        }

        // Verify session
        if header.session_id != self.session_id {
            anyhow::bail!("wrong session");
        }

        match header.packet_type {
            PacketType::Done => {
                info!(
                    "Received transfer done signal ({}/{} blocks complete)",
                    self.completed_blocks.len(),
                    self.total_blocks
                );
                // Don't force-complete blocks we haven't decoded — just return
                // The main loop will exit via the completion check or timeout
                return Ok(None);
            }
            PacketType::Data => {
                let encrypted_payload = &data[2 + header_len..];

                // Decrypt
                let aad = format!("{}-{}", header.block_id, header.symbol_id);
                let decrypted = self.crypto.decrypt(encrypted_payload, aad.as_bytes())?;

                // Deserialize into RaptorQ encoding packet
                let encoding_packet = EncodingPacket::deserialize(&decrypted);

                // Get or create pending block
                let block_id = header.block_id;

                // Use full block size for FEC decoding; the engine truncates the last block on write
                let actual_block_len = self.block_data_len;

                let pending = self.pending_blocks.entry(block_id).or_insert_with(|| {
                    PendingBlock {
                        decoder: FecDecoder::new(actual_block_len, SYMBOL_SIZE),
                        packets_received: 0,
                        first_packet_time: Instant::now(),
                    }
                });

                pending.packets_received += 1;

                // Feed to FEC decoder
                if let Some(decoded_data) = pending.decoder.add_packet(encoding_packet) {
                    let elapsed = pending.first_packet_time.elapsed();
                    let packets_received = pending.packets_received;

                    info!(
                        "Block {} decoded: {} bytes from {} packets in {:?}",
                        block_id,
                        decoded_data.len(),
                        packets_received,
                        elapsed
                    );

                    // Remove from pending
                    self.pending_blocks.remove(&block_id);

                    return Ok(Some(ReceivedBlock {
                        block_id,
                        data: decoded_data,
                        packets_received,
                        elapsed,
                    }));
                }
            }
            _ => {
                debug!("ignoring packet type {:?}", header.packet_type);
            }
        }

        Ok(None)
    }

    pub fn stats(&self) -> ReceiveStats {
        ReceiveStats {
            total_packets: self.total_packets_received,
            total_bytes: self.total_bytes_received,
            blocks_completed: self.completed_blocks.len() as u32,
            elapsed: Duration::ZERO,
        }
    }
}

#[derive(Debug)]
pub struct ReceiveStats {
    pub total_packets: u64,
    pub total_bytes: u64,
    pub blocks_completed: u32,
    pub elapsed: Duration,
}
