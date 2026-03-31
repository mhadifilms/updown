use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use raptorq::EncodingPacket;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::crypto::CryptoContext;
use crate::fec::{AdaptiveFec, DecodeStats, FecDecoder};
use crate::protocol::*;
use crate::transport::rate_control::{timestamp_us, ReceiverRateCalculator};
use crate::transport::timeout_predictor::TimeoutPredictor;

/// A block being received and decoded
struct PendingBlock {
    decoder: FecDecoder,
    packets_received: u32,
    first_packet_time: Instant,
}

/// Result of receiving a complete block (with FEC decode stats)
#[derive(Debug)]
pub struct ReceivedBlock {
    pub block_id: u32,
    pub data: Vec<u8>,
    pub packets_received: u32,
    pub elapsed: Duration,
    pub decode_stats: DecodeStats,
}

/// UDP receiver with FEC reconstruction, adaptive FEC, and receiver-driven rate control.
pub struct UdpReceiver {
    socket: Arc<UdpSocket>,
    session_id: u32,
    crypto: CryptoContext,
    pending_blocks: HashMap<u32, PendingBlock>,
    completed_blocks: Vec<u32>,
    block_data_len: u64,
    file_size: u64,
    total_blocks: u32,
    /// Adaptive FEC controller
    adaptive_fec: AdaptiveFec,
    /// Receiver-side rate calculator (FASP Fig 7)
    rate_calc: ReceiverRateCalculator,
    /// Timeout predictor (FASP Fig 2, module 240)
    timeout_predictor: TimeoutPredictor,
    /// When we started expecting each block
    block_expect_times: HashMap<u32, Instant>,
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
        file_size: u64,
        total_blocks: u32,
        target_rate_mbps: u64,
    ) -> Result<Self> {
        let std_socket = {
            let addr: std::net::SocketAddr = bind_addr;
            let domain = if addr.is_ipv4() {
                socket2::Domain::IPV4
            } else {
                socket2::Domain::IPV6
            };
            let sock = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
            sock.set_reuse_address(true)?;
            sock.set_nonblocking(true)?;
            sock.bind(&addr.into())?;

            // 8 MiB receive buffer
            sock.set_recv_buffer_size(8 * 1024 * 1024).ok();

            sock
        };

        let socket = UdpSocket::from_std(std_socket.into())?;

        info!("UDP receiver listening on {}", socket.local_addr()?);

        Ok(Self {
            socket: Arc::new(socket),
            session_id,
            crypto,
            pending_blocks: HashMap::new(),
            completed_blocks: Vec::new(),
            block_data_len,
            file_size,
            total_blocks,
            adaptive_fec: AdaptiveFec::new(),
            rate_calc: ReceiverRateCalculator::new(target_rate_mbps),
            timeout_predictor: TimeoutPredictor::new(target_rate_mbps, block_data_len as usize),
            block_expect_times: HashMap::new(),
            total_packets_received: 0,
            total_bytes_received: 0,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    /// Main receive loop. Returns completed blocks via the channel.
    pub async fn receive_loop(
        &mut self,
        block_tx: mpsc::Sender<ReceivedBlock>,
    ) -> Result<ReceiveStats> {
        let start = Instant::now();
        let mut buf = vec![0u8; 65536];

        loop {
            if self.completed_blocks.len() as u32 >= self.total_blocks {
                info!("All {} blocks received and decoded", self.total_blocks);
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
                    warn!(
                        "receive timeout ({}/{} blocks complete)",
                        self.completed_blocks.len(),
                        self.total_blocks
                    );
                    break;
                }
            };

            self.total_packets_received += 1;
            self.total_bytes_received += len as u64;

            match self.process_packet(&buf[..len]).await {
                Ok(Some(block)) => {
                    // Feed decode stats to adaptive FEC
                    let mut new_ratio = self.adaptive_fec.update(&block.decode_stats);

                    // Feed to timeout predictor
                    if let Some(expect_time) = self.block_expect_times.remove(&block.block_id) {
                        let late = self.timeout_predictor.record_arrival(
                            block.block_id,
                            expect_time,
                            block.data.len(),
                        );
                        if late {
                            // Boost FEC for late blocks
                            let boost = self.timeout_predictor.fec_boost_factor();
                            new_ratio *= boost;
                        }
                    }

                    // Feed to receiver rate calculator
                    self.rate_calc.compute_rate(self.adaptive_fec.loss_estimate());

                    info!(
                        "Block {} decoded: {} pkts, excess={}, loss={:.2}%, fec={:.1}%",
                        block.block_id,
                        block.packets_received,
                        block.decode_stats.excess_symbols,
                        block.decode_stats.estimated_loss * 100.0,
                        new_ratio * 100.0,
                    );

                    self.completed_blocks.push(block.block_id);
                    if block_tx.send(block).await.is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    debug!("packet error: {}", e);
                }
            }
        }

        Ok(ReceiveStats {
            total_packets: self.total_packets_received,
            total_bytes: self.total_bytes_received,
            blocks_completed: self.completed_blocks.len() as u32,
            elapsed: start.elapsed(),
            final_loss_estimate: self.adaptive_fec.loss_estimate(),
            final_fec_ratio: self.adaptive_fec.recommended_ratio(),
            suggested_rate_bps: self.rate_calc.current_rate_bps(),
        })
    }

    async fn process_packet(&mut self, data: &[u8]) -> Result<Option<ReceivedBlock>> {
        if data.len() < 2 {
            anyhow::bail!("packet too short");
        }

        let header_len = u16::from_le_bytes([data[0], data[1]]) as usize;
        if data.len() < 2 + header_len {
            anyhow::bail!("packet too short for header");
        }

        let header: PacketHeader = bincode::deserialize(&data[2..2 + header_len])?;

        if header.magic != MAGIC {
            anyhow::bail!("bad magic");
        }
        if header.session_id != self.session_id {
            anyhow::bail!("wrong session");
        }

        // Record OWD for rate calculation
        let now_us = timestamp_us();
        if header.timestamp_us > 0 && now_us > header.timestamp_us {
            let owd_us = (now_us - header.timestamp_us) as f64;
            self.rate_calc.record_owd(now_us, owd_us);
        }

        match header.packet_type {
            PacketType::Done => {
                info!(
                    "Done signal ({}/{} blocks complete)",
                    self.completed_blocks.len(),
                    self.total_blocks
                );
                return Ok(None);
            }
            PacketType::Data => {
                let encrypted_payload = &data[2 + header_len..];

                let aad = format!("{}-{}", header.block_id, header.symbol_id);
                let decrypted = self.crypto.decrypt(encrypted_payload, aad.as_bytes())?;

                let encoding_packet = EncodingPacket::deserialize(&decrypted);
                let block_id = header.block_id;

                // Skip if block already completed
                if self.completed_blocks.contains(&block_id) {
                    return Ok(None);
                }

                // Record when we first started expecting this block
                self.block_expect_times.entry(block_id).or_insert_with(Instant::now);

                // Calculate correct block size (last block may be smaller)
                let actual_block_len = if block_id == self.total_blocks - 1 {
                    let offset = block_id as u64 * self.block_data_len;
                    self.file_size - offset
                } else {
                    self.block_data_len
                };
                let pending = self.pending_blocks.entry(block_id).or_insert_with(|| {
                    PendingBlock {
                        decoder: FecDecoder::new(actual_block_len, SYMBOL_SIZE),
                        packets_received: 0,
                        first_packet_time: Instant::now(),
                    }
                });

                pending.packets_received += 1;

                if let Some((decoded_data, decode_stats)) = pending.decoder.add_packet(encoding_packet) {
                    let elapsed = pending.first_packet_time.elapsed();
                    let packets_received = pending.packets_received;
                    self.pending_blocks.remove(&block_id);

                    return Ok(Some(ReceivedBlock {
                        block_id,
                        data: decoded_data,
                        packets_received,
                        elapsed,
                        decode_stats,
                    }));
                }
            }
            _ => {
                debug!("ignoring {:?}", header.packet_type);
            }
        }

        Ok(None)
    }

    /// Get the current adaptive FEC recommendation
    pub fn recommended_fec_ratio(&self) -> f32 {
        self.adaptive_fec.recommended_ratio()
    }

    /// Get the receiver-computed suggested rate
    pub fn suggested_rate_bps(&self) -> u64 {
        self.rate_calc.current_rate_bps()
    }
}

#[derive(Debug)]
pub struct ReceiveStats {
    pub total_packets: u64,
    pub total_bytes: u64,
    pub blocks_completed: u32,
    pub elapsed: Duration,
    pub final_loss_estimate: f32,
    pub final_fec_ratio: f32,
    pub suggested_rate_bps: u64,
}
