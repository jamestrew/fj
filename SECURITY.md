# Security policy

## Reporting a vulnerability

Please report security issues to **security@rasterstate.com** rather than
opening a public issue. Encrypt with the GPG key below if the issue is
sensitive. Expect a response within 72 hours.

Include if possible:

- A clear description of the issue and impact.
- Steps to reproduce, with a minimal example.
- The fj version (`fj --version`) and Forgejo version
  (`fj api /version`).

## Threat model

fj is a CLI that:

- Stores Forgejo API tokens in the OS keychain (macOS Keychain, Linux
  Secret Service, Windows Credential Manager). Tokens are never written
  to disk in `hosts.toml` or anywhere else under our control.
- Sends those tokens as bearer headers over HTTPS to the configured
  hosts. The HTTP client uses rustls with the platform's trust store.
- Shells out to `git` for clone, fetch, and credential-helper setup.
- Opens `$EDITOR` for body inputs and `$FJ_PAGER` / `$PAGER` for long
  output.

Out of scope:

- Vulnerabilities in Forgejo itself. Report those to the Forgejo project.
- Vulnerabilities in `cargo` or in dependent crates. Those should be
  reported via `cargo audit` channels.
- Loss of tokens stored in the OS keychain via OS-level compromise.

## Known sharp edges

- `fj auth token` and `fj auth status --show-token` print plaintext
  tokens. We refuse to write to a TTY by default (since this risks
  capture in shell history). Pass `--force` to override, or pipe to a
  consumer like a credential helper.
- `fj auth setup-git` registers a `credential.helper` that invokes
  `fj auth token` at git-credential time. The hostname is validated
  against a strict DNS-style pattern at setup, but the helper string
  still passes through git's `!`-prefix shell evaluator at use time. If
  you've configured fj to talk to a host that some other tool added an
  attacker-controlled value for, audit your git config.
- `fj extension`'s plugin dispatch shells out to `fj-<name>` binaries
  on PATH. Treat any executable named `fj-*` on PATH as trusted code.

## Token hygiene

- Rotate Forgejo tokens periodically. `fj auth refresh --token NEW`
  replaces the stored value without losing your host config.
- Scope tokens narrowly when the API supports it. fj only needs the
  scopes for the operations you actually run.
- If you suspect a token is compromised, revoke it on the Forgejo side
  immediately, then `fj auth logout --host <host>` and `fj auth login
  --host <host>` to re-pair.

## Versions

We aim to fix security issues in the latest `main` and one prior minor
release. There is no LTS branch yet.

## GPG key

(Not yet published. Email reports without encryption are acceptable for
now; ack within 72 hours.)
