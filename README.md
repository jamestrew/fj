# fj

A command-line tool for [Forgejo](https://forgejo.org) instances, in the spirit of GitHub's `gh`.

Multi-host from day one. Tokens are stored in your OS keychain.

## Install

```sh
cargo install --path .
# or
cargo build --release && cp target/release/fj ~/.local/bin/fj
```

## Quick start

```sh
fj auth login                          # add a host and token
fj auth status                         # show signed-in hosts
fj repo list                           # repos you own
fj repo view owner/name                # repo overview
fj issue list -R owner/name            # issues
fj pr list -R owner/name --state open  # pull requests
fj api /repos/search -X GET -f q=foo   # raw API escape hatch
```

`-R/--repo` is optional inside a git clone. fj detects the upstream from
`upstream`, then `origin`, then any other remote.

## Commands

| Group      | Common operations                                                                 |
| ---------- | --------------------------------------------------------------------------------- |
| `auth`     | login, status, logout, list, switch, token, refresh, setup-git                    |
| `repo`     | list, view, clone, create, fork, sync, edit, rename, archive, unarchive, delete, branches, topics, mirror, mirror-sync |
| `issue`    | list, view, create, edit, close, reopen, comment, develop                         |
| `pr`       | list, view, create, edit, diff, commits, files, checks, ready, review, status, checkout, merge, close, reopen |
| `release`  | list, view, create, edit, delete, upload, download                                |
| `label`    | list, create, edit, delete                                                        |
| `run`      | list, view, rerun, cancel (Forgejo Actions workflow runs)                         |
| `secret`   | list, set, delete (Actions secrets)                                               |
| `variable` | list, set, delete (Actions variables)                                             |
| `search`   | repos, issues, prs, users                                                         |
| `browse`   | open the current repo (or a path within it) in your browser                       |
| `status`   | notifications inbox + `--mark-read`                                               |
| `org`      | list, view, teams                                                                 |
| `ssh-key`  | list, add, delete                                                                 |
| `gpg-key`  | list, add, delete                                                                 |
| `alias`    | list, set, delete                                                                 |
| `config`   | get, set, list, path                                                              |
| `protect`  | list, view, set, delete branch protection rules                                   |
| `hook`     | list, create, delete, test webhooks                                               |
| `gist`     | list, create (thin wrapper over `gist-*` repos)                                   |
| `extension`| list, run discovered `fj-<name>` plugins on PATH                                  |
| `api`      | raw HTTP with `-X`, `-f`, `-F`, `-H`, `-q` (jq-ish), `--paginate`, `--include`    |
| `completion`| Print shell completions (bash, zsh, fish, powershell, elvish)                    |
| `man`      | Generate man pages into a directory                                               |

## Global flags

- `--host <hostname>` (or `FJ_HOST`): pick the host explicitly.
- `--debug` (or `FJ_DEBUG=1`): log every HTTP request to stderr.
- `--web` on most list/view subcommands: open the relevant page in your default browser.

## Config

- Hosts and the current host live in `$XDG_CONFIG_HOME/fj/hosts.toml`
  (`~/Library/Application Support/fj/hosts.toml` on macOS).
- Aliases in `aliases.toml`. Preferences in `config.toml`.
- Tokens live in the OS keychain under service `fj`, keyed by hostname.

## Aliases

```sh
fj alias set co "pr checkout"
fj co 42                   # equivalent to: fj pr checkout 42
```

## Extensions

Any executable on `$PATH` named `fj-<name>` is invokable as `fj <name> [args]`.

```sh
fj extension list          # show discovered plugins
fj my-plugin some-arg      # invokes `fj-my-plugin some-arg`
```

## Plugin-style usage of `fj api`

```sh
# Auto-paginate a list endpoint into a single JSON array:
fj api /repos/search -f q=fj --paginate -q .

# Pass a custom request header:
fj api /user -H "X-Trace-Id: foo"

# Show response headers along with the body:
fj api /version --include

# Send a literal JSON body:
fj api /repos/migrate --input '{"clone_addr":"...","repo_name":"x","repo_owner":"y"}'
```

## Hooks

- `hooks/pre-push` runs `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo test`, and a release build before any push.
- With `FJ_E2E=1`, the hook also runs `scripts/e2e-smoke.sh` against the
  configured host.
- Install via `./scripts/install-hooks.sh`.

## Building

```sh
cargo build --release            # 5-6 MB stripped binary at target/release/fj
./target/release/fj completion zsh > ~/.zfunc/_fj
./target/release/fj man -o ~/man/man1
```

## License

MIT
