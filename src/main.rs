use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use updown::engine::{RecvEngine, SendEngine};
use updown::protocol::{ControlMessage, DEFAULT_BLOCK_SIZE};
use updown::transport::control::{ControlClient, ControlServer};
use updown::transport::rate_control::RateMode;

#[derive(Parser)]
#[command(
    name = "updown",
    about = "Blazing fast file transfer using UDP, fountain codes, and rate-based pacing",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Send a file to a receiver
    Send {
        /// Path to file to send
        file: PathBuf,

        /// Receiver address (host:port)
        #[arg(short, long)]
        to: SocketAddr,

        /// Target transfer rate in Mbps
        #[arg(short, long, default_value = "1000")]
        rate: u64,

        /// Rate control mode: fixed, fair, or scavenger
        #[arg(short, long, default_value = "fixed")]
        mode: String,

        /// FEC repair symbol ratio (0.0-1.0, higher = more loss tolerance)
        #[arg(long, default_value = "0.15")]
        fec_ratio: f32,

        /// Block size in bytes (default 4MB)
        #[arg(long, default_value_t = DEFAULT_BLOCK_SIZE)]
        block_size: usize,

        /// Number of blocks to send in parallel (interleaved)
        #[arg(long, default_value = "4")]
        interleave: usize,

        /// Enable zstd compression
        #[arg(long)]
        compress: bool,

        /// Pre-shared key (hex encoded, 32 bytes)
        #[arg(long, default_value = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")]
        key: String,

        /// Session ID (must match receiver)
        #[arg(long, default_value = "1")]
        session: u32,
    },
    /// Receive a file from a sender
    Recv {
        /// Address to listen on (host:port)
        #[arg(short, long, default_value = "0.0.0.0:9000")]
        bind: SocketAddr,

        /// Output directory
        #[arg(short, long, default_value = ".")]
        output: PathBuf,

        /// Expected filename
        #[arg(short, long)]
        filename: String,

        /// Expected file size in bytes
        #[arg(short, long)]
        size: u64,

        /// Target rate in Mbps (for receiver rate calculator)
        #[arg(short, long, default_value = "1000")]
        rate: u64,

        /// Block size in bytes (must match sender)
        #[arg(long, default_value_t = DEFAULT_BLOCK_SIZE)]
        block_size: usize,

        /// Pre-shared key (hex encoded, 32 bytes)
        #[arg(long, default_value = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")]
        key: String,

        /// Session ID (must match sender)
        #[arg(long, default_value = "1")]
        session: u32,
    },
    /// Start a receiver that auto-accepts transfers via QUIC negotiation.
    /// Usage: updown serve --output ./downloads
    Serve {
        /// Address to listen on (QUIC control + UDP data)
        #[arg(short, long, default_value = "0.0.0.0:9000")]
        bind: SocketAddr,

        /// Output directory for received files
        #[arg(short, long, default_value = ".")]
        output: PathBuf,

        /// Target rate in Mbps
        #[arg(short, long, default_value = "10000")]
        rate: u64,
    },
    /// Run a localhost benchmark (send + receive in same process)
    Bench {
        /// Size of test data in MB
        #[arg(short, long, default_value = "100")]
        size_mb: u64,

        /// Target transfer rate in Mbps
        #[arg(short, long, default_value = "10000")]
        rate: u64,

        /// FEC repair symbol ratio
        #[arg(long, default_value = "0.1")]
        fec_ratio: f32,

        /// Block size in bytes
        #[arg(long, default_value_t = DEFAULT_BLOCK_SIZE)]
        block_size: usize,

        /// Number of blocks to interleave
        #[arg(long, default_value = "4")]
        interleave: usize,
    },
}

