# Changelog

All notable changes will be recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `tokio::signal::ctrl_c()` race in `cli::run` so the pager guard drops
  cleanly on SIGINT.
- 9 wiremock-backed HTTP client integration tests covering retry
  behavior, header forwarding, pagination, and panic-free error paths.
- `Client::for_base_url` test constructor pointing at an arbitrary URL.

### Changed

- Trimmed dependencies (no more `indicatif`, `futures-util`,
  `is-terminal`, `textwrap`, `tempfile`). Dropped reqwest features we
  don't use (`stream`, `brotli`). Release profile now uses `lto = "fat"`
  and `panic = "abort"`.
- HTTP retry loop builds the request once and clones via
  `reqwest::Request::try_clone` per attempt (was rebuilding the full
  RequestBuilder each time).
- Binary size: 5.94 MB → 4.15 MB stripped (-30%).

### Fixed

- Removed the unsafe `std::env::set_var("FJ_NO_PAGER")` from dispatch.
  The `--no-pager` flag is now threaded into `pager::maybe_start`.
- Replaced the panicking `.expect("token contains invalid header chars")`
  in `auth_headers` with a typed error that tells the user how to
  recover.

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
