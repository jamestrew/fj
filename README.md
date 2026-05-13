# fj

A command-line tool for [Forgejo](https://forgejo.org) instances, in the spirit of GitHub's `gh`.

Multi-host from day one. Tokens are stored in your OS keychain.

## Install

```sh
cargo install --path .
```

## Quick start

```sh
fj auth login                          # add a host and token
fj auth status                         # show signed-in hosts
fj repo list                           # repos you own on the default host
fj repo view owner/name                # repo overview
fj issue list -R owner/name            # issues
fj pr list -R owner/name --state open  # pull requests
fj api /repos/search?q=foo             # raw API escape hatch
```

Use `--host <hostname>` on any command to target a specific host.

## Commands

| Group   | Commands                                          |
| ------- | ------------------------------------------------- |
| `auth`  | `login`, `status`, `logout`, `list`, `switch`     |
| `repo`  | `list`, `view`, `clone`, `create`                 |
| `issue` | `list`, `view`, `create`, `close`, `reopen`, `comment` |
| `pr`    | `list`, `view`, `create`, `checkout`, `merge`, `close` |
| `api`   | raw HTTP against `/api/v1` with optional fields and jq-style field selection |

## Config

- Hosts and the current host live in `$XDG_CONFIG_HOME/fj/hosts.toml` (`~/Library/Application Support/fj/hosts.toml` on macOS).
- Tokens live in the OS keychain under service `fj` keyed by hostname.

## License

MIT
