#!/bin/bash
# Test delta rendering performance on buildbox (GTX 1070)
# Usage: BB_PW=<password> ./test_buildbox_delta.sh

set -e

echo "Pulling latest changes to buildbox..."
cd ~/mw && git pull

echo ""
echo "Building temporal benchmark..."
/usr/bin/cargo build --release -p gpu --example r1_temporal_benchmark

echo ""
echo "Running benchmark on GTX 1070..."
cd ~/mw && ./target/release/examples/r1_temporal_benchmark

echo ""
echo "Expected results:"
echo "  - Frame 1: ~15ms (cache miss, full march)"
echo "  - Frames 2-5: <5ms each (cache hits, no marching)"
echo "  - Speedup: >200x from unculled baseline"
echo "  - Target: 200+ fps (<5ms per frame)"
