use std::time::Duration;

/// Format a transfer summary for display
pub fn format_transfer_summary(
    direction: &str, // "Send" or "Receive"
    filename: &str,
    file_size: u64,
    bytes_on_wire: u64,
    packets: u64,
    elapsed: Duration,
    rate_mbps: f64,
    hash: &[u8; 32],
    loss_estimate: Option<f32>,
    fec_ratio: Option<f32>,
    excess_symbols: Option<u64>,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!("--- {} Complete ---", direction));
    lines.push(format!("  File:        {}", filename));
    lines.push(format!("  Size:        {}", format_bytes(file_size)));

    if bytes_on_wire > file_size {
        let overhead = ((bytes_on_wire as f64 / file_size as f64) - 1.0) * 100.0;
        lines.push(format!(
            "  Wire bytes:  {} ({:.1}% FEC overhead)",
            format_bytes(bytes_on_wire),
            overhead
        ));
    }

    lines.push(format!("  Packets:     {}", format_count(packets)));
    lines.push(format!("  Time:        {}", format_duration(elapsed)));
    lines.push(format!("  Speed:       {}", format_rate(rate_mbps)));
    lines.push(format!("  BLAKE3:      {}", hex::encode(hash)));

    if let Some(loss) = loss_estimate {
        lines.push(format!("  Loss est:    {:.2}%", loss * 100.0));
    }
    if let Some(fec) = fec_ratio {
        lines.push(format!("  FEC ratio:   {:.1}% (adaptive)", fec * 100.0));
    }
    if let Some(excess) = excess_symbols {
        lines.push(format!("  FEC excess:  {} symbols", excess));
    }

    lines.join("\n")
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.2} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

pub fn format_rate(mbps: f64) -> String {
    if mbps >= 1000.0 {
        format!("{:.2} Gbps", mbps / 1000.0)
    } else {
        format!("{:.1} Mbps", mbps)
    }
}

pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs >= 60.0 {
        let mins = (secs / 60.0).floor();
        let remaining = secs - mins * 60.0;
        format!("{}m {:.1}s", mins as u64, remaining)
    } else if secs >= 1.0 {
        format!("{:.2}s", secs)
    } else {
        format!("{:.0}ms", secs * 1000.0)
    }
}

pub fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

/// Print a comparison table for benchmark results
pub fn format_benchmark_result(
    _size_mb: u64,
    send_rate: f64,
    recv_rate: f64,
    effective_rate: f64,
    packets_sent: u64,
    packets_recv: u64,
    fec_overhead: f64,
    loss_estimate: f32,
    adaptive_fec: f32,
    excess_symbols: u64,
    elapsed: Duration,
    integrity: bool,
) -> String {
    let mut lines = Vec::new();
    lines.push("=== Results ===".to_string());
    lines.push(format!("  Total time:    {}", format_duration(elapsed)));
    lines.push(format!("  Effective:     {}", format_rate(effective_rate)));
    lines.push(format!("  Send rate:     {}", format_rate(send_rate)));
    lines.push(format!("  Recv rate:     {}", format_rate(recv_rate)));
    lines.push(format!(
        "  Packets:       {} sent / {} recv",
        format_count(packets_sent),
        format_count(packets_recv)
    ));
    lines.push(format!("  FEC overhead:  {:.1}%", fec_overhead));
    lines.push(format!("  FEC excess:    {} symbols", excess_symbols));
    lines.push(format!("  Loss est:      {:.2}%", loss_estimate * 100.0));
    lines.push(format!("  Adaptive FEC:  {:.1}%", adaptive_fec * 100.0));
    lines.push(format!(
        "  Integrity:     {}",
        if integrity { "PASS" } else { "FAIL" }
    ));

    lines.join("\n")
}
