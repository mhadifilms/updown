use std::time::{Duration, Instant};

/// Rate control mode — how aggressively we use bandwidth
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateMode {
    /// Fill the pipe to target rate regardless of other traffic
    Fixed,
    /// Back off when detecting congestion (coexist with TCP)
    Fair,
    /// Only use bandwidth that other flows aren't using
    Scavenger,
}

/// Rate-based congestion controller using one-way delay (OWD) variation.
///
/// Unlike TCP (which uses packet loss as congestion signal), we measure
/// changes in one-way delay to detect queue buildup BEFORE packets drop.
/// This is the core principle behind FASP's speed advantage.
pub struct RateController {
    /// Target sending rate in bytes per second
    target_rate: u64,
    /// Current allowed rate in bytes per second
    current_rate: u64,
    /// Maximum rate (link capacity estimate)
    max_rate: u64,
    /// Minimum rate floor
    min_rate: u64,
    /// Rate control mode
    mode: RateMode,
    /// Minimum OWD observed (approximates propagation delay)
    min_owd: Duration,
    /// Smoothed OWD (EWMA)
    smoothed_owd: Duration,
    /// Previous smoothed OWD for trend detection
    prev_smoothed_owd: Duration,
    /// OWD increase streak count
    owd_increase_streak: u32,
    /// Time of last rate adjustment
    last_adjustment: Instant,
    /// Adjustment interval
    adjust_interval: Duration,
    /// EWMA smoothing factor for OWD (0.0-1.0, higher = more responsive)
    alpha: f64,
}

impl RateController {
    pub fn new(target_rate_mbps: u64, mode: RateMode) -> Self {
        let target_rate = target_rate_mbps * 1_000_000 / 8; // Convert Mbps to bytes/sec
        Self {
            target_rate,
            current_rate: target_rate,
            max_rate: target_rate * 2,
            min_rate: 1_000_000, // 1 MB/s floor
            mode,
            min_owd: Duration::from_secs(999),
            smoothed_owd: Duration::ZERO,
            prev_smoothed_owd: Duration::ZERO,
            owd_increase_streak: 0,
            last_adjustment: Instant::now(),
            adjust_interval: Duration::from_millis(50), // Adjust every 50ms
            alpha: 0.125, // Standard EWMA factor
        }
    }

    /// Calculate inter-packet delay for current rate.
    /// This is the core pacing mechanism — we space packets to match our rate.
    pub fn packet_interval(&self, packet_size: usize) -> Duration {
        if self.current_rate == 0 {
            return Duration::from_millis(1);
        }
        let nanos = (packet_size as u128 * 1_000_000_000) / self.current_rate as u128;
        Duration::from_nanos(nanos as u64)
    }

    /// Update the controller with a new OWD measurement from the receiver.
    /// This is called when we get a RateFeedback packet.
    pub fn update_owd(&mut self, owd: Duration) {
        // Track minimum OWD (propagation delay baseline)
        if owd < self.min_owd {
            self.min_owd = owd;
        }

        // EWMA smoothing
        self.prev_smoothed_owd = self.smoothed_owd;
        if self.smoothed_owd == Duration::ZERO {
            self.smoothed_owd = owd;
        } else {
            let smoothed_us = self.smoothed_owd.as_micros() as f64;
            let owd_us = owd.as_micros() as f64;
            let new_smoothed = smoothed_us * (1.0 - self.alpha) + owd_us * self.alpha;
            self.smoothed_owd = Duration::from_micros(new_smoothed as u64);
        }

        // Detect OWD trend
        if self.smoothed_owd > self.prev_smoothed_owd {
            self.owd_increase_streak += 1;
        } else {
            self.owd_increase_streak = 0;
        }

        self.maybe_adjust_rate();
    }

    /// Core rate adjustment logic
    fn maybe_adjust_rate(&mut self) {
        if self.last_adjustment.elapsed() < self.adjust_interval {
            return;
        }
        self.last_adjustment = Instant::now();

        match self.mode {
            RateMode::Fixed => {
                // Fixed mode: always return to target rate
                // Only back off if OWD is growing rapidly (severe congestion)
                let owd_ratio = self.owd_ratio();
                if owd_ratio > 2.0 && self.owd_increase_streak > 5 {
                    // Severe congestion — reduce to 80% of current
                    self.current_rate = (self.current_rate as f64 * 0.8) as u64;
                    self.current_rate = self.current_rate.max(self.min_rate);
                } else {
                    // Ramp back to target
                    let step = self.target_rate / 20; // 5% steps
                    self.current_rate = (self.current_rate + step).min(self.target_rate);
                }
            }
            RateMode::Fair => {
                // Fair mode: use OWD to detect congestion and back off like a good citizen
                let owd_ratio = self.owd_ratio();
                if owd_ratio > 1.5 || self.owd_increase_streak > 3 {
                    // Congestion detected — multiplicative decrease (but gentler than TCP)
                    self.current_rate = (self.current_rate as f64 * 0.9) as u64;
                    self.current_rate = self.current_rate.max(self.min_rate);
                    self.owd_increase_streak = 0;
                } else if owd_ratio < 1.1 {
                    // No congestion — additive increase
                    let step = self.target_rate / 50; // 2% steps
                    self.current_rate = (self.current_rate + step).min(self.target_rate);
                }
            }
            RateMode::Scavenger => {
                // Scavenger mode: only use truly idle bandwidth
                let owd_ratio = self.owd_ratio();
                if owd_ratio > 1.1 || self.owd_increase_streak > 1 {
                    // Any congestion signal — back off aggressively
                    self.current_rate = (self.current_rate as f64 * 0.5) as u64;
                    self.current_rate = self.current_rate.max(self.min_rate);
                    self.owd_increase_streak = 0;
                } else if owd_ratio < 1.02 {
                    // Very stable — cautiously increase
                    let step = self.target_rate / 100; // 1% steps
                    self.current_rate = (self.current_rate + step).min(self.target_rate);
                }
            }
        }
    }

    /// Ratio of current smoothed OWD to minimum OWD.
    /// 1.0 = no queuing, >1.0 = packets are queuing (congestion building)
    fn owd_ratio(&self) -> f64 {
        if self.min_owd.as_micros() == 0 {
            return 1.0;
        }
        self.smoothed_owd.as_micros() as f64 / self.min_owd.as_micros() as f64
    }

    pub fn current_rate_bps(&self) -> u64 {
        self.current_rate * 8
    }

    pub fn current_rate_mbps(&self) -> f64 {
        (self.current_rate * 8) as f64 / 1_000_000.0
    }

    pub fn target_rate_mbps(&self) -> f64 {
        (self.target_rate * 8) as f64 / 1_000_000.0
    }

    pub fn set_target_rate_mbps(&mut self, mbps: u64) {
        self.target_rate = mbps * 1_000_000 / 8;
        self.max_rate = self.target_rate * 2;
    }
}

/// Calculate a timestamp for OWD measurement (microseconds since some epoch)
pub fn timestamp_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}
