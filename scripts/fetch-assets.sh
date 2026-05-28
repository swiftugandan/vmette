#!/usr/bin/env bash
# Fetch a known-good Firecracker kernel + rootfs for x86_64.
#
# Sources are the official Firecracker CI bucket. Pinned versions so the spike
# is reproducible.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASSETS="$HERE/assets"
mkdir -p "$ASSETS"

ARCH="${ARCH:-x86_64}"
BASE="https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.10/${ARCH}"
KERNEL_NAME="vmlinux-5.10.225"
ROOTFS_NAME="ubuntu-22.04.ext4"

fetch() {
  local url="$1" out="$2"
  if [[ -s "$out" ]]; then
    echo "✓ $(basename "$out") already present"
    return
  fi
  echo "→ downloading $(basename "$out")"
  curl -fsSL --retry 3 -o "$out" "$url"
}

fetch "$BASE/$KERNEL_NAME"            "$ASSETS/vmlinux"
fetch "$BASE/$ROOTFS_NAME"            "$ASSETS/rootfs.ext4"
fetch "$BASE/$ROOTFS_NAME.id_rsa"     "$ASSETS/rootfs.id_rsa" || true
chmod 600 "$ASSETS/rootfs.id_rsa" 2>/dev/null || true

echo
echo "Assets ready in $ASSETS:"
ls -lh "$ASSETS"
