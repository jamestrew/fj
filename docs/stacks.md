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
fj stack ship --squash --delete-branches   # squash-merge, restack the rest, clean up branches
fj stack ship --squash --no-restack        # squash-merge without replaying the branches above
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

`fj stack ship` merges the stack from the base up. `--merge` (the default) and
`--squash` pick the strategy, `--delete-branches` cleans up merged branches,
and `--dry-run` prints the plan without touching anything.

Ship is a client-orchestrated sequence, not an atomic operation. There is no
all-or-nothing land: it makes one merge call per item, and a failure part-way
leaves the items below it merged. Everything below is about making that
sequence correct and making it stop cleanly, not about hiding that.

**Before anything merges** ship pre-flights twice.

- *The stack's shape, in git.* Each unmerged branch must descend from the one
  below it, the lowest unmerged branch must contain the trunk's current tip
  (fetched, not the local copy), and each branch's local tip must match the
  head of the PR that would merge. If any of that doesn't hold, ship refuses
  and tells you to restack and run `fj stack sync`, rather than discovering it
  after the bottom item has already landed.
- *The review state.* Every PR must be approved and green unless `--force`.

**After each item lands**, with a strategy that rewrites history (`--squash`),
ship replays the rest of the stack onto the new trunk tip: one
`git rebase --onto <new trunk> <landed tip> <top branch> --update-refs`, so
every branch ref in between moves in a single pass, then a force-push per
branch with an explicit `--force-with-lease`, then the PR bases are re-synced
(the item directly above the landed one moves to the trunk, the rest keep
targeting the branch below them). Without this the branches above are still
parented on the pre-squash commit, their PRs show the landed diff a second
time, and you have to repair the stack by hand. If the replay conflicts, the
rebase is rolled back, nothing is force-pushed, and ship stops with the
commands to finish by hand.

The PR directly above the landed item still moves to the trunk even then. The
item below it has merged, so the branch that PR was targeting is going away
whatever happened to the replay, and a PR pointed at a deleted branch is a
worse end state than the one `--no-restack` would have produced. Only the
replay is rolled back; the retarget is not. If instead a force-push or a
re-point fails partway through the remainder, the branches already pushed stay
pushed and the error names what is left to finish by hand.

A merge-commit land (the default) leaves the original commits reachable from
the trunk, so nothing has to be replayed; only the next PR's base moves.

The replay needs git 2.38 or newer (for `rebase --update-refs`) and the
stack's branches in your clone. `--no-restack` skips it, along with the local
pre-flight that supports it: ship then merges through the API alone and leaves
the branches above each squash-landed item for you to rebase and force-push.

**Between merges**, once something has landed, ship re-checks the next item
before merging it: still open, still mergeable, still approved, and still at
the head fj most recently put there. If the ground shifted (someone pushed to
the branch, a review flipped, the merge stopped being clean) it stops cleanly,
says what already landed, and leaves the rest for a later run. Re-running is
safe: merged items are skipped.

Forgejo refreshes a PR's head and re-computes its mergeability asynchronously,
so just after a restack it can still be serving the pre-restack head. That is a
lagging read, not somebody else's push, and ship treats the two differently: a
head fj itself superseded is re-read a few times with a short backoff, while a
head fj never pushed stops the run immediately. Nothing on a stale record is
judged in the meantime, because its `mergeable` was computed for the old head
against the base that has since merged away. If the forge never catches up
within the retry budget, ship stops rather than merging on a snapshot it knows
is out of date.

One deliberate exception: when ship has just force-pushed a branch itself, the
CI it re-armed is not re-gated, because the tree was already approved and
green before the replay and the pending state is ship's own doing. Waiting for
those re-armed checks is what `--auto` will be for.

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
