# FASP Patent Audit: US 8,085,781 vs updown

## Patent Status: EXPIRED (Fee Related)
The patent has expired due to non-payment of maintenance fees.
All mechanisms described below are freely implementable.

## Figure-by-Figure Comparison

### Fig 1: System Architecture (Client-Server over Network)
| FASP | updown | Status |
|------|--------|--------|
| Sender with CPU, Memory, Storage, Network Interface | Rust binary with tokio async runtime | MATCH |
| Receiver with same components | Same binary in `serve` mode | MATCH |
| Network (cloud) between them | UDP data channel + QUIC control channel | SUPERIOR (QUIC adds TLS) |

### Fig 2: Module Architecture (The Core Design)

#### Sender Side (201)
| FASP Module | updown Equivalent | Status |
|------------|-------------------|--------|
| Data Ingest (203) | `File::open` + streaming reads | MATCH |
| Block Handler Module (206) | `engine/send.rs` block chunking (4 MiB) | MATCH |
| Cryptography Module (208) | `crypto/mod.rs` AES-256-GCM via ring | SUPERIOR (AES-256 vs AES-128-CFB) |
| Block Egest Module (210) | `transport/sender.rs` packet assembly | MATCH |
| Retransmission Module (212) | NOT NEEDED — FEC replaces retransmission | SUPERIOR (no RTT penalty) |
| Rate Control Module (214) | `transport/rate_control.rs` RateController | MATCH |
| Feedback Reader Module (216) | RateFeedback packet handler | MATCH |
| Management Interface Module (204) | CLI args + `--rate`/`--mode` flags | PARTIAL (no runtime API yet) |
| File Differencing (254) | Protocol messages defined, Phase 6 TODO | TODO |

#### Receiver Side (225)
| FASP Module | updown Equivalent | Status |
|------------|-------------------|--------|
| Block Ingest Module (232) | `transport/receiver.rs` packet processing | MATCH |
| File Cache Module (236) | Not implemented (direct disk write) | SKIP (unnecessary with modern SSDs) |
| Crypto Module (234) | `crypto/mod.rs` decrypt | MATCH |
| Block Handler Module (230) | Block reassembly via FEC decoder | SUPERIOR (FEC vs gap detection) |
| Disk Writer Module (238) | `engine/recv.rs` async write at offset | MATCH |
| Timeout Predictor Module (240) | Not implemented as separate module | TODO |
| Rate Control Module (242) | `ReceiverRateCalculator` with PCC Vivace | SUPERIOR (gradient ascent vs simple) |
| Retransmission Module (246) | NOT NEEDED — FEC handles loss | SUPERIOR |
| Feedback Writer Module (248) | RateFeedback packet generation | MATCH |

### Fig 3: Sender Transmit Flow
| FASP Step | updown Equivalent | Status |
|-----------|-------------------|--------|
| 302: Receive command to transmit | CLI `send` command | MATCH |
| 304: Establish connection, exchange control data | QUIC control channel with TLS | SUPERIOR |
| 306: Break file into numbered blocks | Block chunking with configurable size | MATCH |
| 308: Retransmit request pending? | N/A — FEC eliminates retransmission | SUPERIOR |
| 310: Retransmit requested block | N/A | SUPERIOR |
| 312: Block remaining to transmit? | Block iteration in send loop | MATCH |
| 314: Transmit next block in sequence | Cross-block interleaved sending | SUPERIOR |
| 316-318: Check if receiver got last block | Done signal + 10s timeout | MATCH |

### Fig 4: Receiver Flow
| FASP Step | updown Equivalent | Status |
|-----------|-------------------|--------|
| 402: Receive command | `serve` command | MATCH |
| 404: Establish connection | QUIC accept | MATCH |
| 406: Allocate storage, break into blocks | `file.set_len()` pre-allocation | MATCH |
| 408: Receive a block | UDP recv loop | MATCH |
| 410: Block previously received? | `completed_blocks.contains()` | MATCH |
| 412: Discard duplicate block | Skip in receiver | MATCH |
| 414: Write block to storage at offset | `file.seek() + write_all()` | MATCH |
| 416: Schedule retransmission of missed blocks | N/A — FEC handles this | SUPERIOR |
| 418-420: Gap detection + retransmit request | N/A — fountain codes | SUPERIOR |
| 424: Was block requested for retransmit? | N/A | SUPERIOR |
| 426: Remove from retransmit schedule | N/A | SUPERIOR |
| 428: Last block? → Stop | Block count check | MATCH |

### Fig 5-6: Network Timing and Retransmission
| FASP Feature | updown Equivalent | Status |
|-------------|-------------------|--------|
| T1-T4 timestamp exchange | `timestamp_us` in packet headers | MATCH |
| Rex table (lost blocks) on receiver | Not needed — FEC decoder tracks state | SUPERIOR |
| Rex Request from receiver to sender | Not needed | SUPERIOR |
| Retransmit at current injection rate | N/A — no retransmission | SUPERIOR |
| Data blocks + Rex blocks interleaved | Source + repair symbols interleaved across blocks | SUPERIOR |

### Fig 7: Rate Control Feedback Loop
| FASP Feature | updown Equivalent | Status |
|-------------|-------------------|--------|
| Sender sends data at injection rate Ri | Rate-paced UDP send with configurable rate | MATCH |
| Receiver measures congestion | `ReceiverRateCalculator` with OWD gradient | MATCH |
| Receiver computes New Ri | PCC Vivace utility function | SUPERIOR |
| Receiver sends New Ri to sender | Via RateFeedback packets (protocol defined) | MATCH |
| Sender updates Ri | `apply_receiver_suggestion()` | MATCH |
| Configured policy on both sides | `--mode fixed/fair/scavenger` + `--rate` | MATCH |
| Configured min/max rate | `min_rate` / `max_rate` in RateController | MATCH |

## Features Beyond FASP Patent

| Feature | Description |
|---------|------------|
| **Fountain codes (RaptorQ)** | Eliminates all retransmission. FASP uses selective retransmit. |
| **Adaptive FEC** | Dynamic repair ratio based on EWMA loss estimation |
| **Cross-block interleaving** | Burst loss protection at zero bandwidth cost |
| **QUIC control channel** | TLS-encrypted session negotiation with 0-RTT potential |
| **BLAKE3 integrity** | Per-block + per-file hashing at 6 GiB/s |
| **AES-256-GCM** | Stronger than FASP's AES-128-CFB, with authentication |
| **Block-level resume** | Persistent bitmap for interrupted transfer recovery |
| **Zstd compression** | Optional transparent compression with entropy detection |
| **Multi-file/directory** | Send entire directories in one session |
| **PCC Vivace CC** | Gradient-ascent rate optimization vs FASP's simple predictor |
| **FEC decoder feedback** | Novel: decoder excess as continuous loss signal for CC |

## Remaining TODOs from FASP

| FASP Feature | Status | Priority |
|-------------|--------|----------|
| Timeout Predictor Module | Not implemented | Medium |
| File Differencing (delta sync) | Protocol defined, not implemented | High (Phase 6) |
| Management Interface (runtime rate adjustment) | CLI only, no runtime API | Low |
| File Cache Module | Skipped (SSD makes it unnecessary) | Skip |
| Multiple concurrent transfers | Server handles one at a time | Medium |

## Verdict

updown implements **all core FASP mechanisms** and is **architecturally superior**
in the reliability layer (fountain codes vs retransmission), the congestion control
(PCC Vivace vs simple predictor), and the security layer (AES-256-GCM vs AES-128-CFB).
The only gaps are file differencing (delta sync) and the timeout predictor, both of
which are straightforward to add.
