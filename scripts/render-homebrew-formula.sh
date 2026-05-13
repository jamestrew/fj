#!/usr/bin/env bash
# Render dist/homebrew/fj.rb from the template + a release's SHA256SUMS.
#
# Usage:
#   ./scripts/render-homebrew-formula.sh <version>
#
# Reads SHA256SUMS from the Forgejo release for the given version and writes
# a ready-to-commit fj.rb. Then paste that into rasterandstate/homebrew-tap.
#
# Requires: curl, awk, jq.

set -euo pipefail

VERSION="${1:?version required, e.g. 0.1.0}"
TAG="v${VERSION#v}"
VERSION="${VERSION#v}"
REPO="rasterstate/fj"
HOST="https://rasterhub.com"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

SUMS_URL="$HOST/$REPO/releases/download/$TAG/SHA256SUMS"
echo "fetching $SUMS_URL"
curl -fsSL "$SUMS_URL" -o "$TMP/SHA256SUMS"

extract() {
  awk -v pat="$1" '$2 ~ pat {print $1; exit}' "$TMP/SHA256SUMS"
}

SHA_ARM=$(extract "darwin-aarch64\\.tar\\.gz$")
SHA_X64=$(extract "darwin-x86_64\\.tar\\.gz$")
SHA_LIN=$(extract "linux-x86_64\\.tar\\.gz$")

for s in "$SHA_ARM" "$SHA_X64" "$SHA_LIN"; do
  [ -n "$s" ] || { echo "missing SHA in SHA256SUMS"; cat "$TMP/SHA256SUMS"; exit 1; }
done

OUT="dist/homebrew/fj.rb"
sed \
  -e "s/@@VERSION@@/$VERSION/g" \
  -e "s/@@SHA_DARWIN_AARCH64@@/$SHA_ARM/g" \
  -e "s/@@SHA_DARWIN_X86_64@@/$SHA_X64/g" \
  -e "s/@@SHA_LINUX_X86_64@@/$SHA_LIN/g" \
  dist/homebrew/fj.rb.tmpl > "$OUT"

echo "wrote $OUT"
echo ""
echo "next:"
echo "  cp $OUT ~/path/to/homebrew-tap/Formula/fj.rb"
echo "  cd ~/path/to/homebrew-tap"
echo "  git commit -am 'fj $VERSION'"
echo "  git push"
