#!/usr/bin/env bash
# Create and seed the public demo repo used by fj's docs and screenshots.
#
# It stands up rasterstate/fj-demo with a tiny toy library, a CI workflow, a
# handful of issues, two standalone PRs, a three-PR stack, and a release, so the
# demo tapes (scripts/demo-*.tape) have realistic content to show. The footage
# deliberately uses this repo, never fj itself, so it isn't self-referential.
#
# Prereqs: `fj` on PATH, `fj auth login --host rasterhub.com` done, write access
# to the rasterstate org. Run once; it assumes the repo does not exist yet.
#
# Usage:
#   ./scripts/seed-demo.sh                 # repo=rasterstate/fj-demo, clone in $PWD
#   REPO=rasterstate/fj-demo WORKDIR=/tmp ./scripts/seed-demo.sh

set -euo pipefail

REPO="${REPO:-rasterstate/fj-demo}"
WORKDIR="${WORKDIR:-/tmp}"
NAME="${REPO##*/}"
DIR="$WORKDIR/$NAME"

echo "Creating $REPO ..."
fj repo create "$REPO" --init \
  -d "Demo fixture for the fj CLI: issues, stacked PRs, releases, and Actions used in fj's docs and screenshots."

rm -rf "$DIR"
git clone "https://rasterhub.com/$REPO" "$DIR"
cd "$DIR"

echo "Seeding the toy project + CI ..."
mkdir -p src src/store .forgejo/workflows
cat > Cargo.toml <<'EOF'
[package]
name = "urlshort"
version = "0.1.0"
edition = "2021"
description = "A tiny URL shortener: base62 short codes over an in-memory store."
license = "MIT"

[dependencies]
EOF
cat > src/lib.rs <<'EOF'
//! urlshort: a tiny URL shortener (base62 encode/decode).

const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// Encode a numeric id into a base62 short code.
pub fn encode(mut id: u64) -> String {
    if id == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while id > 0 {
        out.push(ALPHABET[(id % 62) as usize]);
        id /= 62;
    }
    out.reverse();
    String::from_utf8(out).expect("alphabet is ASCII")
}

/// Decode a base62 short code back into its numeric id.
pub fn decode(code: &str) -> Option<u64> {
    let mut id: u64 = 0;
    for b in code.bytes() {
        let v = ALPHABET.iter().position(|&c| c == b)? as u64;
        id = id.checked_mul(62)?.checked_add(v)?;
    }
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trips() {
        for id in [0u64, 1, 61, 62, 12345, 9_999_999] {
            assert_eq!(decode(&encode(id)), Some(id));
        }
    }
}
EOF
cat > .forgejo/workflows/ci.yml <<'EOF'
name: ci
on:
  push:
    branches: [main]
  pull_request:
jobs:
  test:
    runs-on: docker
    container:
      image: catthehacker/ubuntu:act-22.04
    steps:
      - uses: actions/checkout@v4
      - uses: https://rasterhub.com/rasterstate/setup-rust-action@v1.0.1
        with:
          toolchain: '1.95.0'
          components: rustfmt
      - run: cargo fmt --check
      - run: cargo test
EOF
git add -A && git commit -qm "Add the urlshort library and CI" && git push -q origin main

echo "Filing issues ..."
iss() { fj issue create -R "$REPO" -t "$1" -b "$2" --no-pager >/dev/null; }
iss "Persist short codes to a durable store" "In-memory only today; add a pluggable store (start with sqlite)."
iss "decode() should reject codes longer than 11 chars" "A 12-char base62 code can overflow u64. Bound the length first."
iss "Add an HTTP endpoint for shortening" "Expose POST /shorten and GET /:code over a small server."
iss "Document why the alphabet omits look-alike characters" "Explain the 0/O and 1/l decision."
iss "Two ids can collide on the same code" "Reported collision; needs a regression test."
iss "Set up CI on push and PR" "Run fmt and the test suite on every push and PR."
fj issue close 5 -R "$REPO" >/dev/null
fj issue close 6 -R "$REPO" >/dev/null

echo "Opening standalone PRs ..."
git checkout -q -b fix-long-codes main
sed -i.bak 's/    let mut id: u64 = 0;/    if code.len() > 11 {\n        return None;\n    }\n    let mut id: u64 = 0;/' src/lib.rs && rm -f src/lib.rs.bak
git commit -qam "Reject base62 codes longer than 11 chars" && git push -q -u origin fix-long-codes
fj pr create -R "$REPO" -B main -H fix-long-codes \
  -t "Reject base62 codes longer than 11 chars (#2)" \
  -b "Closes #2. Bound the length up front and return None instead of overflowing." >/dev/null
fj pr review 7 -R "$REPO" --event comment --body "One nit: worth a test at the 12-char boundary." >/dev/null || true

git checkout -q -b http-endpoint main
printf '//! HTTP surface (sketch). See issue #3.\n\npub fn shorten_route() {\n    todo!()\n}\n' > src/http.rs
git add -A && git commit -qm "WIP: sketch the HTTP shorten endpoint" && git push -q -u origin http-endpoint
fj pr create -R "$REPO" -B main -H http-endpoint --draft \
  -t "Sketch the HTTP shorten endpoint" -b "WIP toward #3. Draft for early eyes." >/dev/null

echo "Building the 3-PR stack ..."
git checkout -q main
fj stack new "Persistent store" --base main
printf '//! Pluggable storage. See issue #1.\n\npub mod memory;\n\npub trait Store {\n    fn put(&mut self, id: u64, url: &str);\n    fn get(&self, id: u64) -> Option<String>;\n}\n' > src/store/mod.rs
git add -A && git commit -qm "Add a Store trait for the id to url mapping"
printf '//! In-memory Store.\n\nuse std::collections::HashMap;\nuse super::Store;\n\n#[derive(Default)]\npub struct MemoryStore { map: HashMap<u64, String> }\n\nimpl Store for MemoryStore {\n    fn put(&mut self, id: u64, url: &str) { self.map.insert(id, url.to_string()); }\n    fn get(&self, id: u64) -> Option<String> { self.map.get(&id).cloned() }\n}\n' > src/store/memory.rs
git add -A && git commit -qm "Implement an in-memory Store"
printf '//! Sqlite Store (skeleton). Closes the durability half of #1.\n\nuse super::Store;\n\npub struct SqliteStore;\n\nimpl Store for SqliteStore {\n    fn put(&mut self, _id: u64, _url: &str) { todo!() }\n    fn get(&self, _id: u64) -> Option<String> { todo!() }\n}\n' > src/store/sqlite.rs
git add -A && git commit -qm "Add a sqlite Store skeleton"
fj stack split
fj stack sync

echo "Cutting a release ..."
git checkout -q main
git archive --format=tar.gz --prefix=urlshort-0.1.0/ -o "$WORKDIR/urlshort-0.1.0.tar.gz" main
fj release create v0.1.0 -R "$REPO" --target main \
  --title "v0.1.0" \
  --body "First tagged cut of urlshort: base62 encode/decode with a round-trip test." \
  --asset "$WORKDIR/urlshort-0.1.0.tar.gz"

echo "Done. Demo repo seeded at https://rasterhub.com/$REPO (clone: $DIR)"
