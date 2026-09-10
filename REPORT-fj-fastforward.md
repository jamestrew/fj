# fj fast-forward-only merge report

## API finding

Forgejo can express the merge style through the existing pull merge endpoint:

- Endpoint: `POST /api/v1/repos/{owner}/{repo}/pulls/{index}/merge`
- Request field: `Do`
- Accepted style value: `fast-forward-only`

Evidence checked:

- Forgejo/Gitea merge form binding was changed to allow `Do` values `merge,rebase,rebase-merge,squash,fast-forward-only,manually-merged`.
- Forgejo configuration documents `DEFAULT_MERGE_STYLE` valid options including `fast-forward-only`.
- Generated Forgejo clients expose `MergePullRequestOptionDo::FastForwardOnly`.
- Repository payloads expose `allow_fast_forward_only_merge`, separate from `allow_rebase`.

## CLI change

`fj pr merge --style fast-forward-only` now maps to `Do: "fast-forward-only"`.

The repo default merge-style path also accepts `default_merge_style: "fast-forward-only"`, so an ff-only repository does not require spelling the flag on every merge.

Repository policy diagnosis now reads `allow_fast_forward_only_merge` and includes `fast-forward-only` in the allowed-style list when that flag is true.

Fast-forward-only does not create a merge commit. `fj` therefore omits `MergeTitleField` and `MergeMessageField` for this style, does not prefetch the PR body for a default merge message, and rejects explicit `--title` or `--message` locally with:

`fast-forward-only merge does not create a merge commit, so --message is not supported`

The equivalent `--title` message names `--title`.

## Refusal behavior

If Forgejo rejects `fast-forward-only` with a non-mergeable 405 or a message containing `Not possible to fast-forward`, `fj` reports:

`PR #N is not a fast-forward onto BASE; rebase the branch onto BASE, wait for CI, then retry.`

If branch protection explicitly reports "block outdated branch", the same fast-forward-specific message is used for this style.

The command does not fall back to squash, rebase, rebase-merge, or merge.

## Verification

Focused tests:

- `env RUSTC_WRAPPER= cargo test fast_forward_only`: passed, 4 tests.
- `env RUSTC_WRAPPER= cargo test merge_style`: passed, 10 tests.

CLI help proof:

- `env RUSTC_WRAPPER= cargo run -- pr merge --help`
- Help includes possible values: `merge, rebase, rebase-merge, squash, fast-forward-only`.

Wiremock API proof:

- `pull_merge_fast_forward_only_posts_style_without_commit_message_fields` expects exactly `{"Do":"fast-forward-only"}`.
- The same test expects zero `GET /pulls/12`, proving ff-only does not fetch a merge message.
- `pull_merge_fast_forward_only_rejects_commit_message_fields_locally` expects no POST when `message` is supplied.

Local git positive proof:

- Source tip: `6411ec3f7eb102c16db345d3f99a290b3b6fcbd0`
- Target tip after `git merge --ff-only feature`: `6411ec3f7eb102c16db345d3f99a290b3b6fcbd0`
- `git rev-list --count main ^feature`: `0`
- `git rev-list --parents -n 1 main | wc -w`: `2`, meaning the tip line had commit plus one parent, not a merge commit.

Local git negative proof:

- With `main` advanced after the feature branch forked, `git merge --ff-only feature` refused.
- First stderr line: `hint: Diverging branches can't be fast-forwarded, you need to either:`

Full local pre-push:

- `env RUSTC_WRAPPER= hooks/pre-push`: passed.
- Steps covered: `scripts/preflight.sh`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all --locked --no-fail-fast`, `cargo audit --ignore RUSTSEC-2024-0370 --ignore RUSTSEC-2025-0141 --ignore RUSTSEC-2024-0436 -D warnings`, and `cargo build --release --locked`.

The first sandboxed Cargo attempts failed before or during tests because `sccache` was not permitted and wiremock could not bind localhost ports. Rerunning with `RUSTC_WRAPPER=` and elevated localhost permissions passed.
