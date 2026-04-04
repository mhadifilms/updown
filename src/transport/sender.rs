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

/// UDP sender with GSO batching, parallel block interleaving, and rate pacing.
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
            {
                sock.connect(&target.into())?;
                // Enable GSO: UDP_SEGMENT socket option (Linux 4.18+)
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
                    } else {
                        info!("GSO not available (kernel too old?)");
                    }
                }
            }

            sock.set_send_buffer_size(8 * 1024 * 1024).ok();
            sock
        };

        let socket = UdpSocket::from_std(std_socket.into())?;

        info!(
            "UDP sender bound to {}, target={}, platform={}",
            socket.local_addr()?,
            target,
            std::env::consts::OS,
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

        // Phase 2: Build contiguous wire buffer with interleaved packet order
        let max_symbols = all_encoded.iter().map(|e| e.len()).max().unwrap_or(0);
        let estimated_pkt_size = 1500;
        let mut wire_buf: Vec<u8> = Vec::with_capacity(total_symbols * estimated_pkt_size);
        let mut pkt_index: Vec<(usize, usize)> = Vec::with_capacity(total_symbols);

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
                let aad = format!("{}-{}-{}", self.session_id, block_id, symbol_id);
                let encrypted = self.crypto.encrypt(symbol_data, aad.as_bytes())?;

                let pkt_start = wire_buf.len();
                let header_len = header_bytes.len() as u16;
                wire_buf.extend_from_slice(&header_len.to_le_bytes());
                wire_buf.extend_from_slice(&header_bytes);
                wire_buf.extend_from_slice(&encrypted);
                let pkt_len = wire_buf.len() - pkt_start;

                pkt_index.push((pkt_start, pkt_len));
            }
        }

        let prep_time = start.elapsed();
        info!(
            "Prepared {} packets ({:.1} MB): encode={:?} encrypt={:?}",
            pkt_index.len(),
            wire_buf.len() as f64 / 1_048_576.0,
            encode_time,
            prep_time - encode_time,
        );

        // Phase 3: Blast packets
        let target = self.target;
        let target_bps = self.rate_controller.current_rate_bps();
        let target_bytes_per_sec = (target_bps / 8).max(1);

        let blast_start = Instant::now();
        let mut bytes_sent: u64 = 0;
        let mut packets_sent: u64 = 0;

        #[cfg(target_os = "linux")]
        {
            // Linux GSO: concatenate up to 64 packets per sendmsg syscall.
            // 64x fewer syscalls = the key to breaking 1 Gbps.
            let gso_batch = 64usize;
            let mut super_buf: Vec<u8> = Vec::with_capacity(gso_batch * 1500);
            let mut pkt_size = 0usize;
            let mut batch_count = 0usize;
            let mut gso_sends = 0u64;

            for &(offset, len) in pkt_index.iter() {
                if batch_count == 0 {
                    pkt_size = len;
                }

                // GSO requires uniform segment sizes — flush if size changes
                if len != pkt_size && batch_count > 0 {
                    bytes_sent += send_gso_batch(&self.socket, target, &super_buf, pkt_size)?;
                    packets_sent += batch_count as u64;
                    gso_sends += 1;
                    super_buf.clear();
                    batch_count = 0;
                    pkt_size = len;
                }

                super_buf.extend_from_slice(&wire_buf[offset..offset + len]);
                batch_count += 1;

                if batch_count >= gso_batch {
                    bytes_sent += send_gso_batch(&self.socket, target, &super_buf, pkt_size)?;
                    packets_sent += batch_count as u64;
                    gso_sends += 1;
                    super_buf.clear();
                    batch_count = 0;

                    // Rate pacing per GSO batch (~64 packets)
                    let elapsed = blast_start.elapsed();
                    let expected_ns =
                        (bytes_sent as u128 * 1_000_000_000) / target_bytes_per_sec as u128;
                    let actual_ns = elapsed.as_nanos();
                    if actual_ns < expected_ns {
                        let sleep_ns = expected_ns - actual_ns;
                        if sleep_ns > 10_000 {
                            std::thread::sleep(Duration::from_nanos(sleep_ns as u64));
                        }
                    }
                }
            }

            if batch_count > 0 {
                bytes_sent += send_gso_batch(&self.socket, target, &super_buf, pkt_size)?;
                packets_sent += batch_count as u64;
                gso_sends += 1;
            }

            info!(
                "GSO blast: {} packets in {} sendmsg calls ({:.0}x reduction)",
                packets_sent,
                gso_sends,
                if gso_sends > 0 { packets_sent as f64 / gso_sends as f64 } else { 0.0 }
            );
        }

        #[cfg(not(target_os = "linux"))]
        {
            // macOS/other: individual async send_to with coarse-grained pacing.
            let pace_check = 256usize;

            for (idx, &(offset, len)) in pkt_index.iter().enumerate() {
                let pkt = &wire_buf[offset..offset + len];
                match self.socket.send_to(pkt, target).await {
                    Ok(n) => bytes_sent += n as u64,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        tokio::time::sleep(Duration::from_micros(10)).await;
                        self.socket.send_to(pkt, target).await.ok();
                        bytes_sent += len as u64;
                    }
                    Err(_) => bytes_sent += len as u64,
                }
                packets_sent += 1;

                if idx % pace_check == (pace_check - 1) {
                    let elapsed = blast_start.elapsed();
                    let expected_ns =
                        (bytes_sent as u128 * 1_000_000_000) / target_bytes_per_sec as u128;
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

/// Send a GSO super-buffer on Linux.
/// One sendmsg() with UDP_SEGMENT cmsg sends up to 64 packets.
#[cfg(target_os = "linux")]
fn send_gso_batch(
    socket: &Arc<UdpSocket>,
    target: SocketAddr,
    buf: &[u8],
    segment_size: usize,
) -> Result<u64> {
    use std::os::unix::io::AsRawFd;

    let fd = socket.as_raw_fd();
    let addr: libc::sockaddr_in = match target {
        SocketAddr::V4(v4) => {
            let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            addr.sin_family = libc::AF_INET as libc::sa_family_t;
            addr.sin_port = v4.port().to_be();
            addr.sin_addr.s_addr = u32::from_ne_bytes(v4.ip().octets());
            addr
        }
        _ => anyhow::bail!("IPv6 not yet supported for GSO"),
    };

    let seg_size = segment_size as u16;
    let mut cmsg_buf = [0u8; 64];
    let cmsg_len = unsafe {
        let cmsg = cmsg_buf.as_mut_ptr() as *mut libc::cmsghdr;
        (*cmsg).cmsg_level = libc::SOL_UDP;
        (*cmsg).cmsg_type = libc::UDP_SEGMENT;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<u16>() as u32) as usize;
        let data_ptr = libc::CMSG_DATA(cmsg) as *mut u16;
        *data_ptr = seg_size;
        libc::CMSG_SPACE(std::mem::size_of::<u16>() as u32) as usize
    };

    let iov = libc::iovec {
        iov_base: buf.as_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
    };

    let msg = libc::msghdr {
        msg_name: &addr as *const _ as *mut libc::c_void,
        msg_namelen: std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        msg_iov: &iov as *const _ as *mut libc::iovec,
        msg_iovlen: 1,
        msg_control: cmsg_buf.as_mut_ptr() as *mut libc::c_void,
        msg_controllen: cmsg_len as _,
        msg_flags: 0,
    };

    let ret = unsafe { libc::sendmsg(fd, &msg, 0) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::WouldBlock {
            std::thread::sleep(Duration::from_micros(50));
            let ret = unsafe { libc::sendmsg(fd, &msg, 0) };
            if ret >= 0 {
                return Ok(ret as u64);
            }
        }
        return Err(err.into());
    }
    Ok(ret as u64)
}
