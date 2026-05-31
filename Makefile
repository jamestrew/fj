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

.PHONY: build install test lint coverage coverage-html

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
#     layer and are exercised by the e2e smoke instead: cli/auth*.rs,
#     config, org, status, hook, protect, label, milestone, search,
#     workflow, workflow_variable, repo_mirror, repo_social, issue_comment,
#     pr_inspect, pr_status, context
#
# Rule of thumb: a cli command file with no `#[cfg(test)]` module is glue we
# don't unit-test; add it here when you add such a file (or drop it once it
# grows a test module). main.rs and cli/mod.rs are an exception: they stay
# excluded as entry/dispatch even though cli/mod.rs carries a structural
# command-tree guard test (that test validates the clap tree, not the
# dispatch match, so counting the match arms would just re-add noise).
COV_IGNORE := src/(main\.rs|output/pager\.rs|cli/(mod|web|browse|extension|context|auth|auth_login|config|org|status|hook|protect|label|milestone|search|workflow|workflow_variable|repo_mirror|repo_social|issue_comment|pr_inspect|pr_status)\.rs)

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

# Line/region coverage over the surface we intend to unit-test (see
# COV_IGNORE above). Needs cargo-llvm-cov: `cargo install cargo-llvm-cov`.
coverage:
	cargo llvm-cov --all --summary-only --ignore-filename-regex '$(COV_IGNORE)'

# Same scope, rendered as a browsable HTML report under target/llvm-cov/html.
coverage-html:
	cargo llvm-cov --all --open --ignore-filename-regex '$(COV_IGNORE)'
