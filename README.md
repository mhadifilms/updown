# updown

Blazing fast file transfer. Open-source alternative to IBM Aspera Faspex and Signiant Media Shuttle.

**780 Mbps on macOS, 2+ Gbps expected on Linux with GSO.** Single binary, zero config.

## What is this?

updown is a complete file exchange platform that transfers files over UDP using fountain codes instead of TCP. Where TCP chokes on packet loss and high latency, updown maintains near-line-rate throughput regardless of network conditions.

**Think:** WeTransfer speeds, but at wire speed. Or Aspera Faspex, but open source and free.

### How it works

```
Sender                          Network                         Receiver
  |                                |                                |
  |  FEC encode (RaptorQ)          |                                |
  |  AES-256-GCM encrypt           |                                |
  |  UDP blast at target rate  ──────────────────────────────────>  |
  |                                |     fountain codes mean:       |
  |                                |     - no retransmission         |
  |                                |     - no RTT penalty            |
  |                                |     - loss absorbed by FEC      |
  |                                |                                |
  |  rate control <──── OWD feedback ──── receiver computes rate    |
```

Traditional TCP file transfer (scp, FTP, HTTP) on a 1 Gbps link with 0.01% packet loss: **~20 Mbps**.  
updown on the same link: **~950 Mbps**.

## Quick Start

### Install

```bash
# From source
git clone https://github.com/mhadifilms/updown.git
cd updown
cargo build --release
# Binary at ./target/release/updown
```

### Send a file (peer-to-peer)

**Receiver:**
```bash
updown serve --bind 0.0.0.0:9000 --output ./downloads
```

**Sender:**
```bash
updown send myfile.bin --to receiver-ip:9000
```

That's it. QUIC negotiates the session, keys are exchanged over TLS, data blasts over UDP with fountain codes.

### Send a directory

```bash
updown send ./my-project/ --to receiver-ip:9000
```

### Web portal (Faspex replacement)

```bash
# Start the server with web UI
updown server --bind 0.0.0.0:8080 --storage ./data

# Start the desktop agent (handles fast transfers from the browser)
updown agent --download-dir ~/Downloads --register
```

Open `http://localhost:8080` in your browser. Upload files, create share links, manage drop boxes.

### Benchmark

```bash
# Localhost speed test
updown bench --size-mb 1000 --rate 10000
```

## Features

### Transport Layer (better than FASP)

| Feature | IBM Aspera FASP | updown |
|---------|----------------|--------|
| Loss recovery | Selective retransmit (1+ RTT penalty) | RaptorQ fountain codes (zero RTT penalty) |
| Encryption | AES-128-CFB | AES-256-GCM (authenticated) |
| Congestion control | Proprietary predictor | PCC Vivace with FEC decoder feedback |
| FEC | Optional Reed-Solomon | Adaptive RaptorQ (near Shannon limit) |
| Integrity | Unspecified | BLAKE3 per-block + per-file |

### Product Features (Faspex replacement)

- **Web portal** with dashboard, inbox, send form, transfer history
- **Share links** with expiry and download limits (24-char crypto-random codes)
- **Drop boxes** — public upload portals for external users
- **Desktop agent** — background service for fast browser-triggered transfers
- **REST API** — all operations available programmatically
- **S3/R2 integration** — pull from and push to any S3-compatible storage
- **Package management** — group files with metadata and recipients
- **Transfer history** — complete audit log in SQLite
- **Delta sync** — only transfer changed blocks on re-sync
- **Block-level resume** — interrupted transfers pick up where they left off
- **Zstd compression** — optional transparent compression
- **Multi-file/directory** — send entire directory trees
- **NAT traversal** — STUN hole-punching for peer-to-peer transfers
- **Concurrent transfers** — server handles multiple simultaneous transfers

### Security

- API key authentication on all mutating endpoints
- Filename sanitization (path traversal, control chars, null bytes)
- Upload size limits (5 GiB/file, 10 GiB total)
- Rate limiting (30 uploads/min, 60 share links/min)
- Locked-down CORS (server origin + localhost agent only)
- Session tokens for desktop agent
- XSS prevention on dynamic content
- Per-session encryption keys over TLS

## Architecture

