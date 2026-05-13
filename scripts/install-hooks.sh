#!/usr/bin/env bash
# Install the repo's hooks into .git/hooks via symlink so updates in tree
# are picked up automatically.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

hooks_dir=".git/hooks"
mkdir -p "$hooks_dir"

for src in hooks/*; do
  name="$(basename "$src")"
  dest="$hooks_dir/$name"
  rm -f "$dest"
  ln -s "../../$src" "$dest"
  chmod +x "$src"
  echo "✓ linked $name"
done
