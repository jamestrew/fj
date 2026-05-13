# Contributing to fj

## Setup

```sh
git clone ssh://git@rasterhub.com:2222/rasterstate/fj.git
cd fj
./scripts/install-hooks.sh         # symlinks hooks/pre-push into .git/hooks
cargo build --release              # 4 MB binary at target/release/fj
```

The hook gates every push on fmt, clippy (warnings as errors), the full
test suite, and a release build. Set `FJ_E2E=1` to additionally run the
live API smoke (requires `fj auth login` already done).

## Workflow

1. Branch off `main`.
2. Make changes. Keep commits focused.
3. `cargo fmt --all` before committing.
4. The pre-push hook runs the full bar; if it fails, fix the issue
   rather than passing `FJ_SKIP_PREPUSH=1` unless you genuinely need
   to bypass.

## Testing

- 60 tests live in the source tree (51 unit + 9 wiremock integration).
- Unit tests stay close to the code they cover, in inline `#[cfg(test)]`
  modules.
- HTTP integration tests live in `src/client/integration_tests.rs` and
  use a wiremock server. Construct test clients via
  `Client::for_base_url(base, token)`.
- Live API tests live in `scripts/e2e-smoke.sh` and exercise read-only
  operations against the configured Forgejo. Toggle with `FJ_E2E=1`.

## Adding a subcommand

Concrete example: adding `fj sticker list`.

1. `src/api/sticker.rs` — typed API wrappers and request/response
   structs. Use the `serde_util::deserialize_null_to_default` helper
   for fields that Forgejo returns as `null` when empty.
2. `src/cli/sticker.rs` — clap `Args` / `Subcommand` definitions and
   the `run` dispatch.
3. `src/api/mod.rs` — `pub mod sticker;`
4. `src/cli/mod.rs` — `pub mod sticker;`, add to `Command`, add the
   dispatch arm in `dispatch()`.
5. `src/main.rs` — add `"sticker"` to `KNOWN_SUBCOMMANDS` so plugin
   discovery doesn't shadow it.
6. Add inline `#[cfg(test)] mod tests` for any non-trivial pure code.
7. If you touched HTTP behavior, add a wiremock test in
   `src/client/integration_tests.rs`.

## Code style

- See `CLAUDE.md` for in-tree conventions.
- One small line of comment max per non-obvious thing. Don't narrate
  what code does, only why.
- No `.unwrap()` / `.expect()` in non-test code paths. Use `?` and
  `anyhow::Error::context` to attach actionable messages.
- Prefer `Result` over panics. The one exception is `unreachable!()`
  for genuinely impossible match arms, but flag it in review.

## Releasing

```sh
cargo build --release
./target/release/fj completion zsh > ~/.zfunc/_fj
./target/release/fj man -o ./man
```

No cargo-dist or Homebrew tap yet. Binary lives at `target/release/fj`;
symlink into a user-level bin dir.

## License

MIT. By contributing you agree to the same license.
