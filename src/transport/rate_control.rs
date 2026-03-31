use std::time::{Duration, Instant};

/// Rate control mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateMode {
    /// Fill the pipe to target rate regardless of other traffic
    Fixed,
    /// Back off when detecting congestion (coexist with TCP)
    Fair,
    /// Only use bandwidth that other flows aren't using
    Scavenger,
}

/// Phase 2 rate controller: receiver-driven with PCC Vivace-inspired utility.
///
/// The receiver computes congestion signals (OWD variation, FEC decoder excess)
/// and suggests a rate. The sender applies it within policy bounds.
///
/// Key changes from Phase 1:
/// - Supports batch_interval() for GSO batched sends
/// - apply_receiver_suggestion() for FASP-style receiver-driven rate control
/// - PCC Vivace utility function using OWD gradient + FEC loss signal
pub struct RateController {
    /// Target sending rate in bytes per second
    target_rate: u64,
    /// Current allowed rate in bytes per second
    current_rate: u64,
    /// Minimum rate floor
    min_rate: u64,
    /// Maximum rate ceiling
    max_rate: u64,
    /// Rate control mode
    mode: RateMode,
    /// Minimum OWD observed (propagation delay baseline)
    min_owd: Duration,
    /// Smoothed OWD (EWMA)
    smoothed_owd: Duration,
    /// Previous smoothed OWD for gradient
    prev_smoothed_owd: Duration,
    /// OWD increase streak count
    owd_increase_streak: u32,
    /// Time of last rate adjustment
    last_adjustment: Instant,
    /// Adjustment interval
    adjust_interval: Duration,
    /// EWMA smoothing factor
    alpha: f64,
}

impl RateController {
    pub fn new(target_rate_mbps: u64, mode: RateMode) -> Self {
        let target_rate = target_rate_mbps * 1_000_000 / 8;
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
            adjust_interval: Duration::from_millis(50),
            alpha: 0.125,
        }
    }

    /// Calculate inter-packet delay for a single packet at current rate.
    pub fn packet_interval(&self, packet_size: usize) -> Duration {
        if self.current_rate == 0 {
            return Duration::from_millis(1);
        }
        let nanos = (packet_size as u128 * 1_000_000_000) / self.current_rate as u128;
        Duration::from_nanos(nanos as u64)
    }

    /// Calculate delay for a batch of packets (GSO batching).
    /// This is the key difference from Phase 1: we pace batches, not individual packets.
    pub fn batch_interval(&self, batch_total_bytes: usize) -> Duration {
        if self.current_rate == 0 {
            return Duration::from_millis(1);
        }
        let nanos = (batch_total_bytes as u128 * 1_000_000_000) / self.current_rate as u128;
        Duration::from_nanos(nanos as u64)
    }

    /// Update with OWD measurement (sender-side adjustment, used alongside receiver feedback)
    pub fn update_owd(&mut self, owd: Duration) {
        if owd < self.min_owd {
            self.min_owd = owd;
        }

        self.prev_smoothed_owd = self.smoothed_owd;
        if self.smoothed_owd == Duration::ZERO {
            self.smoothed_owd = owd;
        } else {
            let smoothed_us = self.smoothed_owd.as_micros() as f64;
            let owd_us = owd.as_micros() as f64;
            let new_smoothed = smoothed_us * (1.0 - self.alpha) + owd_us * self.alpha;
            self.smoothed_owd = Duration::from_micros(new_smoothed as u64);
        }

        if self.smoothed_owd > self.prev_smoothed_owd {
            self.owd_increase_streak += 1;
        } else {
            self.owd_increase_streak = 0;
        }

        self.maybe_adjust_rate();
    }

    /// FASP-style: receiver computes the ideal rate and tells us.
    /// We apply it within our configured policy bounds.
    pub fn apply_receiver_suggestion(&mut self, suggested_rate_bps: u64) {
        let suggested_bytes = suggested_rate_bps / 8;
        match self.mode {
            RateMode::Fixed => {
                // In fixed mode, only reduce rate if receiver insists (below 80% target)
                if suggested_bytes < (self.target_rate * 80 / 100) {
                    self.current_rate = suggested_bytes.max(self.min_rate);
                } else {
                    self.current_rate = self.target_rate;
                }
            }
            RateMode::Fair | RateMode::Scavenger => {
                // In fair/scavenger mode, respect receiver's suggestion within bounds
                self.current_rate = suggested_bytes
                    .max(self.min_rate)
                    .min(self.max_rate);
            }
        }
        self.last_adjustment = Instant::now();
    }

    fn maybe_adjust_rate(&mut self) {
        if self.last_adjustment.elapsed() < self.adjust_interval {
            return;
        }
        self.last_adjustment = Instant::now();

        let owd_ratio = self.owd_ratio();

        match self.mode {
            RateMode::Fixed => {
                if owd_ratio > 2.0 && self.owd_increase_streak > 5 {
                    self.current_rate = (self.current_rate as f64 * 0.8) as u64;
                    self.current_rate = self.current_rate.max(self.min_rate);
                } else {
                    let step = self.target_rate / 20;
                    self.current_rate = (self.current_rate + step).min(self.target_rate);
                }
            }
            RateMode::Fair => {
                if owd_ratio > 1.5 || self.owd_increase_streak > 3 {
                    self.current_rate = (self.current_rate as f64 * 0.9) as u64;
                    self.current_rate = self.current_rate.max(self.min_rate);
                    self.owd_increase_streak = 0;
                } else if owd_ratio < 1.1 {
                    let step = self.target_rate / 50;
                    self.current_rate = (self.current_rate + step).min(self.target_rate);
                }
            }
            RateMode::Scavenger => {
                if owd_ratio > 1.1 || self.owd_increase_streak > 1 {
                    self.current_rate = (self.current_rate as f64 * 0.5) as u64;
                    self.current_rate = self.current_rate.max(self.min_rate);
                    self.owd_increase_streak = 0;
                } else if owd_ratio < 1.02 {
                    let step = self.target_rate / 100;
                    self.current_rate = (self.current_rate + step).min(self.target_rate);
                }
            }
        }
    }

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

