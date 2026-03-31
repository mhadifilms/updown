use raptorq::{Decoder, Encoder, EncodingPacket, ObjectTransmissionInformation};

use crate::protocol::SYMBOL_SIZE;

/// FEC encoder: takes a data block and produces source + repair symbols.
/// Phase 2: supports adaptive repair ratio based on measured loss.
pub struct FecEncoder {
    symbol_size: u16,
    repair_ratio: f32,
    /// Minimum repair ratio floor (2% — handles sporadic loss without feedback)
    min_repair_ratio: f32,
}

/// Stats returned after encoding a block, used for adaptive FEC tuning
#[derive(Debug, Clone)]
pub struct EncodeStats {
    pub source_symbols: usize,
    pub repair_symbols: usize,
    pub total_symbols: usize,
    pub repair_ratio_used: f32,
}

impl FecEncoder {
    pub fn new(repair_ratio: f32) -> Self {
        Self {
            symbol_size: SYMBOL_SIZE,
            repair_ratio,
            min_repair_ratio: 0.02,
        }
    }

    /// Update repair ratio based on measured loss (adaptive FEC)
    pub fn set_repair_ratio(&mut self, ratio: f32) {
        self.repair_ratio = ratio.max(self.min_repair_ratio);
    }

    pub fn repair_ratio(&self) -> f32 {
        self.repair_ratio
    }

    /// Encode a block, returning symbols and stats
    pub fn encode(&self, data: &[u8]) -> (Vec<EncodingPacket>, EncodeStats) {
        let encoder = Encoder::with_defaults(data, self.symbol_size);
        let mut packets: Vec<EncodingPacket> = Vec::new();
        let mut total_source = 0usize;
        let mut total_repair = 0usize;

        for block_encoder in encoder.get_block_encoders() {
            let source_packets = block_encoder.source_packets();
            let num_source = source_packets.len();
            let num_repair = (num_source as f32 * self.repair_ratio).ceil() as u32;

            total_source += num_source;
            total_repair += num_repair as usize;

            packets.extend(source_packets);
            let repair_packets = block_encoder.repair_packets(0, num_repair);
            packets.extend(repair_packets);
        }

        let stats = EncodeStats {
            source_symbols: total_source,
            repair_symbols: total_repair,
            total_symbols: total_source + total_repair,
            repair_ratio_used: self.repair_ratio,
        };

        (packets, stats)
    }

    pub fn transmission_info(&self, data_len: u64) -> ObjectTransmissionInformation {
        ObjectTransmissionInformation::with_defaults(data_len, self.symbol_size)
    }
}

/// FEC decoder with decode statistics for adaptive FEC feedback
pub struct FecDecoder {
    decoder: Decoder,
    complete: bool,
    /// Number of source symbols expected (K)
    source_symbols: usize,
    /// Number of packets fed to decoder
    packets_fed: usize,
}

/// Stats from decoding a block — feeds the adaptive FEC + congestion controller
#[derive(Debug, Clone)]
pub struct DecodeStats {
    /// Number of source symbols expected
    pub source_symbols: usize,
    /// Number of packets needed to decode
    pub packets_needed: usize,
    /// Excess symbols beyond K needed (0 = perfect, higher = more loss absorbed)
    pub excess_symbols: usize,
    /// Estimated loss ratio for this block
    pub estimated_loss: f32,
}

impl FecDecoder {
    pub fn new(data_len: u64, symbol_size: u16) -> Self {
        let config = ObjectTransmissionInformation::with_defaults(data_len, symbol_size);
        let source_symbols = ((data_len as usize) + symbol_size as usize - 1) / symbol_size as usize;
        Self {
            decoder: Decoder::new(config),
            complete: false,
            source_symbols,
            packets_fed: 0,
        }
    }

