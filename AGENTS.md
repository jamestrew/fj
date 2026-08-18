# AGENTS.md

Standing context for agent sessions working on fj's own source. This repo
already has `CLAUDE.md` (architecture, layout, build/test conventions) and
`CONTRIBUTING.md` (workflow, adding a subcommand, code style) — read those
first; this file only covers things not already there, mostly current traps
and process notes gathered from past sessions.

## Known traps, current as of writing

- **PR coverage gate is diagnostic-only, not a real gate.** `ci.yml`'s
  `coverage` job enforces `COV_MIN=71` on pushes to `main` but runs
  `COV_MIN=0` on pull requests, because CI measures a different (larger)
  region/line surface on PRs than `make coverage-strict` does locally on the
  same commit and toolchain — see open issue **#249** for the exact numbers.
  Several sessions burned hours re-discovering this by guessing (cfg flag,
  test selection, pinned nightly) before it was correctly diagnosed and
  escalated. If a PR's coverage job looks off, don't re-litigate it — it's a
  known, tracked mismatch, not something a new PR is expected to fix
  incidentally.

- **Forgejo uses negative user ids for system/bot accounts** (e.g.
  `forgejo-actions` = `-2`). Any field typed `u64` that can hold a reviewer,
  actor, or requester id will fail to decode against real data. This class
  of bug has recurred (#231, #247; #246 is a still-open variant on `pr
  list`). When touching a decode path that carries a user id, check whether
  it can observe a system/ghost account and prefer a signed type or explicit
  handling over assuming positive ids.

- **Prefer `or_else`/`ok_or_else` over `or`/`ok_or` when the fallback has a
  side effect** (keychain read, network call). `Option::or` evaluates
  eagerly, so a working `FJ_TOKEN`-only environment could still trigger an
  unnecessary (and on headless boxes, failing) keychain lookup. This caused
  a real auth regression fixed in #251; `src/client/resolve.rs` is the
  reference for the current pattern.

## Branch and merge

Branch off `main` (`fix/<issue>-<slug>` or `feat/<slug>` are the prevailing
names). The repo's `default_merge_style` is currently `squash` and recent
merged PRs are single-parent squash commits on `main` — don't infer merge
style from older history further back, which used real merge commits.

## Automated review

Every same-repo PR push gets an advisory review from Forseti (AI reviewer,
`.forgejo/workflows/forseti-review.yml`, `pull_request_target`, cancels and
re-runs on each push). It's a review comment, not a merge gate — treat its
findings like a second reviewer's, not CI.

## Session hygiene

Sessions in this repo get restarted on context limits, often mid-task, and
the operator has to reconstruct state each time. If you're running low on
context, don't leave work stranded: push whatever you have to a branch (even
an incomplete fix or an incomplete audit) and say plainly in your final
report what's done and tested versus what's untouched. A pushed branch with
an honest status beats silently losing the work.
