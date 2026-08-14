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
  stored here. They live in the auth token stores via `auth::*`: OS keychain first, with a 0600 XDG file fallback when the keychain is unavailable.
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

- The request is built once and stored as `reqwest::Request`. Each retry
  uses `Request::try_clone`.
- Idempotent methods (GET, HEAD, OPTIONS, PUT, DELETE) get 3 attempts.
- POST and PATCH get 1 attempt; never retried.
- Backoff is `200ms * 2^attempt` between retries.
- On 5xx or transport error, we retry.
- On 429 we honor the `Retry-After` header (capped at 30 s) and fall
  back to exponential backoff if the header is absent or non-numeric.

## Auto-pagination

`Client::get_all(path, query, total_limit)` follows `Link: rel=next`
until either `total_limit` items have accumulated or there are no more
pages. List commands switch to this path automatically when the user
asks for `--limit > 50` (Forgejo's per-page cap).

## Actions run logs

`fj run logs` and `fj run view --log` use Forgejo's token-authenticated
Actions API when the server exposes it. The run number in the CLI is the
per-repository number shown by `fj run list`; `fj` resolves that number through:

- `GET /api/v1/repos/{owner}/{repo}/actions/runs?page=...&limit=...`
  and matches `index_in_repo` / `run_number`.
- `GET /api/v1/repos/{owner}/{repo}/actions/runs/{internal_run_id}/jobs`
  to map the selected job index to a job id.
- `GET /api/v1/repos/{owner}/{repo}/actions/jobs/{job_id}/logs`
  to fetch the plaintext job log.

Verified on rasterhub 2026-08-14 with a PAT:

```text
GET /api/v1/repos/rasterstate/fleet/actions/runs?page=1&limit=50 -> HTTP 200; run_number=180 internal_run_id=18434
GET /api/v1/repos/rasterstate/fleet/actions/runs/18434/jobs -> HTTP 200; job_index=0 job_id=43624
GET /api/v1/repos/rasterstate/fleet/actions/jobs/43624/logs -> HTTP 200
```

If the native job-log API is unavailable, `fj` can fall back to the fleet Loki
log source when task metadata is present. Otherwise it returns an explicit
server-version error instead of pretending a cookie-gated web route is an API.

`fj run view N` without `--log`, `fj run watch`, `fj run rerun`, and
`fj run cancel` still use Forgejo's human-facing web routes:

- `fj run view N` calls `fetch_run_summary`, which POSTs the ViewPost route
  with an empty `logCursors` body to get run-level state without any log lines.
- `fj run watch` polls incremental per-step JSON:
  `POST /{owner}/{repo}/actions/runs/{run}/jobs/{job}/attempt/{attempt}`
  with body `{"logCursors":[{"step":S,"cursor":C,"expanded":true}]}`.
  An empty `{}` body returns run/job/step state with no lines, which is
  how we cheaply learn the step count before requesting every step.

How it still respects the layering rules:

- No parallel HTTP path. API log reads and web-route calls both go through
  `Client::request`. Web-route calls use absolute same-origin URLs built from
  `Client::base()`, so they still share client plumbing.
- `is_trusted_url` keeps sending credentials only when an absolute URL points
  back to the same trusted host.

Three parameter quirks worth knowing (all documented at the call site):

- `run` is the **per-repo run number** (the `#` column of `fj run list`),
  not the internal run id.
- `job` is the zero-based job position. Forgejo clamps some web-route job
  indices to the first job rather than 404ing, so `cli::workflow::validate_job`
  rejects them against the returned job count where web routes are used.
- `attempt` is 1-based. API log reads use the job's latest attempt unless the
  user pins `--attempt`.

Limitations, surfaced to the user rather than hidden:

- The job-log API is Forgejo-version dependent. If
  `/actions/runs/{id}/jobs` returns 404, `fj` names the server version and says
  the token-authenticated log API is unavailable.
- A Fjord-Account gateway only proxies `/api/v1`, so it won't forward web
  routes. API-backed `fj run logs` may work through a gateway that exposes the
  Forgejo Actions API, but `watch`, plain `view`, `rerun`, and `cancel` still
  refuse early through `cli::workflow::guard_web_routes`.
- `fj run rerun` / `cancel` use the web layer (there is no `/api/v1`
  rerun/cancel), and Forgejo rejects a PAT-authenticated POST to those
  state-changing actions with a handler 404. `post_run_action` translates
  that into a message pointing at the run URL to act on in the browser.

## Pager

Two independent decisions gate the pager.

`cli::paging::should_page(&command)` answers "should this command page at
all?" It is a pure function of the parsed command tree, so it is unit
tested without a terminal. It says yes only for genuinely long,
read-only, human-facing list and view output: `repo`/`issue`/`pr`/
`release`/`label`/`milestone`/`tag`/`run` list and view, `pr diff` /
`commits` / `files` / `checks`, `run logs`, `search`, and `fj api` GET
dumps. It says no for status and summary blocks (`fj status`,
`fj pr status`), for every write and its confirmation (which may also
prompt on the terminal we would otherwise have redirected), for short
single-value reads (`tag view`, `milestone view`, `repo topics`), and for
any machine or browser mode (`--json`, `--web`, `api --silent`).

When it says yes, `cli::run` calls
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

## JSON projection

A global `--json-fields foo,bar` flag works on top of any `--json`
output. `cli::run` reads it once and stores the parsed list in a
`OnceLock` in `output::mod.rs`. Every `output::print_json` call routes
through `json_filter::project` before printing, so individual commands
need no awareness of the filter.

Supports dotted paths (`owner.login`). Missing fields project to `null`
rather than failing, matching gh.

## Test strategy

- **Unit tests** stay inline next to the code they cover. Pure
  functions only (no I/O), with two exceptions marked `#[ignore]`:
  `editor_command_uses_visual_first` and
  `edit_text_with_true_returns_initial` mutate `$EDITOR` / `$VISUAL`,
  which races with the cargo test harness on macOS. Run them with
  `cargo test -- --ignored --test-threads=1`.
- **Integration tests** for HTTP behavior live in
  `src/client/integration_tests.rs` and use a wiremock server.
  Construct test clients via `Client::for_base_url`.
- **Live E2E** lives in `scripts/e2e-smoke.sh`, gated on `FJ_E2E=1`
  in the pre-push hook. It hits the real Forgejo at `rasterhub.com`
  against `stephen/fj-cli-test` locally. CI runs it against
  `rasterstate/fj` with an ephemeral `FJ_TOKEN`, so runner containers
  do not need keychain setup. Admin-only checks, such as branch protection
  listing, are opt-in with `FJ_E2E_ADMIN=1`.
- **Remote CI** at `.forgejo/workflows/ci.yml` runs the same gate
  (fmt, clippy `-D warnings`, test, audit, release build, coverage,
  and live smoke) on every push and PR. The local hook is no longer
  the only gate.
