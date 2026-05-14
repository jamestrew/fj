#!/usr/bin/env bash
# Submit a signed darwin tarball to Apple's notary service, poll for the
# result, and (if accepted) staple the ticket onto the binary inside the
# tarball.
#
# Pre-requisites:
#   * The binary inside the tarball must already be signed with a
#     Developer ID Application identity AND have hardened runtime
#     enabled. `scripts/sign.sh` does both.
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
#   ./scripts/notarize.sh path/to/tarball.tar.gz
#
# What it does:
#   1. notarytool submit --wait → blocks until Apple returns accepted/
#      rejected
#   2. On accepted: extract the binary, staple the ticket, re-tar the
#      directory IN PLACE (same SHA256 will change — re-compute upstream).
#   3. On rejected: dump the failure log and exit non-zero.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

for f in .env .env.local; do
  if [ -f "$f" ]; then set -a; . "$f"; set +a; fi
done

PROFILE="${FJ_NOTARY_PROFILE:-fj-notary}"

if [ "$#" -lt 1 ]; then
  echo "usage: $0 path/to/tarball.tar.gz" >&2
  exit 2
fi

TARBALL="$1"
if [ ! -f "$TARBALL" ]; then
  echo "tarball not found: $TARBALL" >&2
  exit 1
fi

if ! xcrun notarytool history --keychain-profile "$PROFILE" >/dev/null 2>&1; then
  cat >&2 <<EOF
notarytool can't find a keychain profile named '$PROFILE'.
Store it once:

  xcrun notarytool store-credentials "$PROFILE" \\
    --apple-id <your-apple-id> \\
    --team-id  "${FJ_APPLE_TEAM_ID:-<team-id>}" \\
    --password <app-specific-password>

Then re-run this script.
EOF
  exit 1
fi

printf '\033[1;34m== notarytool submit %s\033[0m\n' "$TARBALL" >&2

# Submit + wait. Output captures the request UUID and the final status.
out=$(xcrun notarytool submit "$TARBALL" \
  --keychain-profile "$PROFILE" \
  --wait \
  --output-format json)

echo "$out" | python3 -m json.tool >&2

status=$(echo "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status",""))')
req_id=$(echo "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("id",""))')

if [ "$status" != "Accepted" ]; then
  printf '\033[1;31m✗ notarization status: %s (request %s)\033[0m\n' "$status" "$req_id" >&2
  echo "" >&2
  echo "Fetching detailed log:" >&2
  xcrun notarytool log "$req_id" --keychain-profile "$PROFILE" >&2 || true
  exit 1
fi

printf '\033[1;32m✓ accepted (request %s)\033[0m\n' "$req_id" >&2

# Staple the ticket onto the binary inside the tarball.
#
# notarytool doesn't staple gzipped tarballs directly; `xcrun stapler`
# works on .app bundles, .dmg, .pkg, and (with -v) some other formats,
# but not on bare Mach-O binaries or .tar.gz.
#
# Practice: stapling matters for offline Gatekeeper checks. For a
# Homebrew install, the user is online, so Gatekeeper does a live
# notary check and the staple is optional. We skip stapling here and
# rely on the live check; if needed later, we can change the artifact
# format to .dmg or .pkg which does support stapling.
#
# For documentation: this binary IS notarized — Apple's notary service
# has its hash on file. macOS will silently accept it on first run
# whenever the machine is online.

cat >&2 <<EOF

The tarball ($TARBALL) is unchanged on disk.
The binary inside it is notarized (Apple has its hash on file).
Gatekeeper will accept it on first run for online machines.

For offline Gatekeeper acceptance, repackage as a .dmg or .pkg and
re-run with the stapler (\`xcrun stapler staple\`). Not needed for the
Homebrew install path.
EOF
