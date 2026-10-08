# `gh` → `fj` quick reference

fj's command surface tracks `gh` closely. This table maps the commands
you reach for most. Where the semantics diverge, the difference is
called out.

## Auth

| `gh`                     | `fj`                        | Notes                                                |
| ------------------------ | --------------------------- | ---------------------------------------------------- |
| `gh auth login`          | `fj auth login`             | Multi-host; first run asks Fjord Account vs. server. |
| `gh auth status`         | `fj auth status`            | Tokens prefer OS keychain; headless fallback is a 0600 XDG file. |
| `gh auth logout`         | `fj auth logout`            |                                                      |
| `gh auth refresh`        | `fj auth refresh`           | Re-verifies (or replaces) the stored token.          |
| `gh auth token`          | `fj auth token`             | Refuses to write to a TTY without `--force`.         |
| `gh auth setup-git`      | `fj auth setup-git`         | Installs a credential helper.                        |
| (none)                   | `fj auth proxy`             | Credential for a reverse proxy with HTTP Basic auth. |
| `gh auth switch`         | `fj auth switch`            | Picks the default host for `--host`-less commands.   |

## Repos

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh repo list`                    | `fj repo list`                    |                                                 |
| `gh repo view OWNER/NAME`         | `fj repo view OWNER/NAME`         | Auto-detects from `git remote` when omitted.    |
| `gh repo clone OWNER/NAME`        | `fj repo clone OWNER/NAME`        |                                                 |
| `gh repo create NAME`             | `fj repo create NAME`             | `--clone` to clone after creating.              |
| `gh repo fork`                    | `fj repo fork`                    |                                                 |
| `gh repo sync`                    | `fj repo sync`                    | Needs `merge-upstream`; absent on some builds (clear error if so). |
| `gh repo edit`                    | `fj repo edit`                    |                                                 |
| `gh repo rename`                  | `fj repo rename`                  |                                                 |
| `gh repo archive`                 | `fj repo archive`                 |                                                 |
| `gh repo delete`                  | `fj repo delete`                  | Requires typing the slug to confirm.            |
| (n/a)                             | `fj repo branches`                |                                                 |
| (n/a)                             | `fj repo topics --set a,b,c`      | Replaces the set; prints the confirmed list.    |
| `gh repo edit --add-topic a`      | `fj repo topics add a b`          | Keeps the other topics; prints the confirmed list. |
| `gh repo edit --remove-topic a`   | `fj repo topics remove a`         | Keeps the other topics; prints the confirmed list. |
| (n/a)                             | `fj repo commit list`             | `--ref` / `--branch` / `--tag` and `--path`; shows each commit's signature state when the forge reports one. |
| (n/a)                             | `fj repo commit view SHA`         | Signature state and signer for one commit.      |
| (n/a)                             | `fj repo contents [PATH]`         | Reads a file or directory at `--ref` / `--branch` / `--tag`. A tag is read-only and pinned to its commit. |
| `gh repo list-collaborators`      | `fj repo collaborator list`       | One permission lookup per row; `unknown` when the forge will not disclose it. |
| (n/a)                             | `fj repo collaborator add USER --permission write` | Adds or updates access; prints the confirmed permission. |
| (n/a)                             | `fj repo collaborator remove USER` | Revokes repository access.                     |
| (n/a)                             | `fj repo transfer request OWNER`  | Reviews the receiving owner first; reports moved or pending. |
| (n/a)                             | `fj repo transfer list`           | Pending transfers to you or your orgs. No forge-wide listing exists; see `--help`. |
| (n/a)                             | `fj repo transfer view\|accept\|reject -R owner/name` | Inspect or answer one pending transfer. |
| (n/a)                             | `fj repo mirror <source-url>`     | Forgejo migrate/mirror endpoint.                |
| `gh repo set-default`             | `fj repo set-default owner/name`  | Pins `fj.resolved` in local git config; `--view`/`--unset`. |
| `gh repo deploy-key list`         | `fj repo deploy-key list`         |                                                 |
| `gh repo deploy-key add KEY`      | `fj repo deploy-key add TITLE --from-file KEY` | Read-only unless `--allow-write`.  |
| `gh repo deploy-key delete ID`    | `fj repo deploy-key delete ID`    |                                                 |

## Issues

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh issue list`                   | `fj issue list`                   | Supports `--milestone` and `--mentioned`. `--sort` takes Forgejo's orders: `recentupdate`, `leastupdate`, `latest`, `oldest`, `mostcomment`, `leastcomment`, `nearduedate`, `farduedate`. |
| `gh issue view N`                 | `fj issue view N`                 |                                                 |
| `gh issue create`                 | `fj issue create`                 | Omit `--body` to open `$EDITOR`; `--template [NAME]` seeds repo templates; `--milestone TITLE\|ID` files it under a milestone in the same request. |
| `gh issue edit N`                 | `fj issue edit N`                 |                                                 |
| `gh issue close N`                | `fj issue close N`                |                                                 |
| `gh issue reopen N`               | `fj issue reopen N`               |                                                 |
| `gh issue delete N`               | `fj issue delete N`               | Requires repo admin. Confirm with `-y`.         |
| `gh issue pin N` / `unpin N`      | `fj issue pin N` / `unpin N`      |                                                 |
| `gh issue lock N` / `unlock N`     | `fj issue lock N` / `unlock N`    | `lock` accepts `--reason off-topic|too-heated|resolved|spam`. |
| `gh issue comment N --body B`     | `fj issue comment N --body B`     | New: also `fj issue edit-comment ID`.           |
| `gh issue develop N`              | `fj issue develop N`              | Branches off the default branch.                |
| (n/a)                             | `fj issue reaction add\|remove\|toggle REACTION N` | `+1 -1 laugh hooray confused heart rocket eyes`. `--comment ID` reacts to a comment; `reaction list N` shows the tally. N may be a pull request. |
| (n/a)                             | `fj issue dependency list\|add\|remove N [REF]` | Lists what N depends on and what it blocks. `add N REF` makes N depend on REF; `--blocks` makes N block REF. REF is `12`, `#12`, or `owner/repo#12`. N may be a pull request. |
| (n/a)                             | `fj issue time list\|add\|delete N` | Tracked time: `time add N 1h30m`, `time delete N ID`. The total is summed from the forge's entries. N may be a pull request. |

