#!/usr/bin/env bash
# Does x86 codegen level change anything the ledger hashes, or anything the demo prints?
#
#   bash tools/cpu_codegen_parity.sh
#
# Gate P1.5's four machines are not the same CPU. The N2600 is a Bonnell-era Celeron: SSE2/SSSE3, no AVX2,
# no FMA. A desktop GPU-pushing box from the same list has AVX2 and FMA. If a float contraction or a
# different vector path moved a value that enters `EPOCH_0`, or even a printed presentation number, the two
# would diverge for a reason no test in this repository currently reaches. `-Ctarget-cpu` is how you ask the
# question without owning the silicon: build the same source at both ends of that spread and compare the
# boots and the compiled asset bytes.
#
# The harness refuses to be vacuous, because a previous version of this check was. Three rules:
#   * a build that fails is a failure, never an empty file compared against another empty file;
#   * empty program output is a failure (`e3b0c442…` is the hash of nothing, and it looks like a value);
#   * every variant is compared against the baseline, so a run where nothing was rebuilt still cannot pass.
#
# Exit codes match the repo's harness convention: 0 all equal, 1 a disagreement, 2 the harness could not
# run meaningfully (non-x86 host - the x86 codegen axis does not exist there, and ARM's SIMD axis is
# covered by `tools/wasm_simd_parity.sh`).

set -euo pipefail

VARIANTS="${CPU_CODEGEN_VARIANTS:-x86-64 x86-64-v3}"
BASELINE_FLAGS="${CPU_CODEGEN_BASELINE:-}"
FIXTURE="crates/assetc/tests/fixtures/knot.ron"
WORK="target/cpu-codegen-parity"

if [ "$(uname -m)" != "x86_64" ]; then
  echo "SKIPPED the x86 codegen-level axis does not exist on $(uname -m); ARM SIMD is covered by wasm_simd_parity.sh"
  exit 2
fi
command -v cargo >/dev/null 2>&1 || { echo "FAIL cargo not found"; exit 2; }
test -f "$FIXTURE" || { echo "FAIL fixture $FIXTURE missing - a missing fixture would silently skip the asset check"; exit 2; }

mkdir -p "$WORK"
SHA() { sha256sum | cut -c1-16; }

# The baseline is the build CI and every device comparison already use: no target-cpu flag at all.
echo "// baseline: default rustflags"
RUSTFLAGS="$BASELINE_FLAGS" cargo build --release -q -p engine -p assetc
BASE_BOOT="$(cargo run --release -q -p engine | SHA)"
test -n "$BASE_BOOT" || { echo "FAIL baseline boot produced no hash"; exit 2; }
cargo run --release -q -p assetc -- "$FIXTURE" "$WORK/base.bin" >/dev/null
test -s "$WORK/base.bin" || { echo "FAIL baseline asset compile produced no bytes"; exit 2; }
BASE_ASSET="$(SHA < "$WORK/base.bin")"
echo "//   boot $BASE_BOOT  asset $BASE_ASSET"

FAIL=0
for V in $VARIANTS; do
  echo "// variant -Ctarget-cpu=$V"
  if ! RUSTFLAGS="-Ctarget-cpu=$V" cargo build --release -q -p engine -p assetc 2> "$WORK/build_$V.err"; then
    echo "FAIL the build itself failed; that is not a determinism result. Last lines:"
    tail -4 "$WORK/build_$V.err"
    exit 2
  fi
  BOOT_OUT="$WORK/boot_$V.txt"
  if ! cargo run --release -q -p engine > "$BOOT_OUT"; then
    echo "FAIL the boot exited non-zero under -Ctarget-cpu=$V"
    exit 2
  fi
  test -s "$BOOT_OUT" || { echo "FAIL empty boot output under -Ctarget-cpu=$V - comparing empty to empty proves nothing"; exit 2; }
  BOOT="$(SHA < "$BOOT_OUT")"
  if ! cargo run --release -q -p assetc -- "$FIXTURE" "$WORK/out_$V.bin" > /dev/null; then
    echo "FAIL assetc exited non-zero under -Ctarget-cpu=$V"
    exit 2
  fi
  test -s "$WORK/out_$V.bin" || { echo "FAIL empty asset output under -Ctarget-cpu=$V"; exit 2; }
  ASSET="$(SHA < "$WORK/out_$V.bin")"
  SAME="yes"; [ "$BOOT" = "$BASE_BOOT" ] && [ "$ASSET" = "$BASE_ASSET" ] || SAME="no"
  echo "//   boot $BOOT  asset $ASSET  identical to baseline: $SAME"
  if [ "$SAME" = "no" ]; then
    diff <(cargo run --release -q -p engine | cat) "$BOOT_OUT" | head -8 || true
    FAIL=1
  fi
done

# Restore the baseline build so a later step in the same job is not left holding a target-cpu binary.
RUSTFLAGS="$BASELINE_FLAGS" cargo build --release -q -p engine -p assetc

if [ "$FAIL" -ne 0 ]; then
  echo "RESULT: a boot digest or compiled asset byte stream depends on x86 codegen level. That is a"
  echo "        determinism defect: gate P1.5's machines differ exactly here, and ADR-017 forbids a hashed"
  echo "        value from depending on it."
  exit 1
fi
echo "RESULT: boot output and compiled asset bytes are identical across $(echo "$VARIANTS" | wc -w | tr -d ' ') x86 codegen levels and the baseline, hashed lines and asset serialization alike."
