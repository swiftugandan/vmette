SHELL := /bin/bash

.PHONY: help build dev guest-stale-check header universal dist publish release assets init guest-bin guest-assets-all desktop-agent desktop-agent-all desktop-image run shell test test-desktop test-view clean

# Host guest arch + the guest-side artifacts whose freshness `build`/`dev` check.
# These are produced by `make init` / `make desktop-agent`, NOT by `make build`
# (which only compiles + signs the host binaries), so a guest boot can silently
# use a stale initramfs or a missing agent. guest-stale-check turns those latent
# failures (kernel panic / "vsock unavailable: agent stream closed") into a loud
# build-time reminder.
GUEST_ARCH := $(shell . scripts/guest-arch.sh && vmette_guest_arch)
INITRAMFS  := assets/$(GUEST_ARCH)/initramfs-vmette
AGENT_DIR  := assets/$(GUEST_ARCH)/desktop-agent

help:
	@awk -F':.*##' '/^[a-zA-Z_-]+:.*##/ { printf "  %-12s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

build:         ## cargo build the workspace + codesign vmette/vmetted/vmette-mcp (host arch)
	cargo build --release
	codesign --sign - --force --entitlements entitlements.plist --options=runtime target/release/vmette
	codesign --sign - --force --entitlements entitlements.plist --options=runtime target/release/vmetted
	# vmette-mcp boots one-shot VMs in-process (execute/fetch_url/workspace), so it
	# needs the virtualization entitlement too — it no longer forks the vmette CLI.
	codesign --sign - --force --entitlements entitlements.plist --options=runtime target/release/vmette-mcp
	@$(MAKE) --no-print-directory guest-stale-check

dev:           ## Full live-test prep: build + sign host bins AND rebuild the guest initramfs + desktop agent
	$(MAKE) build
	$(MAKE) init
	$(MAKE) desktop-agent

guest-stale-check: ## Warn (non-fatally) when the guest initramfs/desktop agent are stale vs their sources
	@if [ ! -f "$(INITRAMFS)" ]; then \
	    echo "⚠ $(INITRAMFS) missing — run 'make init' before booting a guest"; \
	elif [ scripts/custom-init.sh -nt "$(INITRAMFS)" ] || [ scripts/build-initramfs.sh -nt "$(INITRAMFS)" ]; then \
	    echo "⚠ $(INITRAMFS) is older than its sources — run 'make init' (a stale initramfs panics or drops to a shell)"; \
	fi
	@if [ ! -x "$(AGENT_DIR)/vmette-desktop-agent" ]; then \
	    echo "⚠ $(AGENT_DIR) missing — run 'make desktop-agent' before a desktop session (else the daemon falls back to a stale in-image agent)"; \
	elif [ guest/vmette-desktop-agent.c -nt "$(AGENT_DIR)/vmette-desktop-agent" ]; then \
	    echo "⚠ $(AGENT_DIR) is older than guest/vmette-desktop-agent.c — run 'make desktop-agent'"; \
	fi

header:        ## Regenerate the checked-in C header from src/ffi.rs (cbindgen)
	cargo build -p vmette --features regenerate-header

# Publish the library crates to crates.io, dependencies before dependents.
# cargo publish waits for each crate to land in the index before returning, so
# the next dependent can resolve it. The three binary crates are publish=false
# and intentionally skipped. Pass FLAGS=--dry-run to rehearse a single crate
# (dependents can only be dry-run once their deps are already on crates.io).
publish:       ## Publish the vmette library crates to crates.io (in dep order)
	cargo publish $(FLAGS) -p vmette-proto
	cargo publish $(FLAGS) -p vmette-assets
	cargo publish $(FLAGS) -p vmette
	cargo publish $(FLAGS) -p vmette-provider-oci
	cargo publish $(FLAGS) -p vmette-provider-squashfs
	cargo publish $(FLAGS) -p vmette-provider-tar
	cargo publish $(FLAGS) -p vmette-providers

release:       ## Cut a release: make release VERSION=X.Y.Z [DRY_RUN=1] [YES=1] — bumps, gates, tags, publishes, pushes
	@VERSION="$(VERSION)" DRY_RUN="$(DRY_RUN)" RELEASE_YES="$(YES)" bash scripts/release.sh

