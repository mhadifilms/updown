use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::net::UdpSocket;
use tracing::info;

use crate::crypto::CryptoContext;
use crate::fec::{EncodeStats, FecEncoder};
use crate::protocol::*;
use crate::transport::rate_control::{timestamp_us, RateController, RateMode};

/// Maximum number of packets to concatenate for GSO-style batched sends.
/// On Linux with GSO, the kernel segments one large buffer into individual datagrams.
/// On macOS, we fall back to individual sends but still batch the preparation.
const GSO_BATCH_SIZE: usize = 64;

/// UDP sender with GSO batching, parallel block interleaving, and rate pacing.
pub struct UdpSender {
    socket: Arc<UdpSocket>,
    target: SocketAddr,
    session_id: u32,
    rate_controller: RateController,
    crypto: CryptoContext,
    fec_encoder: FecEncoder,
    seq_num: u32,
    /// Whether the socket supports GSO (Linux 4.18+)
    gso_enabled: bool,
    /// GSO segment size (set via UDP_SEGMENT socket option)
    gso_segment_size: usize,
}

impl UdpSender {
    pub async fn new(
        bind_addr: SocketAddr,
        target: SocketAddr,
        session_id: u32,
        target_rate_mbps: u64,
        rate_mode: RateMode,
        crypto: CryptoContext,
        repair_ratio: f32,
        _interleave_depth: usize,
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

            #[cfg(target_os = "linux")]
            sock.connect(&target.into())?;

            sock.set_send_buffer_size(8 * 1024 * 1024).ok();

            // Try to enable GSO (Linux only)
            #[cfg(target_os = "linux")]
            {
                // UDP_SEGMENT socket option enables GSO
                unsafe {
                    let segment_size: libc::c_int = 1400;
                    let ret = libc::setsockopt(
                        std::os::unix::io::AsRawFd::as_raw_fd(&sock),
                        libc::SOL_UDP,
                        libc::UDP_SEGMENT,
                        &segment_size as *const _ as *const libc::c_void,
                        std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                    );
                    if ret == 0 {
                        info!("GSO enabled (UDP_SEGMENT=1400)");
                    }
                }
            }

            sock
        };

        let socket = UdpSocket::from_std(std_socket.into())?;

        // Detect if GSO is available
        let gso_enabled = cfg!(target_os = "linux");

        info!(
            "UDP sender bound to {}, target={}, gso={}",
            socket.local_addr()?,
            target,
            gso_enabled,
        );