/// Receiver-side rate calculator (FASP Fig 7: "New Ri Calculation").
/// Uses PCC Vivace-inspired utility with FEC decoder excess as loss signal.
pub struct ReceiverRateCalculator {
    /// Current estimated good rate
    current_rate_bps: u64,
    /// Configured target rate
    target_rate_bps: u64,
    /// Min/max bounds
    min_rate_bps: u64,
    max_rate_bps: u64,
    /// OWD measurements for gradient estimation
    owd_history: Vec<(u64, f64)>, // (timestamp_us, owd_us)
    /// Previous utility value for gradient ascent
    prev_utility: f64,
    /// Previous rate for gradient direction
    prev_rate: u64,
    /// Monitor interval duration
    monitor_interval: Duration,
    last_compute: Instant,
}

impl ReceiverRateCalculator {
    pub fn new(target_rate_mbps: u64) -> Self {
        let target = target_rate_mbps * 1_000_000;
        Self {
            current_rate_bps: target,
            target_rate_bps: target,
            min_rate_bps: 8_000_000,   // 8 Mbps floor
            max_rate_bps: target * 2,
            owd_history: Vec::with_capacity(1024),
            prev_utility: 0.0,
            prev_rate: target,
            monitor_interval: Duration::from_millis(100),
            last_compute: Instant::now(),
        }
    }

    /// Record an OWD sample
    pub fn record_owd(&mut self, timestamp_us: u64, owd_us: f64) {
        self.owd_history.push((timestamp_us, owd_us));
        // Keep last 2 seconds of history
        if self.owd_history.len() > 10000 {
            self.owd_history.drain(0..5000);
        }
    }

    /// Compute the suggested rate using PCC Vivace utility function.
    /// Called periodically (every monitor interval).
    ///
    /// Utility: u(x) = α·x^δ - β·x·L - γ·x·(dOWD/dt)
    ///
    /// Where:
    /// - x = sending rate
    /// - L = loss ratio (from FEC decoder excess)
    /// - dOWD/dt = OWD gradient (from linear regression on recent OWD samples)
    /// - δ = 0.9 (sub-linear throughput → promotes fairness)
    pub fn compute_rate(&mut self, loss_ratio: f32) -> u64 {
        if self.last_compute.elapsed() < self.monitor_interval {
            return self.current_rate_bps;
        }
        self.last_compute = Instant::now();

        // Vivace parameters
        let alpha: f64 = 1.0;
        let beta: f64 = 10.0;
        let gamma: f64 = 5.0;
        let delta: f64 = 0.9;

        let x = self.current_rate_bps as f64;
        let loss = loss_ratio as f64;

        // Compute OWD gradient via linear regression on recent samples
        let owd_gradient = self.owd_gradient();

        // PCC Vivace utility
        let utility = alpha * x.powf(delta) - beta * x * loss - gamma * x * owd_gradient.max(0.0);

        // Gradient ascent: adjust rate in direction of increasing utility
        if self.prev_rate > 0 {
            let rate_delta = x - self.prev_rate as f64;
            let utility_delta = utility - self.prev_utility;

            if rate_delta.abs() > 0.0 {
                let gradient = utility_delta / rate_delta;
                // Step size proportional to gradient magnitude, capped
                let omega = (self.target_rate_bps as f64) * 0.05; // 5% max step
                let step = omega * gradient.signum() * gradient.abs().sqrt().min(1.0);
                let new_rate = (x + step) as u64;
                self.current_rate_bps = new_rate
                    .max(self.min_rate_bps)
                    .min(self.max_rate_bps);
            }
        }

        self.prev_utility = utility;
        self.prev_rate = self.current_rate_bps;

        self.current_rate_bps
    }

    /// Estimate OWD gradient (dOWD/dt) via linear regression on recent samples.
    /// Positive gradient = queuing building up (congestion).
    /// Negative gradient = queue draining.
    fn owd_gradient(&self) -> f64 {
        if self.owd_history.len() < 10 {
            return 0.0;
        }

        // Use last 200 samples for regression
        let start = self.owd_history.len().saturating_sub(200);
        let samples = &self.owd_history[start..];
        let n = samples.len() as f64;

        let t0 = samples[0].0 as f64;
        let sum_t: f64 = samples.iter().map(|(t, _)| (*t as f64) - t0).sum();
        let sum_owd: f64 = samples.iter().map(|(_, owd)| *owd).sum();
        let sum_t_owd: f64 = samples.iter().map(|(t, owd)| ((*t as f64) - t0) * owd).sum();
        let sum_t2: f64 = samples.iter().map(|(t, _)| { let dt = (*t as f64) - t0; dt * dt }).sum();

        let denom = n * sum_t2 - sum_t * sum_t;
        if denom.abs() < 1e-10 {
            return 0.0;
        }

        // Slope of OWD over time (microseconds per microsecond = dimensionless)
        (n * sum_t_owd - sum_t * sum_owd) / denom
    }

    pub fn current_rate_bps(&self) -> u64 {
        self.current_rate_bps
    }
}

/// Calculate a timestamp for OWD measurement
pub fn timestamp_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}
