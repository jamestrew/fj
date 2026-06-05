#!/usr/bin/env bash
# Record the fj README demo GIFs from scripts/demo-*.tape using vhs.
#
# vhs is from Charmbracelet (https://github.com/charmbracelet/vhs): a
# declarative tape format that drives a headless terminal and writes a GIF
# directly. Reproducible, scriptable, no manual typing.
#
# The footage runs against the public demo repo rasterstate/fj-demo, never fj
# itself, so it isn't self-referential. Seed that repo once with
# scripts/seed-demo.sh, then point this at the resulting clone.
#
# Pre-flight:
#   * brew install vhs
#   * fj on PATH and `fj auth login --host rasterhub.com` done
#   * ./scripts/seed-demo.sh    (creates + seeds rasterstate/fj-demo)
#
# Usage:
#   FJ_DEMO_DIR=/path/to/fj-demo ./scripts/record-demo.sh         # all three
#   FJ_DEMO_DIR=/path/to/fj-demo ./scripts/record-demo.sh stacks  # one tape
#
# Outputs (embedded in README.md):
#   assets/demo-everyday.gif
#   assets/demo-stacks.gif
#   assets/demo-automation.gif

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

export FJ_DEMO_DIR="${FJ_DEMO_DIR:-/tmp/fj-demo}"

command -v vhs >/dev/null || { echo "vhs not found. Install with: brew install vhs" >&2; exit 1; }
command -v fj  >/dev/null || { echo "fj not found on PATH." >&2; exit 1; }
[ -d "$FJ_DEMO_DIR/.git" ] || { echo "No clone at FJ_DEMO_DIR=$FJ_DEMO_DIR. Run scripts/seed-demo.sh first." >&2; exit 1; }

mkdir -p assets
if [ $# -gt 0 ]; then tapes=("$@"); else tapes=(everyday stacks automation); fi

for t in "${tapes[@]}"; do
  echo "Recording scripts/demo-$t.tape ..."
  vhs "scripts/demo-$t.tape"
  echo "✓ wrote assets/demo-$t.gif"
done
