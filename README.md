# sandbox spikes

Three parallel takes on "a local sandbox primitive on this iMac (Intel
Skylake, macOS 14)". Only one runs here; the others are preserved as
portable artifacts for other hosts.

| dir                  | what                                            | runs on this iMac?     |
|----------------------|-------------------------------------------------|------------------------|
| `firecracker-spike/` | Rust client driving Firecracker via UNIX-socket API | ❌ — needs `/dev/kvm`; HVF won't nest on Skylake |
| `libkrun-spike/`     | Rust FFI to libkrun (in-process microVM lib)    | ❌ — libkrun macOS backend is arm64-only |
| `vz-spike/`          | ObjC + Apple's Virtualization.framework, fully wired | ✅ — native HVF, no nesting |

`vz-spike` is the working track. The rest is scaffolding for the day this
moves to a Linux host or an Apple-Silicon Mac.

## vz-spike at a glance

```sh
make vz-run                                           # default probe command
bash scripts/run-vz.sh 'exit 42'                      # exit code propagates → host
bash scripts/run-vz.sh --net 'wget -O - http://example.com | head -5'
bash scripts/run-vz.sh 'echo hi | vsock-send $SPIKE_VSOCK_PORT'   # bidi vsock
bash scripts/run-vz.sh --switch-root 'cat /proc/1/comm'
bash scripts/run-vz.sh --timeout 3 'sleep 30'         # → exit 124
bash scripts/run-vz.sh --ro-rootfs-share 'mount | head -1'
```

Total wall time for a no-op: ~1 s from invocation to prompt back.

End-to-end flow per invocation:

1. `vz-spike` builds a `VZVirtualMachineConfiguration` with:
   - direct Linux kernel boot (`VZLinuxBootLoader`, alpine `vmlinuz-virt`)
   - a repacked initramfs whose `/init` is `scripts/custom-init.sh`
   - virtio console → host stdin/stdout
   - virtio-fs `rootfs` tag → `assets/alpine-rootfs/` (guest `/`)
   - any extra virtio-fs shares from `--share TAG=PATH`
   - any virtio-blk disks from `--disk PATH`
   - virtio-net + NAT when `--net` is passed
   - virtio-vsock device + a host listener on a per-invocation random port
   - virtio-rng + memory balloon
2. The shell command is base64-encoded into the kernel cmdline as
   `spike.exec=<b64>` (so spaces, quotes, semicolons survive).
3. Guest boots in ~0.5 s. `/init` runs `depmod`, `modprobe`s virtio modules,
   mounts virtio-fs shares, optionally configures network, decodes
   `spike.exec`, chroots (or `switch_root`s) into the rootfs share, runs
   the command, writes the exit code to `/.vz-spike-exit`, `poweroff -f`.
4. Apple's VZ fires `guestDidStopVirtualMachine:`; host reads the exit
   file and exits with the guest's code.

## CLI surface

```
vz-spike --kernel PATH --initramfs PATH [options]

required:
  --kernel           PATH      bzImage on x86_64
  --initramfs        PATH      built by scripts/build-initramfs.sh

workload:
  --rootfs-share     PATH      host dir mounted as guest /  (virtio-fs tag 'rootfs')
  --ro-rootfs-share            mount rootfs share read-only (exit-code propagation off)
  --share            TAG=PATH  extra virtio-fs mount at /mnt/<TAG> (repeatable)
  --disk             PATH      raw block image as virtio-blk (repeatable)
  --exec             CMD       shell command to run in guest, then poweroff
  --net                        attach virtio-net with NAT; /init runs udhcpc on eth0
  --switch-root                use switch_root instead of chroot for the exec env

runtime:
  --timeout          N         force-stop the VM after N seconds, exit 124
  --cmdline          STR       extra kernel cmdline (default 'console=hvc0 quiet')
  --vsock-port       N         -1=disable; 0=auto-pick 50000-59999 (default); >0=explicit
                               chosen port exported into the guest as SPIKE_VSOCK_PORT
  --vcpus            N         default 1
  --mem-mib          N         default 512

snapshot (macOS 14+, Apple Silicon only — see below):
  --build-snapshot   PATH      boot, wait for guest READY signal, pause, save
  --resume-snapshot  PATH      restore, send --exec via vsock, drain output
  --guest-vsock-port N         guest vsock-runner listens on this port (default 1025)
```

## Status of each wired feature

| feature                | wired | end-to-end verified on this iMac                              |
|------------------------|-------|---------------------------------------------------------------|
| virtio-fs rootfs share | ✅    | guest `mount` shows `rootfs on / type virtiofs`               |
| virtio-fs extra shares | ✅    | bidirectional file I/O host ↔ guest                           |
| virtio-blk disks       | ✅    | config code present; supply `--disk PATH` to attach           |
| virtio-net + NAT       | ✅    | `--net` → eth0 gets `192.168.64.x`, DNS configured, HTTP works |
| virtio-vsock device    | ✅    | `/dev/vsock`; bidirectional bytes via `vsock-send`             |
| custom-init cmd runner | ✅    | per-invocation random vsock port; exit code propagates         |
| `--switch-root`        | ✅    | PID 1 is the wrapper script (verified via `/proc/1/comm`)     |
| `--ro-rootfs-share`    | ✅    | rootfs mounts ro; writes return `Read-only file system`        |
| `--timeout N`          | ✅    | force-stop + exit 124 on overrun                              |
| snapshot/restore       | ⚠️    | wired in code; **Apple-Silicon-only at the SDK level**         |

### Snapshot/restore — wired but not runnable here

`vz-spike` includes a full snapshot/resume pipeline:

