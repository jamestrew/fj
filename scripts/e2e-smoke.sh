#!/usr/bin/env bash
# Live-API smoke: hit a curated set of read-only endpoints via the just-built
# binary. Requires the user to be logged in (`fj auth login`). Designed for
# the pre-push hook when FJ_E2E=1.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

FJ="${FJ_BIN:-./target/release/fj}"
TEST_REPO="${FJ_E2E_REPO:-stephen/fj-cli-test}"

run() {
  printf '  • %s\n' "$*"
  "$@" >/dev/null
}

if [[ ! -x "$FJ" ]]; then
  echo "missing $FJ; run cargo build --release first" >&2
  exit 1
fi

echo "== auth =="
run "$FJ" auth status
run "$FJ" auth list

echo "== api =="
run "$FJ" api /version
run "$FJ" api /user

echo "== repo =="
run "$FJ" repo list -L 5
run "$FJ" repo view "$TEST_REPO"
run "$FJ" repo view "$TEST_REPO" --json

echo "== issue =="
run "$FJ" issue list -R "$TEST_REPO" --state all
run "$FJ" issue list -R "$TEST_REPO" --json

echo "== pr =="
run "$FJ" pr list -R "$TEST_REPO" --state all
run "$FJ" pr list -R "$TEST_REPO" --json

# The command modules below are intentionally excluded from unit-test
# coverage (see COV_IGNORE in the Makefile): they are thin handlers over the
# api layer. This smoke is the automated check that actually exercises them,
# so keep a read-only invocation here for every excluded command surface.
echo "== labels / milestones =="
run "$FJ" label list -R "$TEST_REPO"
run "$FJ" milestone list -R "$TEST_REPO"

echo "== branches / topics / protect =="
run "$FJ" repo branches "$TEST_REPO"
run "$FJ" repo topics "$TEST_REPO"
run "$FJ" protect list -R "$TEST_REPO"

echo "== actions =="
run "$FJ" workflow list -R "$TEST_REPO"
run "$FJ" run list -R "$TEST_REPO" -L 5

echo "== search =="
run "$FJ" search repos "$TEST_REPO" -L 3
run "$FJ" search issues "is:issue" -L 3

echo "✓ e2e smoke passed against $TEST_REPO"
