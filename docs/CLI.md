# vmette CLI reference

```
vmette --kernel PATH --initramfs PATH [options]
```

## Required

| Flag | Argument | Description |
|------|----------|-------------|
| `--kernel` | PATH | bzImage on x86_64; vmlinuz from alpine `linux-virt` apk. |
| `--initramfs` | PATH | Initramfs built by `scripts/build-initramfs.sh`. |

## Workload

| Flag | Argument | Description |
|------|----------|-------------|
| `--rootfs-share` | PATH | Host directory mounted as guest `/` via virtio-fs (tag `rootfs`). |
| `--ro-rootfs-share` | — | Mount the rootfs share read-only. Disables exit-code propagation (guest can't write `/.vmette-exit`). |
| `--share` | TAG=PATH | Extra virtio-fs mount at `/mnt/<TAG>` in the guest. Repeatable. |
| `--disk` | PATH | Raw block image attached as virtio-blk. Repeatable. |
| `--exec` | CMD | Shell command to run in the guest, then `poweroff -f`. Encoded as base64 in `vmette.exec=<b64>` on the kernel cmdline (~3000 char limit). |
| `--net` | — | Attach virtio-net with NAT. `/init` runs `udhcpc` on eth0. |
| `--switch-root` | — | Use `switch_root` instead of `chroot` for the exec environment. Cleaner PID-1 (useful for systemd-style workloads). |

## Runtime

| Flag | Argument | Description |
|------|----------|-------------|
| `--timeout` | N | Force-stop the VM after N seconds; host exits 124. |
| `--cmdline` | STR | Override the base kernel cmdline. Default: `console=hvc0 quiet`. |
| `--vsock-port` | N | `-1`: disable vsock device entirely. `0`: auto-pick 50000–59999 (default). `>0`: explicit port. The chosen port is exported into the guest's exec env as `VMETTE_VSOCK_PORT`. |
| `--vcpus` | N | Default 1. |
| `--mem-mib` | N | Default 512. |

## Snapshot (Apple Silicon only)

| Flag | Argument | Description |
|------|----------|-------------|
| `--build-snapshot` | PATH | Boot, wait for guest READY, pause, save VM state to PATH, exit. |
| `--resume-snapshot` | PATH | Restore from PATH, send `--exec` via vsock, drain output. Requires `--exec`. |
| `--guest-vsock-port` | N | Port the guest's vsock-runner listens on (default 1025). |

On Intel, snapshot flags exit 1 with a clear error pointing at Apple's
`#if defined(__arm64__)` gate.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Guest exited 0 (or interactive shell ended cleanly). |
| 1 | Library/runtime error (config invalid, VM start failed, snapshot unsupported, etc). |
| 2 | CLI usage error. |
| 124 | `--timeout` reached. |
| _N_ | Guest's exit code (1–123 propagated verbatim from the workload). |

## Guest environment

The guest's exec environment (passed via `/init`) sets:

| Var | Value |
|-----|-------|
| `PATH` | `/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin` |
| `VMETTE_VSOCK_PORT` | Host port the vsock listener bound to (empty if `--vsock-port -1`). |
| `VMETTE_GUEST_VSOCK_PORT` | Set in snapshot-server mode only. |

## Examples

```sh
# basic
vmette --kernel ./assets/vmlinuz-virt --initramfs ./assets/initramfs-vmette \
       --rootfs-share ./assets/alpine-rootfs --exec 'uname -a; exit 0'

# extra share + bidirectional file IO
mkdir -p /tmp/scratch
vmette ... --share host=/tmp/scratch --exec 'date > /mnt/host/from-guest.txt'

# pinned vsock port + roundtrip
vmette ... --vsock-port 9000 --exec 'echo hi | vsock-send 9000'

# bounded run
vmette ... --timeout 30 --exec 'long_command_here'

# read-only host share
vmette ... --ro-rootfs-share --exec 'mount | grep rootfs'
```
