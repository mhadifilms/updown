#!/bin/bash
# =============================================================================
# Linux GSO Benchmark Script for updown
# =============================================================================
# Run this on any Linux box with kernel 4.18+ to test GSO performance.
#
# Usage:
#   curl -sSf https://sh.rustup.rs | sh -s -- -y
#   source ~/.cargo/env
#   git clone https://github.com/mhadifilms/updown.git
#   cd updown
#   bash test_linux_gso.sh
#
# Expected: 1.5-2.5 Gbps with GSO vs ~780 Mbps without
# =============================================================================

set -e

echo "=== updown Linux GSO Benchmark ==="
echo ""

# Check kernel version for GSO support
KERNEL=$(uname -r)
KERNEL_MAJOR=$(echo $KERNEL | cut -d. -f1)
KERNEL_MINOR=$(echo $KERNEL | cut -d. -f2)
echo "Kernel:  $KERNEL"

if [ "$KERNEL_MAJOR" -lt 4 ] || ([ "$KERNEL_MAJOR" -eq 4 ] && [ "$KERNEL_MINOR" -lt 18 ]); then
    echo "WARNING: Kernel < 4.18 — GSO (UDP_SEGMENT) not supported"
fi
if [ "$KERNEL_MAJOR" -lt 5 ]; then
    echo "WARNING: Kernel < 5.0 — GRO (UDP_GRO) not supported"
fi

echo "Arch:    $(uname -m)"
echo "CPU:     $(nproc) cores"
echo ""

# Build
echo "[1/3] Building release binary..."
cargo build --release 2>&1 | tail -3
echo ""

# Check UDP buffer sizes
echo "[2/3] System configuration:"
echo "  rmem_max: $(cat /proc/sys/net/core/rmem_max 2>/dev/null || echo 'N/A')"
echo "  wmem_max: $(cat /proc/sys/net/core/wmem_max 2>/dev/null || echo 'N/A')"
echo ""

# Optionally increase buffer sizes (needs root)
if [ "$(id -u)" -eq 0 ]; then
    sysctl -w net.core.rmem_max=16777216 >/dev/null 2>&1
    sysctl -w net.core.wmem_max=16777216 >/dev/null 2>&1
    echo "  Increased socket buffer limits to 16MB"
    echo ""
fi

# Run benchmarks
echo "[3/3] Running benchmarks..."
echo ""

for SIZE_MB in 100 500 1000; do
    echo "--- ${SIZE_MB} MB ---"
    RUST_LOG=warn ./target/release/updown bench --size-mb $SIZE_MB --rate 10000 --interleave 4 2>&1 | \
        grep -E "(Total time|Effective|Send rate|Recv rate|Packets|FEC|Loss|Integrity)"
    echo ""
done

echo "--- 5 GB stress test ---"
RUST_LOG=warn ./target/release/updown bench --size-mb 5000 --rate 10000 --interleave 4 2>&1 | \
    grep -E "(Total time|Effective|Send rate|Recv rate|Packets|FEC|Loss|Integrity)"
echo ""

echo "=== Done ==="
