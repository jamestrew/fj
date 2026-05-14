#!/usr/bin/env bash
# Preflight safety net. Runs two checks:
#
# 1. .env audit. If any .env file (excluding .env.example) contains
#    non-empty values for keys that look like secrets, or values that
#    look like paths to keystores / passwords, refuse to proceed. The
#    intent is to catch a future release script that might accidentally
#    propagate such a value into codesign / git config / a release
#    asset.
#
# 2. Denylist grep. Scans tracked + untracked working-tree content for
#    secret-shaped patterns (PEM block markers, Apple app-specific
#    password format, etc.).
#
# Used standalone (pre-release) and as the first step of the
# pre-push hook.
#
# Usage:
#   ./scripts/preflight.sh                # full check
#   ./scripts/preflight.sh --quick        # skip the git-grep pass
#   ./scripts/preflight.sh --help

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

QUICK=0
case "${1:-}" in
  --quick) QUICK=1 ;;
  ""|-) ;;
  --help|-h)
    sed -n '/^# /{s/^# \{0,1\}//;p;}' "$0" | head -25
    exit 0
    ;;
  *)
    echo "unknown arg: $1 (try --help)" >&2
    exit 2
    ;;
esac

fail=0

step() {
  printf '\n\033[1;34m== %s ==\033[0m\n' "$*" >&2
}
err() {
  printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2
  fail=1
}
ok() {
  printf '\033[1;32m✓ %s\033[0m\n' "$*" >&2
}

# ── 1. .env audit ────────────────────────────────────────────────────────────
step ".env audit"

# Sensitive key-name patterns: if a key matches and its value is non-empty,
# refuse. These names suggest the value WILL be passed to codesign,
# notarization, or git config in a way that could leak.
sensitive_key_re='^(.*PASSWORD|.*SECRET|.*TOKEN|.*API_KEY|.*PRIVATE_KEY|.*KEYCHAIN_PATH|.*P12_PATH|.*P8_PATH|.*NOTARY_PASSWORD|.*APPLE_ID_PASSWORD|.*KEYSTORE)$'

# Suspicious value patterns regardless of key name.
keystore_value_re='\.(p12|p8|pem|key|keychain)(["[:space:]]|$)'
apple_password_re='^[a-z]{4}-[a-z]{4}-[a-z]{4}-[a-z]{4}$'

env_files=()
while IFS= read -r -d '' f; do
  env_files+=("$f")
done < <(find . -maxdepth 2 \( -name '.env' -o -name '.env.local' -o -name '.env.*' \) \
  -not -name '.env.example' -not -path './target/*' -not -path './.git/*' -print0 2>/dev/null)

if [ ${#env_files[@]} -eq 0 ]; then
  ok "no .env files present"
else
  for f in "${env_files[@]}"; do
    if git ls-files --error-unmatch "$f" >/dev/null 2>&1; then
      err "$f is tracked by git; .env files must be gitignored"
    fi

    while IFS= read -r line; do
      [[ "$line" =~ ^[[:space:]]*# ]] && continue
      [[ -z "${line// }" ]] && continue
      [[ "$line" =~ ^([A-Za-z_][A-Za-z0-9_]*)=(.*)$ ]] || continue
      key="${BASH_REMATCH[1]}"
      value="${BASH_REMATCH[2]}"
      value="${value%\"}"
      value="${value#\"}"
      value="${value%\'}"
      value="${value#\'}"

      [[ -z "$value" ]] && continue

      if [[ "$key" =~ $sensitive_key_re ]]; then
        err "$f: '$key' has a non-empty value. Sensitive keys must live in the OS keychain or CI secrets, not .env."
      fi
      if [[ "$value" =~ $keystore_value_re ]]; then
        err "$f: '$key=$value' looks like a path to a keystore. Move it out of .env or delete."
      fi
      if [[ "$value" =~ $apple_password_re ]]; then
        err "$f: '$key' holds an Apple-style app-specific password. Run 'xcrun notarytool store-credentials' to move it into the keychain, then clear the value here."
      fi
    done < "$f"

    [[ "$fail" -eq 0 ]] && ok "$f: no sensitive values"
  done
fi

# ── 2. Denylist grep ─────────────────────────────────────────────────────────
if [ "$QUICK" = 1 ]; then
  echo ""
  echo "(skipping git-grep pass — quick mode)"
else
  step "denylist grep"

  # Patterns are anchored where possible so identifiers in lock files
  # (`iana-time-zone-haiku`, etc.) don't trigger false positives.
  declare -a denylist=(
    "-----BEGIN (RSA |OPENSSH |EC |DSA |ENCRYPTED |)PRIVATE KEY-----"
    "AKIA[0-9A-Z]{16}"
    # Apple app-specific password: ONLY when it appears as a value (after
    # = or :, optionally quoted) so dashed identifiers don't match.
    "(=|:)[[:space:]]*[\"']?[a-z]{4}-[a-z]{4}-[a-z]{4}-[a-z]{4}[\"']?[[:space:]]*$"
    "(api[_-]?key|secret|password|token)[[:space:]]*[=:][[:space:]]*[\"']?[A-Za-z0-9+/=_-]{32,}[\"']?"
    "ghp_[A-Za-z0-9]{36,}"
    "ghs_[A-Za-z0-9]{36,}"
    "xox[baprs]-[0-9a-zA-Z-]{10,}"
  )

  declare -a excludes=(
    ':!scripts/preflight.sh'
    ':!.env.example'
    ':!docs/*'
    ':!CHANGELOG.md'
    ':!CLAUDE.md'
    ':!CONTRIBUTING.md'
    ':!SECURITY.md'
    ':!README.md'
    # Machine-generated / vendor-managed content. Identifiers in these
    # files often look secret-shaped without being secrets.
    ':!Cargo.lock'
    ':!*.lock'
    ':!assets/*'
  )

  for pat in "${denylist[@]}"; do
    if hits=$(git grep -nE --untracked "$pat" -- "${excludes[@]}" 2>/dev/null); then
      err "denylist match: $pat"
      echo "$hits" | head -5 | sed 's/^/    /' >&2
    fi
  done

  [[ "$fail" -eq 0 ]] && ok "no denylist matches"
fi

# ── Result ───────────────────────────────────────────────────────────────────
echo ""
if [[ "$fail" -ne 0 ]]; then
  printf '\033[1;31m✗ preflight failed — release should NOT proceed\033[0m\n' >&2
  exit 1
fi
printf '\033[1;32m✓ preflight passed\033[0m\n' >&2