fn parse_hex_key(hex: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(hex).map_err(|e| anyhow::anyhow!("invalid hex key: {}", e))?;
    if bytes.len() != 32 {
        anyhow::bail!("key must be exactly 32 bytes (64 hex chars)");
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

fn parse_rate_mode(mode: &str) -> RateMode {
    match mode.to_lowercase().as_str() {
        "fair" => RateMode::Fair,
        "scavenger" => RateMode::Scavenger,
        _ => RateMode::Fixed,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Send {
            file,
            to,
            rate,
            mode,
            fec_ratio,
            block_size,
            interleave,
            compress,
            key,
            session: _,
        } => {
            let rate_mode = parse_rate_mode(&mode);

            // Collect files to send (single file or directory)
            let files = if file.is_dir() {
                updown::engine::multi::walk_directory(&file).await?
            } else {
                vec![updown::engine::multi::single_file_entry(&file).await?]
            };

            let total_size: u64 = files.iter().map(|(_, e)| e.file_size).sum();
            println!(
                "Sending {} file(s), {} total",
                files.len(),
                updown::engine::multi::format_bytes(total_size)
            );

            let engine = SendEngine::new(rate, rate_mode)
                .with_block_size(block_size)
                .with_repair_ratio(fec_ratio)
                .with_interleave(interleave)
                .with_compression(compress);

            let start = std::time::Instant::now();
            let mut total_bytes_sent = 0u64;
            let mut total_packets = 0u64;

            for (i, (path, entry)) in files.iter().enumerate() {
                println!(
                    "[{}/{}] {} ({})",
                    i + 1,
                    files.len(),
                    entry.relative_path,
                    updown::engine::multi::format_bytes(entry.file_size)
                );

                // Negotiate each file via QUIC (or fall back to PSK)
                let negotiated = negotiate_send(path, to, block_size).await;
                let (shared_key, data_addr, session_id, _delta_hashes) = match negotiated {
                    Ok(nr) => {
                        if !nr.receiver_block_hashes.is_empty() {
                            // Delta sync available — compute which blocks changed
                            let source_hashes = updown::engine::delta::compute_block_hashes(path, block_size).await?;
                            let changed = updown::engine::delta::diff_block_hashes(&source_hashes, &nr.receiver_block_hashes);
                            let stats = updown::engine::delta::DeltaSyncStats::from_diff(
                                source_hashes.len() as u32, &changed
                            );
                            println!(
                                "    Delta: {}/{} blocks changed ({:.0}% savings)",
                                stats.changed_blocks, stats.total_blocks, stats.savings_percent
                            );
                        }
                        (nr.shared_key, nr.data_addr, nr.session_id, nr.receiver_block_hashes)
                    }
                    Err(_) => {
                        let key = parse_hex_key(&key)?;
                        (key, to, rand::random(), Vec::new())
                    }
                };

                let result = engine
                    .send_file_with_session(path, data_addr, &shared_key, session_id)
                    .await?;

                total_bytes_sent += result.total_bytes_sent;
                total_packets += result.total_packets_sent;
            }

            let elapsed = start.elapsed();
            let rate_mbps = (total_size as f64 * 8.0) / (elapsed.as_secs_f64() * 1_000_000.0);
            println!("\n--- Transfer Complete ---");
            println!("  Files:         {}", files.len());
            println!("  Total size:    {}", updown::engine::multi::format_bytes(total_size));
            println!("  Bytes sent:    {}", updown::engine::multi::format_bytes(total_bytes_sent));
            println!("  Packets:       {}", total_packets);
            println!("  Time:          {:.2?}", elapsed);
            println!("  Rate:          {:.1} Mbps", rate_mbps);
        }

        Commands::Recv {
            bind,
            output,
            filename,
            size,
            rate,
            block_size,
            key,
            session,
        } => {
            let shared_key = parse_hex_key(&key)?;
            let total_blocks = ((size as usize + block_size - 1) / block_size) as u32;

            let engine = RecvEngine::new(output)
                .with_block_size(block_size)
                .with_target_rate(rate);

            println!("Waiting for transfer...");
            println!("  Expecting: {} ({} bytes, {} blocks)", filename, size, total_blocks);

            let result = engine
                .receive_file(bind, session, &filename, size, total_blocks, &shared_key)
                .await?;

            println!("\n--- Receive Complete ---");
            println!("  Output:        {}", result.output_path.display());
            println!("  File size:     {} bytes", result.file_size);
            println!("  Packets recv:  {}", result.total_packets);
            println!("  Blocks recv:   {}", result.blocks_received);
            println!("  Time:          {:.2?}", result.elapsed);
            println!("  Rate:          {:.1} Mbps", result.rate_mbps);
            println!("  BLAKE3 hash:   {}", hex::encode(result.blake3_hash));
            println!("  Loss estimate: {:.2}%", result.final_loss_estimate * 100.0);
            println!("  FEC ratio rec: {:.1}%", result.final_fec_ratio * 100.0);
        }

        Commands::Serve {
            bind,
            output,
            rate,
        } => {
            run_serve(bind, output, rate).await?;
        }

        Commands::Bench {
            size_mb,
            rate,
            fec_ratio,
            block_size,
            interleave,
        } => {
            run_benchmark(size_mb, rate, fec_ratio, block_size, interleave).await?;
        }
    }

    Ok(())
}

/// Result of QUIC negotiation
struct NegotiateResult {
    shared_key: [u8; 32],
    data_addr: SocketAddr,
    session_id: u32,
    /// Block hashes from receiver (for delta sync). Empty if no existing file.
    receiver_block_hashes: Vec<[u8; 32]>,
}

/// Try to negotiate transfer via QUIC control channel with a `serve` endpoint.
async fn negotiate_send(
    file_path: &Path,
    server_addr: SocketAddr,
    block_size: usize,
) -> Result<NegotiateResult> {
    use tokio::fs::File;
    use tokio::io::AsyncReadExt;

    let metadata = tokio::fs::metadata(file_path).await?;
    let file_size = metadata.len();
    let filename = file_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let total_blocks = ((file_size as usize + block_size - 1) / block_size) as u32;

    // Hash the file
    let file_hash = {
        let mut hasher = blake3::Hasher::new();
        let mut f = File::open(file_path).await?;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = f.read(&mut buf).await?;
            if n == 0 { break; }
            hasher.update(&buf[..n]);
        }
        *hasher.finalize().as_bytes()
    };

    let session_id: u32 = rand::random();

    // Connect to server's QUIC control channel
    let mut conn = ControlClient::connect(server_addr).await?;

    // Send transfer request
    conn.send_msg(&ControlMessage::TransferRequest {
        session_id,
        filename,
        file_size,
        block_size: block_size as u32,
        total_blocks,
        blake3_hash: file_hash,
        data_port: 0, // Server will tell us
    })
    .await?;

    // Receive accept
    let msg = conn.recv_msg().await?;
    let data_port = match msg {
        ControlMessage::TransferAccept { data_port, .. } => data_port,
        ControlMessage::TransferReject { reason, .. } => {
            anyhow::bail!("Transfer rejected: {}", reason);
        }
        _ => anyhow::bail!("Unexpected response"),
    };

    // Receive key exchange
    let msg = conn.recv_msg().await?;
    let shared_key = match msg {
        ControlMessage::KeyExchange { public_key, .. } => {
            let mut key = [0u8; 32];
            if public_key.len() != 32 {
                anyhow::bail!("Invalid key length");
            }
            key.copy_from_slice(&public_key);
            key
        }
        _ => anyhow::bail!("Expected KeyExchange"),
    };

    // Check for optional delta sync hashes (non-blocking: receiver may not send them)
    let receiver_block_hashes = match tokio::time::timeout(
        std::time::Duration::from_millis(500),
        conn.recv_msg(),
    ).await {
        Ok(Ok(ControlMessage::DeltaSyncHashes { block_hashes, .. })) => block_hashes,
        _ => Vec::new(), // No delta sync or timeout — full transfer
    };

    let data_addr = SocketAddr::new(server_addr.ip(), data_port);
    Ok(NegotiateResult {
        shared_key,
        data_addr,
        session_id,
        receiver_block_hashes,
    })
}

async fn run_serve(bind: SocketAddr, output: PathBuf, rate_mbps: u64) -> Result<()> {
    use std::sync::atomic::{AtomicU16, Ordering};

    println!("=== updown server ===");
    println!("  Listening on:  {}", bind);
    println!("  Output dir:    {}", output.display());
    println!("  Target rate:   {} Mbps", rate_mbps);
    println!();
    println!("Waiting for incoming transfers...");
    println!("  Sender runs: updown send <file> --to {}", bind);
    println!();

    let control = ControlServer::bind(bind).await?;

    // Atomic counter for dynamic data port allocation (concurrent transfers)
    let port_counter = Arc::new(AtomicU16::new(bind.port() + 1));

    loop {
        let mut conn = control.accept().await?;

        let msg = conn.recv_msg().await?;
        let (session_id, filename, file_size, block_size, total_blocks, file_hash) = match msg {
            ControlMessage::TransferRequest {
                session_id,
                filename,
                file_size,
                block_size,
                total_blocks,
                blake3_hash,
                data_port: _,
            } => (
                session_id,
                filename,
                file_size,
                block_size as usize,
                total_blocks,
                blake3_hash,
            ),
            _ => {
                eprintln!("Unexpected control message, expected TransferRequest");
                continue;
            }
        };

        println!(
            "Incoming: {} ({} bytes, {} blocks)",
            filename, file_size, total_blocks
        );

        // Allocate a unique data port for this transfer
        let data_port = port_counter.fetch_add(1, Ordering::Relaxed);
        let data_addr = SocketAddr::new(bind.ip(), data_port);

        let shared_key: [u8; 32] = rand::random();

        conn.send_msg(&ControlMessage::TransferAccept {
            session_id,
            data_port,
        })
        .await?;

        conn.send_msg(&ControlMessage::KeyExchange {
            session_id,
            public_key: shared_key.to_vec(),
        })
        .await?;

        // Delta sync: check if file already exists on receiver
        let existing_path = output.join(&filename);
        if existing_path.exists() && existing_path.is_file() {
            if let Ok(existing_meta) = tokio::fs::metadata(&existing_path).await {
                if existing_meta.len() == file_size {
                    // Same size — hash blocks and send delta info
                    if let Ok(hashes) = updown::engine::delta::compute_block_hashes(
                        &existing_path,
                        block_size,
                    ).await {
                        println!("  Delta sync: hashing {} existing blocks", hashes.len());
                        conn.send_msg(&ControlMessage::DeltaSyncHashes {
                            session_id,
                            file_index: 0,
                            block_hashes: hashes,
                        }).await.ok();
                    }
                }
            }
        }

        // Spawn the transfer in a background task (concurrent transfers)
        let output = output.clone();
        tokio::spawn(async move {
            let engine = RecvEngine::new(output)
                .with_block_size(block_size)
                .with_target_rate(rate_mbps);

            match engine
                .receive_file(
                    data_addr,
                    session_id,
                    &filename,
                    file_size,
                    total_blocks,
                    &shared_key,
                )
                .await
            {
                Ok(result) => {
                    conn.send_msg(&ControlMessage::TransferComplete {
                        session_id,
                        success: result.blake3_hash == file_hash,
                        bytes_transferred: result.file_size,
                        duration_ms: result.elapsed.as_millis() as u64,
                    })
                    .await
                    .ok();

                    let hash_match = result.blake3_hash == file_hash;
                    println!("\n--- Receive Complete ---");
                    println!("  File:          {}", result.output_path.display());
                    println!("  Size:          {} bytes", result.file_size);
                    println!("  Rate:          {:.1} Mbps", result.rate_mbps);
                    println!("  Time:          {:.2?}", result.elapsed);
                    println!(
                        "  Integrity:     {}",
                        if hash_match { "PASS" } else { "FAIL" }
                    );
                    println!();
                }
                Err(e) => {
                    eprintln!("Transfer failed: {}", e);
                }
            }
        });
    }
}

async fn run_benchmark(
    size_mb: u64,
    rate_mbps: u64,
    fec_ratio: f32,
    block_size: usize,
    interleave: usize,
) -> Result<()> {
    use std::time::Instant;

    println!("=== updown Phase 2 benchmark ===");
    println!("  Data size:     {} MB", size_mb);
    println!("  Target rate:   {} Mbps", rate_mbps);
    println!("  FEC ratio:     {:.0}%", fec_ratio * 100.0);
    println!("  Block size:    {} bytes", block_size);
    println!("  Interleave:    {} blocks", interleave);
    println!();

    let file_size = size_mb * 1024 * 1024;
    let total_blocks = ((file_size as usize + block_size - 1) / block_size) as u32;
    let shared_key = [42u8; 32];
    let session_id: u32 = rand::random();

    let temp_dir = tempfile::tempdir()?;
    let test_file = temp_dir.path().join("test_data.bin");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&test_file)?;
        let chunk = vec![0xABu8; 1024 * 1024];
        for i in 0..size_mb {
            let mut chunk = chunk.clone();
            chunk[0] = (i & 0xFF) as u8;
            chunk[1] = ((i >> 8) & 0xFF) as u8;
            f.write_all(&chunk)?;
        }
        f.flush()?;
    }

    let source_hash = blake3::hash(&std::fs::read(&test_file)?);
    println!("  Source hash:   {}", source_hash.to_hex());
    println!();

    let recv_addr: SocketAddr = "127.0.0.1:9876".parse()?;
    let output_dir = temp_dir.path().join("output");
    std::fs::create_dir_all(&output_dir)?;

    let recv_key = shared_key;
    let recv_output = output_dir.clone();
    let recv_handle = tokio::spawn(async move {
        let engine = RecvEngine::new(recv_output)
            .with_block_size(block_size)
            .with_target_rate(rate_mbps);
        engine
            .receive_file(
                recv_addr,
                session_id,
                "test_data.bin",
                file_size,
                total_blocks,
                &recv_key,
            )
            .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let start = Instant::now();

    let send_engine = SendEngine::new(rate_mbps, RateMode::Fixed)
        .with_block_size(block_size)
        .with_repair_ratio(fec_ratio)
        .with_interleave(interleave);

    let send_result = send_engine
        .send_file_with_session(&test_file, recv_addr, &shared_key, session_id)
        .await?;

    let recv_result = recv_handle.await??;
    let total_elapsed = start.elapsed();

    let hashes_match = send_result.file_hash == recv_result.blake3_hash;

    let effective_rate = (file_size as f64 * 8.0) / (total_elapsed.as_secs_f64() * 1_000_000.0);
    let fec_overhead = ((send_result.total_bytes_sent as f64 / file_size as f64) - 1.0) * 100.0;

    println!();
    println!(
        "{}",
        updown::engine::stats::format_benchmark_result(
            size_mb,
            send_result.rate_mbps,
            recv_result.rate_mbps,
            effective_rate,
            send_result.total_packets_sent,
            recv_result.total_packets,
            fec_overhead,
            recv_result.final_loss_estimate,
            recv_result.final_fec_ratio,
            recv_result.total_excess_symbols,
            total_elapsed,
            hashes_match,
        )
    );
    println!("  Source hash:     {}", hex::encode(send_result.file_hash));
    println!("  Received hash:   {}", hex::encode(recv_result.blake3_hash));

    if !hashes_match {
        anyhow::bail!("File integrity check failed!");
    }

    Ok(())
}