## Pull requests

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh pr list`                      | `fj pr list`                      | Supports `--base`, `--head`, `--draft`, `--no-draft`. |
| `gh pr view N`                    | `fj pr view N`                    | `--comments` shows reviews + issue comments.    |
| `gh pr create`                    | `fj pr create`                    | Head defaults to current branch; `--template [NAME]` seeds repo templates. |
| `gh pr create --fill`             | `fj pr create --fill`             | Title and body from the commits in `base..head`; `--fill-first` uses only the first commit. `--title` / `--body` / `-F` override. |
| `gh pr edit N`                    | `fj pr edit N`                    | `--attach FILE` uploads files and links them in the body. |
| _(no equivalent)_                 | `fj pr attach N FILE...`          | Uploads attachments and appends links to the description. |
| `gh pr diff N`                    | `fj pr diff N`                    | Raw unified diff.                               |
| `gh pr checks N`                  | `fj pr checks N`                  | A `REQUIRED` column marks the checks the base branch's rule requires (`?` when the rule cannot be read), and a required check that has not reported gets its own row. |
| `gh pr ready N`                   | `fj pr ready N`                   |                                                 |
| `gh pr comment N --body B`        | `fj pr comment N --body B`        | Also `fj pr comment edit ID` / `delete ID`, and `--edit-last` / `--create-or-edit` for idempotent status comments. |
| `gh pr update-branch N`           | `fj pr update-branch N`           | `--style merge` (default) or `rebase`.          |
| `gh pr review N --approve`        | `fj pr review N --event approve`  |                                                 |
| `gh pr review N --request-changes`| `fj pr review N --event request-changes` |                                          |
| (no equivalent; `gh api` only)    | `fj pr review N --line-comment PATH LINE BODY` | Comment on one diff line. Repeat the flag to batch several into the same review as `--event`; without `--event` they post on their own. `LINE` is `N`/`new:N` (the PR's version) or `old:N` (the base version). `--line-comments-file FILE` reads a JSON array instead. Single lines only. |
| `gh pr status`                    | `fj pr status`                    |                                                 |
| `gh pr checkout N`                | `fj pr checkout N`                |                                                 |
| `gh pr merge N`                   | `fj pr merge N`                   | `--style merge|rebase|rebase-merge|squash|fast-forward-only`. A required check that is failing, pending or not reported, or a missing required approval, is explained before the merge is attempted. There is no override flag. |
| `gh pr merge N --auto`            | `fj pr merge N --auto`            | Schedules a server-side merge once required checks pass; merges now if already green. Honors `--style` / `--title` / `--message` / `--delete-branch`. |
| `gh pr merge N --disable-auto`    | `fj pr merge N --disable-auto`    | Cancels a pending scheduled merge.              |
| `gh pr lock N` / `unlock N`        | `fj pr lock N` / `unlock N`       | `lock` accepts `--reason off-topic|too-heated|resolved|spam`. |
| `gh pr close N`                   | `fj pr close N`                   |                                                 |
| `gh pr reopen N`                  | `fj pr reopen N`                  |                                                 |
| (n/a in gh)                       | `fj pr request-review N USER USER` | New: assign specific reviewers.               |
| (n/a in gh)                       | `fj pr unrequest-review N USER`   | New: remove a reviewer request.                 |

## Releases

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh release list`                 | `fj release list`                 |                                                 |
| `gh release view TAG`             | `fj release view TAG`             |                                                 |
| `gh release create TAG`           | `fj release create TAG`           | `--asset path` repeatable.                      |
| `gh release upload TAG file`      | `fj release upload TAG file`      |                                                 |
| `gh release download TAG`         | `fj release download TAG`         |                                                 |
| `gh release delete TAG`           | `fj release delete TAG`           |                                                 |
| `gh release delete-asset TAG A`   | `fj release delete-asset TAG A`   | `A` is the asset name. Confirm with `-y`.       |

