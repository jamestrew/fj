<p align="center">
  <img src="assets/logo.png" alt="fj logo" width="128">
</p>

<h1 align="center">fj</h1>

<p align="center">
  <strong>A native CLI for <a href="https://forgejo.org">Forgejo</a> and Gitea-compatible forges.</strong><br>
  Open PRs, triage issues, ship releases, and tail Actions logs from the terminal,<br>
  all from a single binary. Tokens prefer your OS keychain, with a 0600 file fallback for headless hosts. Multi-host from day one.
</p>

<p align="center">
  <a href="https://rasterhub.com/rasterstate/fj/actions"><img src="https://rasterhub.com/rasterstate/fj/actions/workflows/ci.yml/badge.svg" alt="ci"></a>
  <a href="https://rasterhub.com/rasterstate/fj/releases/latest"><img src="https://img.shields.io/badge/release-v0.4.1-blue" alt="version"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-green" alt="license"></a>
  <a href="https://rasterhub.com/rasterstate/fj/src/branch/main/CHANGELOG.md"><img src="https://img.shields.io/badge/changelog-keep--a--changelog-orange" alt="changelog"></a>
</p>

<p align="center">
  <img src="assets/demo-everyday.gif" alt="fj: view a repo, list issues and PRs from the terminal" width="800">
</p>

## Why fj

If you self-host Forgejo or Gitea, scripting it has meant pasting curl
commands, juggling a `~/.netrc`, or wrapping your own scripts around `git` and
the web UI. `tea` exists but lags Forgejo features and ships no signed release.

`fj` is the piece in the middle: a single binary that tracks the Forgejo
`/api/v1` surface, ships **signed and notarized** macOS builds (no Gatekeeper
warnings), a Linux binary, and a Homebrew tap. Tokens prefer the
macOS Keychain, the Linux Secret Service, or the Windows Credential Manager.
On headless systems without a usable keychain, fj falls back to a 0600
`tokens.toml` in its XDG config directory. `FJ_TOKEN` is always checked first
and overrides persistent storage for that process.

What you get over hand-rolled scripts:

- **Repo auto-detection.** Inside any clone, `fj pr list` already knows the
  repo and the host. No `-R` plumbing.
- **Retry, pagination, and a jq-ish projector** built into every call.
- **JSON-native output** (`--json`, `--json-fields`) so scripts and AI agents
  can drive it without screen-scraping.

**Compatibility:** built and tested against Forgejo (7.x and newer); most
commands work on Gitea too. See [docs/compatibility.md](docs/compatibility.md).

## Install

**Homebrew** (macOS and Linux x86_64), signed and notarized:

```sh
brew tap rasterandstate/tap https://rasterhub.com/rasterstate/homebrew-tap
brew install fj
```

**Linux tarball:**

```sh
curl -fsSL https://rasterhub.com/rasterstate/fj/releases/download/v0.4.1/fj-v0.4.1-linux-x86_64.tar.gz | tar -xz
sudo mv fj-v0.4.1-linux-x86_64/fj /usr/local/bin/fj
```

**From source** (any platform with a current Rust toolchain):

```sh
cargo install --git https://rasterhub.com/rasterstate/fj --tag v0.4.1
```

Windows compiles but is untested. [File an issue](https://rasterhub.com/rasterstate/fj/issues/new) if you try it.

## Quickstart

```sh
fj auth login                            # pick Fjord Account or a Forgejo token; stored in keychain or 0600 file fallback
fj repo view                             # auto-detects the repo from your git remote
fj pr list --state all -L 10             # latest 10 PRs
fj pr list --base main --no-draft        # branch/draft filters
fj issue list --milestone v1 --mentioned alice
fj issue create --template bug            # seed body from repo templates
fj pr create --template                   # choose a PR template interactively
fj api /version                          # raw API escape hatch
```

`-R/--repo` is always optional inside a clone (fj reads `origin` then
`upstream` to find the slug). Outside a clone, pass it explicitly. `create
--template` uses Forgejo's issue-template API when present and falls back to
reading template files from the repository's default branch.

## Authentication

`fj` signs in two ways, both stored in your OS keychain (0600 file fallback on
headless hosts):

- **Forgejo personal access token** (the default). `fj auth login --host
  rasterhub.com` prompts for a token, or pass it non-interactively with
  `--token` / `--with-token` (stdin).
- **Fjord Account** (`--fjord`). One sign-in that talks to every Forgejo
  instance your account owns, through the fjord-platform gateway. After signing
  in, use `fj instances` to list and pick an instance.

```sh
fj auth login --host rasterhub.com          # Forgejo PAT
fj auth login --fjord                        # Fjord Account (opens your browser)
fj instances                                 # list/pick an instance after --fjord
fj auth status                               # per-host scheme, sign-in, and session
```

### Signing in to a Fjord Account across devices

On an SSO/OIDC deployment, plain `--fjord` captures the sign-in on a local
loopback listener, which only works when your browser is on the **same machine**
as `fj`. When it isn't (a remote server, an SSH session, or you want to approve
on your phone or iPad), use one of:

