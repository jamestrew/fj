#!/usr/bin/env bash
# Run this, not bare `cargo audit`, so the expiry is enforced. cargo audit with the waivers in .cargo/audit.toml, failing once the dated chrono waiver (PLAT-906) has expired.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
expiry=2027-02-17
# AUDIT_TODAY is for testing the expiry only.
today="${AUDIT_TODAY:-$(date -u +%F)}"
if grep -q '"RUSTSEC-2026-0543"' .cargo/audit.toml && [[ "$today" > "$expiry" ]]; then
  echo "audit waiver RUSTSEC-2026-0543 (chrono, PLAT-906) expired on $expiry: renew it or move off chrono" >&2
  exit 1
fi
exec cargo audit -D warnings "$@"
