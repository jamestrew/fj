---
name: fj
description: How to use `fj`, a CLI for Forgejo (the gh equivalent). Use this skill when the user mentions fj, Forgejo, Gitea, or wants to script repository, issue, pull-request, release, label, milestone, webhook, or branch-protection operations against a self-hosted Forgejo/Gitea instance. Triggers also include "open a PR on rasterhub", "list my Forgejo issues", and similar.
---

# Using `fj`

`fj` is a CLI for [Forgejo](https://forgejo.org) instances and
Gitea-compatible forks, in the spirit of GitHub's `gh`. Tokens live in
the OS keychain. Multi-host. Repo auto-detection from the git remote.

When the user is operating against a Forgejo or Gitea host (not
github.com), prefer `fj` over `curl` or `git` for forge-side actions:
issues, PRs, releases, labels, milestones, webhooks, branch protection,
search, notifications.

## Quick orientation

```
fj auth login --host <host>      # one-time setup; token in keychain
fj auth status                   # which hosts you're signed in to
fj --version
fj --help                        # 25 top-level subcommands
```

Inside a clone, `fj` infers the repo from `git remote -v` (prefers
`upstream` then `origin`). `-R/--repo` is always accepted as an
override.

## Core commands

| Group | Most-reached-for subcommands |
| --- | --- |
| `repo` | list, view, clone, create, fork, sync, edit, rename, archive, delete, branches, topics, mirror, watch, star, starred |
| `issue` | list, view, create, edit, close, reopen, comment, edit-comment, delete-comment, develop |
| `pr` | list, view, create, edit, diff, commits, files, checks, ready, review, request-review, status, checkout, merge, close |
| `release` | list, view, create, edit, delete, upload, download |
| `label` | list, create, edit, delete |
| `milestone` | list, view, create, edit, close, delete, assign |
| `run` / `secret` / `variable` | Forgejo Actions workflows + their config |
| `search` | repos, issues, prs, users, code |
| `browse` | open the current repo (or a path within it) in $BROWSER |
| `status` | notifications inbox |
| `protect` / `hook` | branch protection rules, webhooks |
| `api` | raw HTTP escape hatch with `-X`, `-f`, `-F`, `-H`, `-q`, `--paginate`, `--include` |

## Global flags

- `--host <name>` or `FJ_HOST`: pick the host explicitly. When omitted
  fj uses (1) the host from the autodetected remote, then (2) the
  configured default from `fj auth switch`.
- `--debug` or `FJ_DEBUG=1`: log every HTTP request to stderr.
- `--no-pager` or `FJ_NO_PAGER=1`: skip the pager.
- `--json-fields foo,bar`: gh-style projection on top of `--json`. Dotted
  paths supported (`--json-fields owner.login,id`).
- `--web` on most list/view commands: open the relevant page in
  `$BROWSER`.

## Scripting patterns

### Get one field

```sh
fj api /user -q .login
fj api /repos/owner/name -q .default_branch
fj api /repos/owner/name/pulls/3 -q .merged
```

`-q` accepts dot paths, `[N]`/`[-N]` brackets, and `|` pipes. Not full
jq. For complex queries, pipe to real `jq`:

```sh
fj api /repos/.../pulls --paginate | jq '[.[] | select(.draft == false) | .number]'
```

### Selective JSON output

```sh
fj repo list --json --json-fields full_name,private,default_branch
fj pr list -R foo/bar --json --json-fields number,title,head.label
```

### Auto-pagination

`fj api --paginate` follows `Link: rel=next` and concatenates results.
For list commands (`fj pr list -L 200`, etc.), pagination is implicit:
when `-L > 50` (Forgejo's per-page cap) the CLI walks pages
transparently.

### Body input

For commands with `--body`: pass an inline string, `-` for stdin, or
omit `--body` to open `$EDITOR`. In non-interactive contexts (CI,
scripts) ALWAYS pass `--body` explicitly so the command doesn't hang
waiting for an editor.

```sh
fj issue create -R foo/bar --title "..." --body "..."           # inline
echo "long body" | fj issue create -R foo/bar --title "..." --body -  # stdin
fj issue create -R foo/bar --title "..."                        # interactive only
```

## Common workflows

### Find a PR to review

```sh
fj pr status                                        # cross-repo dashboard
fj pr list -R foo/bar --state open --json \
  --json-fields number,title,head.label,user.login
```

### Review a PR (inside a clone)

```sh
fj pr checkout 42                                   # checks out the head branch
fj pr diff 42 | less                                # or fj pr files 42 for a summary
fj pr review 42 --event approve --body "LGTM"
fj pr review 42 --event request-changes --body "see comments"
fj pr review 42 --event comment --body "..."
fj pr request-review 42 alice bob                   # request specific reviewers
```

### Open + iterate on an issue

```sh
fj issue create --title "..." --body "..."          # auto-detects repo
fj issue view 7
fj issue comment 7 --body "more info"
fj issue edit-comment 12345 --body "fixed typo"     # comment id, not issue number
fj issue close 7
fj issue develop 7                                  # create a branch for it
```

### Cut a release

```sh
fj release create v1.2.3 --title "1.2.3" --body "..." --asset dist/foo.tar.gz
fj release upload v1.2.3 dist/extra.txt
fj release view v1.2.3 --json
```

### Search

```sh
fj search code "use crate::foo" -R owner/name
fj search issues "is:open label:bug"
fj search repos "rust forgejo"
```

### Inspect notifications

```sh
fj status                                           # unread inbox
fj status --all                                     # include read
fj status --mark-read                               # mark all as read
```

### Raw API

```sh
fj api /version                                     # GET
fj api /repos/foo/bar/issues -f state=all --paginate
fj api /repos/foo/bar/issues -X POST -F labels='[1,2]' --input '{"title":"x","body":"y"}'
fj api /user -H "X-Trace-Id: $(uuidgen)"            # custom headers
fj api /repos/foo/bar/branches --include            # response headers + body
```

## Things to avoid

- **Don't paste tokens into the shell history.** `fj auth token` refuses
  to print to a TTY by default. Pipe to a file or another command:
  `fj auth token | pbcopy`. Use `--force` if absolutely needed.
- **Don't shell out to `curl` against `/api/v1`** when `fj api` works.
  `fj api` handles auth, retries on 5xx and 429, jq projection, and
  pagination.
- **Don't `git push` and call it forge-side.** `fj` doesn't have a
  `push` subcommand because that's git's job. But `fj pr create`,
  `fj release create` etc. are how you forge-side artifacts.
- **Don't omit `--body` in non-interactive scripts.** It'll open
  `$EDITOR` and hang.
- **Don't assume gh-equivalence everywhere.** Check `docs/gh-to-fj.md`
  if uncertain. Notable gaps: no `gh codespace`, no `gh attestation`,
  no `gh ruleset` (Forgejo uses branch protection via `fj protect`).

## When things go wrong

- HTTP 401: `fj auth refresh` (re-verifies) or `fj auth refresh --token
  NEW` (replaces). If a token is revoked, `fj auth login --host <h>`
  to start over.
- "no host selected": pass `--host` or `fj auth switch <h>` to set a
  default.
- Mysterious 404s on commands like `fj pr ready` or `fj search code`:
  the target server may be older than the Forgejo 7.x baseline. Check
  `fj api /version`. See [`docs/compatibility.md`](https://rasterhub.com/rasterstate/fj/src/branch/main/docs/compatibility.md).
- Hangs in CI: probably waiting on `$EDITOR`. Always pass `--body`.
- "shadowed by other commands": you have two `fj` binaries on PATH.
  Pick one (usually `~/.local/bin/fj` for source builds vs.
  `/opt/homebrew/bin/fj` for Homebrew installs).

## Reference

- Repo: <https://rasterhub.com/rasterstate/fj>
- gh→fj mapping: <https://rasterhub.com/rasterstate/fj/src/branch/main/docs/gh-to-fj.md>
- Architecture: <https://rasterhub.com/rasterstate/fj/src/branch/main/docs/architecture.md>
- Troubleshooting: <https://rasterhub.com/rasterstate/fj/src/branch/main/docs/troubleshooting.md>
- jq syntax: <https://rasterhub.com/rasterstate/fj/src/branch/main/docs/jq.md>
