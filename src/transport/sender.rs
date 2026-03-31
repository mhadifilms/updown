use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use tracing::info;

use crate::crypto::CryptoContext;
use crate::fec::FecEncoder;
use crate::protocol::*;
use crate::transport::rate_control::{timestamp_us, RateController, RateMode};

/// UDP sender that blasts FEC-encoded, encrypted data at a paced rate.
pub struct UdpSender {
    socket: Arc<UdpSocket>,
    target: SocketAddr,
    session_id: u32,
    rate_controller: RateController,
    crypto: CryptoContext,
    fec_encoder: FecEncoder,
    seq_num: u32,
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
    ) -> Result<Self> {
        let socket = UdpSocket::bind(bind_addr)
            .await
            .context("failed to bind UDP sender socket")?;

        // Set large send buffer (8 MiB)
        let sock_ref = socket2::SockRef::from(&socket);
        sock_ref.set_send_buffer_size(8 * 1024 * 1024).ok();

        info!(
            "UDP sender bound to {}, targeting {}",
            socket.local_addr()?,
            target
        );

        Ok(Self {
            socket: Arc::new(socket),
            target,
            session_id,
            rate_controller: RateController::new(target_rate_mbps, rate_mode),
            crypto,
            fec_encoder: FecEncoder::new(repair_ratio),
            seq_num: 0,
        })
    }

    /// Send a single block of file data with FEC encoding and encryption.
    /// This is the core send loop for one block.
    pub async fn send_block(&mut self, block_id: u32, data: &[u8]) -> Result<BlockSendStats> {
        let start = Instant::now();

        // FEC encode the block into symbols
        let packets = self.fec_encoder.encode(data);
        let total_symbols = packets.len();

        info!(
            "Block {} encoded into {} symbols ({} bytes source data)",
            block_id,
            total_symbols,
            data.len()
        );

        let mut bytes_sent: u64 = 0;
        let mut packets_sent: u64 = 0;

        for packet in packets {
            let serialized = packet.serialize();
            let symbol_id = packets_sent as u32; // Simple sequential ID

            // Build packet header
            let header = PacketHeader {
                magic: MAGIC,
                packet_type: PacketType::Data,
                session_id: self.session_id,
                block_id,
                symbol_id,
                timestamp_us: timestamp_us(),
                seq_num: self.seq_num,
            };
            self.seq_num += 1;

            // Serialize header
            let header_bytes = bincode::serialize(&header)?;

            // Build AAD from block_id + symbol_id for authenticated encryption
            let aad = format!("{}-{}", block_id, symbol_id);

            // Encrypt the symbol data
            let encrypted = self.crypto.encrypt(&serialized, aad.as_bytes())?;

            // Assemble final UDP packet: [header_len(2)][header][encrypted_payload]
            let header_len = header_bytes.len() as u16;
            let mut udp_payload =
                Vec::with_capacity(2 + header_bytes.len() + encrypted.len());
            udp_payload.extend_from_slice(&header_len.to_le_bytes());
            udp_payload.extend_from_slice(&header_bytes);
            udp_payload.extend_from_slice(&encrypted);

            // Rate-paced send
            let interval = self.rate_controller.packet_interval(udp_payload.len());

            self.socket.send_to(&udp_payload, self.target).await?;
            bytes_sent += udp_payload.len() as u64;
            packets_sent += 1;

            // Pace: sleep for the inter-packet interval
            if interval > Duration::from_micros(10) {
                tokio::time::sleep(interval).await;
            } else {
                // At very high rates, yield occasionally to not starve the runtime
                if packets_sent % 64 == 0 {
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

        Ok(BlockSendStats {
            block_id,
            bytes_sent,
            packets_sent,
            symbols_sent: total_symbols as u64,
            elapsed,
            rate_mbps,
        })
    }

    /// Send a "transfer done" signal
    pub async fn send_done(&mut self) -> Result<()> {
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

        // Send done signal multiple times for reliability
        for _ in 0..5 {
            self.socket.send_to(&udp_payload, self.target).await?;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        Ok(())
    }

    /// Feed OWD measurements from the receiver back to the rate controller
    pub fn update_rate(&mut self, owd: Duration) {
        self.rate_controller.update_owd(owd);
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
