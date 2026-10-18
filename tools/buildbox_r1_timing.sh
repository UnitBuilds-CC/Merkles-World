#!/bin/bash
# Run R1's armed descent timing on buildbox's GTX 1070
# Usage: Copy this to buildbox, chmod +x, then run it there
# This prices the remaining gap after specialization (49.424 ms -> needs 16.67 ms)

set -e

cd "$(git rev-parse --show-toplevel)"

echo "=== Buildbox R1 Armed Descent Timing ==="
echo "Date: $(date)"
echo "Commit: $(git describe --always --dirty)"
echo ""

# Verify GPU is present
echo "--- GPU Info ---"
nvidia-smi --query-gpu=name,driver_version --format=csv || echo "WARNING: nvidia-smi not found"
echo ""

# Run the armed descent at cap 96 (the demo's law)
echo "--- Armed descent at cap 96 (R1's grid, demo content) ---"
cargo run --release -p gpu --example r1_descent_arm 2>&1 | grep -E "(^//|cap|shipped|culled|ratio)"

echo ""
echo "--- Specialized frame at declared depth (for comparison) ---"
cargo run --release -p gpu --example r1_specialized_frame 2>&1 | grep -E "(^//|ms|budget|x budget)"

echo ""
echo "=== Summary ==="
echo "If armed descent shows significant savings over specialized frame,"
echo "that is the lever for closing R1's remaining ~3x gap."
echo "If both are similar, the gap requires resolution reduction or fewer rays."