universal:     ## Build a fat x86_64+arm64 binary at target/universal/release/
	@rustup target add x86_64-apple-darwin aarch64-apple-darwin
	cargo build --release --target x86_64-apple-darwin  --workspace
	cargo build --release --target aarch64-apple-darwin --workspace
	mkdir -p target/universal/release
	lipo -create -output target/universal/release/vmette \
	    target/x86_64-apple-darwin/release/vmette \
	    target/aarch64-apple-darwin/release/vmette
	lipo -create -output target/universal/release/vmetted \
	    target/x86_64-apple-darwin/release/vmetted \
	    target/aarch64-apple-darwin/release/vmetted
	lipo -create -output target/universal/release/vmette-mcp \
	    target/x86_64-apple-darwin/release/vmette-mcp \
	    target/aarch64-apple-darwin/release/vmette-mcp
	lipo -create -output target/universal/release/libvmette.dylib \
	    target/x86_64-apple-darwin/release/libvmette.dylib \
	    target/aarch64-apple-darwin/release/libvmette.dylib
	codesign --sign - --force --entitlements entitlements.plist --options=runtime \
	    target/universal/release/vmette
	codesign --sign - --force --entitlements entitlements.plist --options=runtime \
	    target/universal/release/vmetted
	# vmette-mcp boots one-shot VMs in-process now, so it needs the entitlement too.
	codesign --sign - --force --entitlements entitlements.plist --options=runtime \
	    target/universal/release/vmette-mcp
	@lipo -info target/universal/release/vmette target/universal/release/vmetted target/universal/release/vmette-mcp target/universal/release/libvmette.dylib

assets:        ## Download alpine vmlinuz + initramfs + minirootfs for the host guest arch
	bash scripts/fetch-assets.sh
	bash scripts/fetch-alpine-rootfs.sh

init: assets   ## Repack initramfs with vmette's custom /init
	bash scripts/build-initramfs.sh

guest-bin: assets  ## Cross-compile static guest helpers for the host guest arch
	bash scripts/build-vsock-send.sh

guest-assets-all: ## Build boot assets + guest helpers for both x86_64 and aarch64 guests
	for arch in x86_64 aarch64; do \
	    ARCH=$$arch bash scripts/fetch-assets.sh; \
	    ARCH=$$arch bash scripts/fetch-alpine-rootfs.sh; \
	    ARCH=$$arch bash scripts/build-initramfs.sh; \
	    ARCH=$$arch bash scripts/build-vsock-send.sh; \
	done

desktop-agent: ## Build the host-injected static desktop agent (host guest arch) → assets/<arch>/desktop-agent/
	bash scripts/build-desktop-agent-static.sh

desktop-agent-all: ## Cross-compile the static desktop agent for both x86_64 and aarch64 guests (no Docker)
	for arch in x86_64 aarch64; do \
	    ARCH=$$arch bash scripts/build-desktop-agent-static.sh || exit 1; \
	done

desktop-image: ## OPTIONAL: build a custom desktop rootfs → assets/<arch>/vmette-desktop-rootfs.tar (needs Docker; default start uses the published image)
	bash scripts/build-desktop-image.sh --export

run: init guest-bin   ## Build + sign vmette, boot guest, run default probe
	bash scripts/run.sh

shell: init guest-bin ## Boot guest with no --exec; interactive shell
	bash scripts/run.sh 'exec /bin/sh -l'

test:          ## Run cargo unit tests + end-to-end one-shot VM smoke
	cargo test --workspace
	bash tests/run.sh

test-desktop:  ## End-to-end desktop smoke: boots a real Xvfb desktop VM via vmetted (pulls the published desktop image on first use)
	bash tests/desktop.sh

test-view:     ## End-to-end live-view (VNC) smoke: opens a desktop_view and drives it with an RFB client
	bash tests/view.sh

VERSION   ?= $(shell git describe --tags --abbrev=0 2>/dev/null || echo v0.1.0-dev)
DIST_NAME := vmette-$(VERSION)-universal-apple-darwin

