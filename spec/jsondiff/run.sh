#!/bin/bash
# The JSON differential test: fabric-core's reader against Python's json, on
# the same corpus. Every line must match. Usage: spec/jsondiff/run.sh [N] [OUT_DIR]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
n=${1:-30000}
out=${2:-$(mktemp -d)}
mkdir -p "$out"
python3 -I "$here/jsondiff.py" gen "$n" > "$out/corpus.hex"
python3 -I "$here/jsondiff.py" check < "$out/corpus.hex" > "$out/python.txt"
cargo run --quiet -p fabric-core --example jsondiff < "$out/corpus.hex" > "$out/rust.txt"
total=$(wc -l < "$out/corpus.hex" | tr -d ' ')
errs=$(grep -c '^ERR$' "$out/python.txt" || true)
if cmp -s "$out/python.txt" "$out/rust.txt"; then
  echo "jsondiff: $total inputs, all match ($errs refused by both)"
else
  echo "jsondiff: MISMATCH; first differing input numbers:"
  diff <(cat -n "$out/python.txt") <(cat -n "$out/rust.txt") | grep '^<' | head -5
  exit 1
fi