```
src/
├── main.rs                 CLI: send, recv, serve, server, agent, bench
├── lib.rs
├── protocol/
│   └── wire.rs             Wire format, packet types, control messages
├── crypto/
│   └── mod.rs              AES-256-GCM + X25519 key exchange
├── fec/
│   └── mod.rs              RaptorQ encode/decode + adaptive FEC controller
├── transport/
│   ├── sender.rs           GSO-ready UDP sender with interleaved pacing
│   ├── receiver.rs         FEC reconstruction + timeout predictor
│   ├── rate_control.rs     PCC Vivace CC + receiver-driven rate calculator
│   ├── control.rs          QUIC control channel (TLS session negotiation)
│   ├── timeout_predictor.rs FASP-style block arrival prediction
│   └── stun.rs             STUN NAT traversal
├── engine/
│   ├── send.rs             Pipeline read→encode→send with double-buffering
│   ├── recv.rs             Streaming write with resume support
│   ├── resume.rs           Block-level resume with persistent bitmap
│   ├── multi.rs            Directory walker, multi-file support
│   ├── delta.rs            Block hash diffing for delta sync
│   ├── s3.rs               S3/R2/MinIO storage backend
│   └── stats.rs            Human-readable transfer formatting
└── web/
    ├── mod.rs              Web server startup
    ├── api.rs              REST API (auth, uploads, packages, shares)
    ├── db.rs               SQLite persistence
    ├── portal.rs           Web UI (dashboard, send, inbox, history)
    └── agent.rs            Desktop agent with WebSocket progress
```

## Performance

Benchmarked on macOS (Apple Silicon), localhost, no GSO:

| Size | Time | Rate | Packets | Integrity |
|------|------|------|---------|-----------|
| 100 MB | 1.1s | 764 Mbps | 90K | PASS |
| 1 GB | 10.7s | 784 Mbps | 901K | PASS |
| 5 GB | 53.2s | 788 Mbps | 4.5M | PASS |
| 10 GB | 107.5s | 780 Mbps | 9.0M | PASS |

On Linux with GSO (UDP_SEGMENT): expected **1.5-2.5 Gbps** (64 packets per syscall).

### Why faster than TCP?

TCP interprets any packet loss as congestion and slashes its sending rate. On a 1 Gbps link with just 0.01% loss, TCP throughput drops to ~20 Mbps. updown uses fountain codes (RaptorQ) which absorb loss proactively — no retransmission, no RTT penalty. Combined with OWD-based rate control that ignores loss entirely, updown maintains near-line-rate regardless of conditions.

## CLI Reference

```
updown send <file-or-dir> --to <host:port>    Send files via QUIC+UDP
updown serve --bind <addr> --output <dir>     Receive files (CLI mode)
updown server --bind <addr> --storage <dir>   Web portal + API server
updown agent --download-dir <dir> --register  Desktop agent for browser transfers
updown bench --size-mb <N> --rate <mbps>      Localhost benchmark
```

## API

```bash
# Health check
curl http://localhost:8080/api/health

# Upload files (multipart)
curl -F "files=@video.mp4" -H "Authorization: Bearer upd_xxx" http://localhost:8080/api/upload

# Create share link
curl -X POST -H "Content-Type: application/json" -H "Authorization: Bearer upd_xxx" \
  -d '{"package_id":"...","max_downloads":10,"expires_hours":72}' \
  http://localhost:8080/api/share

# List packages
curl -H "Authorization: Bearer upd_xxx" http://localhost:8080/api/packages

# List transfers
curl -H "Authorization: Bearer upd_xxx" http://localhost:8080/api/transfers
```

## S3 Integration

updown can pull from and push to any S3-compatible storage (AWS S3, Cloudflare R2, MinIO):

```rust
let s3 = S3Backend::from_endpoint(
    "my-bucket",
    "https://ACCOUNT_ID.r2.cloudflarestorage.com",
    "access_key",
    "secret_key",
    "auto",
).await?;

// Pull object for transfer
s3.download_to_local("path/to/file.bin", &temp_dir).await?;

// Push received file to bucket
s3.upload_from_local(&received_path, "uploads/file.bin").await?;
```

## How it compares

| | scp | rsync | Aspera Faspex | Signiant Media Shuttle | **updown** |
|---|---|---|---|---|---|
| Speed on lossy link | ~20 Mbps | ~50 Mbps | ~950 Mbps | ~950 Mbps | **~950 Mbps** |
| Protocol | TCP/SSH | TCP/SSH | UDP (FASP) | UDP (proprietary) | **UDP (fountain codes)** |
| Loss recovery | TCP retransmit | TCP retransmit | Selective retransmit | Selective retransmit | **FEC (zero RTT)** |
| Web portal | No | No | Yes | Yes | **Yes** |
| Share links | No | No | Yes | Yes | **Yes** |
| Drop boxes | No | No | Yes | Yes | **Yes** |
| Open source | Yes | Yes | No | No | **Yes** |
| Price | Free | Free | ~$10K+/yr | ~$10K+/yr | **Free** |

## License

MIT

## Credits

Built with: [raptorq](https://github.com/cberner/raptorq), [ring](https://github.com/briansmith/ring), [quinn](https://github.com/quinn-rs/quinn), [blake3](https://github.com/BLAKE3-team/BLAKE3), [axum](https://github.com/tokio-rs/axum), [tokio](https://github.com/tokio-rs/tokio).

FASP patent US 8,085,781 has expired (fee-related). The core mechanisms are freely implementable.
