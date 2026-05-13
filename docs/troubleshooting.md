# Troubleshooting

## macOS keeps prompting for keychain access

After every `cargo build --release` the binary's hash changes, so the
macOS keychain treats it as a new application and re-prompts. Click
"Always Allow" once and it'll stick until the next rebuild.

For automated test environments, see
`src/client/integration_tests.rs` — those tests use
`Client::for_base_url` to bypass the keychain entirely.

## `fj` hangs on the first command after a rebuild

Same root cause as above. macOS is prompting for keychain access in a
modal dialog you can't see (the binary doesn't have a TTY). Bring the
Keychain Access prompt to the foreground and approve it, or run
`fj auth status` from a regular terminal first to surface the prompt.

## `error: HTTP 401 ... authentication failed`

The stored token is invalid or revoked. Re-authenticate:

```sh
fj auth refresh                  # re-verify the existing token
fj auth refresh --token NEW      # replace the stored token
fj auth login --host <host>      # full re-login (overwrites)
```

## I want to see the raw HTTP requests fj is making

```sh
fj --debug pr list -R foo/bar
# or
FJ_DEBUG=1 fj pr list -R foo/bar
```

You'll get `→ METHOD URL?query` and `← STATUS url` for every request,
plus a 200-character preview of the request body if one was sent.

## A command is hanging in my CI pipeline

CI runners often don't have an `$EDITOR`. `fj issue create` and friends
will hang waiting for `$EDITOR` to return if you omit `--body`.

- Pass `--body "text"` explicitly.
- Or pass `--body -` to read from stdin.
- Set `$VISUAL` or `$EDITOR` to a non-interactive program.

## The pager doesn't exit / output gets paged when I don't want it

```sh
fj --no-pager repo list          # opt out per-command
FJ_NO_PAGER=1 fj repo list       # opt out in your shell
```

The default pager is `less -FRX`, which exits when output fits one
screen. Override with `$FJ_PAGER` or `$PAGER`:

```sh
FJ_PAGER='less -R' fj repo list  # don't auto-quit
FJ_PAGER=cat fj repo list        # disable paging
```

## `error: unexpected argument '-R' found`

Some `fj repo` subcommands take a positional `repo` arg AND accept `-R`
(for symmetry with the rest of the CLI). If you see this on an older
build, update to a build after commit `eb716ee`. All commands now accept
both forms.

## `-R` works in some shells, not in others (zsh globbing)

If `fj api /repos/foo` returns a "no matches found" error before fj
even runs, you've hit shell globbing on `/`. Quote the path:

```sh
fj api '/repos/foo/bar'
```

## Aliases aren't expanding

Aliases are looked up before clap parses, but only the FIRST positional
is matched. So:

```sh
fj alias set co "pr checkout"
fj co 42                         # works: co → pr checkout
fj --host other.host co 42       # works: --host is parsed by clap after expansion
fj help co                       # does NOT expand co; "help" is a builtin
```

If your alias name collides with a real subcommand, the real one wins.
Aliases never shadow built-in commands.

## Extension `fj-foo` isn't being found

`fj` looks for `fj-<name>` on PATH only when `<name>` isn't a known
subcommand. The list of known subcommands lives in
`src/main.rs::KNOWN_SUBCOMMANDS`. If your plugin name collides with a
built-in, rename it.

## `fj pr ready` returns "pull does not allow editing draft state"

Forgejo only lets you flip `draft: false` via PATCH if the API user is
the PR author or has write permissions. If you're not the author and
not a maintainer, this fails. There's no workaround on the client side.

## The hook hangs on `cargo test`

Likely a keychain re-prompt for the test binary's new hash. Either
approve the prompt or rebuild outside the hook context first, then push:

```sh
cargo test --all                 # accept any keychain prompts
git push                         # hook reruns; tests fast-cache hit
```

If genuinely stuck:

```sh
FJ_SKIP_PREPUSH=1 git push       # bypass once, only when necessary
```

## My commit was rejected because clippy complains about new code

The hook runs `cargo clippy --all-targets --all-features -- -D warnings`.
Every warning is an error. Either fix it or, very rarely and with a
comment explaining why, add a targeted `#[allow(clippy::lint_name)]`.

Don't add `#[allow(dead_code)]` to silence "never used" warnings on
real code — that's a signal you have unused code paths to remove.
