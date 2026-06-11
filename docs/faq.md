# FAQ

## How do I get a token?

In Forgejo: Settings → Applications → Generate New Token. Pick the
scopes you need (typically `repo`, `notification`, and optionally
`admin:public_key` if you'll manage SSH keys). `fj auth login` will
prompt you to paste it.

## Where are my tokens stored?

fj checks `FJ_TOKEN` first; when it is set to a non-empty value, that
process uses it instead of any persistent store. Otherwise tokens are stored
in your OS keychain (macOS Keychain, Linux Secret Service, Windows Credential
Manager) under the service name `fj`, keyed by hostname. If the keychain is
unavailable, such as on headless Linux without Secret Service, `fj auth login`
falls back to `$XDG_CONFIG_HOME/fj/tokens.toml` with file mode 0600. Tokens
are never written to `hosts.toml`.

## How do I add a second host?

```sh
fj auth login --host other.example.com
fj auth switch other.example.com   # make it the default
# or pin per-command:
fj --host other.example.com repo list
```

## What's the difference between `fj api --jq` and `fj --json-fields`?

- `fj api --jq` projects a single JSON document with dotted paths,
  brackets, and pipes. See [`jq.md`](jq.md). It only works on
  `fj api`'s output.
- `fj --json-fields foo,bar` is a global flag that applies to ANY
  command's `--json` output. It keeps only those top-level (or dotted)
  fields. This is the gh-style behavior.

## Why does `fj` keep re-prompting the macOS keychain after I rebuild?

The keychain identifies applications by binary hash. Every
`cargo build --release` produces a new hash, so macOS treats it as a
new application. Click "Always Allow" once and it'll stick until the
next rebuild. See `troubleshooting.md`.

## How do I make `fj` work in a non-interactive script?

- Set `FJ_TOKEN` for an explicit process-scoped token override. This is the recommended path for CI jobs.
- On headless hosts where a per-shell env var is inconvenient, run `fj auth login`; if the OS keychain is unavailable, fj stores the token in `$XDG_CONFIG_HOME/fj/tokens.toml` with mode 0600.
- Set `FJ_HOST` to pin the host.
- Always pass `--body "..."` or `--body -` so `fj` never opens
  `$EDITOR`.
- Pipe output through `--json` for parseable results.
- Set `FJ_NO_PAGER=1` (or pass `--no-pager`).

## Can `fj` work without a git remote?

Yes. Without `-R/--repo` fj falls back to `git remote -v`. With `-R`
the remote isn't consulted at all. So you can run `fj repo view foo/bar`
from anywhere, including outside a git repo.

## How do I delete the test repo I created?

```sh
fj repo delete stephen/fj-cli-test     # asks you to type the slug
fj repo delete stephen/fj-cli-test -y  # skip the confirmation
```

## What if Forgejo and `gh` flag names disagree?

We track `gh`. See [`gh-to-fj.md`](gh-to-fj.md) for the mapping. When
`gh` doesn't have an equivalent (e.g. `fj milestone`, `fj protect`,
`fj hook`, `fj search code`), we follow Forgejo's terminology.

## Why are some `fj-*` plugins not found?

Plugin dispatch only triggers when the first positional isn't a known
subcommand. See `KNOWN_SUBCOMMANDS` in `src/main.rs`. Renaming your
plugin if it collides is the workaround.

## Can I script `git push` to also push via `fj`?

`fj` doesn't push git refs; `git` does that. But `fj auth setup-git`
installs a credential helper so that `git push` to your Forgejo host
authenticates via your stored token, no `~/.netrc` needed.

## How do I see what `fj` is sending?

```sh
fj --debug repo list -R foo/bar
# or
FJ_DEBUG=1 fj repo list -R foo/bar
```

Stderr will show `→ GET https://… ?query` and `← 200 url` for every
request, with a body preview if applicable.

## How do I customize the pager?

```sh
FJ_PAGER='less -R' fj repo list       # don't auto-quit on short output
FJ_PAGER=cat fj repo list             # disable paging
FJ_NO_PAGER=1 fj repo list            # same as above
fj --no-pager repo list               # per-command opt out
```

## Will `fj` work on Windows?

Honest answer: nobody's tried. The code compiles for Windows
(`#[cfg(windows)]` stubs in `output::pager` and `cli::web`), but no
release binary is built or tested for it and the keychain code path
(via the `keyring` crate's `windows-native` feature) has never been
exercised by a real user. If you want to be the first, build from
source with a current Rust toolchain and report back at
[issue #2](https://rasterhub.com/rasterstate/fj/issues/new) with what
broke.

## Where does `fj` store its config?

- macOS: `~/Library/Application Support/com.rasterstate.fj/`
- Linux: `$XDG_CONFIG_HOME/fj/` (`~/.config/fj/`)
- Windows: `%APPDATA%\rasterstate\fj\`

Files: `hosts.toml`, `aliases.toml`, `config.toml`.

## How do I read Actions run logs?

```sh
fj run list                 # find the run number in the `#` column
fj run view 56 --log        # full log of every job in the run
fj run view 56 --log-failed # failed steps, across every job that failed
fj run watch 56             # stream the log live until the job ends
```

By default `--log` spans all of a run's jobs, and `--log-failed` shows
only the jobs that did not finish green (so a failure in a later job
isn't hidden behind an earlier passing one). Narrow to one job with
`--job <index>` (0-based). After a rerun, the latest attempt is shown by
default; pin one with `--attempt <n>`.

Two things to know, because they look like bugs but aren't:

- Forgejo's REST API has no log endpoint. `fj` reads logs from the same
  web routes the browser Actions UI uses (see `docs/architecture.md` →
  "Actions run logs"). This is why log commands work even though
  `fj api /repos/.../actions/runs/N/logs` returns 404.
- "logs are no longer available for this run" means Forgejo pruned them
  after its retention period. They're gone server-side; there's nothing
  `fj` can fetch.

Logs are not available for hosts logged in with a Fjord Account: the
gateway only proxies `/api/v1`, not the web routes. Use a host with a
personal access token.
