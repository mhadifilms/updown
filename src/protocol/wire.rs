use serde::{Deserialize, Serialize};

/// Maximum UDP payload we'll send (fits in typical MTU with headers)
pub const MAX_UDP_PAYLOAD: usize = 1350;

/// Size of the packet header when serialized
pub const PACKET_HEADER_SIZE: usize = 32;

/// Maximum data per UDP packet after header + encryption overhead
pub const MAX_PACKET_DATA: usize = MAX_UDP_PAYLOAD - PACKET_HEADER_SIZE - 28; // 28 = AES-GCM tag + nonce overhead

/// Default block size for file chunking (4 MiB)
pub const DEFAULT_BLOCK_SIZE: usize = 4 * 1024 * 1024;

/// Default symbol size for RaptorQ encoding
/// Chosen to fit well in UDP packets after header + encryption
pub const SYMBOL_SIZE: u16 = 1280;

/// Magic bytes for packet identification
pub const MAGIC: [u8; 4] = [0x55, 0x50, 0x44, 0x4E]; // "UPDN"

/// Packet types on the data channel (UDP)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum PacketType {
    /// Encoded data symbol (source or repair)
    Data = 1,
    /// Block completion acknowledgment from receiver
    BlockAck = 2,
    /// Rate feedback from receiver (OWD measurements)
    RateFeedback = 3,
    /// Transfer complete signal
    Done = 4,
}

/// Header for every UDP packet on the data channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketHeader {
    pub magic: [u8; 4],
    pub packet_type: PacketType,
    /// Unique transfer session ID
    pub session_id: u32,
    /// Block index within the file
    pub block_id: u32,
    /// Encoding symbol ID within the block (for RaptorQ)
    pub symbol_id: u32,
    /// Sender timestamp in microseconds (for OWD measurement)
    pub timestamp_us: u64,
    /// Sequence number for ordering/loss detection
    pub seq_num: u32,
}

/// Data packet: header + encrypted encoded symbol
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataPacket {
    pub header: PacketHeader,
    /// Encrypted RaptorQ-encoded symbol data
    pub payload: Vec<u8>,
}

/// Receiver tells sender a block was fully reconstructed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockAck {
    pub header: PacketHeader,
    pub block_id: u32,
    pub blake3_hash: [u8; 32],
}

/// Receiver sends OWD measurements for rate control
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateFeedback {
    pub header: PacketHeader,
    /// Smoothed one-way delay in microseconds
    pub owd_us: u64,
    /// Minimum OWD seen so far
    pub min_owd_us: u64,
    /// Packets received in the last measurement window
    pub packets_received: u32,
    /// Packets expected (based on seq nums) in the last window
    pub packets_expected: u32,
    /// Receiver's suggested rate adjustment (-100 to +100 percent)
    pub rate_suggestion: i8,
}

/// Messages sent on the QUIC control channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMessage {
    /// Sender initiates a transfer
    TransferRequest {
        session_id: u32,
        filename: String,
        file_size: u64,
        block_size: u32,
        total_blocks: u32,
        blake3_hash: [u8; 32],
        /// UDP port the sender will blast data on
        data_port: u16,
    },
    /// Receiver accepts and tells sender where to send UDP data
    TransferAccept {
        session_id: u32,
        /// UDP port the receiver is listening on
        data_port: u16,
    },
    /// Receiver rejects the transfer
    TransferReject {
        session_id: u32,
        reason: String,
    },
    /// Key exchange for data channel encryption
    KeyExchange {
        session_id: u32,
        /// X25519 public key
        public_key: Vec<u8>,
    },
    /// Transfer is complete, all blocks verified
    TransferComplete {
        session_id: u32,
        success: bool,
        bytes_transferred: u64,
        duration_ms: u64,
    },
    /// Request retransmission of specific blocks (fallback if FEC insufficient)
    BlockRetransmitRequest {
        session_id: u32,
        block_ids: Vec<u32>,
    },
    /// Multi-file transfer: manifest of all files to transfer
    MultiFileManifest {
        session_id: u32,
        files: Vec<FileEntry>,
    },
    /// Delta sync: receiver sends block hashes for an existing file
    DeltaSyncHashes {
        session_id: u32,
        file_index: u32,
        block_hashes: Vec<[u8; 32]>,
    },
    /// Delta sync: sender responds with which blocks need updating
    DeltaSyncPlan {
        session_id: u32,
        file_index: u32,
        blocks_to_send: Vec<u32>,
    },
}

/// Entry in a multi-file manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative path from the source root
    pub relative_path: String,
    /// File size in bytes
    pub file_size: u64,
    /// BLAKE3 hash of the file
    pub blake3_hash: [u8; 32],
}

/// File metadata for a transfer session
#[derive(Debug, Clone)]
pub struct TransferManifest {
    pub session_id: u32,
    pub filename: String,
    pub file_size: u64,
    pub block_size: usize,
    pub total_blocks: u32,
    pub file_hash: [u8; 32],
}

impl TransferManifest {
    pub fn new(filename: String, file_size: u64, block_size: usize) -> Self {
        let total_blocks = ((file_size as usize + block_size - 1) / block_size) as u32;
        Self {
            session_id: rand::random(),
            filename,
            file_size,
            block_size,
            total_blocks,
            file_hash: [0u8; 32],
        }
    }
}
