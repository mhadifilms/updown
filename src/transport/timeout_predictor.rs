use std::time::{Duration, Instant};

/// FASP-style Timeout Predictor (Fig 2, module 240).
///
/// Predicts when each block should arrive based on the current injection rate
/// and block size. If a block doesn't arrive within the predicted window,
/// signals that the FEC repair ratio should increase.
///
/// This is proactive congestion detection: if blocks consistently arrive late,
/// the network is experiencing queuing or loss, and we should increase FEC.
pub struct TimeoutPredictor {
    /// Current estimated arrival rate in bytes per second
    estimated_rate_bps: f64,
    /// Block size in bytes
    block_size: usize,
    /// Safety margin multiplier (1.5 = allow 50% over predicted time)
    safety_margin: f64,
    /// Last block arrival times for rate estimation
    arrival_history: Vec<(u32, Instant, usize)>, // (block_id, arrival_time, bytes)
    /// Count of blocks that arrived late (past predicted deadline)
    late_blocks: u32,
    /// Count of blocks that arrived on time
    on_time_blocks: u32,
}

impl TimeoutPredictor {
    pub fn new(target_rate_mbps: u64, block_size: usize) -> Self {
        Self {
            estimated_rate_bps: target_rate_mbps as f64 * 1_000_000.0 / 8.0,
            block_size,
            safety_margin: 2.0, // Initially generous
            arrival_history: Vec::with_capacity(256),
            late_blocks: 0,
            on_time_blocks: 0,
        }
    }

    /// Predict how long a block should take to arrive (with safety margin)
    pub fn predicted_block_duration(&self) -> Duration {
        if self.estimated_rate_bps <= 0.0 {
            return Duration::from_secs(30);
        }
        let base_secs = self.block_size as f64 / self.estimated_rate_bps;
        Duration::from_secs_f64(base_secs * self.safety_margin)
    }

    /// Record a block arrival. Returns true if the block arrived "late"
    /// (past the predicted deadline from when we first started expecting it).
    pub fn record_arrival(
        &mut self,
        block_id: u32,
        started_expecting: Instant,
        block_bytes: usize,
    ) -> bool {
        let elapsed = started_expecting.elapsed();
        let predicted = self.predicted_block_duration();
        let late = elapsed > predicted;

        if late {
            self.late_blocks += 1;
            // Reduce safety margin slightly — our predictions are too tight
            self.safety_margin = (self.safety_margin * 1.1).min(5.0);
        } else {
            self.on_time_blocks += 1;
            // Tighten safety margin gradually
            self.safety_margin = (self.safety_margin * 0.99).max(1.2);
        }

        // Update rate estimate from actual arrival time
        if elapsed.as_secs_f64() > 0.0 {
            let actual_rate = block_bytes as f64 / elapsed.as_secs_f64();
            // EWMA with α=0.2
            self.estimated_rate_bps =
                0.2 * actual_rate + 0.8 * self.estimated_rate_bps;
        }

        self.arrival_history.push((block_id, Instant::now(), block_bytes));
        if self.arrival_history.len() > 256 {
            self.arrival_history.drain(0..128);
        }

        late
    }

    /// Get the recommended FEC boost factor based on late block ratio.
    /// Returns a multiplier (1.0 = no boost, 2.0 = double the repair ratio).
    pub fn fec_boost_factor(&self) -> f32 {
        let total = self.late_blocks + self.on_time_blocks;
        if total < 4 {
            return 1.0; // Not enough data
        }
        let late_ratio = self.late_blocks as f32 / total as f32;
        // Linear boost: 0% late → 1.0x, 50% late → 2.0x
        1.0 + late_ratio * 2.0
    }

    pub fn late_block_ratio(&self) -> f32 {
        let total = self.late_blocks + self.on_time_blocks;
        if total == 0 {
            return 0.0;
        }
        self.late_blocks as f32 / total as f32
    }

    pub fn estimated_rate_mbps(&self) -> f64 {
        self.estimated_rate_bps * 8.0 / 1_000_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_predicted_duration() {
        // 1 Gbps, 4 MB blocks → ~32ms base
        let predictor = TimeoutPredictor::new(1000, 4 * 1024 * 1024);
        let pred = predictor.predicted_block_duration();
        // With safety_margin=2.0: ~64ms
        assert!(pred.as_millis() > 50 && pred.as_millis() < 100);
    }

    #[test]
    fn test_late_detection() {
        let mut predictor = TimeoutPredictor::new(1000, 4 * 1024 * 1024);

        // Simulate on-time arrivals — use Instant::now() per call
        // so elapsed() is always ~0 (well within the predicted window)
        for i in 0..5 {
            predictor.record_arrival(i, Instant::now(), 4 * 1024 * 1024);
        }
        assert_eq!(predictor.on_time_blocks, 5);

        // FEC boost should be minimal with all on-time
        assert!(predictor.fec_boost_factor() < 1.1);
    }
}
