#!/usr/bin/env bash
#
# Release preflight for fj. Run this BEFORE pushing a `v*` tag — it catches the
# things that have silently broken past releases, in seconds, instead of one
# ~8-minute signed-and-notarized build cycle per discovery.
#
#   ./scripts/release-preflight.sh [vX.Y.Z]      # or: make release-preflight TAG=vX.Y.Z
#
# Checks:
#   1. Cargo.toml version == the tag you intend to push (the Homebrew formula's
#      `test do` asserts `fj --version` matches the tag; a mismatch fails the
#      release at the very end).
#   2. An online (non-offline) self-hosted runner exists for every label the
#      release matrix needs — else the jobs sit in `waiting` forever.
#   3. Every Actions secret the release uses is present (names only; values are
#      write-only and can't be read back).
#
# It does NOT (can't) validate secret *values*. For the signing cert
# specifically, the failure mode that bit us was a key-only `.p12` (no
# certificate). Verify your source `.p12` is a full identity before encoding it:
#
#   openssl pkcs12 -in cert.p12 -passin pass:PW -nokeys          | grep -c CERTIFICATE      # >= 1
#   openssl pkcs12 -in cert.p12 -passin pass:PW -nocerts -nodes  | grep -c 'PRIVATE KEY'    # == 1
#
# Requires: fj (authenticated against the release host), jq.

set -euo pipefail

REPO="${FJ_RELEASE_REPO:-rasterstate/fj}"
ORG="${REPO%%/*}"

# Labels the release matrix targets (see .forgejo/workflows/release.yml).
REQUIRED_LABELS=(macos-15 docker)

# Secrets the release workflow consumes (signing + notarization).
REQUIRED_SECRETS=(
  MACOS_CERT_P12_BASE64
  MACOS_CERT_PASSWORD
  MACOS_KEYCHAIN_PASSWORD
  APPLE_API_KEY_BASE64
  APPLE_API_KEY_ID
  APPLE_API_ISSUER
)

fail=0
ok()   { printf '  \033[32mok\033[0m   %s\n' "$1"; }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=1; }
note() { printf '  ..   %s\n' "$1"; }

command -v fj >/dev/null 2>&1 || { echo "fj not on PATH"; exit 2; }
command -v jq >/dev/null 2>&1 || { echo "jq not on PATH"; exit 2; }

# ── 1. version vs tag ────────────────────────────────────────────────────────
echo "== version =="
ver=$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
if [ "$#" -ge 1 ]; then
  tag="$1"; want="${tag#v}"
  if [ "$ver" = "$want" ]; then
    ok "Cargo.toml $ver matches tag $tag"
  else
    bad "Cargo.toml is $ver but you're tagging $tag — bump Cargo.toml (+ Cargo.lock) first"
  fi
else
  note "Cargo.toml version is $ver; the tag must be v$ver (pass it as an arg to assert)"
fi

# ── 2. runners ───────────────────────────────────────────────────────────────
echo "== runners (need an online runner for: ${REQUIRED_LABELS[*]}) =="
runners=$(fj api "/api/v1/orgs/$ORG/actions/runners" 2>/dev/null || echo '[]')
for lbl in "${REQUIRED_LABELS[@]}"; do
  n=$(printf '%s' "$runners" | jq -r --arg l "$lbl" \
        '[.[]? | select((.status // "") != "offline") | select((.labels // []) | index($l))] | length' 2>/dev/null || echo 0)
  if [ "${n:-0}" -gt 0 ]; then
    ok "label '$lbl': $n online runner(s)"
  else
    bad "label '$lbl': no online runner — jobs would sit in 'waiting'"
  fi
done

# ── 3. secrets ───────────────────────────────────────────────────────────────
echo "== secrets (present? values are write-only) =="
secrets=$( { fj api "/api/v1/repos/$REPO/actions/secrets" 2>/dev/null || true
             fj api "/api/v1/orgs/$ORG/actions/secrets"   2>/dev/null || true; } )
have=$(printf '%s' "$secrets" | jq -r '.[]?.name // empty' 2>/dev/null | sort -u)
if [ -z "$have" ]; then
  note "couldn't read any secret names (token scope, or none set) — verify manually in Settings → Actions → Secrets"
else
  for s in "${REQUIRED_SECRETS[@]}"; do
    if printf '%s\n' "$have" | grep -qx "$s"; then ok "$s"; else bad "$s missing"; fi
  done
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "preflight OK — safe to tag $([ "$#" -ge 1 ] && echo "$1" || echo "v$ver")."
else
  echo "preflight FAILED — fix the above before tagging." >&2
  exit 1
fi
