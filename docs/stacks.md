# Stacked branches and PRs

> Status: preview. The surface is stable enough to use daily, but expect
> sharp edges and occasional changes.

`fj stack` manages a chain of dependent branches and their pull requests:
each branch builds on the one below it, each PR targets the branch below it,
and the whole chain merges bottom-up. State is tracked locally and
git-natively in `.git/fj/stack.json`. There is no server-side state and no
committed manifest, so a stack is yours alone until you push it.

## Commands at a glance

```sh
fj stack new "Login Flow"                  # author a stack and its first branch
fj stack new "Login Flow" --base develop   # base on something other than the default branch
fj stack new "Login Flow" --from issue 123 # link an issue (best-effort title)
fj stack review                            # each item: PR number, review, CI, mergeable, reviewers
fj stack review --open                      # open every PR in the browser
fj stack review --request alice            # request alice's review across the whole stack
fj stack sync                              # push branches and create/update PRs
fj stack sync --draft --dry-run            # plan only, no pushes or PR writes
fj stack ship                              # merge bottom-up behind a green-and-approved gate
fj stack ship --squash --delete-branches   # squash-merge and clean up branches
fj stack split                             # turn a single branch into one entry per commit
fj stack absorb                            # fold staged changes into the entries that introduced them
```

## new

`fj stack new` derives a safe slug and first branch (`<handle>/<slug>-1`) and
records the schema, repo, base, timestamps, linked issue, and items. It will
not clobber an existing stack without `--force`. `--no-branch` records the
stack without creating a branch, and `--json` prints the result for scripting.

## sync

`fj stack sync` pushes each branch to origin and creates or updates its PR,
chaining the bases so item _n_ targets item _n-1_'s branch and the first item
targets the stack base. It writes PR numbers and URLs back into the stack file
and is idempotent, so re-running it only does what changed. `--draft` and
`--ready` control PR state; `--dry-run` prints the plan without pushing or
writing PRs.

Once the PRs exist, sync mirrors a navigation block into each PR description:
the whole stack, linked, with the current entry marked. The block is wrapped
in `fj-stack` markers so any hand-written body around it survives a re-sync.
It is a convenience, never the source of truth, and `--no-annotate` skips it.
The block also carries a versioned JSON payload so other tools can read the
stack as data instead of scraping markdown (see
[stack-annotation.md](stack-annotation.md)).

## ship

`fj stack ship` merges the stack from the base up. It pre-flights every PR
(each must be approved and green unless `--force`), merges the bottom item,
re-points the next PR onto the trunk, and repeats. It stops on the first
failure with recovery instructions, and already-merged items are skipped on a
re-run. `--merge` (the default) and `--squash` pick the strategy,
`--delete-branches` cleans up merged branches, and `--dry-run` prints the plan
without touching anything.

## split

`fj stack split` turns a single-branch stack into one stacked entry per commit.
The _N_ commits ahead of the base become branches `<handle>/<slug>-1..N`, each
chained on the one below. It only moves local refs (no push, no API), so it is
offline and reversible. It refuses a dirty tree or a multi-entry stack, and
`--dry-run` shows the commit-to-branch mapping.

## absorb

`fj stack absorb` folds your staged changes into the entries that introduced
them, the way `git absorb` does. It attributes each staged file to the most
recent stack commit that touched it (per file), creates a `fixup!` commit per
target, then runs one autosquash `git rebase --update-refs` to rewrite the
entries in place and move every branch ref. Files with no matching entry are
left staged. It needs git 2.38 or newer, operates on the stack's top branch,
and rewrites history, so the branches need a force-push afterward. `--dry-run`
and `--json` print the attribution plan without touching anything.
