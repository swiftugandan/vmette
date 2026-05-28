#!/usr/bin/env bash
# Boot a microVM end-to-end:
#   1. start firecracker bound to a UNIX socket
#   2. drive its API with the rust spike to configure + start the VM
#   3. tail serial output (firecracker stdout) for a few seconds
#
# Run this *inside* the Lima guest (or any Linux host with /dev/kvm).

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASSETS="$HERE/assets"
BIN="$HERE/target/release/firecracker-spike"
SOCK="${SOCK:-/tmp/firecracker.sock}"
LOG="${LOG:-/tmp/firecracker.log}"

if [[ ! -e /dev/kvm ]]; then
  echo "✗ /dev/kvm not present — firecracker cannot run on this host." >&2
  echo "  See /var/log/kvm-probe.log inside the Lima guest for the verdict." >&2
  exit 1
fi

if [[ ! -x "$BIN" ]]; then
  echo "✗ $BIN missing — run \`cargo build --release\` first." >&2
  exit 1
fi

for f in "$ASSETS/vmlinux" "$ASSETS/rootfs.ext4"; do
  if [[ ! -s "$f" ]]; then
    echo "✗ asset missing: $f — run scripts/fetch-assets.sh first." >&2
    exit 1
  fi
done

rm -f "$SOCK" "$LOG"

echo "→ launching firecracker (api-sock=$SOCK)"
firecracker --api-sock "$SOCK" >"$LOG" 2>&1 &
FC_PID=$!
cleanup() {
  kill "$FC_PID" 2>/dev/null || true
  wait "$FC_PID" 2>/dev/null || true
  rm -f "$SOCK"
}
trap cleanup EXIT

# Wait for the socket.
for _ in $(seq 1 50); do
  [[ -S "$SOCK" ]] && break
  sleep 0.1
done
if [[ ! -S "$SOCK" ]]; then
  echo "✗ firecracker did not create $SOCK" >&2
  cat "$LOG" >&2 || true
  exit 1
fi

echo "→ configuring microVM via API"
"$BIN" \
  --socket  "$SOCK" \
  --kernel  "$ASSETS/vmlinux" \
  --rootfs  "$ASSETS/rootfs.ext4" \
  --vcpus   1 \
  --mem-mib 128

echo
echo "=== firecracker stdout (5s sample) ==="
sleep 5
tail -n +1 "$LOG"
echo "======================================"
echo "microVM is still running (pid $FC_PID); Ctrl-C to stop."
wait "$FC_PID"
