<!--
Thanks for the PR. Before you submit:

1. `cargo fmt --all`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --all`
4. If you added a new subcommand or API wrapper, see CONTRIBUTING.md
   for the walkthrough; also update README.md and docs/gh-to-fj.md if
   the user-facing surface changed.
-->

## What

<!-- One sentence describing the change. -->

## Why

<!-- The motivation. Link any related issue. -->

## How

<!-- Implementation notes. Specific decisions you made that a reviewer
should know about. Skippable for one-liners. -->

## Test plan

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo test --all`
- [ ] New code has a test (or here's why not):

## Surface changes

<!-- If you added/changed/removed a flag, subcommand, or output format,
list it here so the changelog entry writes itself. -->

- [ ] README.md updated (if the command table changed)
- [ ] docs/gh-to-fj.md updated (if the gh-equivalence shifted)
- [ ] CHANGELOG.md updated under `[Unreleased]`
