#!/usr/bin/env bash
# Cross-build the Linux x86_64 release binary from macOS via Docker.
# Output: target/fj-linux-x86_64 (the binary itself, not a tarball).
#
# Requires Docker reachable (Colima, Docker Desktop, OrbStack, etc.).
#
# Uses persistent named volumes for the cargo registry and target dir,
# so subsequent runs are incremental and don't re-download crates.
#
# Tooling rationale:
# * `cross` was attempted first; v0.2.5 misroutes when the host triple
#   is aarch64-apple-darwin (treats it as the target). Going direct
#   via the rust:bookworm image avoids the issue.
# * QEMU under linux/amd64 emulation prints harmless `jemalloc:
#   MADV_DONTNEED` warnings during the build. Ignore.
#
# Usage:
#   ./scripts/build-linux.sh

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

if ! docker info >/dev/null 2>&1; then
  cat >&2 <<EOF
docker daemon not reachable.
  Colima:  colima start
  Docker Desktop / OrbStack: open the app
EOF
  exit 1
fi

IMAGE="rust:1.95-bookworm"
TARGET="x86_64-unknown-linux-gnu"

printf '\033[1;34m== docker build %s\033[0m\n' "$TARGET" >&2

docker run --rm --platform=linux/amd64 \
  -v "$PWD":/work -w /work \
  -v fj-cargo-registry:/usr/local/cargo/registry \
  -v fj-cargo-git:/usr/local/cargo/git \
  -v fj-linux-target:/work/target-linux \
  -e CARGO_TARGET_DIR=/work/target-linux \
  "$IMAGE" \
  bash -c "cargo build --release --target $TARGET --locked"

# Copy the binary out of the named volume into target/ on the host.
docker run --rm --platform=linux/amd64 \
  -v "$PWD":/work \
  -v fj-linux-target:/work/target-linux \
  "$IMAGE" \
  cp "/work/target-linux/$TARGET/release/fj" /work/target/fj-linux-x86_64

ls -lh target/fj-linux-x86_64 >&2
file target/fj-linux-x86_64 >&2

printf '\n\033[1;32m✓ wrote target/fj-linux-x86_64\033[0m\n' >&2
