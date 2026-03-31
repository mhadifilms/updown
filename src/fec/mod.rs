use raptorq::{Decoder, Encoder, EncodingPacket, ObjectTransmissionInformation};

use crate::protocol::SYMBOL_SIZE;

/// FEC encoder: takes a data block and produces source + repair symbols
pub struct FecEncoder {
    symbol_size: u16,
    repair_ratio: f32,
}

impl FecEncoder {
    pub fn new(repair_ratio: f32) -> Self {
        Self {
            symbol_size: SYMBOL_SIZE,
            repair_ratio,
        }
    }

    /// Encode a block of data into RaptorQ symbols.
    /// Returns (source_symbols + repair_symbols) as serialized EncodingPackets.
    pub fn encode(&self, data: &[u8]) -> Vec<EncodingPacket> {
        let encoder = Encoder::with_defaults(data, self.symbol_size);

        let mut packets: Vec<EncodingPacket> = Vec::new();

        // Collect all source symbols
        for block_encoder in encoder.get_block_encoders() {
            let source_packets = block_encoder.source_packets();
            let num_repair = (source_packets.len() as f32 * self.repair_ratio).ceil() as u32;

            // Add source symbols
            packets.extend(source_packets);

            // Add repair symbols
            let repair_packets = block_encoder.repair_packets(0, num_repair);
            packets.extend(repair_packets);
        }

        packets
    }

    /// Get the ObjectTransmissionInformation needed by the decoder
    pub fn transmission_info(&self, data_len: u64) -> ObjectTransmissionInformation {
        ObjectTransmissionInformation::with_defaults(data_len, self.symbol_size)
    }
}

/// FEC decoder: collects symbols and reconstructs the original block
pub struct FecDecoder {
    decoder: Decoder,
    complete: bool,
}

impl FecDecoder {
    pub fn new(data_len: u64, symbol_size: u16) -> Self {
        let config = ObjectTransmissionInformation::with_defaults(data_len, symbol_size);
        Self {
            decoder: Decoder::new(config),
            complete: false,
        }
    }

    /// Feed a received encoding packet to the decoder.
    /// Returns Some(decoded_data) if the block can now be fully reconstructed.
    pub fn add_packet(&mut self, packet: EncodingPacket) -> Option<Vec<u8>> {
        if self.complete {
            return None;
        }

        if let Some(data) = self.decoder.decode(packet) {
            self.complete = true;
            Some(data)
        } else {
            None
        }
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }
}

/// Calculate how many symbols are needed for a given data size
pub fn symbols_for_block(block_size: usize) -> usize {
    (block_size + SYMBOL_SIZE as usize - 1) / SYMBOL_SIZE as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let data = vec![42u8; 50_000]; // 50KB test block
        let encoder = FecEncoder::new(0.1); // 10% repair overhead

        let packets = encoder.encode(&data);
        assert!(!packets.is_empty());

        // Decoder should reconstruct from source symbols alone
        let mut decoder = FecDecoder::new(data.len() as u64, SYMBOL_SIZE);
        let mut result = None;
        for packet in packets {
            if let Some(decoded) = decoder.add_packet(packet) {
                result = Some(decoded);
                break;
            }
        }

        let result = result.expect("should have decoded");
        assert_eq!(result, data);
    }

    #[test]
    fn test_decode_with_loss() {
        let data = vec![7u8; 100_000]; // 100KB test block
        let encoder = FecEncoder::new(0.3); // 30% repair overhead

        let packets = encoder.encode(&data);
        let total = packets.len();

        // Simulate 20% packet loss by dropping every 5th packet
        let surviving: Vec<_> = packets
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % 5 != 0)
            .map(|(_, p)| p)
            .collect();

        assert!(surviving.len() < total);

        let mut decoder = FecDecoder::new(data.len() as u64, SYMBOL_SIZE);
        let mut result = None;
        for packet in surviving {
            if let Some(decoded) = decoder.add_packet(packet) {
                result = Some(decoded);
                break;
            }
        }

        let result = result.expect("should have decoded even with 20% loss");
        assert_eq!(result, data);
    }
}