## Labels & milestones

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh label list`                   | `fj label list`                   | `--org ORG` lists an organization's labels instead of a repo's. |
| `gh label create NAME --color HEX` | `fj label create NAME --label-color HEX` | `--color` is the global ANSI flag. `--exclusive` with a `group/name` label; `--org ORG` for an organization label. |
| `gh label edit NAME`              | `fj label edit NAME`              | `--label-color`, `--exclusive` / `--no-exclusive`, `--org ORG`. |
| `gh label delete NAME`            | `fj label delete NAME`            | Type the name to confirm, or `-y`. `--org ORG` for an organization label. |
| `gh label clone SRC`              | `fj label clone SRC`              | Copies labels from `SRC`; `--force` overwrites. |
| (n/a)                             | `fj milestone list`               | New: Forgejo milestones.                        |
| (n/a)                             | `fj milestone create TITLE`       |                                                 |
| (n/a)                             | `fj milestone assign N --milestone ID` | Assign an issue/PR to a milestone.         |

## Workflows & secrets

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh run list`                     | `fj run list`                     | Forgejo Actions.                                |
| `gh run view N`                   | `fj run view N`                   | `N` is the run number from `fj run list`.       |
| `gh run view N --exit-status`     | `fj run view N --exit-status`     | Exits 1 unless the conclusion is success/skipped. |
| `gh run view N --log`             | `fj run view N --log`             | Token API on Forgejo 16+ / Gitea 1.25+.         |
| `gh run view N --log-failed`      | `fj run view N --log-failed`      | Only steps that did not succeed.                |
| `gh run watch N`                  | `fj run watch N`                  | Streams logs live until the job finishes.       |
| `gh run watch N --exit-status`    | `fj run watch N --exit-status`    | Exits 1 unless the conclusion is success/skipped. |
| `gh run download N`               | `fj run download N`               | `--list` to list, `-n` to pick one.             |
| `gh run rerun N`                  | `fj run rerun N`                  | Gitea 1.26+. Forgejo has no rerun API: prints the run URL. |
| `gh run rerun N --failed`         | `fj run rerun N --failed`         | Reruns only the non-green jobs in the run.      |
| `gh run rerun --job ID`           | `fj run rerun N --job J`          | Reruns one job; `J` is the job index from `fj run view`. |
| `gh run cancel N`                 | `fj run cancel N`                 | Forgejo 16+. Elsewhere prints the run URL.      |
| `gh workflow list`                | `fj workflow list`                | Reads workflow files via the contents API.      |
| `gh workflow run FILE`            | `fj workflow run FILE`            | `--ref` (default branch) and `-f key=value` inputs. |
| `gh secret list`                  | `fj secret list`                  |                                                 |
| `gh secret set NAME -b VAL`       | `fj secret set NAME --value VAL`  | Also `--from-file path` (or `-` for stdin).     |
| `gh variable list`                | `fj variable list`                | Shows name, value, and scope. Variables are readable; secrets are not. |
| `gh variable set NAME -b VAL`     | `fj variable set NAME --value VAL` | Creates or updates, then confirms the stored name and scope. |
| `gh variable delete NAME`         | `fj variable delete NAME`         |                                                 |

