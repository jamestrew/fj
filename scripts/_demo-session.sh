#!/usr/bin/env bash
# Drives an asciinema recording. Don't invoke directly; run record-demo.sh.

set -u
PS1='\$ '
export PS1

PAUSE_BETWEEN=0.6
PAUSE_AFTER=1.2

say() { printf '\033[1;34m# %s\033[0m\n' "$*"; sleep "$PAUSE_BETWEEN"; }
do_() { printf '\033[1m$ %s\033[0m\n' "$*"; sleep "$PAUSE_BETWEEN"; eval "$@"; sleep "$PAUSE_AFTER"; }

clear
say "fj: a CLI for Forgejo. Multi-host, tokens in the keychain."
do_ "fj --version"

say "Inside a clone, no flags needed: fj infers the repo from your git remote."
do_ "fj repo view | head -8"

say "Issues, PRs, releases all work the same way."
do_ "fj issue list --state all -L 5"
do_ "fj pr list --state all -L 5"

say "The api escape hatch with a jq-ish projector."
do_ "fj api /version"
do_ "fj api /user -q .login"

say "Selective JSON for scripting."
do_ "fj repo list -L 3 --json --json-fields full_name,private"

say "Or just browse on the web."
say "  fj browse src/main.rs"
sleep 1.5

clear
say "60+ subcommands. Try fj --help."
sleep 2
