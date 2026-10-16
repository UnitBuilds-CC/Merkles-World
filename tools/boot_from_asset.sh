#!/usr/bin/env bash
# The pitch demo boots from content: the shipped compiler writes the demo scene to a .bin, the shipped
# binary boots from that file, and every line of its published output except the one that names the scene
# source must be byte-identical to the boot that compiles its own scene in memory.
#
#   bash tools/boot_from_asset.sh
#
# Why this is the check and not a digest bump: the boot's 18 stdout lines are gate P1.5's published
# whole-output evidence (docs/evidence.json's `db71f8a0…`, which both named devices are recorded against),
# and `scene_from_disk` prints the path it read — an absolute temp path — on one of them. So the disk boot
# cannot become the published run without re-recording two devices. What it *can* prove, and what this
# proves, is that the scene's origin is invisible everywhere else: the probe ray lands at the same `t` after
# the same number of evaluations, the 64x48 raster reports the same picture, and the epoch root is the same
# digest. That is the demo's rendering clause stated about the binary rather than about an example.
#
# Two arms, both required:
#   1. agreement  - code-sourced and asset-sourced boots differ on exactly one line, and that line is
#                   `scene source`, and both sides of it report the same node count and depth.
#   2. falsifying - a tampered authoring file (one sphere centre moved) must move the probe ray's `t`. If it
#                   does not, the diff above compared two runs that never looked at the asset.
#
# Timing is not evidence anywhere in this file (ADR-017); nothing here asserts a duration.
set -euo pipefail
cd "$(dirname "$0")/.."

fixture="crates/assetc/tests/fixtures/boot_scene.ron"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cargo build --release -q -p assetc -p engine

# 1. The compiler, as a tool, exactly as docs/09 stage one describes it: .ron in, .bin out.
./target/release/assetc "$fixture" "$work/boot_scene.bin" > "$work/compile.log" 2>&1
if [ ! -s "$work/boot_scene.bin" ]; then
  echo "FAIL: assetc wrote an empty asset for $fixture" >&2
  cat "$work/compile.log" >&2
  exit 1
fi
asset_bytes="$(wc -c < "$work/boot_scene.bin" | tr -d ' ')"
echo "asset            : $asset_bytes bytes compiled from $fixture by the shipped assetc CLI"

# 2. The two boots. Same binary, same release profile, one argument apart.
cargo run --release -q -p engine > "$work/code.txt"
cargo run --release -q -p engine -- "$work/boot_scene.bin" > "$work/asset.txt"

grep -v '^scene source' "$work/code.txt"  > "$work/code.rest"
grep -v '^scene source' "$work/asset.txt" > "$work/asset.rest"

code_lines="$(wc -l < "$work/code.txt" | tr -d ' ')"
asset_lines="$(wc -l < "$work/asset.txt" | tr -d ' ')"
if [ "$code_lines" != "$asset_lines" ]; then
  echo "FAIL: the two boots printed $code_lines and $asset_lines lines - a different number of lines is not a" >&2
  echo "      path difference, it is a different run" >&2
  exit 1
fi
if ! diff -u "$work/code.rest" "$work/asset.rest"; then
  echo "FAIL: booting from the compiled asset changed the demo's output beyond the line that names the source" >&2
  exit 1
fi

# The two `scene source` lines have to describe one scene, or the diff above compared a boot that ignored
# the file it was handed. The in-code form reports the bytes it compiled in memory; the disk form reports
# the bytes it read, which are this asset's own length.
code_scene="$(grep '^scene source' "$work/code.txt")"
asset_scene="$(grep '^scene source' "$work/asset.txt")"
echo "scene, in code   : $code_scene"
echo "scene, from file : $asset_scene"
for field in "11 nodes" "depth 4"; do
  case "$code_scene" in *"$field"*) ;; *) echo "FAIL: the code boot no longer reports '$field'" >&2; exit 1;; esac
  case "$asset_scene" in *"$field"*) ;; *) echo "FAIL: the asset boot no longer reports '$field'" >&2; exit 1;; esac
done
case "$asset_scene" in
  *"$asset_bytes bytes read from"*) ;;
  *) echo "FAIL: the disk boot did not read the $asset_bytes-byte asset this script compiled" >&2; exit 1;;
esac

# 3. The line that actually re-enters the march, quoted rather than only diffed.
march_line="$(grep '^scene march' "$work/asset.txt")"
echo "asset-sourced    : $march_line"

# 4. Falsification: move one sphere in a copy of the authoring file and require the demo to notice. The
#    envelope cannot tell the two files apart - same node count, same pool length, so the same byte count -
#    which is exactly why the picture has to be what differs. The copy keeps the *file name* too, because
#    `assetc` stamps the stem as the asset label and a renamed copy would differ in bytes for a reason that
#    has nothing to do with geometry.
tampered_dir="$work/tampered"
mkdir -p "$tampered_dir"
tampered="$tampered_dir/boot_scene.ron"
sed 's/centre: (1.0, 3.2, 7.5)/centre: (1.0, 3.2, 1.0)/' "$fixture" > "$tampered"
if cmp -s "$fixture" "$tampered"; then
  echo "FAIL: the tampering sed changed nothing in $fixture, so this script proves nothing about the diff" >&2
  exit 1
fi
./target/release/assetc "$tampered" "$work/tampered.bin" > "$work/tamper-compile.log" 2>&1
tampered_bytes="$(wc -c < "$work/tampered.bin" | tr -d ' ')"
if [ "$tampered_bytes" != "$asset_bytes" ]; then
  echo "FAIL: the tampered asset is $tampered_bytes bytes against $asset_bytes, so the two differ in shape" >&2
  echo "      rather than in a value, and the diff below would not be a test of the march" >&2
  exit 1
fi
cargo run --release -q -p engine -- "$work/tampered.bin" > "$work/tampered.txt"
grep -v '^scene source' "$work/tampered.txt" > "$work/tampered.rest"
if diff -q "$work/code.rest" "$work/tampered.rest" > /dev/null; then
  echo "FAIL: a sphere moved four units left the whole demo output unchanged, so the boot never marched the" >&2
  echo "      asset it was given and the agreement in step 2 compared two runs that ignored their input" >&2
  exit 1
fi
echo "falsification    : the tampered asset is the same $tampered_bytes bytes as the honest one (same node"
echo "                   count, same pool), yet the boot's output differs on these lines:"
diff -u "$work/code.rest" "$work/tampered.rest" | grep -E '^[+-]' | grep -vE '^(\+\+\+|---)' || true

echo "RESULT: the demo boots from a compiled asset, and the only line that knows where its scene came from is the one that says so."
rm -f "$work/boot_scene.bin" "$work/tampered.bin"
