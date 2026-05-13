# `gh` → `fj` quick reference

fj's command surface tracks `gh` closely. This table maps the commands
you reach for most. Where the semantics diverge, the difference is
called out.

## Auth

| `gh`                     | `fj`                        | Notes                                                |
| ------------------------ | --------------------------- | ---------------------------------------------------- |
| `gh auth login`          | `fj auth login`             | Multi-host; first run asks for the hostname.         |
| `gh auth status`         | `fj auth status`            | Tokens stored in OS keychain, never on disk.         |
| `gh auth logout`         | `fj auth logout`            |                                                      |
| `gh auth refresh`        | `fj auth refresh`           | Re-verifies (or replaces) the stored token.          |
| `gh auth token`          | `fj auth token`             | Refuses to write to a TTY without `--force`.         |
| `gh auth setup-git`      | `fj auth setup-git`         | Installs a credential helper.                        |
| `gh auth switch`         | `fj auth switch`            | Picks the default host for `--host`-less commands.   |

## Repos

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh repo list`                    | `fj repo list`                    |                                                 |
| `gh repo view OWNER/NAME`         | `fj repo view OWNER/NAME`         | Auto-detects from `git remote` when omitted.    |
| `gh repo clone OWNER/NAME`        | `fj repo clone OWNER/NAME`        |                                                 |
| `gh repo create NAME`             | `fj repo create NAME`             | `--clone` to clone after creating.              |
| `gh repo fork`                    | `fj repo fork`                    |                                                 |
| `gh repo sync`                    | `fj repo sync`                    | Calls Forgejo's `merge-upstream` endpoint.      |
| `gh repo edit`                    | `fj repo edit`                    |                                                 |
| `gh repo rename`                  | `fj repo rename`                  |                                                 |
| `gh repo archive`                 | `fj repo archive`                 |                                                 |
| `gh repo delete`                  | `fj repo delete`                  | Requires typing the slug to confirm.            |
| (n/a)                             | `fj repo branches`                |                                                 |
| (n/a)                             | `fj repo topics --set a,b,c`      |                                                 |
| (n/a)                             | `fj repo mirror <source-url>`     | Forgejo migrate/mirror endpoint.                |
| `gh repo set-default`             | (not yet)                         | Multi-remote default picking.                   |

## Issues

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh issue list`                   | `fj issue list`                   |                                                 |
| `gh issue view N`                 | `fj issue view N`                 |                                                 |
| `gh issue create`                 | `fj issue create`                 | Omit `--body` to open `$EDITOR`.                |
| `gh issue edit N`                 | `fj issue edit N`                 |                                                 |
| `gh issue close N`                | `fj issue close N`                |                                                 |
| `gh issue reopen N`               | `fj issue reopen N`               |                                                 |
| `gh issue comment N --body B`     | `fj issue comment N --body B`     | New: also `fj issue edit-comment ID`.           |
| `gh issue develop N`              | `fj issue develop N`              | Branches off the default branch.                |

## Pull requests

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh pr list`                      | `fj pr list`                      |                                                 |
| `gh pr view N`                    | `fj pr view N`                    | `--comments` shows reviews + issue comments.    |
| `gh pr create`                    | `fj pr create`                    | Head defaults to current branch.                |
| `gh pr edit N`                    | `fj pr edit N`                    |                                                 |
| `gh pr diff N`                    | `fj pr diff N`                    | Raw unified diff.                               |
| `gh pr checks N`                  | `fj pr checks N`                  |                                                 |
| `gh pr ready N`                   | `fj pr ready N`                   |                                                 |
| `gh pr review N --approve`        | `fj pr review N --event approve`  |                                                 |
| `gh pr review N --request-changes`| `fj pr review N --event request-changes` |                                          |
| `gh pr status`                    | `fj pr status`                    |                                                 |
| `gh pr checkout N`                | `fj pr checkout N`                |                                                 |
| `gh pr merge N`                   | `fj pr merge N`                   | `--style merge|rebase|rebase-merge|squash`.     |
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

## Labels & milestones

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh label list`                   | `fj label list`                   |                                                 |
| `gh label create NAME`            | `fj label create NAME`            |                                                 |
| `gh label edit NAME`              | `fj label edit NAME`              |                                                 |
| `gh label delete NAME`            | `fj label delete NAME`            |                                                 |
| (n/a)                             | `fj milestone list`               | New: Forgejo milestones.                        |
| (n/a)                             | `fj milestone create TITLE`       |                                                 |
| (n/a)                             | `fj milestone assign N --milestone ID` | Assign an issue/PR to a milestone.         |

## Workflows & secrets

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh run list`                     | `fj run list`                     | Forgejo Actions.                                |
| `gh run view N`                   | `fj run view N`                   |                                                 |
| `gh run rerun N`                  | `fj run rerun N`                  |                                                 |
| `gh run cancel N`                 | `fj run cancel N`                 |                                                 |
| `gh secret list`                  | `fj secret list`                  |                                                 |
| `gh secret set NAME -b VAL`       | `fj secret set NAME --value VAL`  | Also `--from-file path` (or `-` for stdin).     |
| `gh variable list`                | `fj variable list`                |                                                 |

## Search

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh search repos QUERY`           | `fj search repos QUERY`           |                                                 |
| `gh search issues QUERY`          | `fj search issues QUERY`          |                                                 |
| `gh search prs QUERY`             | `fj search prs QUERY`             |                                                 |
| `gh search code QUERY`            | `fj search code QUERY`            | New. Optional `-R owner/name` restricts scope.  |

## Other groups

| `gh`                              | `fj`                              | Notes                                           |
| --------------------------------- | --------------------------------- | ----------------------------------------------- |
| `gh browse`                       | `fj browse`                       | `fj browse src/main.rs` deep-links.             |
| `gh status`                       | `fj status`                       | Notifications inbox.                            |
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
| (gh uses `less` by default)       | `fj ... --no-pager` / `FJ_NO_PAGER` / `FJ_PAGER` |
| `gh ... --json field,field`       | `fj ... --json --json-fields field,field` (global flag, gh-style projection) |
