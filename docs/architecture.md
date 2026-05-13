# Architecture

`fj` is a thin Rust CLI over Forgejo's `/api/v1`. The whole binary is
~10k LOC and shapes itself like `gh`: clap-parsed subcommands, one
typed wrapper per API resource, one HTTP client funnel for everything.

## Module graph

```
              ┌─────────────┐
              │   main.rs   │  alias expansion + fj-<x> plugin dispatch
              └──────┬──────┘
                     │
              ┌──────▼──────┐
              │  cli::run   │  Ctrl+C race + pager guard
              └──────┬──────┘
                     │
       ┌─────────────┼────────────────────┐
       │             │                    │
  ┌────▼─────┐ ┌─────▼─────┐       ┌──────▼──────┐
  │ cli::auth│ │ cli::repo │  ...  │  cli::api   │  raw escape hatch
  └────┬─────┘ └─────┬─────┘       └──────┬──────┘
       │             │                    │
       │       ┌─────▼─────┐              │
       │       │ context:: │  -R or git-remote autodetect
       │       │resolve_repo│
       │       └─────┬─────┘              │
       │             │                    │
       │       ┌─────▼─────┐              │
       └──────►│  api::*   │◄─────────────┘
               │  typed    │
               │ wrappers  │
               └─────┬─────┘
                     │
               ┌─────▼─────┐
               │  client   │  one funnel: auth headers, retry, pagination,
               │           │  debug logging, custom headers
               └─────┬─────┘
                     │
               ┌─────▼─────┐
               │  reqwest  │  rustls-tls, JSON, multipart
               └───────────┘
```

## Layering rules

- `cli::*` handles flags, argument validation, output formatting. It
  never makes HTTP calls directly: it calls `api::*`.
- `api::*` is one file per Forgejo resource. Each function takes a
  `&Client` and returns typed `Result<T>`.
- `client::*` is the only place that touches `reqwest`. All HTTP goes
  through `Client::request_with_headers`, which handles auth headers,
  retries, debug logging, and custom header merging. There is no
  parallel HTTP path (other than the multipart asset upload in
  `api::release::upload_asset`, which goes through `client.http()` but
  still uses `client.token()` for auth).
- `config::*` is the on-disk shape of `hosts.toml`. Tokens are NOT
  stored here. They live in the OS keychain via `auth::*`.
- `git::*` shells out to the `git` binary. The only place we call git.

## Repo resolution

Every command that operates on a specific repo calls
`cli::context::resolve_repo(explicit_repo, explicit_host)`. The
algorithm:

1. If `-R/--repo` was given, parse it as `owner/name`.
2. Otherwise call `git remote -v` and pick the first remote that parses
   as a Forgejo URL. Preference order is `upstream`, then `origin`,
   then any other.
3. The host comes from `--host`, then the detected remote's host, then
   the configured default host in `hosts.toml`.

This is the magic that lets `fj pr list` work without any flags inside
a clone.

## HTTP retry

In `client::request_with_headers`:

- The request is built once and stored as `reqwest::Request`.
- Idempotent methods (GET, HEAD, OPTIONS, PUT, DELETE) get 3 attempts.
- POST and PATCH get 1 attempt — they're never retried.
- Backoff is `200ms * 2^attempt` between retries.
- On 5xx or transport error, we retry. On any other status, we return.

## Auto-pagination

`Client::get_all(path, query, total_limit)` follows `Link: rel=next`
until either `total_limit` items have accumulated or there are no more
pages. List commands switch to this path automatically when the user
asks for `--limit > 50` (Forgejo's per-page cap).

## Pager

When `cli::run` dispatches a command whose output can run long (list,
view, diff, search, status, api), it calls
`output::pager::maybe_start(force_disabled)`. That function:

1. Bails if stdout isn't a TTY or `--no-pager` / `FJ_NO_PAGER` is set.
2. Spawns `$FJ_PAGER` / `$PAGER` / `less -FRX`.
3. `dup`s our stdout fd (saving it for restore) then `dup2`s the pager
   child's stdin fd onto stdout (fd 1).
4. Returns a `PagerGuard` whose `Drop` impl flushes stdout, restores
   the saved fd, closes the saved fd, and waits on the child.

This means the rest of `fj` keeps using `println!`/`print!` unmodified
even though it's now writing to the pager.

## SIGINT

`cli::run` `tokio::select!`s the command future against
`tokio::signal::ctrl_c()`. On SIGINT the command future is dropped,
which propagates to the `_pager` binding's `Drop` and tears down the
pager cleanly.

## Aliases and extensions

`main.rs` runs two passes before clap:

1. **Alias expansion** via `cli::alias::expand_argv`. Reads
   `aliases.toml`. If `argv[1]` matches a defined alias, replaces it
   with the tokenized expansion.
2. **Plugin dispatch** via `main::detect_plugin`. If `argv[1]` isn't a
   known subcommand AND `fj-<name>` is on PATH, exec it.

Only if neither pass triggers do we hand off to clap.

## Test strategy

- **Unit tests** stay inline next to the code they cover. Pure
  functions only (no I/O).
- **Integration tests** for HTTP behavior live in
  `src/client/integration_tests.rs` and use a wiremock server.
  Construct test clients via `Client::for_base_url`.
- **Live E2E** lives in `scripts/e2e-smoke.sh`, gated on `FJ_E2E=1`
  in the pre-push hook. It hits the real Forgejo at `rasterhub.com`
  against `stephen/fj-cli-test`.