- **`--device`** (recommended): durable cross-device sign-in via the OAuth 2.0
  Device Authorization Grant (RFC 8628). `fj` prints a short user code, a
  verification link, and a scannable QR code; approve on any device and `fj`
  picks up the session automatically. No loopback and no copy-paste. It is
  auto-selected on a headless / SSH session (opt back out with
  `FJ_FORCE_LOOPBACK=1`).
- **`--manual`** (alias `--no-browser`): print the sign-in URL, complete it on
  any device, and paste the redirected `127.0.0.1` URL (or the bare code) back.
  Superseded by `--device` for the cross-device case.

```sh
fj auth login --fjord --device               # show a code + QR; approve anywhere
fj auth login --fjord --manual               # print URL, paste the result back
fj auth login --fjord --platform-url http://localhost:5180   # staging / local dev
```

A Fjord Account session under OIDC keeps a refresh token, so it renews silently
(and on demand via `fj auth refresh`).

### Web session for cookie-gated Actions routes

`fj run logs` and `fj run view --log` read job logs through Forgejo's
token-authenticated Actions API on supported Forgejo builds. The remaining
human-facing Actions routes, including `fj run watch` and `fj run rerun`, are
cookie-gated on rasterhub (fj#103). To use those browser-backed routes on a
private repo, import the `i_like_gitea` cookie from a signed-in browser:

```sh
fj auth session --with-cookie                # paste the cookie on stdin (kept out of history)
fj auth session --clear                      # forget the stored web session
```

The cookie is stored under a keychain slot separate from your API token, is
never echoed, and honours the `FJ_SESSION` env override. `fj auth status` shows
a "Web session" line per host so you can see whether these routes are enabled.

## Everyday workflows

**Review a PR:**

```sh
fj pr checkout 42                        # fetch and check out the PR branch
fj pr diff 42 | less                     # or `fj pr files 42` for a file summary
fj pr review 42 --event approve --body "LGTM"
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
  --asset dist/foo-x86_64.tar.gz
```

**Inspect an Actions run:**

```sh
fj run logs 42 --failed                  # failed jobs in the run
fj run logs 42 --job 1                   # one job's full log
fj run download 42 --list --json         # artifacts as JSON
```

## Built for automation and agents

`fj` is JSON-native, so it scripts cleanly and AI agents can drive it without
parsing tables:

```sh
fj pr list --json --json-fields number,title,user.login   # selective JSON projection
fj api /repos/foo/bar/pulls --paginate -q '.[].number'    # raw API + jq-ish path
```

<p align="center">
  <img src="assets/demo-automation.gif" alt="fj: Actions runs, JSON projection, and raw API from the terminal" width="800">
</p>

It also ships a **[Claude Code](https://docs.claude.com/en/docs/claude-code)
plugin** so agents (and people) can drive fj from natural language:

```text
/plugin marketplace add rasterandstate/fj-claude-plugin
/plugin install fj@rasterandstate
```

And `fj agent review` / `fix` / `explain` bring AI code ops to the terminal,
with local secret redaction before anything leaves your machine.
See [docs/agent.md](docs/agent.md). _(preview)_

## Stacked PRs

`fj stack` manages a chain of dependent branches and their PRs: each PR targets
the one below it, and the chain merges bottom-up. State is local and
git-native in `.git/fj/stack.json`, with no server state.

```sh
fj stack new "Login Flow"     # author a stack and its first branch
fj stack review               # PR number, review, CI, and mergeability per item
fj stack sync                 # push branches and create/update the PRs
fj stack ship                 # merge bottom-up behind a green-and-approved gate
fj stack ship --squash        # ... and replay the rest of the stack onto the new trunk tip
```

<p align="center">
  <img src="assets/demo-stacks.gif" alt="fj stack review and sync on a three-PR stack" width="800">
</p>

Full guide, including `split` and `absorb`: [docs/stacks.md](docs/stacks.md). _(preview)_

## Command reference

Run `fj --help` for the live list; every group has its own `--help`.

| Area            | Commands                                                        |
| --------------- | -------------------------------------------------------------- |
| Repos and code  | `repo`, `issue`, `pr`, `release`, `search`, `browse`            |
| Actions         | `run`, `workflow`, `secret`, `variable`                         |
| Account and host| `auth`, `instances`, `org`, `ssh-key`, `gpg-key`               |
| Power and config| `api`, `alias`, `config`, `protect`, `hook`, `extension`, `completion`, `man` |
| Preview         | `work`, `stack`, `agent`                                        |

**Global flags** (work on every command): `--host` / `FJ_HOST`, `--debug`,
`--color`, `--no-pager`, and `--json-fields`.

`--color=auto|always|never` controls ANSI color output. Precedence is
`--color`, then `NO_COLOR` / `FORCE_COLOR`, then `fj config set no_color true`,
then terminal detection.

`--json` applies to data-returning commands such as `list`, `view`, and other
commands that document JSON output. `--web` applies to list/view commands that
can open results in the browser.

## Configuration

| Location | What |
| -------- | ---- |
| `$XDG_CONFIG_HOME/fj/hosts.toml` (`~/Library/Application Support/fj/hosts.toml` on macOS) | host registry + current default |
| `$XDG_CONFIG_HOME/fj/aliases.toml` | user-defined command shortcuts |
| `$XDG_CONFIG_HOME/fj/config.toml` | editor / pager / browser / color preferences |
| OS keychain, service `fj`, key = hostname | Preferred persistent API token store |
| `$XDG_CONFIG_HOME/fj/tokens.toml` | 0600 fallback token store when the OS keychain is unavailable |
| `FJ_TOKEN` | Process-scoped token override for headless/CI jobs; checked before persistent stores |
| `FJ_CONFIG_DIR` | Overrides the config directory outright, whatever the platform default would be |
| `FJ_NO_KEYCHAIN` | Set to `1` to skip the OS keychain and use the `tokens.toml` file store alone |

## Contributing

```sh
git clone https://rasterhub.com/rasterstate/fj
cd fj
cargo build --release        # ~4 MB stripped binary at target/release/fj
./scripts/install-hooks.sh   # local gates: fmt, clippy -D warnings, tests, audit, release build
```

`make install` puts a signed copy on your PATH and installs the man pages.
See [CONTRIBUTING.md](CONTRIBUTING.md) for the full workflow and
[docs/architecture.md](docs/architecture.md) for the module graph and design.

## Documentation

- [`docs/architecture.md`](docs/architecture.md): module graph, HTTP funnel, repo resolution, test strategy
- [`docs/stacks.md`](docs/stacks.md): stacked branches and PRs
- [`docs/agent.md`](docs/agent.md): AI-assisted code ops
- [`docs/jq.md`](docs/jq.md): `fj api --jq` projection syntax
- [`docs/compatibility.md`](docs/compatibility.md): Forgejo version matrix and known caveats
- [`docs/faq.md`](docs/faq.md): tokens, hosts, debug, scripting, plugins
- [`docs/troubleshooting.md`](docs/troubleshooting.md): keychain prompts, 401s, hook bypass, pager opt-out
- [`CONTRIBUTING.md`](CONTRIBUTING.md): build / test / release workflow
- [`CHANGELOG.md`](CHANGELOG.md): release notes
- [`SECURITY.md`](SECURITY.md): security policy + threat model

## License

MIT.
