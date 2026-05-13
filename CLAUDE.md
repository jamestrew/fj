# fj — project notes for Claude

`fj` is a Forgejo CLI, in the spirit of GitHub's `gh`. Multi-host, tokens
in the OS keychain, ~10k LOC Rust with full feature parity to `gh` for
the surface Forgejo exposes.

## Layout

```
src/
  main.rs              entry: alias expansion, fj-<name> plugin dispatch
  cli/                 clap subcommand tree
    mod.rs             top-level Cli + dispatch + Ctrl+C race + pager
    context.rs         RepoFlag + resolve_repo (`-R` or git-remote detect)
    editor.rs          $EDITOR + body resolution helpers
    web.rs             cross-platform `--web` opener
    <subcommand>.rs    one file per command group (auth, repo, pr, …)
  api/                 typed wrappers around /api/v1
    mod.rs             split_repo + shared serde helpers
    <resource>.rs      one file per resource (issue, pull, release, …)
  client/              HTTP client
    mod.rs             Client, retry-on-5xx, debug logging, headers
    pagination.rs      Page<T> + Link header parser
    error.rs           ApiError
    integration_tests  wiremock-based tests
  config/              hosts.toml shape + load/save
  auth/                OS keychain wrapper
  git/                 git invocation + remote URL parser
  output/              tables, colors, relative_time, state_pill, pager
hooks/pre-push         local CI gate
scripts/               install-hooks.sh, e2e-smoke.sh
docs/                  architecture, jq syntax, troubleshooting
```

## Build / test / hook

- `cargo build --release` produces a ~4 MB binary at `target/release/fj`.
- `cargo test --all` runs 60 tests (51 unit + 9 wiremock integration).
- `cargo clippy --all-targets --all-features -- -D warnings` is the lint
  bar. CI (the pre-push hook) treats warnings as errors.
- `./scripts/install-hooks.sh` symlinks `hooks/pre-push` into `.git/hooks`.
- `FJ_E2E=1` enables the live API smoke against `stephen/fj-cli-test`.
- `FJ_SKIP_PREPUSH=1` bypasses the hook for genuine emergencies only.

## Key conventions

- Every repo-scoped subcommand accepts `-R/--repo` AND auto-detects from
  the git remote (`upstream` then `origin`). The resolver is
  `cli::context::resolve_repo`.
- Bodies (`--body`) support: an inline string, `-` for stdin, or omit to
  open `$EDITOR`. See `cli::editor`.
- Lists with `--limit > 50` transparently follow `Link: rel=next` via
  `Client::get_all`. Per-page cap is the Forgejo default (50).
- The simple jq projector lives in `cli::api::extract_path`. Supports
  `.field`, `.0`, `.[0]`, `[-1]`, and `|` pipes. See `docs/jq.md`.
- All commands respect `--host` / `FJ_HOST`, `--debug` / `FJ_DEBUG`,
  `--no-pager`, `FJ_PAGER` / `PAGER`.

## Things to remember

- Tokens are stored in the OS keychain (service `fj`, key = hostname).
  Never serialized to disk. Re-prompts after rebuilds are expected on
  macOS because the binary hash changes; click "Always Allow."
- `cargo test` builds a separate test binary with a different hash, so
  it gets its own keychain prompt. Tests that need keychain access are
  the wiremock-backed ones in `src/client/integration_tests.rs`, which
  bypass the keychain via `Client::for_base_url`.
- The repo URL in `Cargo.toml` is `https://rasterhub.com/rasterstate/fj`.
- Commit author is `Stephen Way <stephen@rasterstate.com>`. The user has
  a CLAUDE.md preference: no em-dashes, terse responses, no preamble.
- MSRV is 1.82 (we use `Option::is_none_or`).

## Where to look first

- Adding a new subcommand: copy an existing `cli/<x>.rs` (e.g. `label`).
  Wire into `cli/mod.rs` (`Command` enum, `dispatch`) and `main.rs`
  (`KNOWN_SUBCOMMANDS`). Typed API wrapper in `api/<x>.rs`.
- Adding a new Forgejo endpoint: `api/<resource>.rs` is the right home.
- Touching HTTP behavior: `client/mod.rs::request_with_headers` is the
  one spot all requests funnel through. Don't add a parallel code path.
- Test a new API surface: add to `src/client/integration_tests.rs`
  rather than `tests/` (this is a binary crate, no `[lib]` target, so
  integration tests live inline).
