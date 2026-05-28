SHELL      := /bin/bash
VM_NAME    := firecracker-spike
PROJECT    := $(CURDIR)

.PHONY: help vm-up vm-shell vm-stop vm-delete assets build run probe-kvm vz-assets vz-run clean

help:
	@awk -F':.*##' '/^[a-zA-Z_-]+:.*##/ { printf "  %-14s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

vm-up:        ## Start the Lima VM (idempotent)
	limactl start --name=$(VM_NAME) --tty=false lima/firecracker.yaml || \
	  limactl start $(VM_NAME)

vm-shell:     ## Open an interactive shell in the Lima VM
	limactl shell $(VM_NAME)

vm-stop:      ## Stop the Lima VM
	limactl stop $(VM_NAME)

vm-delete:    ## Destroy the Lima VM
	limactl delete -f $(VM_NAME)

probe-kvm:    ## Show whether the Lima guest has /dev/kvm
	limactl shell $(VM_NAME) cat /var/log/kvm-probe.log

assets:       ## Download vmlinux + rootfs.ext4 (runs inside the VM)
	limactl shell $(VM_NAME) bash -lc "cd $(PROJECT) && bash scripts/fetch-assets.sh"

build:        ## Build the rust client inside the VM
	limactl shell $(VM_NAME) bash -lc "cd $(PROJECT) && source ~/.cargo/env && cargo build --release"

run: build assets ## Boot a microVM inside the Lima guest
	limactl shell $(VM_NAME) bash -lc "cd $(PROJECT) && bash scripts/run-microvm.sh"

vz-assets:    ## Download Alpine vmlinuz + initramfs + minirootfs
	bash scripts/fetch-vz-assets.sh
	bash scripts/fetch-alpine-rootfs.sh

vz-init: vz-assets ## Repack initramfs with vz-spike's custom /init
	bash scripts/build-initramfs.sh

vz-vsock-send: vz-assets ## Build static musl vsock-send into alpine-rootfs
	bash scripts/build-vsock-send.sh

vz-run: vz-init   ## Build + sign vz-spike, boot guest, run default probe command
	bash scripts/run-vz.sh

vz-shell: vz-init ## Boot guest with no --exec; lands in interactive chroot shell
	bash scripts/run-vz.sh 'exec /bin/sh -l'

clean:        ## Remove build artifacts and downloaded assets
	rm -rf target assets vz-spike/vz-spike
	rm -rf /tmp/vz-share-test
	rm -f tests/fixtures/share/from-guest*
