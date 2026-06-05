<p align="center">
  <img src="assets/logo.png" alt="fj logo" width="128">
</p>

<h1 align="center">fj</h1>

<p align="center">
  <strong>A native CLI for <a href="https://forgejo.org">Forgejo</a> and Gitea-compatible forges.</strong><br>
  <code>fj</code> drives your forge from the terminal: open PRs, triage issues,<br>
  ship releases, tail Actions logs, drive workflows, all from a single binary.<br>
  Tokens stay in your OS keychain. Multi-host from day one.
</p>

<p align="center">
  <a href="https://rasterhub.com/rasterstate/fj/actions"><img src="https://rasterhub.com/rasterstate/fj/actions/workflows/ci.yml/badge.svg" alt="ci"></a>
  <a href="https://rasterhub.com/rasterstate/fj/releases/latest"><img src="https://img.shields.io/badge/release-v0.2.0-blue" alt="version"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-green" alt="license"></a>
  <a href="https://rasterhub.com/rasterstate/fj/src/branch/main/CHANGELOG.md"><img src="https://img.shields.io/badge/changelog-keep--a--changelog-orange" alt="changelog"></a>
</p>

<p align="center">
  <img src="assets/demo.gif" alt="fj demo" width="800">
</p>

## Why fj

If you self-host a Forgejo or Gitea instance, your options for scripting it
have been: paste curl commands, juggle a `~/.netrc`, or wrap your own scripts
around `git` and the web UI. `tea` exists but lags Forgejo features and has
no signed release.

`fj` is the thing in the middle. It tracks the Forgejo `/api/v1` surface,
ships **signed and notarized** macOS binaries (no Gatekeeper warnings, no
re-prompting keychain), one Linux binary, and a Homebrew tap. Tokens never
touch disk: they live in the macOS Keychain, Linux Secret Service, or
Windows Credential Manager.

Three concrete wins over hand-rolled curl scripts:

- **Repo auto-detection** from your git remote. Inside any Forgejo clone,
  `fj pr list` knows which repo and which host. No `-R` plumbing.
- **Retry, pagination, and a jq-ish projector** baked into every call. `fj
  api /repos/foo/bar/issues --paginate -q '.[].title'` Just Works.
