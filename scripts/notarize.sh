#!/usr/bin/env bash
# Submit a signed darwin Mach-O binary to Apple's notary service.
#
# Pre-requisites:
#   * The binary is already signed with a Developer ID Application
#     identity AND has hardened runtime enabled. `scripts/sign.sh`
#     does both.
#   * Notarization credentials stored in the keychain via:
#
#       xcrun notarytool store-credentials "$FJ_NOTARY_PROFILE" \
#         --apple-id <apple-id-email> \
#         --team-id  "$FJ_APPLE_TEAM_ID" \
#         --password <app-specific-password>
#
#     Get the app-specific password from appleid.apple.com. The profile
#     name defaults to `fj-notary`; override via FJ_NOTARY_PROFILE.
#
# Usage:
#   ./scripts/notarize.sh path/to/tarball.tar.gz   # extracts the binary, zips, submits
#   ./scripts/notarize.sh path/to/binary            # zips, submits
#
# What it does:
#   1. If the input is a .tar.gz, extract the Mach-O binary from inside.
#   2. Zip the binary (notary requires .zip / .pkg / .dmg).
#   3. notarytool submit --wait → blocks until Apple returns accepted /
#      rejected.
#   4. On rejected: dump the failure log and exit non-zero.
#
# The tarball / source binary is NOT modified. Apple's notary records
# the binary hash; the tarball on disk stays bit-identical and the
# Homebrew SHA256 doesn't need to be updated.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

for f in .env .env.local; do
  if [ -f "$f" ]; then set -a; . "$f"; set +a; fi
done

PROFILE="${FJ_NOTARY_PROFILE:-fj-notary}"

if [ "$#" -lt 1 ]; then
  echo "usage: $0 path/to/{tarball.tar.gz,binary}" >&2
  exit 2
fi

INPUT="$1"
if [ ! -f "$INPUT" ]; then
  echo "not found: $INPUT" >&2
  exit 1
fi

# notarytool's `info` subcommand exits 0 with no args when the profile
# exists, errors with a clear message otherwise. Use that as the probe.
if ! xcrun notarytool info --keychain-profile "$PROFILE" >/dev/null 2>&1; then
  # That's expected — info needs a submission id. We just want to know
  # the profile resolves. Try a no-op that DOES exist.
  if ! xcrun notarytool history --keychain-profile "$PROFILE" 2>&1 | head -1 | grep -qiE "successfully|history|created"; then
    cat >&2 <<EOF
notarytool can't find or auth a keychain profile named '$PROFILE'.
Store it once:

  xcrun notarytool store-credentials "$PROFILE" \\
    --apple-id <your-apple-id> \\
    --team-id  "${FJ_APPLE_TEAM_ID:-<team-id>}" \\
    --password <app-specific-password>

Then re-run this script.
EOF
    exit 1
  fi
fi

# ── Prepare the zip notary expects ───────────────────────────────────────────
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

case "$INPUT" in
  *.tar.gz|*.tgz)
    tar xzf "$INPUT" -C "$tmp"
    # Find the Mach-O binary inside (handles both bare-binary and
    # staged-directory tarballs).
    bin=$(find "$tmp" -type f -name 'fj' | head -1)
    if [ -z "$bin" ]; then
      echo "no fj binary found inside $INPUT" >&2
      exit 1
    fi
    ;;
  *)
    bin="$INPUT"
    ;;
esac

zip_name=$(basename "$INPUT" .tar.gz).zip
[ "$zip_name" = "$(basename "$INPUT")" ] && zip_name="$(basename "$bin").zip"
zip_path="$tmp/$zip_name"

(cd "$(dirname "$bin")" && zip -q "$zip_path" "$(basename "$bin")")

printf '\033[1;34m== notarytool submit %s\033[0m\n' "$zip_name" >&2

# ── Submit + wait ─────────────────────────────────────────────────────────────
out=$(xcrun notarytool submit "$zip_path" \
  --keychain-profile "$PROFILE" \
  --wait \
  --output-format json)

echo "$out" | python3 -m json.tool >&2

n_status=$(echo "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status",""))')
req_id=$(echo "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("id",""))')

if [ "$n_status" != "Accepted" ]; then
  printf '\033[1;31m✗ notarization status: %s (request %s)\033[0m\n' "$n_status" "$req_id" >&2
  echo "" >&2
  echo "Fetching detailed log:" >&2
  xcrun notarytool log "$req_id" --keychain-profile "$PROFILE" >&2 || true
  exit 1
fi

printf '\033[1;32m✓ accepted (request %s)\033[0m\n' "$req_id" >&2

cat >&2 <<EOF

Apple's notary service now has this binary's hash on file. The
tarball on disk is unchanged; Gatekeeper will silently accept the
binary on first run for any online macOS machine.

(Stapling would let Gatekeeper accept offline machines too, but that
requires a .dmg or .pkg artifact format. We ship .tar.gz to keep parity
with the Linux release, and Homebrew users are online anyway.)
EOF
