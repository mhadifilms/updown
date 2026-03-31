use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use updown::engine::{RecvEngine, SendEngine};
use updown::protocol::DEFAULT_BLOCK_SIZE;
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

        /// Pre-shared key (hex encoded, 32 bytes). For testing only.
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

        /// Block size in bytes (must match sender)
        #[arg(long, default_value_t = DEFAULT_BLOCK_SIZE)]
        block_size: usize,

        /// Pre-shared key (hex encoded, 32 bytes). For testing only.
        #[arg(long, default_value = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")]
        key: String,

        /// Session ID (must match sender)
        #[arg(long, default_value = "1")]
        session: u32,
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
            key,
            session: _,
        } => {
            let shared_key = parse_hex_key(&key)?;
            let rate_mode = parse_rate_mode(&mode);

            let engine = SendEngine::new(rate, rate_mode)
                .with_block_size(block_size)
                .with_repair_ratio(fec_ratio);

            let result = engine.send_file(&file, to, &shared_key).await?;

            println!("\n--- Transfer Complete ---");
            println!("  File size:     {} bytes", result.file_size);
            println!("  Bytes sent:    {} bytes (with FEC overhead)", result.total_bytes_sent);
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
            block_size,
            key,
            session,
        } => {
            let shared_key = parse_hex_key(&key)?;
            let total_blocks = ((size as usize + block_size - 1) / block_size) as u32;

            let engine = RecvEngine::new(output).with_block_size(block_size);

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
        }

        Commands::Bench {
            size_mb,
            rate,
            fec_ratio,
            block_size,
        } => {
            run_benchmark(size_mb, rate, fec_ratio, block_size).await?;
        }
    }

    Ok(())
}

async fn run_benchmark(size_mb: u64, rate_mbps: u64, fec_ratio: f32, block_size: usize) -> Result<()> {
    use std::time::Instant;

    println!("=== updown benchmark ===");
    println!("  Data size:     {} MB", size_mb);
    println!("  Target rate:   {} Mbps", rate_mbps);
    println!("  FEC ratio:     {:.0}%", fec_ratio * 100.0);
    println!("  Block size:    {} bytes", block_size);
    println!();

    let file_size = size_mb * 1024 * 1024;
    let total_blocks = ((file_size as usize + block_size - 1) / block_size) as u32;
    let shared_key = [42u8; 32];
    let session_id: u32 = rand::random();

    // Create test data file
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

    // Start receiver
    let recv_addr: SocketAddr = "127.0.0.1:9876".parse()?;
    let output_dir = temp_dir.path().join("output");
    std::fs::create_dir_all(&output_dir)?;

    let recv_key = shared_key;
    let recv_output = output_dir.clone();
    let recv_handle = tokio::spawn(async move {
        let engine = RecvEngine::new(recv_output).with_block_size(block_size);
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

    // Give receiver a moment to bind
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Start sender
    let start = Instant::now();

    let send_engine = SendEngine::new(rate_mbps, RateMode::Fixed)
        .with_block_size(block_size)
        .with_repair_ratio(fec_ratio);

    let send_result = send_engine
        .send_file_with_session(&test_file, recv_addr, &shared_key, session_id)
        .await?;

    // Wait for receiver
    let recv_result = recv_handle.await??;
    let total_elapsed = start.elapsed();

    // Verify
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
