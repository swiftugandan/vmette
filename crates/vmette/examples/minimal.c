/*
 * Minimal C caller of libvmette. Boots an alpine guest and runs a
 * one-shot command via the C ABI.
 *
 * Build (from repo root):
 *   clang -I crates/vmette/include -L target/release -lvmette \
 *         -Wl,-rpath,@executable_path/../target/release \
 *         -o examples/minimal_c crates/vmette/examples/minimal.c
 *   codesign --sign - --force --entitlements entitlements.plist \
 *            --options=runtime examples/minimal_c
 *
 * Run:
 *   ./examples/minimal_c \
 *       ./assets/vmlinuz-virt ./assets/initramfs-vmette \
 *       ./assets/alpine-rootfs \
 *       'echo "hello from C"; exit 42'
 */

#include <stdio.h>
#include <stdlib.h>
#include "vmette.h"

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: %s KERNEL INITRAMFS ROOTFS [CMD]\n", argv[0]);
        return 2;
    }
    const char *cmd = (argc >= 5) ? argv[4] : "uname -a; cat /etc/alpine-release; exit 0";

    fprintf(stderr, "vmette version: %s\n", vmette_version());

    vmette_config_t *cfg = vmette_config_new(argv[1], argv[2]);
    if (!cfg) {
        fprintf(stderr, "error: vmette_config_new failed\n");
        return 1;
    }
    vmette_config_set_rootfs_share(cfg, argv[3], /*read_only=*/false);
    vmette_config_set_exec(cfg, cmd);
    vmette_config_set_vsock_port(cfg, 0); /* auto-allocate */

    vmette_run_output_t *out = NULL;
    VmetteStatus rc = vmette_run(cfg, &out);
    /* In the happy path vmette_run never returns — the guest's exit
     * code propagates via the delegate's std::process::exit. We only
     * reach here for snapshot-build/resume paths or hard errors. */
    if (rc != Ok) {
        fprintf(stderr, "vmette_run returned status %d\n", (int)rc);
        vmette_config_free(cfg);
        return 1;
    }
    int code = vmette_run_output_exit_code(out);
    vmette_run_output_free(out);
    vmette_config_free(cfg);
    return code;
}
