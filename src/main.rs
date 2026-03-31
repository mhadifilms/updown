use std::net::SocketAddr;
use std::path::{Path, PathBuf};

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

            // Try QUIC auto-negotiation first
            let negotiated = negotiate_send(&file, to, block_size).await;

            let (shared_key, data_addr, session_id) = match negotiated {
                Ok((key, addr, sid)) => {
                    println!("Negotiated via QUIC control channel");
                    (key, addr, sid)
                }
                Err(_) => {
                    // Fall back to pre-shared key mode
                    let key = parse_hex_key(&key)?;
                    (key, to, rand::random())
                }
            };

            let engine = SendEngine::new(rate, rate_mode)
                .with_block_size(block_size)
                .with_repair_ratio(fec_ratio)
                .with_interleave(interleave)
                .with_compression(compress);

            let result = engine
                .send_file_with_session(&file, data_addr, &shared_key, session_id)
                .await?;

            println!("\n--- Transfer Complete ---");
            println!("  File size:     {} bytes", result.file_size);
            println!("  Bytes sent:    {} bytes (with FEC)", result.total_bytes_sent);
            println!("  Packets sent:  {}", result.total_packets_sent);
            println!("  Time:          {:.2?}", result.elapsed);
            println!("  Rate:          {:.1} Mbps", result.rate_mbps);
            println!(
                "  FEC overhead:  {:.1}%",
                ((result.total_bytes_sent as f64 / result.file_size as f64) - 1.0) * 100.0
            );
            println!("  BLAKE3 hash:   {}", hex::encode(result.file_hash));
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

/// Try to negotiate transfer via QUIC control channel with a `serve` endpoint.
/// Returns (shared_key, data_addr, session_id) on success.
async fn negotiate_send(
    file_path: &Path,
    server_addr: SocketAddr,
    block_size: usize,
) -> Result<([u8; 32], SocketAddr, u32)> {
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

    let data_addr = SocketAddr::new(server_addr.ip(), data_port);
    Ok((shared_key, data_addr, session_id))
}

async fn run_serve(bind: SocketAddr, output: PathBuf, rate_mbps: u64) -> Result<()> {
    println!("=== updown server ===");
    println!("  Listening on:  {}", bind);
    println!("  Output dir:    {}", output.display());
    println!("  Target rate:   {} Mbps", rate_mbps);
    println!();
    println!("Waiting for incoming transfers...");
    println!("  Sender runs: updown send <file> --to {}",  bind);
    println!();

    // QUIC control server on the bind port
    let control = ControlServer::bind(bind).await?;

    loop {
        // Accept incoming QUIC connection from sender
        let mut conn = control.accept().await?;

        // Receive transfer request
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

        // Generate shared encryption key and send accept with key exchange
        let shared_key: [u8; 32] = rand::random();

        // Pick a UDP data port (use bind port + 1)
        let data_port = bind.port() + 1;
        let data_addr = SocketAddr::new(bind.ip(), data_port);

        // Send accept with key exchange
        conn.send_msg(&ControlMessage::TransferAccept {
            session_id,
            data_port,
        })
        .await?;

        // Exchange encryption key (simplified: send key over the TLS-encrypted QUIC channel)
        conn.send_msg(&ControlMessage::KeyExchange {
            session_id,
            public_key: shared_key.to_vec(),
        })
        .await?;

        // Start receiving
        let engine = RecvEngine::new(output.clone())
            .with_block_size(block_size)
            .with_target_rate(rate_mbps);

        let result = engine
            .receive_file(
                data_addr,
                session_id,
                &filename,
                file_size,
                total_blocks,
                &shared_key,
            )
            .await?;

        // Send completion
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
        println!("Waiting for next transfer...");
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

    println!();
    println!("=== Results ===");
    println!("  Total time:      {:.2?}", total_elapsed);
    println!(
        "  Effective rate:  {:.1} Mbps",
        (file_size as f64 * 8.0) / (total_elapsed.as_secs_f64() * 1_000_000.0)
    );
    println!("  Send rate:       {:.1} Mbps", send_result.rate_mbps);
    println!("  Recv rate:       {:.1} Mbps", recv_result.rate_mbps);
    println!("  Packets sent:    {}", send_result.total_packets_sent);
    println!("  Packets recv:    {}", recv_result.total_packets);
    println!(
        "  FEC overhead:    {:.1}%",
        ((send_result.total_bytes_sent as f64 / file_size as f64) - 1.0) * 100.0
    );
    println!("  Excess symbols:  {}", recv_result.total_excess_symbols);
    println!("  Loss estimate:   {:.2}%", recv_result.final_loss_estimate * 100.0);
    println!("  Adaptive FEC:    {:.1}%", recv_result.final_fec_ratio * 100.0);
    println!(
        "  Integrity:       {}",
        if hashes_match { "PASS" } else { "FAIL" }
    );
    println!("  Source hash:     {}", hex::encode(send_result.file_hash));
    println!("  Received hash:   {}", hex::encode(recv_result.blake3_hash));

    if !hashes_match {
        anyhow::bail!("File integrity check failed!");
    }

    Ok(())
}
