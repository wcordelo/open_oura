#!/usr/bin/env bash
# End-to-end hardware checklist for the Berendo Labs POC.
# Run on a machine with Bluetooth and a paired Oura ring.
set -euo pipefail

KEY_FILE="${KEY_FILE:-key.hex}"
PORT="${PORT:-8080}"
BINARY="${BINARY:-./target/release/oura}"

if [[ ! -f "$KEY_FILE" ]]; then
  echo "Missing auth key: $KEY_FILE"
  echo "Set KEY_FILE=/path/to/key.hex or pair first: oura pair --key-file key.hex"
  exit 1
fi

if [[ ! -x "$BINARY" ]]; then
  echo "Building release binary…"
  cargo build --release
fi

echo "== 1/4 Scan for Oura ring =="
"$BINARY" scan

echo ""
echo "== 2/4 Device info =="
"$BINARY" --key-file "$KEY_FILE" info

echo ""
echo "== 3/4 Headless JSONL log (5 s) =="
OUT="e2e-$(date +%s).jsonl"
"$BINARY" --key-file "$KEY_FILE" log --seconds 5 --output "$OUT"
LINES=$(wc -l < "$OUT" | tr -d ' ')
echo "Logged $LINES lines to $OUT"
head -3 "$OUT"
if [[ "$LINES" -lt 1 ]]; then
  echo "FAIL: no samples — is the ring worn?"
  exit 1
fi

echo ""
echo "== 4/4 POC dashboard =="
echo "Starting POC on http://127.0.0.1:$PORT (Ctrl-C to stop)"
echo "  → Open the URL, click Start, move your hand"
echo "  → Confirm viz updates and sample count rises"
echo "  → Click Download JSONL"
"$BINARY" --key-file "$KEY_FILE" poc --port "$PORT" --output "poc-$OUT"
