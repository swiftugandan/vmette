#!/usr/bin/env bash
# vz-spike orchestrator: ensure assets exist, build + sign vz-spike, then
# boot a microVM that runs whatever command was passed on the CLI.
#
# Usage:
#   ./scripts/run-vz.sh                              # default probe command
#   ./scripts/run-vz.sh 'uname -a; cat /etc/os-release'
#   SHARE_DIR=/path/to/dir ./scripts/run-vz.sh 'ls /mnt/host'

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASSETS="$HERE/assets/vz"
ROOTFS="$HERE/assets/alpine-rootfs"
SRC="$HERE/vz-spike/main.m"
BIN="$HERE/vz-spike/vz-spike"
ENT="$HERE/vz-spike/entitlements.plist"

# Prereqs
[[ -s "$ASSETS/vmlinuz-virt"             ]] || bash "$HERE/scripts/fetch-vz-assets.sh"
[[ -s "$ASSETS/initramfs-virt"           ]] || bash "$HERE/scripts/fetch-vz-assets.sh"
[[ -s "$ASSETS/initramfs-spike"          ]] || bash "$HERE/scripts/build-initramfs.sh"
[[ -x "$ROOTFS/bin/sh"                   ]] || bash "$HERE/scripts/fetch-alpine-rootfs.sh"
[[ -x "$ROOTFS/usr/local/bin/vsock-send" ]] || bash "$HERE/scripts/build-vsock-send.sh"

echo "→ compiling vz-spike"
clang -O2 -fobjc-arc -fmodules \
    -framework Foundation -framework Virtualization \
    -o "$BIN" "$SRC"

echo "→ codesigning"
codesign --sign - --force --entitlements "$ENT" --options=runtime "$BIN" >/dev/null

# Default exec if none given on CLI.
if [[ $# -gt 0 ]]; then
    CMD="$*"
else
    CMD='echo "=== uname ==="; uname -a; echo; echo "=== os ==="; cat /etc/os-release 2>/dev/null || cat /etc/alpine-release; echo; echo "=== id ==="; id; echo; echo "=== mounts ==="; mount; echo; echo "=== /mnt ==="; ls /mnt 2>/dev/null'
fi

# Optional extra share via SHARE_DIR env.
SHARE_ARGS=()
if [[ -n "${SHARE_DIR:-}" ]]; then
    if [[ ! -d "$SHARE_DIR" ]]; then
        echo "✗ SHARE_DIR=$SHARE_DIR is not a directory" >&2
        exit 1
    fi
    SHARE_ARGS=(--share "host=$SHARE_DIR")
fi

exec "$BIN" \
    --kernel       "$ASSETS/vmlinuz-virt" \
    --initramfs    "$ASSETS/initramfs-spike" \
    --rootfs-share "$ROOTFS" \
    "${SHARE_ARGS[@]}" \
    --exec         "$CMD" \
    --vcpus        1 \
    --mem-mib      512
