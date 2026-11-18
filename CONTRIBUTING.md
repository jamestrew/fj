# Contributing to fj

## Setup

```sh
git clone ssh://git@rasterhub.com:2222/rasterstate/fj.git
cd fj
./scripts/install-hooks.sh         # symlinks repo hooks into .git/hooks
cargo build --release              # 4 MB binary at target/release/fj
```

The pre-commit hook runs quick preflight and fmt checks. The pre-push hook
gates every push on secret preflight, fmt, clippy (warnings as errors), the
full test suite, `scripts/audit.sh` (cargo audit plus the waiver expiry check), and a release build. Set `FJ_E2E=1` to
additionally run the live API smoke (requires `fj auth login` already done).
CI runs the same read-only live smoke against `rasterstate/fj` with an
ephemeral `FJ_TOKEN`, so the normal PR gate covers command handlers that
need a real Forgejo API.

## Workflow

1. Branch off `main`.
2. Make changes. Keep commits focused.
3. `cargo fmt --all` before committing.
4. Sign off every commit (see below).
5. The pre-commit hook catches quick issues before the commit is created.
6. The pre-push hook runs the full bar; if it fails, fix the issue
   rather than passing `FJ_SKIP_PREPUSH=1` unless you genuinely need
   to bypass.

### Sign-off (DCO)

Every commit carries a `Signed-off-by:` line with your real name and an
email you can be reached at, added with `git commit -s`:

```
Signed-off-by: Jane Doe <jane@example.com>
```

By signing off you certify the [Developer Certificate of Origin](https://developercertificate.org/):
you wrote the change or have the right to submit it under this project's
license. Commits without the line will be asked to add it. On a branch that
only you have pushed (your own fork's PR branch), replay the commits with the
line and update the branch:

```sh
git rebase --force-rebase --signoff main
git push --force-with-lease
```

`--force-rebase` matters: without it git skips commits it does not need to
move, and they stay unsigned. Signing a commit cryptographically (GPG or SSH)
is a separate thing and optional for contributors.

## Testing

- Before opening a PR, run what CI runs, with the Rust toolchain in
  `rust-version` (1.95):

  ```sh
  cargo fmt --all -- --check
  cargo clippy --all-targets --all-features -- -D warnings
  cargo test --all --locked
  ```

  CI also runs `scripts/audit.sh` (`cargo install cargo-audit` first if you
  want it locally) and a release build. A full `cargo test` takes a while on
  a cold build; `cargo test <name>` runs one test.
- Unit tests stay close to the code they cover, in inline `#[cfg(test)]`
  modules.
- Tests under `tests/` run the real `fj` binary as a subprocess. Give each
  one its own config directory so it never reads or writes your real
  `hosts.toml`: set `FJ_CONFIG_DIR` on the `Command`, as
  `tests/support/account_override.rs` does
  (`.env("FJ_CONFIG_DIR", dir.join("fj"))`), and `FJ_NO_KEYCHAIN=1` so the
  test never touches the OS keychain. Setting `XDG_CONFIG_HOME` alone is not
  enough: macOS ignores it, so the binary would read your real config.
  Linux honours either, so set `FJ_CONFIG_DIR` and the test behaves the same
  on both.
- HTTP integration tests live in `src/client/integration_tests.rs` and
  use a wiremock server. Construct test clients via
  `Client::for_base_url(base, token)`.
- Live API tests live in `scripts/e2e-smoke.sh` and exercise read-only
  operations against the configured Forgejo. Toggle locally with `FJ_E2E=1`.
  In CI, `FJ_TOKEN` is used as an ephemeral PAT fallback so no keychain is
  needed in the runner container. Admin-only coverage, such as branch
  protection listing, is opt-in with `FJ_E2E_ADMIN=1`.

## Adding a subcommand

Concrete example: adding `fj sticker list`.

1. `src/api/sticker.rs`: typed API wrappers and request/response
   structs. Use the `serde_util::deserialize_null_to_default` helper
   for fields that Forgejo returns as `null` when empty.
2. `src/cli/sticker.rs`: clap `Args` / `Subcommand` definitions and
   the `run` dispatch.
3. `src/api/mod.rs`: `pub mod sticker;`
4. `src/cli/mod.rs`: `pub mod sticker;`, add to `Command`, add the
   dispatch arm in `dispatch()`.
5. `src/main.rs`: add `"sticker"` to `KNOWN_SUBCOMMANDS` so plugin
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