- **Agent-friendly.** Ships a [Claude Code](#claude-code-plugin) plugin so
  AI agents can drive fj from natural-language requests, and a `--json
  --json-fields name,number` projection mode for scripts.

## Install

**Homebrew** (macOS + Linux x86_64), signed and notarized binaries:

```sh
brew tap rasterandstate/tap
brew install fj
```

**Linux tarball:**

```sh
curl -fsSL https://rasterhub.com/rasterstate/fj/releases/download/v0.2.0/fj-v0.2.0-linux-x86_64.tar.gz | tar -xz
sudo mv fj-v0.2.0-linux-x86_64/fj /usr/local/bin/fj
```

**From source** (any platform with a current Rust toolchain):

```sh
cargo install --git https://rasterhub.com/rasterstate/fj --tag v0.2.0
```

Windows compiles but is untested. [File an issue](https://rasterhub.com/rasterstate/fj/issues/new) if you try it.

## Quick start

```sh
fj auth login                            # paste a Forgejo token; stored in your OS keychain
fj repo view                             # auto-detects the repo from your git remote
fj pr list --state all -L 10             # latest 10 PRs
fj issue create --title "..." --body "..."
fj api /version                          # raw API escape hatch
```

`-R/--repo` is always optional inside a clone (fj reads `origin` / `upstream`
to figure out the slug). Outside a clone, pass it explicitly.

## Common workflows

**Review a PR (inside a clone):**

```sh
fj pr checkout 42                        # fetch + checkout PR branch
fj pr diff 42 | less                     # or `fj pr files 42` for a file summary
fj pr review 42 --event approve --body "LGTM"
fj pr review 42 --event request-changes --body "see comments"
fj pr request-review 42 alice bob        # tag specific reviewers
```

**Triage your inbox:**

```sh
fj pr status                             # cross-repo dashboard of PRs you care about
fj status                                # notifications inbox
fj status --mark-read                    # clear it
```

**Cut a release:**

```sh
fj release create v1.2.3 \
  --title "1.2.3" \
  --body "$(cat RELEASE_NOTES.md)" \
  --asset dist/foo-x86_64.tar.gz \
  --asset dist/foo-aarch64.tar.gz
```

**Inspect an Actions run, scripting-friendly:**

```sh
fj run view 42 --log-failed                 # failed steps across every job
fj run view 42 --log                        # full log, all jobs in the run
fj run view 42 --log --job 1                # just the second job's log
fj run download 42 --list --json            # artifacts as JSON
```

**Drop into raw API when you need it:**

```sh
fj api /repos/foo/bar/pulls --paginate -q '[].number'
fj api -X POST /repos/foo/bar/labels --input '{"name":"bug","color":"d73a4a"}'
fj api /version --include                # response headers + body
```

## Command surface

Twenty-nine top-level groups. Run `fj --help` for the live list; each group
has its own `--help`. Highlights:

| Group       | What it covers                                                                  |
| ----------- | ------------------------------------------------------------------------------- |
| `auth`      | login, status, logout, switch, token, refresh, setup-git                        |
| `instances` | list and select Forgejo instances on a Fjord Account                            |
| `repo`      | list, view, clone, create, fork, sync, edit, rename, archive, delete, branches, topics, mirror, watch, star, set-default, deploy-key |
| `issue`     | list, view, create, edit, close, reopen, delete, pin, comment, edit-comment, delete-comment, develop |
| `pr`        | list, view, create, edit, diff, commits, files, checks, ready, comment, update-branch, review, request-review, status, checkout, merge, close, reopen |
| `release`   | list, view, create, edit, delete, upload, download, delete-asset                |
| `label`     | CRUD + clone                                                                    |
| `milestone` | CRUD + assign                                                                   |
| `run`       | workflow runs (Forgejo Actions): list, view, `--log`, watch, download           |
| `workflow`  | list workflow files; trigger a `workflow_dispatch` run                          |
| `secret`    | Actions secrets                                                                 |
| `variable`  | Actions variables                                                               |
| `search`    | repos, issues, prs, users                                                       |
| `browse`    | open the current repo (or a path within it) in `$BROWSER`                       |
| `status`    | notifications inbox + `--mark-read`                                             |
| `org`       | list, view, teams                                                               |
| `ssh-key`   | account keys                                                                    |
| `gpg-key`   | account keys                                                                    |
| `alias`     | user-defined command shortcuts                                                  |
| `config`    | local preferences (editor, pager, browser)                                      |
| `protect`   | branch protection rules                                                         |
| `hook`      | webhook CRUD + test delivery                                                    |
| `extension` | discover and run `fj-<name>` plugins on PATH                                    |
| `api`       | raw HTTP with `-X`, `-f`, `-F`, `-H`, `-q`, `--paginate`, `--include`           |
| `work`      | working-context / session: context; resume, continue _(preview)_               |
| `stack`     | stacked branches / PRs: new, review, sync, ship; split, absorb _(preview)_      |
| `agent`     | AI-assisted code ops: review, fix, explain _(preview)_                          |
| `completion`| shell completions (bash, zsh, fish, powershell, elvish)                         |
| `man`       | generate man pages                                                              |

### Work context

`fj work context` summarizes where you are: repo + host, your Fjord
account/auth, the current branch, working-tree state and ahead/behind, and,
best-effort over the Forgejo API, the default branch, the open PR for the
branch and its CI/check status, plus the current local stack. Local data
works offline; the API parts degrade gracefully (shown as unavailable) when
the server or token isn't reachable.

```sh
fj work context                   # human-readable summary
fj work context --json            # same, as JSON (honors --json-fields)
```

### Stacks

Stacked branches/PRs, tracked locally and git-natively in
`.git/fj/stack.json` (no server state, no committed manifest).

```sh
fj stack new "Login Flow"             # author a stack + its first branch
fj stack new "Login Flow" --base develop
fj stack new "Login Flow" --from issue 123   # link an issue (best-effort title)
fj stack new "Login Flow" --no-branch --json
fj stack review                       # each item: PR#, review, CI, mergeable, reviewers
fj stack review --open                # open every PR in the browser
fj stack review --request alice       # request alice's review on the whole stack
fj stack sync                         # push branches + create/update PRs
fj stack sync --draft --dry-run       # plan only (no pushes / PR writes)
fj stack ship                         # merge bottom-up (green + approved gate)
fj stack ship --dry-run               # print the merge plan, change nothing
fj stack ship --squash --delete-branches
```

`fj stack new` derives a safe slug and branch (`<handle>/<slug>-1`), records
schema/repo/base/timestamps/issue/items, and won't clobber an existing stack
without `--force`. `fj stack sync` pushes each branch to origin and
creates/updates its PR, chaining bases (item _n_ targets item _n-1_'s branch,
the first targets the stack base) and writing PR numbers/URLs back into the
stack file; it's idempotent and `--draft`/`--ready` control PR state. After the
PRs exist it mirrors a navigation block (the whole stack, linked, with the
current entry marked) into each PR description, wrapped in `fj-stack` markers so
any hand-written body around it is preserved; the block is a convenience, never
the source of truth, and `--no-annotate` skips it. The block also carries a
versioned JSON payload so the Fjord apps can read the stack as data instead of
scraping markdown (see [docs/stack-annotation.md](docs/stack-annotation.md)).
`fj stack ship` merges the stack bottom-up: it pre-flights every PR (each must
be approved and green unless `--force`), merges from the base up, re-points the
next PR onto the trunk after each merge, and stops on the first failure with
recovery instructions (merged items are skipped on re-run). `--merge` (default)
/ `--squash` pick the strategy, `--delete-branches` cleans up merged branches,
and `--dry-run` prints the plan without touching anything.
`fj stack split` turns a single-branch stack into one stacked entry per commit:
the _N_ commits ahead of the base become branches `<handle>/<slug>-1..N`, each
chained on the one below. It only moves local refs (no push, no API), so it's
offline and reversible; it refuses a dirty tree or a multi-entry stack, and
`--dry-run` shows the commit-to-branch mapping.
`fj stack absorb` folds your staged changes into the entries that introduced
them, the way `git absorb` does: it attributes each staged file to the most
recent stack commit that touched it (per-file), creates a `fixup!` commit per
target, then runs one autosquash `git rebase --update-refs` to rewrite the
entries in place and move every branch ref. Files with no matching entry are
left staged. It needs git >= 2.38, operates on the stack's top branch, and
rewrites history, so the branches need a force-push afterward; `--dry-run` (and
`--json`) print the attribution plan without touching anything.

### Agent (AI-assisted)

`fj agent review` / `fix` / `explain` route through a provider configured by
`FJ_AGENT_PROVIDER` / `FJ_AGENT_API_KEY` / `FJ_AGENT_MODEL` (the Fjord token is
never reused).

`fj agent review` is a local diff-analysis command. It gathers the working-tree
diff (or `--pr <n>`, or each `--stack` item), redacts secrets and sensitive
files before anything leaves your machine (`.env`, private keys, `secret = …`
assignments, known token prefixes, PEM blocks), and prints severity-tagged
findings with file references: secret-bearing files, source changed without
tests, and risky areas (migrations, auth/crypto, CI, dependency manifests).
That local analysis runs even with no AI backend configured; when a provider
*is* set it adds a narrative review on top. `--security` focuses the pass, and
`--json` emits the full report for editor/mobile clients.

`fj agent fix` asks the provider for a patch over the current change (the
`git diff HEAD`). By default it just prints the patch and changes nothing.
`--write` applies the patch to your working tree (validating it first, leaving
the result unstaged to review). `--pr` carries the current change plus the fix
onto a fresh branch (`--branch`, default `agent-fix`), commits (`--title`,
default "Apply agent fix"), pushes, and opens a PR against the branch you ran
from; every local check runs before anything touches the network, and on
failure it restores your previous branch. `--dry-run` reports the plan without
calling the provider.

```sh
fj agent review                 # analyze the working-tree diff
fj agent review --stack         # review each stack item separately
fj agent review --security --json
fj agent review --pr 42
fj agent fix                    # print a proposed patch
fj agent fix --write            # apply it to the working tree
fj agent fix --pr --branch tidy # open it as a PR
fj agent explain src/main.rs --json
```

## Global flags

- `--host <name>` or `FJ_HOST`: explicit host, otherwise inferred from git
  remote, otherwise the configured default.
- `--debug` or `FJ_DEBUG=1`: log every HTTP request to stderr.
- `--json`: switch list/view output to JSON.
- `--json-fields field1,field2,owner.login`: field projection on top of
  `--json`. Dotted paths supported.
- `--no-pager` or `FJ_NO_PAGER=1`: skip the pager.
- `--web` on list/view: open the relevant page in `$BROWSER`.

## Claude Code plugin

`fj` ships a [Claude Code](https://docs.claude.com/en/docs/claude-code)
plugin so AI agents (and developers using Claude Code) can drive fj from
natural-language requests:

```text
/plugin marketplace add rasterandstate/fj-claude-plugin
/plugin install fj@rasterandstate
```

The skill activates whenever the user mentions fj, Forgejo, Gitea, or any
forge-side action. Source lives in [`claude/`](claude/).

## Configuration

| Location | What |
| -------- | ---- |
| `$XDG_CONFIG_HOME/fj/hosts.toml` (`~/Library/Application Support/fj/hosts.toml` on macOS) | host registry + current default |
| `$XDG_CONFIG_HOME/fj/aliases.toml` | user-defined command shortcuts |
| `$XDG_CONFIG_HOME/fj/config.toml` | editor / pager / browser preferences |
| OS keychain, service `fj`, key = hostname | API tokens (never on disk) |

## Build it yourself

```sh
git clone https://rasterhub.com/rasterstate/fj
cd fj
cargo build --release            # ~4 MB stripped binary at target/release/fj
./target/release/fj completion zsh > ~/.zfunc/_fj
./target/release/fj man -o ~/man/man1
```

The pre-push hook (`hooks/pre-push`) gates every push on `cargo fmt`,
`cargo clippy -D warnings`, the test suite, and a release build. Install
once via `./scripts/install-hooks.sh`. Set `FJ_E2E=1` to also run the live-API
smoke against your configured host.

## Documentation

- [`docs/architecture.md`](docs/architecture.md): module graph, HTTP funnel, repo resolution, test strategy
- [`docs/jq.md`](docs/jq.md): `fj api --jq` projection syntax
- [`docs/compatibility.md`](docs/compatibility.md): Forgejo version matrix and known caveats
- [`docs/faq.md`](docs/faq.md): tokens, hosts, debug, scripting, plugins
- [`docs/troubleshooting.md`](docs/troubleshooting.md): keychain prompts, 401s, hook bypass, pager opt-out
- [`CONTRIBUTING.md`](CONTRIBUTING.md): build / test / release workflow
- [`CHANGELOG.md`](CHANGELOG.md): release notes
- [`SECURITY.md`](SECURITY.md): security policy + threat model

## License

MIT.
