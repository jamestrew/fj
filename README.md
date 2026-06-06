<p align="center">
  <img src="assets/logo.png" alt="fj logo" width="128">
</p>

<h1 align="center">fj</h1>

<p align="center">
  <strong>A native CLI for <a href="https://forgejo.org">Forgejo</a> and Gitea-compatible forges.</strong><br>
  Open PRs, triage issues, ship releases, and tail Actions logs from the terminal,<br>
  all from a single binary. Tokens stay in your OS keychain. Multi-host from day one.
</p>

<p align="center">
  <a href="https://rasterhub.com/rasterstate/fj/actions"><img src="https://rasterhub.com/rasterstate/fj/actions/workflows/ci.yml/badge.svg" alt="ci"></a>
  <a href="https://rasterhub.com/rasterstate/fj/releases/latest"><img src="https://img.shields.io/badge/release-v0.2.0-blue" alt="version"></a>
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
warnings), a Linux binary, and a Homebrew tap. Tokens never touch disk. They
live in the macOS Keychain, the Linux Secret Service, or the Windows
Credential Manager.

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

## Quickstart

```sh
fj auth login                            # pick Fjord Account or a Forgejo token; stored in your OS keychain
fj repo view                             # auto-detects the repo from your git remote
fj pr list --state all -L 10             # latest 10 PRs
fj issue create --title "..." --body "..."
fj api /version                          # raw API escape hatch
```

`-R/--repo` is always optional inside a clone (fj reads `origin` then
`upstream` to find the slug). Outside a clone, pass it explicitly.

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
fj run view 42 --log-failed              # failed steps across every job
fj run view 42 --log --job 1             # one job's full log
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
`--json`, `--json-fields`, `--no-pager`, and `--web` on list/view commands.

## Configuration

| Location | What |
| -------- | ---- |
| `$XDG_CONFIG_HOME/fj/hosts.toml` (`~/Library/Application Support/fj/hosts.toml` on macOS) | host registry + current default |
| `$XDG_CONFIG_HOME/fj/aliases.toml` | user-defined command shortcuts |
| `$XDG_CONFIG_HOME/fj/config.toml` | editor / pager / browser preferences |
| OS keychain, service `fj`, key = hostname | API tokens (never on disk) |

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
