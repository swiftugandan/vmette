#!/usr/bin/env bash
# Fetch the assets we need for vz-spike:
#   * Alpine netboot initramfs (we use it for busybox + base tree)
#   * Alpine linux-virt apk     (matching kernel + a complete modules tree,
#                                including the vsock + virtiofs bits that
#                                aren't shipped in netboot/initramfs-virt)
#
# Final layout under assets/vz/ :
#   vmlinuz-virt              ← from the apk (matches its modules)
#   initramfs-virt            ← from netboot (busybox source for repack)
#   linux-virt.apk            ← raw apk, kept so we can re-extract on demand
#   linux-virt-extract/       ← extracted apk tree

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$HERE/assets/vz"
SERIES="${ALPINE_SERIES:-3.20}"
ARCH="${ARCH:-x86_64}"
APK_NAME="${APK_NAME:-linux-virt-6.6.141-r0.apk}"

NETBOOT_BASE="https://dl-cdn.alpinelinux.org/alpine/v${SERIES}/releases/${ARCH}/netboot"
MAIN_BASE="https://dl-cdn.alpinelinux.org/alpine/v${SERIES}/main/${ARCH}"

mkdir -p "$DEST"

fetch() {
    local url="$1" out="$2"
    if [[ -s "$out" ]]; then
        echo "✓ $(basename "$out") already present"
        return
    fi
    echo "→ downloading $(basename "$out")"
    curl -fsSL --retry 3 -o "$out" "$url"
}

fetch "$NETBOOT_BASE/initramfs-virt"  "$DEST/initramfs-virt"
fetch "$MAIN_BASE/$APK_NAME"          "$DEST/linux-virt.apk"

if [[ ! -d "$DEST/linux-virt-extract/boot" ]]; then
    echo "→ extracting linux-virt apk"
    rm -rf "$DEST/linux-virt-extract"
    mkdir -p "$DEST/linux-virt-extract"
    # apk files are tar.gz; macOS tar reads them fine.
    tar -xzf "$DEST/linux-virt.apk" -C "$DEST/linux-virt-extract"
fi

# Use the apk's kernel so it matches the apk's modules.
cp -f "$DEST/linux-virt-extract/boot/vmlinuz-virt" "$DEST/vmlinuz-virt"

KVER="$(ls "$DEST/linux-virt-extract/lib/modules/" 2>/dev/null | head -1)"
echo
echo "VZ assets ready (kernel $KVER):"
ls -lh "$DEST"