- Build: `--build-snapshot PATH` boots the guest, runs `/init` through
  device setup + `modprobe`, then execs `vsock-runner` which connects to
  the host and writes `READY\n` before blocking on `accept()` for a
  command. The host receives `READY`, pauses the VM, saves state to PATH.
- Resume: `--resume-snapshot PATH --exec CMD` restores the paused VM,
  resumes it, opens an outgoing vsock connection to the (still-blocked)
  guest `vsock-runner`, writes the command, streams output back, exits
  with the guest's exit code (encoded as a trailing `__EXIT__:<N>`
  marker).

Apple gates `saveMachineStateToURL:` / `restoreMachineStateFromURL:`
behind `#if defined(__arm64__)` in
`Virtualization.framework/Headers/VZVirtualMachine.h`. On Intel Macs the
selectors don't exist — `vz-spike` fails fast with a clear error if you
try, but the guest-side `vsock-runner.c` + `/init` snapshot-mode wiring
is preserved and will work as soon as this runs on an Apple Silicon Mac.

Same pattern as `libkrun-spike/`: scaffolding for a future host, not
runnable locally.

### How the vsock side ended up working

Alpine's stock netboot `linux-virt` kernel (6.6.134) was built without
`CONFIG_VHOST_VSOCK` / `CONFIG_VIRTIO_VSOCKETS` and ships no modules
either, so `/dev/vsock` never showed up. We pull the full
`linux-virt-6.6.141-r0.apk` package (38 MB) instead — its modules tree
includes `vsock.ko.gz` + `vmw_vsock_virtio_transport.ko.gz` + virtiofs
and fuse. `scripts/build-initramfs.sh` extracts busybox from the netboot
initramfs, swaps in the apk's modules tree, and injects our `/init`.

The guest-side tools live at `assets/alpine-rootfs/usr/local/bin/`:

- `vsock-send` (25 KB) — pipes stdin → host vsock, prints any reply.
- `vsock-runner` (30 KB) — the snapshot command server; listens for one
  command, runs it, streams output back, exits.

Both are cross-compiled statically with `x86_64-linux-musl-gcc` from
macOS via `scripts/build-vsock-send.sh`.

## Layout

```
Cargo.toml                workspace = [firecracker-spike, libkrun-spike]
firecracker-spike/        Rust: HTTP-over-UnixStream client (Linux+KVM)
libkrun-spike/            Rust: FFI to libkrun (Apple Silicon / Linux)
vz-spike/
  main.m                  ObjC: VZ config, vsock listener, snapshot flow
  vsock-send.c            C: static musl AF_VSOCK client (guest)
  vsock-runner.c          C: snapshot-mode command server (guest)
  entitlements.plist      com.apple.security.virtualization + .hypervisor
lima/firecracker.yaml     Ubuntu VM that probes /dev/kvm (spoiler: no)
scripts/
  fetch-vz-assets.sh      alpine netboot initramfs + linux-virt apk
  fetch-alpine-rootfs.sh  alpine-minirootfs tarball → assets/alpine-rootfs/
  build-initramfs.sh      repack initramfs with apk modules + custom /init
  build-vsock-send.sh     cross-compile vsock-send + vsock-runner
  custom-init.sh          PID-1: depmod, modprobe, virtiofs, network, chroot/switch_root
  run-vz.sh               compile + sign + run with sensible defaults
  fetch-assets.sh         firecracker kernel+rootfs (unused on this host)
  run-microvm.sh          firecracker boot driver (Linux-only)
tests/fixtures/share/     stable host dir used as --share target in smoke tests
Makefile                  vm-up | probe-kvm | run | vz-assets | vz-init | vz-run | vz-shell
```

## Why the moving parts are the way they are

- **ObjC, not Swift.** This iMac's CommandLineTools install has a mismatched
  `PackageDescription` dylib that breaks SwiftPM *and* direct `swiftc`
  builds. ObjC + clang sidesteps the entire Swift module system. ~60 KB
  binary.
- **Custom `/init` replaces alpine's.** Alpine's `/init` is built around
  `modloop` + `apkovl`, neither of which we have. Our `/init` does the
  minimum: bootstrap busybox applet symlinks (bsdcpio drops the hardlinks
  during repack), mount virtio-fs shares, bring up network if asked,
  decode and exec the command, poweroff.
- **base64'd cmdline, not virtio-fs file.** The cmdline route keeps each
  invocation stateless — fully described by `vz-spike`'s argv, no host
  filesystem mutation between runs.
- **Ad-hoc codesigning.** VZ refuses to run without
  `com.apple.security.virtualization`. `codesign --sign -` with an
  entitlements plist is enough for local use — no Developer ID required.
- **Random vsock port per invocation.** Default `--vsock-port 0` picks a
  port in 50000-59999, so two parallel `vz-spike` runs never collide on
  the host listener. The chosen port is exported into the guest exec env
  as `SPIKE_VSOCK_PORT`.
- **Apk's kernel + apk's modules, busybox from netboot initramfs.** The
  netboot initramfs is a convenient busybox-shipping vehicle but its
  bundled modules don't match the apk's kernel; we replace `lib/modules`
  wholesale at repack time.

## What's left

- **Snapshot/restore on Apple Silicon.** Code is here, needs the right
  hardware to actually run.
- **Real ext4 disk image** (via `--disk`) when the workload needs POSIX
  semantics virtio-fs can't deliver (some database engines, dbus sockets).
- **Proper vsock RPC.** The host listener just echoes. Could swap for a
  framed protocol on host + matching helper in guest for a real control
  plane that bypasses the serial console entirely.
- **`/init` log forwarding over vsock.** Currently `/init` and guest cmd
  output interleave on host stdout. A second vsock port for init logs
  would clean that up. Modest improvement; deferred.