dist: universal guest-assets-all desktop-agent-all header ## Produce dist/$(DIST_NAME).tar.gz with universal binaries + both guest arch assets/helpers + desktop agents + LICENSE
	rm -rf dist
	mkdir -p dist/staging/$(DIST_NAME)/{bin,lib,include,assets,share/vmette/guest}
	cp target/universal/release/vmette     dist/staging/$(DIST_NAME)/bin/
	cp target/universal/release/vmetted    dist/staging/$(DIST_NAME)/bin/
	cp target/universal/release/vmette-mcp dist/staging/$(DIST_NAME)/bin/
	cp target/universal/release/libvmette.dylib dist/staging/$(DIST_NAME)/lib/
	cp crates/vmette/include/vmette.h      dist/staging/$(DIST_NAME)/include/
	for arch in x86_64 aarch64; do \
	    if [[ -s assets/$$arch/vmlinuz-virt && -s assets/$$arch/initramfs-vmette ]]; then \
	        mkdir -p dist/staging/$(DIST_NAME)/assets/$$arch; \
	        cp assets/$$arch/vmlinuz-virt dist/staging/$(DIST_NAME)/assets/$$arch/; \
	        cp assets/$$arch/initramfs-vmette dist/staging/$(DIST_NAME)/assets/$$arch/; \
	    fi; \
	    if [[ -x assets/$$arch/alpine-rootfs/usr/local/bin/vsock-send ]]; then \
	        mkdir -p dist/staging/$(DIST_NAME)/share/vmette/guest/$$arch; \
	        cp assets/$$arch/alpine-rootfs/usr/local/bin/vsock-send dist/staging/$(DIST_NAME)/share/vmette/guest/$$arch/; \
	        cp assets/$$arch/alpine-rootfs/usr/local/bin/vsock-runner dist/staging/$(DIST_NAME)/share/vmette/guest/$$arch/; \
	    fi; \
	    if [[ -f assets/$$arch/desktop-agent/vmette-desktop-agent ]]; then \
	        mkdir -p dist/staging/$(DIST_NAME)/assets/$$arch/desktop-agent; \
	        cp assets/$$arch/desktop-agent/vmette-desktop-agent  dist/staging/$(DIST_NAME)/assets/$$arch/desktop-agent/; \
	        cp assets/$$arch/desktop-agent/vmette-desktop-run.sh dist/staging/$(DIST_NAME)/assets/$$arch/desktop-agent/; \
	        chmod +x dist/staging/$(DIST_NAME)/assets/$$arch/desktop-agent/vmette-desktop-agent \
	                 dist/staging/$(DIST_NAME)/assets/$$arch/desktop-agent/vmette-desktop-run.sh; \
	    fi; \
	done
	if [[ -s assets/vmlinuz-virt && -s assets/initramfs-vmette ]]; then \
	    cp assets/vmlinuz-virt dist/staging/$(DIST_NAME)/assets/; \
	    cp assets/initramfs-vmette dist/staging/$(DIST_NAME)/assets/; \
	fi
	if [[ -x assets/alpine-rootfs/usr/local/bin/vsock-send ]]; then \
	    cp assets/alpine-rootfs/usr/local/bin/vsock-send dist/staging/$(DIST_NAME)/share/vmette/guest/; \
	    cp assets/alpine-rootfs/usr/local/bin/vsock-runner dist/staging/$(DIST_NAME)/share/vmette/guest/; \
	fi
	cp entitlements.plist                  dist/staging/$(DIST_NAME)/
	cp LICENSE                             dist/staging/$(DIST_NAME)/
	cp README.md                           dist/staging/$(DIST_NAME)/
	tar -C dist/staging -czf dist/$(DIST_NAME).tar.gz $(DIST_NAME)
	rm -rf dist/staging
	cd dist && shasum -a 256 *.tar.gz > SHA256SUMS
	@echo
	@echo "✓ dist artifacts:"
	@ls -lh dist/

clean:         ## Remove build artifacts and downloaded assets
	cargo clean
	rm -rf assets
	rm -f tests/fixtures/share/from-guest*
