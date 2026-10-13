#!/usr/bin/env bash
# Local verification of the AS2 comparison step's logic, run against synthetic digest files.
# The gate exists to fail when one machine serialises differently and to pass when three agree, and both
# of those have to be demonstrated rather than assumed - the first version of this step was red on three
# machines that agreed, which is the worst direction a gate can fail in.
set -uo pipefail

compare_body() {
  # Paste of the workflow step body, with `find .` kept so the fixtures are laid out like artefacts.
  set -euo pipefail
  files=$(find . -name asset-digest.txt | sort)
  found=$(printf '%s\n' "$files" | grep -c . || true)
  if [ "$found" -lt 2 ]; then
    echo "AS2 needs at least two machines to agree; found $found digest file(s)" >&2
    exit 1
  fi
  for file in $files; do
    echo "--- $file"
    cat "$file"
    lines=$(awk 'END { print NR }' "$file")
    if [ "$lines" != 2 ]; then
      echo "^^^ $file should carry one digest per fixture (2), found $lines" >&2
      exit 1
    fi
    # Agreement is worthless if there is nothing to agree on: the empty-input digest is a constant, and
    # two fixtures hashing alike means only one byte stream was ever compared.
    if grep -q '^e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855' "$file"; then
      echo "^^^ $file contains the empty-input digest - a machine agreed on nothing" >&2
      exit 1
    fi
    if [ "$(awk '{print $1}' "$file" | sort -u | wc -w | tr -d ' ')" != "2" ]; then
      echo "^^^ $file reports one byte stream for two fixtures - the comparison would prove nothing" >&2
      exit 1
    fi
  done
  pairs=$(printf '%s\n' "$files" \
    | while read -r file; do
      awk '{ name = $2; sub(/^\*/, "", name); printf "%s %s\n", $1, name }' "$file"
    done \
    | sort -u | grep -c .)
  if [ "$pairs" != 2 ]; then
    echo "AS2 expected 2 distinct asset digests across $found machines, saw $pairs - one machine serialised differently" >&2
    exit 1
  fi
  echo "AS2 ok: $found machines produced byte-identical assets from the same source"
}

run_case() {
  local name="$1"
  local root
  root=$(mktemp -d)
  mkdir -p "$root/linux" "$root/windows" "$root/macos"
  printf '%s\n' "$2" > "$root/linux/asset-digest.txt"
  printf '%s\n' "$3" > "$root/windows/asset-digest.txt"
  printf '%s\n' "$4" > "$root/macos/asset-digest.txt"
  echo "=== $name"
  (cd "$root" && bash -c "$(declare -f compare_body); compare_body")
  local status=$?
  echo "--- exit $status (expected $5)"
  rm -rf "$root"
  if [ "$status" != "$5" ]; then
    echo "MISMATCH in case: $name"
    return 1
  fi
  return 0
}

A='4f9da1a4d4b45090a0a6cd84c9b376a6fd75bb9accbea8469302d50448ed925e  sphere.bin
52b85b4b13e5429ac74af3f986dc9a1a6f6923e30216b82ebb1dcd3081df633a  knot.bin'
# Same two hashes, Windows text mode marker on the names.
B='4f9da1a4d4b45090a0a6cd84c9b376a6fd75bb9accbea8469302d50448ed925e *sphere.bin
52b85b4b13e5429ac74af3f986dc9a1a6f6923e30216b82ebb1dcd3081df633a *knot.bin'
# One different knot hash.
C='4f9da1a4d4b45090a0a6cd84c9b376a6fd75bb9accbea8469302d50448ed925e  sphere.bin
eeeeeb4b13e5429ac74af3f986dc9a1a6f6923e30216b82ebb1dcd3081df633a  knot.bin'
# One asset only - a truncated digest file.
D='4f9da1a4d4b45090a0a6cd84c9b376a6fd75bb9accbea8469302d50448ed925e  sphere.bin'
# Every machine agreed - on nothing. Two zero-byte assets hash to the empty-input digest, which is a
# constant, so an agreement built on it proves nothing and has to be refused.
E='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  sphere.bin
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  knot.bin'
# Both fixtures produced the same byte stream: two names, one comparison, so a compiler that ignores its
# input would still "agree across machines".
S='4f9da1a4d4b45090a0a6cd84c9b376a6fd75bb9accbea8469302d50448ed925e  sphere.bin
4f9da1a4d4b45090a0a6cd84c9b376a6fd75bb9accbea8469302d50448ed925e  knot.bin'

failures=0
run_case "three machines agree, one using the Windows mode marker" "$A" "$B" "$A" 0 || failures=$((failures + 1))
run_case "one machine serialised a different knot asset" "$A" "$A" "$C" 1 || failures=$((failures + 1))
run_case "one machine published a truncated digest" "$A" "$A" "$D" 1 || failures=$((failures + 1))
run_case "every machine agreed on empty assets" "$A" "$E" "$E" 1 || failures=$((failures + 1))
run_case "the two fixtures hashed identically on every machine" "$S" "$S" "$S" 1 || failures=$((failures + 1))

echo
if [ "$failures" = 0 ]; then
  echo "AS2 comparison logic verified: passes on agreement, fails on divergence and on a broken artefact"
else
  echo "$failures case(s) did not behave as expected"
  exit 1
fi
