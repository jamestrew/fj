# fj: project notes for Claude

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
    integration_tests.rs  wiremock-based tests (a single file, not a dir)
  config/              hosts.toml shape + load/save
  auth/                OS keychain wrapper
  git/                 git invocation + remote URL parser
  output/              tables, colors, relative_time, state_pill, pager,
                       json_filter (--json-fields projection)
.forgejo/workflows/    CI mirror of the pre-push hook
hooks/pre-commit       fast local gate
hooks/pre-push         local CI gate
scripts/               install-hooks.sh, e2e-smoke.sh
docs/                  architecture, jq, gh-to-fj, faq, troubleshooting
```

## Build / test / hook

- `cargo build --release` produces a ~4 MB binary at `target/release/fj`.
- `cargo test --all` runs ~410 tests (unit + wiremock integration).
  2 tests are `#[ignore]` (env-mutating, racy with cargo's harness).
- `cargo clippy --all-targets --all-features -- -D warnings` is the lint
  bar. Both the pre-push hook and `.forgejo/workflows/ci.yml` treat
  warnings as errors.
- `cargo audit --ignore RUSTSEC-2024-0370 -D warnings` is the dependency
  security gate. The ignored advisory is the known unmaintained
  `proc-macro-error` transitive dependency; new warnings fail locally and
  in CI.
- `make coverage` reports line/region coverage (needs `cargo-llvm-cov`),
  scoped to the surface we intend to unit-test. The exclusion set lives
  in `COV_IGNORE` in the Makefile: entry/dispatch wiring, platform glue,
  and thin command-handler modules. Don't chase coverage on those; add a
  new glue file to `COV_IGNORE` if it has no `#[cfg(test)]` module.
  `make coverage-html` opens a browsable report. A test
  (`cli::tests::cov_ignore_stays_in_sync_with_test_modules`) fails if
  `COV_IGNORE` drifts from which `cli/*.rs` files carry tests.
- `make coverage-strict` (needs a nightly toolchain + `llvm-tools-preview`)
  additionally drops the untested async command handlers inside the mixed
  command modules, via `#[cfg_attr(coverage_nightly, coverage(off))]` on
  every cli `async fn` (inert on stable). It's the intent-precise number
  (~89% lines). When you add a cli `async fn` handler, keep the attribute
  on it. CI enforces `COV_MIN=88`, about a point under the observed number,
  and the runner and a local run measure the same surface. The target starts
  with a full `cargo llvm-cov clean`, not `clean --workspace`: stale
  instrumented objects in a restored `target/` are counted as a second,
  unexecuted copy of the code and drag the number down (fj#249). Local runs
  need cargo-llvm-cov 0.9 or newer; 0.8 misses the test binary under the
  pinned nightly's build layout and reports about 2%.
- `./scripts/install-hooks.sh` symlinks repo hooks into `.git/hooks`.
- `FJ_SKIP_PRECOMMIT=1` bypasses the fast hook for genuine emergencies only.
- `FJ_E2E=1` enables the live API smoke against `stephen/fj-cli-test`.
  It read-exercises the command handlers that `COV_IGNORE` excludes from
  unit coverage (label, milestone, workflow, run, search, …), so keep a
  read-only invocation there for any new excluded command surface.
  CI also runs this smoke against `rasterstate/fj` using `FJ_TOKEN` as an
  ephemeral PAT fallback, which avoids keychain setup in containers.
  Admin-only checks such as `fj protect list` are opt-in with
  `FJ_E2E_ADMIN=1`.
- `FJ_SKIP_PREPUSH=1` bypasses the hook for genuine emergencies only.

## Key conventions

- Every repo-scoped subcommand accepts `-R/--repo` AND auto-detects from
  the git remote (`upstream` then `origin`). The resolver is
  `cli::context::resolve_repo`.
- Bodies (`--body`) support: an inline string, `-` for stdin, or omit to
  open `$EDITOR`. See `cli::editor`.
- Lists with `--limit > 50` transparently follow `Link: rel=next` via
  `Client::get_all`. Per-page cap is the Forgejo default (50).
- HTTP retry: GET/HEAD/OPTIONS/PUT/DELETE get 3 attempts with 200/400/800
  ms exponential backoff. 429 honors `Retry-After` (capped 30 s). POST
  and PATCH are never retried.
- The jq-ish path engine lives in `output::json_path::extract_path`.
  Supports `.field`, `.0`, `.[0]`, `[-1]`, and `|` pipes. See `docs/jq.md`.
  Two consumers share it: `fj api --jq` (via `cli::api`) takes the
  `Result` so a bad path errors; `--json-fields` maps a miss to `null`.
- `--json-fields foo,bar` is a GLOBAL flag that applies gh-style
  projection to ANY `--json` output. Lives in
  `output::json_filter::project`, set from `cli::run`, read by
  `output::print_json`. It resolves each field through
  `output::json_path` (null on miss).
- All commands respect `--host` / `FJ_HOST`, `--debug` / `FJ_DEBUG`,
  `--no-pager`, `FJ_PAGER` / `PAGER`, `--json-fields`.
- The pager is Unix-only. `output::pager::imp` is gated `#[cfg(unix)]`
  with a no-op stub for other platforms.

## Things to remember

- Tokens are stored in the OS keychain (service `fj`, key = hostname).
  When the keychain is unavailable, or `FJ_NO_KEYCHAIN=1` is set, they
  fall back to `tokens.toml` in the config dir with 0600 permissions
  (`auth::token_file_path`). The keychain pins "Always Allow" to the
  binary's code signature; an ad-hoc signature (what `cargo build`
  emits) is pinned to the cdhash, which changes every rebuild, so the
  prompt keeps coming back. Fix: `make install` puts a real copy on
  PATH and signs it with a stable Developer ID identity, so trust
  survives rebuilds (click "Always Allow" once after the first signed
  install). Don't symlink `~/.local/bin/fj` to `target/release/fj`;
  that re-introduces the per-build churn.
- `cargo test` builds a separate test binary with a different hash, so
  it gets its own keychain prompt. Tests that need keychain access are
  the wiremock-backed ones in `src/client/integration_tests.rs`, which
  bypass the keychain via `Client::for_base_url`.
- Ctrl+C has two mechanisms, in `crate::interrupt`. `cli::run` selects the
  command future against `ctrl_c()` (graceful: destructors run), and a
  spawned backstop force-exits 130 if that select is still wedged 250 ms
  later. The select alone is not enough because the keychain and the
  synchronous stdin reads park the `block_on` thread in a syscall, and
  tokio has already taken SIGINT's default disposition away. If you add a
  `Command::status()` that inherits our stdio, wrap it in
  `interrupt::ForegroundChild::enter()` so the backstop stands down and the
  child gets to interpret its own SIGINT. Nothing on the shutdown path may
  run on the blocking pool: dropping the runtime waits for `spawn_blocking`
  tasks, and that wait is the original hang (fj#257). `spawn_blocking` is
  still the right tool elsewhere.
- `fj auth setup-git` interpolates the hostname into a shell-evaluated
  credential helper string. The host is validated against a strict DNS
  pattern (`validate_hostname` in `cli/auth.rs`) before interpolation.
  Don't bypass that validation if you refactor.
- `fj auth token` and `auth status --show-token` refuse to write a
  plaintext token to a TTY by default (use `--force`).
- The repo URL in `Cargo.toml` is `https://rasterhub.com/rasterstate/fj`.
- The user's CLAUDE.md preferences: no em-dashes, terse responses, no
  preamble. They handle their own git author identity locally; don't
  hardcode or assume any specific author in scripts.
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