## Search

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh search repos QUERY`           | `fj search repos QUERY`           |                                                 |
| `gh search issues QUERY`          | `fj search issues QUERY`          |                                                 |
| `gh search prs QUERY`             | `fj search prs QUERY`             |                                                 |
| `gh search code QUERY`            | (none)                            | Forgejo has no code-search API. Removed.        |
| `gh search commits QUERY`         | (none)                            | No commit-search API.                           |

## Other groups

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh browse`                       | `fj browse`                       | `fj browse src/main.rs` deep-links; `--branch B` or `--tag T` picks the ref. |
| `gh status`                       | `fj status`                       | Notifications inbox. Also `fj status read ID`, `unsubscribe ID`, and a local per-account repo mute (`mute` / `unmute` / `muted`). |
| `gh ssh-key list`                 | `fj ssh-key list`                 |                                                 |
| `gh gpg-key list`                 | `fj gpg-key list`                 |                                                 |
| `gh alias list`                   | `fj alias list`                   |                                                 |
| `gh config get KEY`               | `fj config get KEY`               |                                                 |
| `gh extension list`               | `fj extension list`               |                                                 |
| `gh api PATH`                     | `fj api PATH`                     | See `docs/jq.md` for `--jq` syntax.             |
| (n/a)                             | `fj protect`                      | Branch protection rules.                        |
| (n/a)                             | `fj hook`                         | Webhooks.                                       |
| (n/a)                             | `fj repo watch / unwatch`         | Subscribe to repo notifications.                |
| (n/a)                             | `fj repo star / unstar / starred` |                                                 |

## Global flags

| `gh`                              | `fj`                              |
| --------------------------------- | --------------------------------- |
| `gh ... --hostname`               | `fj ... --host` (also `FJ_HOST`)  |
| (n/a)                             | `fj ... --debug` (or `FJ_DEBUG`)  |
| `gh ... --color <when>`           | `fj ... --color <auto|always|never>` |
| (gh uses `less` by default)       | `fj ... --no-pager` / `FJ_NO_PAGER` / `FJ_PAGER` |
| `gh ... --json field,field`       | `fj ... --json --json-fields field,field` (global flag, gh-style projection) |