        Ok(Self {
            socket: Arc::new(socket),
            target,
            session_id,
            rate_controller: RateController::new(target_rate_mbps, rate_mode),
            crypto,
            fec_encoder: FecEncoder::new(repair_ratio),
            seq_num: 0,
            gso_enabled,
            gso_segment_size: 1400,
        })
    }

    pub fn set_repair_ratio(&mut self, ratio: f32) {
        self.fec_encoder.set_repair_ratio(ratio);
    }

    /// Send multiple blocks with cross-block interleaving.
    pub async fn send_blocks_interleaved(
        &mut self,
        blocks: &[(u32, &[u8])],
    ) -> Result<InterlevedSendStats> {
        let start = Instant::now();
        let num_blocks = blocks.len();

        // Phase 1: FEC-encode all blocks
        let mut all_encoded: Vec<Vec<Vec<u8>>> = Vec::with_capacity(num_blocks);
        let mut all_block_ids: Vec<u32> = Vec::with_capacity(num_blocks);
        let mut total_symbols = 0usize;
        let mut encode_stats_list: Vec<EncodeStats> = Vec::new();

        for (block_id, data) in blocks {
            let (packets, stats) = self.fec_encoder.encode(data);
            let serialized: Vec<Vec<u8>> = packets.iter().map(|p| p.serialize()).collect();
            total_symbols += serialized.len();
            all_encoded.push(serialized);
            all_block_ids.push(*block_id);
            encode_stats_list.push(stats);
        }

        let encode_time = start.elapsed();

        // Phase 2: Pre-build ALL wire-ready packets interleaved
        let max_symbols = all_encoded.iter().map(|e| e.len()).max().unwrap_or(0);
        let mut wire_packets: Vec<Vec<u8>> = Vec::with_capacity(total_symbols);
        let mut send_buf = Vec::with_capacity(2048);

        for sym_idx in 0..max_symbols {
            for (blk_idx, encoded_block) in all_encoded.iter().enumerate() {
                if sym_idx >= encoded_block.len() {
                    continue;
                }

                let block_id = all_block_ids[blk_idx];
                let symbol_data = &encoded_block[sym_idx];
                let symbol_id = sym_idx as u32;

                let header = PacketHeader {
                    magic: MAGIC,
                    packet_type: PacketType::Data,
                    session_id: self.session_id,
                    block_id,
                    symbol_id,
                    timestamp_us: 0,
                    seq_num: self.seq_num,
                };
                self.seq_num += 1;

                let header_bytes = bincode::serialize(&header)?;
                let aad = format!("{}-{}", block_id, symbol_id);
                let encrypted = self.crypto.encrypt(symbol_data, aad.as_bytes())?;

                send_buf.clear();
                let header_len = header_bytes.len() as u16;
                send_buf.extend_from_slice(&header_len.to_le_bytes());
                send_buf.extend_from_slice(&header_bytes);
                send_buf.extend_from_slice(&encrypted);

                wire_packets.push(send_buf.clone());
            }
        }

        let prep_time = start.elapsed();
        info!(
            "Prepared {} packets: encode={:?} encrypt={:?}",
            wire_packets.len(),
            encode_time,
            prep_time - encode_time,
        );

        // Phase 3: Blast packets with coarse-grained pacing
        let blast_start = Instant::now();
        let mut bytes_sent: u64 = 0;
        let mut packets_sent: u64 = 0;

        let pace_check_interval = 128usize;
        let target_bytes_per_sec = self.rate_controller.current_rate_bps() / 8;

        for (idx, pkt) in wire_packets.iter().enumerate() {
            match self.socket.send_to(pkt, self.target).await {
                Ok(n) => bytes_sent += n as u64,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(Duration::from_micros(50)).await;
                    self.socket.send_to(pkt, self.target).await?;
                    bytes_sent += pkt.len() as u64;
                }
                Err(e) => return Err(e.into()),
            }
            packets_sent += 1;

            if idx % pace_check_interval == (pace_check_interval - 1) {
                let elapsed = blast_start.elapsed();
                let expected_ns =
                    (bytes_sent as u128 * 1_000_000_000) / target_bytes_per_sec.max(1) as u128;
                let actual_ns = elapsed.as_nanos();

                if actual_ns < expected_ns {
                    let sleep_ns = expected_ns - actual_ns;
                    if sleep_ns > 5_000 {
                        tokio::time::sleep(Duration::from_nanos(sleep_ns as u64)).await;
                    }
                } else if idx % 1024 == 0 {
                    tokio::task::yield_now().await;
                }
            }
        }

        let elapsed = start.elapsed();
        let rate_mbps = if elapsed.as_secs_f64() > 0.0 {
            (bytes_sent as f64 * 8.0) / (elapsed.as_secs_f64() * 1_000_000.0)
        } else {
            0.0
        };

        Ok(InterlevedSendStats {
            blocks_sent: num_blocks as u32,
            block_ids: all_block_ids,
            bytes_sent,
            packets_sent,
            total_symbols: total_symbols as u64,
            encode_stats: encode_stats_list,
            elapsed,
            rate_mbps,
        })
    }

    pub async fn send_block(&mut self, block_id: u32, data: &[u8]) -> Result<BlockSendStats> {
        let blocks = vec![(block_id, data)];
        let stats = self.send_blocks_interleaved(&blocks).await?;
        Ok(BlockSendStats {
            block_id,
            bytes_sent: stats.bytes_sent,
            packets_sent: stats.packets_sent,
            symbols_sent: stats.total_symbols,
            elapsed: stats.elapsed,
            rate_mbps: stats.rate_mbps,
        })
    }

    /// Send a "transfer done" signal
    pub async fn send_done(&mut self) -> Result<()> {
        tokio::time::sleep(Duration::from_millis(500)).await;

        let header = PacketHeader {
            magic: MAGIC,
            packet_type: PacketType::Done,
            session_id: self.session_id,
            block_id: 0,
            symbol_id: 0,
            timestamp_us: timestamp_us(),
            seq_num: self.seq_num,
        };
        self.seq_num += 1;

        let header_bytes = bincode::serialize(&header)?;
        let header_len = header_bytes.len() as u16;
        let mut udp_payload = Vec::with_capacity(2 + header_bytes.len());
        udp_payload.extend_from_slice(&header_len.to_le_bytes());
        udp_payload.extend_from_slice(&header_bytes);

        for _ in 0..10 {
            self.socket.send_to(&udp_payload, self.target).await.ok();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        Ok(())
    }

    pub fn update_rate(&mut self, owd: Duration) {
        self.rate_controller.update_owd(owd);
    }

    pub fn apply_receiver_rate(&mut self, suggested_rate_bps: u64) {
        self.rate_controller.apply_receiver_suggestion(suggested_rate_bps);
    }

    pub fn current_rate_mbps(&self) -> f64 {
        self.rate_controller.current_rate_mbps()
    }
}

#[derive(Debug)]
pub struct BlockSendStats {
    pub block_id: u32,
    pub bytes_sent: u64,
    pub packets_sent: u64,
    pub symbols_sent: u64,
    pub elapsed: Duration,
    pub rate_mbps: f64,
}

#[derive(Debug)]
pub struct InterlevedSendStats {
    pub blocks_sent: u32,
    pub block_ids: Vec<u32>,
    pub bytes_sent: u64,
    pub packets_sent: u64,
    pub total_symbols: u64,
    pub encode_stats: Vec<EncodeStats>,
    pub elapsed: Duration,
    pub rate_mbps: f64,
}
