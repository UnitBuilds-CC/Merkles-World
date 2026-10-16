#!/usr/bin/env bash
# Falsify the header write on the reused frame path: with the step-cap header never uploaded, the asserts in
# r1_round_trip_split must refuse the rows rather than print them. Snapshots the bytes in memory and restores
# from them (no git checkout - the work under test is uncommitted).
set -u
repo="C:/Users/visse/OneDrive/Documentos/Merkles world"
src="$repo/crates/gpu/src/device.rs"
snap="$(mktemp)"
cp "$src" "$snap"
grep -n 'self.queue.write_buffer(&frame.header, 0, &header);' "$src" || { echo "PLANT NOT APPLIED: line absent"; rm -f "$snap"; exit 2; }
sed -i 's|self.queue.write_buffer(&frame.header, 0, &header);|// PLANT: header never uploaded|' "$src"
grep -n 'PLANT: header never uploaded' "$src"
cd "$repo" || exit 2
cargo run --release -p gpu --example r1_round_trip_split 2>&1 | tail -12
code="${PIPESTATUS[0]}"
cp "$snap" "$src"
rm -f "$snap"
if diff -q <(grep -c 'write_buffer(&frame.header' "$src") <(echo 1) >/dev/null; then
  echo "RESTORED: header write present again"
else
  echo "RESTORE FAILED - inspect $src by hand"
fi
echo "PLANT_RUN_EXIT=$code (0 would mean the instrument cannot see a header that never lands)"
