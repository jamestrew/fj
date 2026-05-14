# Changelog

All notable changes will be recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2] — 2026-05-14

### Added

- **Code-signed macOS binaries.** Both `darwin-aarch64` and
  `darwin-x86_64` release tarballs now ship binaries signed with the
  Raster & State Developer ID (`62Y3FRM8PD`), hardened-runtime
  enabled. macOS keychain ACLs persist across version bumps, so a
  user who clicks "Always Allow" once doesn't get re-prompted for
  every release. (Notarization is a follow-up; signing alone solves
  the prompt-storm.)
- `scripts/sign.sh`: signs `target/release/fj` (or any path you pass)
  with the Developer ID. Reads the identity from `.env` /
  `.env.local`, falling back to the value in `.env.example`. Use
  this after every local `cargo build --release` so your symlinked
  dev binary keeps the same code identity across rebuilds.
- `scripts/preflight.sh` + integration into the pre-push hook. Two
  checks: (1) refuses to proceed if any `.env*` file (other than the
  example) contains non-empty values for sensitive keys or
  keystore-path-shaped values; (2) git-greps the working tree for
  PEM markers, AWS/GitHub/Slack tokens, and Apple app-specific
  password format (anchored to value position so dashed identifiers
  in `Cargo.lock` don't false-positive).
- `.env.example` documenting the variables release/signing scripts
  read (`FJ_APPLE_TEAM_ID`, `FJ_APPLE_DEVELOPER_ID`,
  `FJ_NOTARY_PROFILE`, `FJ_RELEASE_HOST`, `FJ_E2E_REPO`).
  Notarization passwords stay in the OS keychain, never in `.env`.

### Changed

- `Cargo.toml` `authors` is now `"Raster & State <support@rasterstate.com>"`.
- All in-tree references to `hello@rasterstate.com` switched to
  `support@rasterstate.com` (marketplace.json, plugin.json,
  CLAUDE.md). Personal-email references removed from tracked files.

## [0.1.1] — 2026-05-14

### Fixed

- **`fj auth setup-git` installed a broken credential helper.** Git
  invokes the helper via `sh -c '<helper> "$@"' sh <verb>`, which
  meant `sed 's/^/password=/'` would see `get` / `store` / `erase` as
  a filename and bail with `sed: get: No such file or directory`. Every
  `git push` and `git pull` against the configured host failed
  ([#1](https://rasterhub.com/rasterstate/fj/issues/1)). The helper is
  now wrapped in a shell function `f() { … }; f` that ignores its
  positional args. Three regression tests cover the structural shape,
  the verb-positional simulation, and a real `sh -c` invocation with a
  stub `fj` on PATH.

  **Recovery on already-affected machines:** re-run `fj auth setup-git
  --host <hostname>`.

## [0.1.0] — 2026-05-13 — distribution & open-source ready

### Added (distribution + open-source ready)

- `LICENSE` (MIT) at the repo root. Cargo.toml had always declared MIT;
  the file was just missing.
- `.forgejo/workflows/release.yml`: on `v*` tags, builds for
  `darwin-aarch64`, `darwin-x86_64`, `linux-x86_64`; uploads tarballs,
  `SHA256SUMS`, and a pre-rendered `fj.rb` to the Forgejo release.
- Homebrew formula template at `dist/homebrew/fj.rb.tmpl` and
  `scripts/render-homebrew-formula.sh` to fill SHA256s post-release.
  Publishes into the existing `rasterandstate/homebrew-tap`.
- `.forgejo/issue_template/{bug,feature,api-gap}.md` and
  `.forgejo/pull_request_template.md` to keep triage cheap.
- `scripts/record-demo.sh` + `scripts/_demo-session.sh` to record an
  asciinema demo; README has a placeholder embed for the v0.1.0 cast.
- `docs/compatibility.md`: tested Forgejo versions, caveats for older
  Gitea, and Forgejo-only endpoints.
- One-shot version probe during `fj auth login` that warns when the
  server reports a pre-7.x version (with tests for the parser).

### Added (agent-focused Forgejo gaps)

- `fj issue edit-comment` / `delete-comment`. Lets an agent (or you)
  fix or remove a wrong comment after the fact.
- `fj search code "..."` (and `-R owner/name` to scope to one repo).
  Powered by Forgejo's `/repos/search/code` endpoint.
- `fj pr request-review N user1 user2,user3` and
  `fj pr unrequest-review N user1`. Distinct from `pr review`, which
  submits your own review.
- `fj repo watch` / `unwatch` / `star` / `unstar` / `starred`.
- `fj milestone {list,view,create,edit,close,reopen,delete,assign}`.
  Includes `fj milestone assign N --milestone ID|none` to attach an
  issue or PR to a milestone.

### Added (UX + stability)

- `--json-fields field1,field2` global flag. gh-style projection on top
  of any `--json` output, with dotted-path support
  (`--json-fields owner.login,id`).
- 429 / Retry-After honored in the retry loop with a 30 s cap. Wiremock
  test added.
- `did you mean` suggestions on typo'd subcommands via clap's
  `suggestions` feature.
- `fj auth token` and `fj auth status --show-token` now refuse to write
  to a TTY (use `--force` to override). Avoids accidental shoulder-
  surfing or capture in shell history.
- `tokio::signal::ctrl_c()` race in `cli::run` so the pager guard drops
  cleanly on SIGINT.
- 10 wiremock-backed HTTP client integration tests covering retry
  behavior (5xx, 429), header forwarding, pagination, and panic-free
  error paths.
- `Client::for_base_url` test constructor pointing at an arbitrary URL.
- `.forgejo/workflows/ci.yml` runs the same gate as the pre-push hook
  on every push and PR.

### Added (docs)

- `SECURITY.md` covering threat model, known sharp edges, and reporting.
- `docs/gh-to-fj.md` — complete command-by-command mapping.
- `docs/faq.md` — common questions about tokens, hosts, debug,
  scripting, plugins.

### Changed

- Trimmed dependencies (no more `indicatif`, `futures-util`,
  `is-terminal`, `textwrap`, `tempfile` in prod). Dropped reqwest
  features we don't use (`stream`, `brotli`). Release profile uses
  `lto = "fat"` and `panic = "abort"`.
- HTTP retry loop builds the request once and clones via
  `reqwest::Request::try_clone` per attempt.
- Binary size: 5.94 MB → 4.15 MB stripped (-30%).

### Fixed

- **Shell injection** in `fj auth setup-git`. The hostname now must
  match a strict DNS pattern before being interpolated into the
  credential-helper string, and we call `git config` directly with
  separate args instead of going through `sh -c`.
- **Pager won't compile on Windows**. The libc-based `dup2` redirect
  now lives behind `#[cfg(unix)]`; non-Unix gets a no-op stub that
  returns `None` from `maybe_start`.
- Removed the unsafe `std::env::set_var("FJ_NO_PAGER")` from dispatch.
  `--no-pager` is now threaded into `pager::maybe_start(force_disabled)`.
- Replaced the panicking `.expect("token contains invalid header chars")`
  in `auth_headers` with a typed error.

## 0.1.0 — 2026-05-13

Initial release. Multi-host Forgejo CLI with feature parity to `gh`
across the surface Forgejo exposes. Commands:

- `auth`: login, status, logout, list, switch, token, refresh, setup-git
- `repo`: list, view, clone, create, fork, sync, edit, rename, archive,
  unarchive, delete, branches, topics, mirror, mirror-sync
- `issue`: list, view, create, edit, close, reopen, comment, develop
- `pr`: list, view, create, edit, diff, commits, files, checks, ready,
  review, status, checkout, merge, close, reopen
- `release`: list, view, create, edit, delete, upload, download
- `label`, `run`, `secret`, `variable`, `search`, `browse`, `status`,
  `org`, `ssh-key`, `gpg-key`, `alias`, `config`, `protect`, `hook`,
  `extension`, `gist`, `api`, `completion`, `man`

Other highlights:

- Repo auto-detection from `upstream` / `origin` git remote.
- `--web` flag on all list/view subcommands.
- `$EDITOR` integration for body inputs.
- `fj api` with `-H`, `-X`, `-f`, `-F`, `--paginate`, `--include`,
  `--silent`, `--jq` (dot-paths, `[N]`/`[-N]`, pipes).
- `--debug` / `FJ_DEBUG` request logging.
- Tokens in the OS keychain.
- Pager via `dup2` redirect to `$FJ_PAGER` / `$PAGER` / `less -FRX`.
- Pre-push hook running fmt, clippy `-D warnings`, tests, and release
  build before any push. Live API smoke gated on `FJ_E2E=1`.
