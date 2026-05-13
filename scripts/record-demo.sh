#!/usr/bin/env bash
# Record the fj README demo from scripts/demo.tape using vhs.
#
# vhs is from Charmbracelet (https://github.com/charmbracelet/vhs):
# a declarative tape format that drives a headless terminal and writes
# GIF/MP4/WebM directly. Reproducible, scriptable, no manual typing.
#
# Pre-flight:
#   * `brew install vhs` (or download from the vhs releases page)
#   * `cargo build --release && ln -sf $PWD/target/release/fj ~/.local/bin/fj`
#   * `fj auth login --host rasterhub.com`
#
# Outputs:
#   assets/demo.gif    embed in README
#   assets/demo.mp4    optional, higher-fidelity for social cards
#
# Usage:
#   ./scripts/record-demo.sh                 # records both outputs
#   ./scripts/record-demo.sh --gif-only      # GIF only (faster)

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

GIF_ONLY=0
case "${1:-}" in
  --gif-only) GIF_ONLY=1 ;;
  "") ;;
  --help|-h)
    sed -n '/^# /{s/^# \{0,1\}//;p;}' "$0" | head -25
    exit 0
    ;;
  *)
    echo "unknown arg: $1 (try --help)" >&2
    exit 2
    ;;
esac

require() {
  local cmd=$1 hint=$2
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "missing $cmd. $hint" >&2
    exit 1
  fi
}

require vhs "Install with: brew install vhs"
require fj  "Build with: cargo build --release && ln -sf \$PWD/target/release/fj ~/.local/bin/fj"

if ! fj auth status >/dev/null 2>&1; then
  echo "fj is not authenticated. Run: fj auth login --host rasterhub.com" >&2
  exit 1
fi

mkdir -p assets

if [ "$GIF_ONLY" = 1 ]; then
  TAPE=$(mktemp -t fj-demo.XXXXXX.tape)
  trap 'rm -f "$TAPE"' EXIT
  grep -v '^Output assets/demo\.mp4' scripts/demo.tape > "$TAPE"
  vhs "$TAPE"
else
  vhs scripts/demo.tape
fi

echo ""
echo "✓ wrote assets/demo.gif"
[ "$GIF_ONLY" = 1 ] || echo "✓ wrote assets/demo.mp4"
