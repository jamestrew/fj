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
MANDIR     ?= $(PREFIX)/share/man/man1
# A stable code-signing identity (see `security find-identity -v -p codesigning`).
SIGN_ID    ?= Developer ID Application: Raster & State LLC (62Y3FRM8PD)
# Pin the signing identifier so every re-sign satisfies the same keychain trust.
IDENTIFIER ?= fj-a88ab591fd97b299
UNAME      := $(shell uname)

.PHONY: build install man test lint audit coverage coverage-html coverage-strict coverage-strict-html release-preflight

# Files excluded from coverage so the number reflects what we actually
# intend to unit-test, not glue we don't.
#
# What we DO test (kept in the report): the api/ wrappers (via the wiremock
# suite in client/integration_tests.rs), the HTTP client, config/git/auth/
# fjord/output layers, and every pure helper or parser. Command modules that
# also hold tested helpers (pr, repo, issue, release, workflow_run, key,
# api, instances, …) stay in too; their partial number is honest.
#
# What we DON'T unit-test (excluded here):
#   - entry + dispatch wiring: main.rs, cli/mod.rs
#   - platform / process glue: output/pager.rs, cli/web.rs, cli/browse.rs,
#     cli/extension.rs
#   - thin async command handlers that only orchestrate the (tested) api
#     layer: cli/auth*.rs, config, org, status, hook, protect, label,
#     milestone, search, workflow, workflow_variable, repo_mirror,
#     repo_social, issue_comment, pr_inspect, pr_status.
#     Their read paths are exercised end-to-end by scripts/e2e-smoke.sh
#     (FJ_E2E=1); write paths are manually verified.
#
# Rule of thumb: a cli command file with no `#[cfg(test)]` module is glue we
# don't unit-test; add it here when you add such a file (or drop it once it
# grows a test module). main.rs and cli/mod.rs are an exception: they stay
# excluded as entry/dispatch even though cli/mod.rs carries a structural
# command-tree guard test (that test validates the clap tree, not the
# dispatch match, so counting the match arms would just re-add noise).
COV_IGNORE := src/(main\.rs|output/pager\.rs|cli/(mod|branch|tag|web|browse|extension|auth|auth_login|config|org|status|hook|protect|label|milestone|search|workflow|workflow_variable|repo_mirror|repo_social|issue_comment|pr_inspect|pr_status)\.rs)

build:
	cargo build --release

# Generate the man pages (top level + one per subcommand) into target/man via
# the built binary. `fj man` renders them from the clap definitions, so they
# never drift from the actual command surface. Generation reads no token, so it
# is safe to run an unsigned binary here.
man: build
	target/release/fj man -o target/man
	@echo "Generated man pages in target/man"

# Build, install a real copy to $(BINDIR), install the man pages to $(MANDIR)
# so `man fj` works out of the box, and (on macOS) code-sign the binary with a
# stable identity so the keychain "Always Allow" survives future rebuilds.
install: build
	install -d "$(BINDIR)"
	rm -f "$(BINDIR)/fj"
	install -m 0755 target/release/fj "$(BINDIR)/fj"
	target/release/fj man -o target/man
	install -d "$(MANDIR)"
	install -m 0644 target/man/*.1 "$(MANDIR)"
ifeq ($(UNAME),Darwin)
	codesign --force --sign "$(SIGN_ID)" --identifier "$(IDENTIFIER)" "$(BINDIR)/fj"
	@echo "Installed and signed $(BINDIR)/fj"
	@echo "Man pages installed to $(MANDIR) (run: man fj)"
	@echo 'Run any fj command once and click "Always Allow" — it will stick across rebuilds.'
else
	@echo "Installed $(BINDIR)/fj"
	@echo "Man pages installed to $(MANDIR) (run: man fj)"
endif

test:
	cargo test --all

# Validate the release environment BEFORE pushing a v* tag: Cargo.toml version
# vs the tag, an online runner for each required label, and that the signing /
# notarization secrets are set. See scripts/release-preflight.sh.
#   make release-preflight TAG=v0.2.1
release-preflight:
	@bash scripts/release-preflight.sh $(TAG)

lint:
	cargo clippy --all-targets --all-features -- -D warnings
	cargo fmt --all -- --check

audit:
	cargo audit --ignore RUSTSEC-2024-0370 -D warnings

# Line/region coverage over the surface we intend to unit-test (see
# COV_IGNORE above). Needs cargo-llvm-cov: `cargo install cargo-llvm-cov`.
#
# Two tiers:
#   - `coverage` (stable): excludes whole glue files via COV_IGNORE. The
#     mixed command modules (pr, repo, issue, …) stay in, so their untested
#     async handlers still count. ~50%. Works on any toolchain.
#   - `coverage-strict` (nightly): additionally drops the async command
#     handlers inside those mixed files, via #[cfg_attr(coverage_nightly,
#     coverage(off))] on every cli `async fn`. This is the number that
#     reflects intent most precisely (only tested helpers + the api/client/
#     util layers count). ~76%. Needs a nightly toolchain with
#     llvm-tools-preview: `rustup toolchain install nightly && rustup
#     component add llvm-tools-preview --toolchain nightly`.
coverage:
	cargo llvm-cov --all --summary-only --ignore-filename-regex '$(COV_IGNORE)'

# Same scope, rendered as a browsable HTML report under target/llvm-cov/html.
coverage-html:
	cargo llvm-cov --all --open --ignore-filename-regex '$(COV_IGNORE)'

# Intent-precise coverage: handler regions excluded on top of COV_IGNORE.
# COV_MIN is the minimum line coverage; 0 locally (pure report), set higher
# in CI (see .forgejo/workflows/ci.yml) so a regression fails the build.
COV_MIN ?= 0
coverage-strict:
	cargo +nightly llvm-cov --all --summary-only \
		--fail-under-lines $(COV_MIN) --ignore-filename-regex '$(COV_IGNORE)'

coverage-strict-html:
	cargo +nightly llvm-cov --all --open --ignore-filename-regex '$(COV_IGNORE)'
