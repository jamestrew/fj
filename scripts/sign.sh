#!/usr/bin/env bash
# Code-sign a fj binary with the Raster & State Developer ID.
#
# A signed binary stays the same code-identity across rebuilds, so the
# macOS keychain ACLs persist and you don't get re-prompted every time
# you `cargo build --release`. This is also a prerequisite for
# notarization.
#
# Defaults:
#   identity:   $FJ_APPLE_DEVELOPER_ID, falling back to the value in
#               `.env.example` if present, else aborts.
#   binary:     target/release/fj
#
# Usage:
#   ./scripts/sign.sh                        # signs target/release/fj
#   ./scripts/sign.sh path/to/some/fj        # signs an arbitrary binary
#   FJ_APPLE_DEVELOPER_ID=... ./scripts/sign.sh
#
# Reads `.env` and `.env.local` if present so FJ_APPLE_DEVELOPER_ID can
# live there.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# Source any local env, in order (later wins).
for f in .env .env.local; do
  if [ -f "$f" ]; then
    set -a; . "$f"; set +a
  fi
done

# If still unset, peek at .env.example for the default.
if [ -z "${FJ_APPLE_DEVELOPER_ID:-}" ] && [ -f .env.example ]; then
  FJ_APPLE_DEVELOPER_ID=$(grep -E '^FJ_APPLE_DEVELOPER_ID=' .env.example | head -1 | sed -E 's/^[^=]+=//; s/^"(.*)"$/\1/')
fi

if [ -z "${FJ_APPLE_DEVELOPER_ID:-}" ]; then
  cat >&2 <<EOF
FJ_APPLE_DEVELOPER_ID is unset. Set it in .env / .env.local or pass via
environment, e.g.:

  FJ_APPLE_DEVELOPER_ID="Developer ID Application: Raster & State LLC (62Y3FRM8PD)"

Or copy .env.example to .env to pick up the default.
EOF
  exit 1
fi

BIN="${1:-target/release/fj}"

if [ ! -f "$BIN" ]; then
  echo "binary not found: $BIN" >&2
  echo "(tip: cargo build --release first)" >&2
  exit 1
fi

# Sanity: verify the identity is in the keychain before we try.
if ! security find-identity -p codesigning -v 2>/dev/null | grep -qF "$FJ_APPLE_DEVELOPER_ID"; then
  echo "identity not found in any keychain: $FJ_APPLE_DEVELOPER_ID" >&2
  echo "available identities:" >&2
  security find-identity -p codesigning -v >&2
  exit 1
fi

printf '\033[1;34m== codesign %s\033[0m\n' "$BIN" >&2

codesign \
  --force \
  --sign "$FJ_APPLE_DEVELOPER_ID" \
  --timestamp \
  --options=runtime \
  "$BIN"

printf '\033[1;34m== verify\033[0m\n' >&2
codesign --verify --verbose=2 "$BIN"
printf '\n' >&2
codesign -dv "$BIN" 2>&1 | grep -E "Identifier|Authority|Signed Time|TeamIdentifier|Timestamp" || true

printf '\n\033[1;32m✓ signed %s\033[0m\n' "$BIN" >&2