    /// Feed a received encoding packet to the decoder.
    /// Returns Some((decoded_data, decode_stats)) if reconstruction succeeded.
    pub fn add_packet(&mut self, packet: EncodingPacket) -> Option<(Vec<u8>, DecodeStats)> {
        if self.complete {
            return None;
        }
        self.packets_fed += 1;

        if let Some(data) = self.decoder.decode(packet) {
            self.complete = true;
            let excess = self.packets_fed.saturating_sub(self.source_symbols);
            let stats = DecodeStats {
                source_symbols: self.source_symbols,
                packets_needed: self.packets_fed,
                excess_symbols: excess,
                estimated_loss: if self.packets_fed > 0 {
                    1.0 - (self.source_symbols as f32 / self.packets_fed as f32)
                } else {
                    0.0
                },
            };
            Some((data, stats))
        } else {
            None
        }
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn packets_fed(&self) -> usize {
        self.packets_fed
    }
}

/// Adaptive FEC controller: adjusts repair ratio based on measured loss
pub struct AdaptiveFec {
    /// Current estimated loss rate (EWMA)
    loss_estimate: f32,
    /// EWMA smoothing factor
    alpha: f32,
    /// Minimum repair ratio
    min_ratio: f32,
    /// Maximum repair ratio
    max_ratio: f32,
    /// Safety margin multiplier on estimated loss
    safety_margin: f32,
    /// Number of blocks observed
    blocks_observed: u32,
}

impl AdaptiveFec {
    pub fn new() -> Self {
        Self {
            loss_estimate: 0.0,
            alpha: 0.3,       // Responsive but not jittery
            min_ratio: 0.02,  // 2% floor
            max_ratio: 0.50,  // 50% ceiling
            safety_margin: 1.3,
            blocks_observed: 0,
        }
    }

    /// Update with decode stats from a completed block.
    /// Returns the recommended repair ratio for the next block.
    pub fn update(&mut self, stats: &DecodeStats) -> f32 {
        self.blocks_observed += 1;

        // EWMA loss estimate
        let measured = stats.estimated_loss.max(0.0);
        if self.blocks_observed == 1 {
            self.loss_estimate = measured;
        } else {
            self.loss_estimate = self.alpha * measured + (1.0 - self.alpha) * self.loss_estimate;
        }

        self.recommended_ratio()
    }

    /// Get the current recommended repair ratio
    pub fn recommended_ratio(&self) -> f32 {
        let ratio = (self.loss_estimate * self.safety_margin).max(self.min_ratio);
        ratio.min(self.max_ratio)
    }

    pub fn loss_estimate(&self) -> f32 {
        self.loss_estimate
    }

    pub fn blocks_observed(&self) -> u32 {
        self.blocks_observed
    }
}

pub fn symbols_for_block(block_size: usize) -> usize {
    (block_size + SYMBOL_SIZE as usize - 1) / SYMBOL_SIZE as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let data = vec![42u8; 50_000];
        let encoder = FecEncoder::new(0.1);
        let (packets, stats) = encoder.encode(&data);
        assert!(stats.repair_symbols > 0);

        let mut decoder = FecDecoder::new(data.len() as u64, SYMBOL_SIZE);
        let mut result = None;
        for packet in packets {
            if let Some((decoded, dstats)) = decoder.add_packet(packet) {
                assert_eq!(dstats.excess_symbols, 0); // No loss = no excess
                result = Some(decoded);
                break;
            }
        }
        assert_eq!(result.unwrap(), data);
    }

    #[test]
    fn test_decode_with_loss_reports_stats() {
        let data = vec![7u8; 100_000];
        let encoder = FecEncoder::new(0.3);
        let (packets, stats) = encoder.encode(&data);
        let total = packets.len();

        // Drop every 5th packet (20% loss)
        let surviving: Vec<_> = packets
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % 5 != 0)
            .map(|(_, p)| p)
            .collect();

        assert!(surviving.len() < total);

        let mut decoder = FecDecoder::new(data.len() as u64, SYMBOL_SIZE);
        for packet in surviving {
            if let Some((decoded, dstats)) = decoder.add_packet(packet) {
                assert_eq!(decoded, data);
                // With loss, we needed more packets than the minimum K
                assert!(dstats.packets_needed >= dstats.source_symbols);
                return;
            }
        }
        panic!("should have decoded even with 20% loss");
    }

    #[test]
    fn test_adaptive_fec_adjusts_ratio() {
        let mut adaptive = AdaptiveFec::new();

        // Zero loss → min ratio (2% floor)
        let stats = DecodeStats {
            source_symbols: 100,
            packets_needed: 100,
            excess_symbols: 0,
            estimated_loss: 0.0,
        };
        let ratio = adaptive.update(&stats);
        assert!((ratio - 0.02).abs() < 0.01, "zero loss should give min ratio, got {}", ratio);

        // Sustained high loss → high ratio
        // Feed multiple high-loss observations to overcome EWMA smoothing
        for _ in 0..10 {
            let stats = DecodeStats {
                source_symbols: 100,
                packets_needed: 130,
                excess_symbols: 30,
                estimated_loss: 0.30,
            };
            adaptive.update(&stats);
        }
        let ratio = adaptive.recommended_ratio();
        assert!(ratio > 0.20, "sustained 30% loss should give ratio > 20%, got {}", ratio);
    }
}
