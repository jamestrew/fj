# Agent (AI-assisted code ops)

> Status: preview. The commands are useful today; the provider surface is
> still settling.

`fj agent review` / `fix` / `explain` route through a provider configured by
environment variables:

```sh
export FJ_AGENT_PROVIDER=...   # which backend to use
export FJ_AGENT_API_KEY=...    # its credential
export FJ_AGENT_MODEL=...      # optional model override
```

The Fjord/Forgejo token is never reused for AI. With nothing configured, the
commands that need a backend say so plainly, and `agent review` still runs its
local analysis (see below). A built-in `mock` provider exists for development.

## review

`fj agent review` is a local diff-analysis command. It gathers the working-tree
diff (or `--pr <n>`, or each `--stack` item), redacts secrets and sensitive
files before anything leaves your machine, and prints severity-tagged findings
with file references:

- secret-bearing files,
- source changed without tests,
- risky areas (migrations, auth/crypto, CI, dependency manifests).

Redaction covers `.env` files, private keys, `secret = ...` assignments, known
token prefixes, and PEM blocks. The local analysis runs even with no AI backend
configured; when a provider is set, it adds a narrative review on top.
`--security` focuses the pass, and `--json` emits the full report for
editor and mobile clients.

```sh
fj agent review                 # analyze the working-tree diff
fj agent review --stack         # review each stack item separately
fj agent review --pr 42         # review a PR's diff
fj agent review --security --json
```

## fix

`fj agent fix` asks the provider for a patch over the current change (the
`git diff HEAD`). By default it just prints the patch and changes nothing.

- `--write` applies the patch to your working tree, validating it first and
  leaving the result unstaged so you can review it.
- `--pr` carries the current change plus the fix onto a fresh branch
  (`--branch`, default `agent-fix`), commits (`--title`, default "Apply agent
  fix"), pushes, and opens a PR against the branch you ran from. Every local
  check runs before anything touches the network, and on failure it restores
  your previous branch.
- `--dry-run` reports the plan without calling the provider.

```sh
fj agent fix                    # print a proposed patch
fj agent fix --write            # apply it to the working tree
fj agent fix --pr --branch tidy # open it as a PR
```

## explain

`fj agent explain` explains a file, a git revision, or the working-tree diff
when you pass no target.

```sh
fj agent explain src/main.rs --json
fj agent explain HEAD~1
fj agent explain                # explain the current working-tree diff
```
