#!/usr/bin/env bash
# Record an asciinema demo of fj. The result is a .cast file you can:
#   - Upload to asciinema.org via `asciinema upload dist/demo.cast`
#   - Embed as a static SVG/GIF via `agg` (https://github.com/asciinema/agg)
#   - Link from README.md
#
# Requires: asciinema, fj (installed and authenticated), `jq` for nicer output.
#
# Usage:
#   ./scripts/record-demo.sh           # records dist/demo.cast
#   ./scripts/record-demo.sh foo.cast  # records to foo.cast
#
# The session below is deliberately short (~30 seconds) and covers what
# someone reading the README in their first 10 seconds cares about.

set -euo pipefail

OUT="${1:-dist/demo.cast}"
mkdir -p "$(dirname "$OUT")"

if ! command -v asciinema >/dev/null 2>&1; then
  cat >&2 <<EOF
asciinema not found.
  macOS:  brew install asciinema
  Linux:  pipx install asciinema
EOF
  exit 1
fi

if ! command -v fj >/dev/null 2>&1; then
  echo "fj not found on PATH; build it first (cargo build --release)" >&2
  exit 1
fi

# Drive a representative session through asciinema.
# Idle pauses keep readability up; trim them in post if you want a snappier
# embedded GIF.
asciinema rec \
  --overwrite \
  --idle-time-limit 2 \
  --title "fj: a CLI for Forgejo" \
  --command "bash $(dirname "$0")/_demo-session.sh" \
  "$OUT"

echo ""
echo "✓ Recorded $OUT"
echo ""
echo "Next:"
echo "  asciinema play $OUT          # preview"
echo "  asciinema upload $OUT        # publish (returns a URL)"
echo "  agg $OUT $OUT.gif            # render to GIF"
