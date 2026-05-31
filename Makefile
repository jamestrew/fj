# Local dev tasks for fj.
#
# The important one is `make install`. fj reads its token from the macOS
# keychain, and the keychain pins "Always Allow" to the binary's code
# signature. An ad-hoc signature (what `cargo build` produces) is pinned to
# the cdhash, which changes on every rebuild, so the prompt comes back each
# time. `make install` puts a real copy on PATH and signs it with a STABLE
# Developer ID identity, so the trust survives rebuilds. Click "Always Allow"
# once after the first signed install and it sticks.
#
# Override on the command line if your identity or prefix differ, e.g.
#   make install PREFIX=$HOME/.cargo SIGN_ID="Apple Development: ..."

PREFIX     ?= $(HOME)/.local
BINDIR     := $(PREFIX)/bin
# A stable code-signing identity (see `security find-identity -v -p codesigning`).
SIGN_ID    ?= Developer ID Application: Raster & State LLC (62Y3FRM8PD)
# Pin the signing identifier so every re-sign satisfies the same keychain trust.
IDENTIFIER ?= fj-a88ab591fd97b299
UNAME      := $(shell uname)

.PHONY: build install test lint

build:
	cargo build --release

# Build, install a real copy to $(BINDIR), and (on macOS) code-sign it with a
# stable identity so the keychain "Always Allow" survives future rebuilds.
install: build
	install -d "$(BINDIR)"
	rm -f "$(BINDIR)/fj"
	install -m 0755 target/release/fj "$(BINDIR)/fj"
ifeq ($(UNAME),Darwin)
	codesign --force --sign "$(SIGN_ID)" --identifier "$(IDENTIFIER)" "$(BINDIR)/fj"
	@echo "Installed and signed $(BINDIR)/fj"
	@echo 'Run any fj command once and click "Always Allow" — it will stick across rebuilds.'
else
	@echo "Installed $(BINDIR)/fj"
endif

test:
	cargo test --all

lint:
	cargo clippy --all-targets --all-features -- -D warnings
	cargo fmt --all -- --check
