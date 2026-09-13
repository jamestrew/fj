# Changelog

All notable changes will be recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-09-13

### Added

- **`fj stack ship` restacks the rest of the stack after a squash land.** A
  squash merge replaces the item's commits, so every branch above it was left
  parented on history the trunk no longer had: the PRs above showed the landed
  diff a second time and the stack had to be repaired by hand. Ship now replays
  the remainder onto the new trunk tip with a single
  `git rebase --onto <new trunk> <landed tip> <top branch> --update-refs` (the
  plumbing `stack absorb` already used), force-pushes each branch with an
  explicit `--force-with-lease`, and re-syncs the PR bases. A conflicting
  replay rolls the rebase back and force-pushes nothing, then ship stops with
  the commands to finish by hand. The PR directly above the landed item is
  still re-pointed at the trunk, because the base branch it was targeting has
  merged away and leaving it aimed at a deleted branch would be worse than the
  no-restack end state. Merge-commit lands are unaffected, since the original
  commits stay reachable. `--no-restack` opts out. The replay needs git 2.38 or
  newer.
- **`fj stack ship` pre-flights the stack's shape in git.** Before merging
  anything it verifies that each unmerged branch descends from the one below
  it, that the lowest one contains the trunk's *fetched* tip, and that each
  local branch tip matches the head of the PR that would merge. A stale or
  non-linear stack is refused up front with a "restack, then `fj stack sync`"
  message rather than failing mid-merge with the bottom already landed.
- **`fj stack ship` re-validates between merges.** After an item lands, the
  next one is re-checked (still open, still mergeable, still approved, still at
  the head fj most recently put there) before it is merged. If the ground
  shifted, ship stops cleanly, reports what already landed, and leaves the rest
  for a re-run. Checks that ship itself re-armed by force-pushing a restacked
  branch are not re-gated. A forge still serving the pre-restack head is a
  lagging read rather than a shift, so ship re-reads a few times; if it never
  catches up, that is a stop in its own right, since every field on that
  snapshot describes a shape the restack has already replaced.
- `stack ship --json` now reports the branches restacked after each item
  (`restacked`), and `stack review --json` carries each PR's head sha
  (`head_sha`). Both fields are additive.
- **`fj run logs` prints Actions job logs.** The singular `fj run log` stays as
  an alias, so existing invocations keep working. A personal access token is
  enough for log reads on this Forgejo build; the auth docs no longer claim a
  browser session cookie is required. A session is still needed for the
  cookie-gated web routes, `run watch`, `run rerun`, and `run cancel`.
- **`fj instances` lists the Fjord Commons.** `fj` read only
  `/me/forge-instances`, which by design excludes the Commons, so there was no
  instance id to address and no way to reach it. The Commons is now read
  alongside your own instances and routed through the platform gateway, with no
  personal access token anywhere: the gateway mints the forge token server-side
  and nothing minted reaches the client. A Commons you have not joined is
  withheld from `instances use` but still named in `fj instances`, on a dimmed
  line, so "not joined" does not read as "no such forge".
- **Host-scoped `FJ_TOKEN_<HOST>` and `FJ_SESSION_<HOST>`.** The host suffix is
  `H` followed by the uppercase hex of the host bytes, so `rasterhub.com` is
  `FJ_TOKEN_H7261737465726875622E636F6D`. The encoding is injective, so
  `foo-bar.com` and `foo.bar.com` cannot collide onto one variable, and CI can
  set a credential for one host without enumerating the rest.
- **`fj pr merge --style fast-forward-only`.** Forgejo's fast-forward-only
  merge style is accepted both as an explicit `--style` and as a repository's
  `default_merge_style`, which previously failed as unsupported. A PR that is
  not a fast-forward onto its base is reported as such, with the rebase to do,
  instead of a raw forge error.

### Changed

- **`fj pr merge` follows the repository's default merge style.** An omitted
  `--style` resolved to `merge` regardless of how the repository was
  configured, so merging from the CLI could produce a merge commit on a repo
  whose web UI squashes. It now reads the repository's `default_merge_style`,
  and fails explicitly if that cannot be read or is not a style fj supports
  rather than falling back to a guess. **Scripts that relied on the implicit
  `merge` need `--style merge` spelled out.** Merge-style is also reported in
  push-rejection diagnostics, so a signed-commit protection failure is no
  longer misread as something else.
- **A generic `FJ_TOKEN` is no longer sent to a host it was not meant for.**
  It used to be picked up for any host. It is now accepted for the default or
  current host, including when you spell that same host with `--host`, and
  ignored when `--host` names a different configured host. Use
  `FJ_TOKEN_<HOST>` for that case. `FJ_SESSION` follows the same rule.
- **The default Fjord platform URL is `https://fjord.sh`,** replacing the old
  preview URL. An HTML response on a platform call now names `--platform-url`
  instead of surfacing an opaque JSON decode error.
- **Global `--host` reaches the auth subcommands.** `fj auth status`, `token`,
  `refresh`, `session`, `logout`, and `setup-git` now honor it, so
  `fj auth status --host X` reports host `X` alone.

### Fixed

- **`cargo test --all` now passes on macOS.** Four defects kept it from
  finishing there, and each one was a test reading the developer's real machine
  instead of its own scratch state. `ProjectDirs` ignores `XDG_CONFIG_HOME` on
  macOS, so tests that thought they had redirected the config directory were
  loading the real `hosts.toml`; when that named a host whose token lives in the
  login keychain, the lookup then blocked forever in securityd, because a
  keychain item's ACL names the binaries allowed to read it and a freshly built
  test binary is not one of them. `FJ_CONFIG_DIR` and `FJ_NO_KEYCHAIN` give
  those tests, and any scripted run that wants its own state, a way to say so.
  The editor test hardcoded `/bin/false`, which exists on Linux but not on
  macOS, so it asserted on a spawn failure rather than the nonzero exit it was
  written for. And the loopback sign-in test drove real sockets on a paused
  clock, which cannot work: tokio's auto-advance does not wait for socket
  readiness, so it jumped the clock past the five-minute callback deadline while
  the silent connection was still in the accept queue. That one now runs on the
  real clock with the request-line bound shortened for the test; the shipped
  ten-second bound is unchanged.

- **The pager no longer swallows short, write, and machine output.** The
  decision to page was made per top-level command family, so every subcommand
  under `repo`, `issue`, `pr`, `release`, `label`, `milestone`, `tag`, `run`,
  `search`, `status`, and `api` was piped into `less`, including one-line write
  confirmations (`pr merge`, `issue close`, `repo star`, `tag create`, ...),
  fixed status blocks (`fj status`, `fj pr status`), short reads (`tag view`,
  `milestone view`, `repo topics`), and every `--json` and `--web` invocation.
  Destructive commands that prompt for confirmation (`repo delete`,
  `release delete`, `tag delete`) were prompting on a stdout already redirected
  into the pager. Paging is now decided per subcommand by
  `cli::paging::should_page`, which pages only genuinely long read-only views:
  list and view commands, `pr diff` / `commits` / `files` / `checks`,
  `run log`, `search`, and `fj api` GET dumps. `--no-pager`, `FJ_NO_PAGER`,
  `FJ_PAGER` / `PAGER`, and the non-TTY check are unchanged.
- **`fj pr merge` no longer discards the pull request description.** With no
  `--message`, fj sent an empty merge-commit body, and Forgejo does not fall
  back to the description the way its web merge dialog does. Every pull request
  merged through the CLI therefore landed with its body gone, silently: the
  merge succeeded and the commit looked normal. The message now defaults to the
  pull request body. An explicit `--message` still wins, and `--message ""`
  still produces a deliberately empty body.
- **Ctrl+C works when fj is blocked in a synchronous call.** SIGINT was raced
  against the command future, so it only took effect while that future was
  still pollable. Several paths parked the thread inside a blocking syscall for
  an unbounded time, and because tokio had already replaced SIGINT's default
  disposition the signal did not terminate the process either: Ctrl+C was not
  slow, it was inert. The two confirmed states were an OS keychain read waiting
  on an authorization dialog it cannot present (over ssh, in CI, from a git
  hook, which is what deadlocked the pre-push hook) and the synchronous stdin
  reads behind `--body -`, `--input -`, `--with-token`, and the interactive
  Fjord Account prompts. A backstop now gives the graceful path 250 ms to
  unwind and then exits 130. A child that owns the terminal's foreground
  process group still gets to decide what SIGINT means, so `less` absorbing it
  is unaffected.
- **Ctrl+C exits during an OIDC sign-in.** `fj auth login` waited for the
  loopback redirect on a blocking task, so an interrupted sign-in printed
  `error: interrupted` and left the process alive. The wait is now async and
  actually cancellable. A stalled connection no longer wedges the sign-in
  either: a socket that connected and sent nothing (a browser preconnect, a
  port scanner) blocked the accept loop forever and the real redirect behind it
  was never served. Request lines are now bounded at 10s, and the overall 300s
  budget also covers connection handling.
- **`FJ_TOKEN` no longer blocks `fj auth login --fjord`.** The variable was
  bound to `--token` by clap, so having one set forced the token flow even when
  `--fjord` was spelled out. Explicit `--fjord` now wins. `FJ_SESSION` was
  bound to `fj auth session --cookie` the same way, where it conflicted with
  `--clear`; that binding is gone too.
- **`fj pr checks` handles a head with no reported checks.** Forgejo returns
  `statuses: null` there, which failed to decode. It now prints
  `no checks reported on <sha>` and exits 0; `--json` still emits a normal
  combined-status object with `statuses: []`.
- **`fj run view --log` reads logs on Forgejo 16.** Logs are resolved through
  the run-jobs and job-log APIs, and displayed run numbers are mapped to API
  run ids first, including for runs past the first page. `FJ_TOKEN` precedence
  is preserved, so an env-token-only environment does not touch an unreadable
  token store. A Forgejo 15.x host, which does not expose the
  token-authenticated job log API, is now reported as such by version rather
  than as a parse error.
- **`fj run view --log` no longer downloads every task in the repository on
  Forgejo 16.** The native job-log path fetched the workflow task list first
  and discarded it, and on 16 that endpoint ignores its `run` filter: about
  3 MB and 30 seconds on every log read against a busy repository. The task
  list is now loaded only by the Loki fallback, which is the one path that
  needs it, and that fallback also decodes the `workflow_runs` envelope 16
  returns instead of aborting on an unexpected response shape.
- **`fj pr list` and `fj pr view` decode Forgejo's system actors.** Forgejo
  uses negative sentinel user ids (`-1` for Ghost, `-2` for `forgejo-actions`),
  and the shared `User` model read `id` as unsigned, so any repository with a
  system actor as reviewer, assignee, poster, or author failed the whole decode
  with `invalid value: integer -2, expected u64`. The field is signed now,
  which covers every actor position at once.
- **A failed list decode names where it failed.** List responses are decoded
  through `serde_path_to_error`, so the message carries the endpoint and the
  JSON field path (for example `GET /api/v1/repos/o/r/pulls?...` and
  `field [0].id`) instead of only the type error.
- **Tag releases publish their assets, and the generated Homebrew formula
  installs.** The `v0.4.1` release was published with zero assets, so
  `brew install fj` served 0.3.0 from 22 to 30 July: one build leg failed, and
  re-running could not recover because artifact keys are scoped by run id, so
  the legs that had already uploaded hit "artifact already exists" and the
  publish job never ran. The assets were uploaded on 30 July by the recovery
  dispatch these changes made possible, and the tap has served 0.4.1 since.
  Artifacts are overwritten on a re-run now, and a release can be re-run by
  `workflow_dispatch` with a `tag` input, from a branch carrying a fix, without
  moving the tag. The formula could not have installed even if publish had run:
  it did `cd fj-<version>-<target>`, but Homebrew has already entered the
  tarball's top-level directory and that directory keeps the tag's `v` prefix,
  which the formula's version drops, so `brew install` ended in
  `Errno::ENOENT: chdir_path`. Three failures that used to pass quietly now
  stop the run: a missing matrix leg rendering `sha256 ""`, an asset glob
  matching nothing, and a missing CHANGELOG section for the tag (release
  bodies were always empty, and are now taken from that section). A dispatched
  tag also reaches the shell as data rather than as a substituted expression,
  must exist in the repository's tags, and is checked out as `refs/tags/<tag>`
  with the resolved sha asserted in every job, so a branch sharing a
  version-shaped name cannot be built and published as if it were the tag.

## [0.4.1] - 2026-07-22

### Fixed

- **Silent Fjord Account session refresh across every command.** An expired
  Fjord access token is now renewed transparently at the shared HTTP client: on
  a 401 from a host signed with a Fjord bearer, fj refreshes via the stored
  refresh token and retries the request once, so `fj api`, `fj pr`, `fj issue`,
  `fj repo`, and the Actions log routes no longer fail with "sign in again" once
  the short-lived token lapses. Previously only `fj instances` and `fj auth`
  refreshed. PAT hosts (whose tokens are not refreshable) and the case with no
  usable refresh token surface the original 401 unchanged, and the refresh is
  bounded to one attempt per request so a persistently-401 endpoint cannot loop.

## [0.4.0] - 2026-07-22

### Added

- **Fjord Account sign-in under SSO/OIDC**: `fj auth login --fjord` now reads
  the deployment's sign-in shape from `GET /api/auth/status` and, on an `oidc`
  deployment, runs an OAuth 2.0 Authorization Code + PKCE flow over a loopback
  redirect. `fj` opens your browser to the hosted sign-in page and captures the
  result on `127.0.0.1`, no copy-paste. The email + password flow still runs on
  `credentials` deployments and remains the fallback. The session's refresh
  token is stored in the OS keychain alongside the access token, so an expired
  session renews itself silently (also on demand via `fj auth refresh`), and
  `fj instances` refreshes and retries once on a 401.
- **`fj auth login --fjord --device`**: durable cross-device sign-in for
  SSO/OIDC Fjord Accounts via the OAuth 2.0 Device Authorization Grant
  (RFC 8628). `fj` shows a short user code, the verification link, and a
  scannable terminal QR code; you approve on any device (phone, iPad, another
  laptop) and `fj` polls until the session lands, honoring the server's poll
  `interval`, `slow_down` back-off, and the code's expiry. No loopback and no
  copy-paste, so it works when `fj` runs on a remote server or headless box. It
  is auto-selected when no local browser is reachable (a `$SSH_CONNECTION`
  session, or a Unix session with no display server); `FJ_FORCE_LOOPBACK=1`
  opts back out. The resulting session is identical to the loopback flow (same
  keychain slots, silent refresh, `fj auth status` shows the Fjord Account).
  This supersedes `--manual` for the cross-device case.
- **`fj auth login --fjord --manual`** (alias `--no-browser`): manual,
  cross-device sign-in for SSO/OIDC Fjord Accounts. The default loopback capture
  only works when your browser is on the same machine as `fj`; `--manual` prints
  the sign-in URL, lets you complete it on any device (phone, iPad, another
  laptop), and reads back the redirected `127.0.0.1` URL (or the bare code) you
  paste, even though that page fails to load on the other device. It keeps the
  same Authorization Code + PKCE exchange, validates the returned `state`, and
  needs no platform changes. Superseded by `--device` for the cross-device case.
- The browser page shown after a loopback sign-in is now a branded, self-contained
  Fjord page (inline styles, embedded mark, light/dark aware) for the success,
  failure, and state-mismatch outcomes.
- **`fj auth session`**: import a Forgejo web session cookie for the cookie-gated
  Actions log and rerun routes, which reject a PAT and authenticate by browser
  session only (fj#103). Pass the `i_like_gitea` cookie via `--cookie` or
  `--with-cookie` (read from stdin, kept out of shell history), or `--clear` to
  forget it. The value is never echoed and is stored under a keychain slot
  disjoint from the API token (with the same `FJ_SESSION` env override and 0600
  file fallback). `fj auth status` now shows a per-host "Web session" line so you
  can see whether the log and rerun routes are enabled.
- **`fj run log`**: a dedicated subcommand to print a run's logs, alongside the
  existing `fj run view --log`. Takes `--job/-j` to scope to one job, `--attempt`
  to pick an attempt, `--failed` to keep only jobs that did not succeed, and
  `--json`. It reads from the API/task-log path with a Loki fallback, so logs
  remain reachable on deployments where the Forgejo web log route is unavailable.

## [0.3.0] - 2026-06-29

### Added

- **`fj pr create --fill` / `--fill-first`**: derive the PR title and body from
  the commits in `base..head` instead of retyping them, mirroring `gh`. A single
  commit fills the title from its subject and the body from its message; multiple
  commits use the first subject as the title and a bullet list of subjects as the
  body. `--fill-first` uses only the first (oldest) commit. It reads `git log`
  locally with no extra API call, and an explicit `--title` / `--body` / `-F`
  still overrides the filled value.
- **`fj pr merge --auto`**: schedule a merge the server completes once all
  required checks pass, instead of polling and merging by hand. An already-green
  PR merges immediately; `--auto` honors `--style` / `--title` / `--message` /
  `--delete-branch`. `--disable-auto` cancels a pending schedule. `pr view` and
  `pr checks` show `Auto-merge: scheduled` while one is pending (Forgejo records
  the schedule on the timeline only, so the chosen style is not shown there). On
  a host build without scheduled-merge support the merge falls back to the #162
  diagnosis with a note that `--auto` did not schedule.
- **`fj run rerun --failed` / `--job J`**: rerun just part of a run instead of
  the whole thing. `--failed` reruns only the jobs that did not finish green;
  `--job J` reruns a single job, where `J` is the index shown by `fj run view`.
  The two are mutually exclusive, and bare `fj run rerun N` still reruns the
  whole run. Both go through Forgejo's per-job web rerun route.
- **`fj pr attach` / `pr create --attach` / `pr edit --attach`**: upload file
  attachments to a pull request and link them in the description. PRs share the
  issue index, so files post to `issues/{number}/assets`; image extensions are
  embedded with `![]()` and everything else is linked. `pr attach N FILE...`
  appends to an existing PR's body; the `--attach` flag on create/edit does the
  upload-and-splice in one step.
- **`fj stack split`**: turn a single-branch stack into one stacked entry per
  commit. The N commits ahead of the base become chained branches
  `<handle>/<slug>-1..N`; it moves local refs only (no push, no API), so it's
  offline and reversible, and `--dry-run` shows the commit-to-branch mapping.
- **`fj stack absorb`**: fold staged changes into the stack entries that
  introduced them, the way `git absorb` does. It attributes each staged file to
  the most recent entry that touched it, makes a `fixup!` per target, then runs
  one autosquash `git rebase --update-refs` to rewrite the entries in place.
  Needs git >= 2.38; `--dry-run` / `--json` print the attribution plan.
- **`fj agent fix --write` / `--pr`**: `--write` applies the proposed patch to
  the working tree (validated first, left unstaged to review); `--pr` carries
  the current change plus the fix onto a fresh branch, commits, pushes, and
  opens a PR, restoring your previous branch on failure. The default still
  prints the patch and changes nothing.
- **`fj stack sync` PR annotations**: once the PRs exist, sync mirrors a
  navigation block (the whole stack, linked, current entry marked) into each PR
  description, wrapped in `fj-stack` markers so hand-written body text is
  preserved. The block carries a versioned JSON payload so the Fjord apps can
  read the stack as data instead of scraping markdown; `--no-annotate` skips it.
- **Man pages on install**: `make install` now generates and installs man pages
  to `$(MANDIR)`, so `man fj` works out of the box. The top-level page gained a
  real description and examples instead of echoing the one-line summary.
- **`fj workflow enable` / `disable` / `view`**: manage and inspect Actions
  workflows from the terminal.
- **`fj branch-create` / `fj branch-delete`**: create and delete branches on the
  remote without a local clone.
- **`fj issue lock` / `unlock` and `fj pr lock` / `unlock`**: lock or unlock a
  conversation, with an optional reason.
- **`fj repo collaborator`**: list, add, and remove repository collaborators.
- **`fj tag`** (list/create/delete/view) and branch ref commands for managing
  git refs directly through the API.
- **Issue templates**: create issues from a repository's issue templates.
- **`fj release` gains `--pattern`, `--clobber`, and implicit-latest**: glob
  asset selection, overwrite existing assets, and omit the tag to act on the
  latest release for download/upload/view.
- **`fj workflow run` returns a run handle**: `--json`, `--watch`, and
  `--exit-status` let scripts trigger a workflow and follow it to completion.
- **`fj run` exit status and `run list` filters**: gate scripts on a run's
  outcome and filter the run list.
- **`fj secret` / `fj variable --org`**: read and write org-scoped CI
  credentials shared across repos. Values are never printed.
- **`fj pr comment` edit/delete and idempotent upsert**, plus `-F` /
  `--body-file` body input across `issue`, `pr`, `release`, `milestone`, and the
  comment verbs.
- **`fj api` reads the request body from stdin or a file** for scripted writes.
- **List filters, pagination, and counts**: `issue list` / `pr list` gain state,
  `--label`, and `--author` filters; list commands gain `--paginate` and total
  counts.
- **`--json` on read-only `pr` / `secret` / `variable` commands** for scripting.
- **Label and assignee support**: set labels and assignees on `pr`, and add or
  remove them additively on `issue` create and edit.
- **File-backed token store fallback**: `fj auth` falls back to a file token
  store when the OS keychain or Secret Service is unavailable.

### Changed

- **`fj auth login` first run**: an interactive run with no disambiguating
  flags now opens a menu to choose between a Fjord Account (email + password)
  and your own Forgejo server (access token), mirroring the mobile apps' first
  screen. Explicit intent is unchanged and skips the menu: `--fjord` goes
  straight to Fjord, and any PAT input (`--host`, `--token`, `--with-token`,
  or `FJ_TOKEN`) goes straight to the token flow, so scripts and the
  non-interactive paths behave exactly as before.
- **Global `--color` flag** and the `no_color` config / `NO_COLOR` env are now
  honored across colored output.
- **Reworked API error UX and exit codes**: friendly, actionable messages now
  also cover 409 conflict and 422 validation responses, with consistent
  non-zero exit codes.
- **Unavailable keychains are treated as a missing token** (prompting a clean
  re-auth) rather than surfacing a hard error.
- **The `fj stack` engine now consumes the shared `fjord-core` library** for
  naming, plan and ship gating, split/absorb routing, review summarization, and
  PR-body annotation, keeping the CLI and the Fjord apps in sync.

### Fixed

- Following `Link: rel=next` pagination no longer errors on the `null` body some
  Forgejo list endpoints return for an empty collection. `get_all` normalizes it
  to an empty list, matching single-page `get_page`.
- `fj man` is back in the plugin allowlist, so a `fj-man` binary on PATH can no
  longer shadow the built-in man generator. A test now keeps the allowlist in
  sync with the command tree.
- `fj api --jq` iteration (`.[]`) and the `-L` / `--limit` flag on `fj run list`.
- The stack PR-annotation caption renders again: Forgejo's sanitizer drops the
  raw `<sub>` tag, so it is now plain markdown italics.
- `fj issue edit --add-assignee` / `--remove-assignee` no longer error with a
  spurious "not found". Forgejo has no `/issues/{n}/assignees` sub-resource, so
  the assignee list is now read and PATCHed onto the issue as a merged set
  (additive add, subtractive remove). Applies to `fj pr edit` too.
- `fj pr merge` no longer reports a bare `405 Method Not Allowed` (or `500`) for
  every non-mergeable PR. On failure it inspects the PR's mergeability and prints
  the specific reason: merge conflicts with the base, a disabled merge style
  (naming the allowed ones), a failing required check, missing approvals, an
  out-of-date branch, or an already-merged/closed PR. When nothing specific is
  found it falls back to the raw Forgejo message, which is now surfaced instead
  of swallowed. `fj pr view` and `fj pr checks` gained a `Mergeable:` line.
- Piped `fj --version` output is no longer truncated.
- Quoted arguments are parsed correctly in alias expansions.
- scp-form remote URLs with a stray colon in the path are rejected, and
  malformed hostnames are rejected before they reach the git credential helper.
- Digit-only object keys resolve correctly in the JSON path engine.
- No longer panics slicing a multibyte debug body preview.
- Corrected Forgejo host URL handling.

#### Audit pass (commands, correctness, and UX)

A sweep of every subcommand and flag fixed the following:

- Crashes in `fj status`, `fj secret list`, and `fj variable list` from live
  payloads returned as bare arrays or with omitted fields.
- `fj agent fix` now redacts secrets and drops sensitive files before sending
  the diff to the provider, matching `fj agent review`.
- Error bodies print to stderr (stdout stays clean on failure); `fj api -i`
  writes headers to stdout.
- `fj milestone list -s` validates the state filter (open/closed/all) instead of
  silently returning everything.
- `fj pr status --json-fields` projects correctly, and `fj pr checks` reports
  "no checks" on a no-CI PR instead of a decode error.
- `fj run list` shows a real conclusion, `fj instances list --json` works, and
  `fj stack ship --merge` selects a merge-commit style.
- Not-found errors name the missing sub-resource (branch protection, release,
  milestone, tag) rather than the repo.
- Usage errors exit non-zero; `fj alias set` refuses to shadow built-in
  subcommands; `fj man` recurses into nested subcommands; `fj config` gains
  `unset` and rejects unknown keys.
- `fj browse` gains `--print-url` / `--no-browser`; `fj pr create --base`
  defaults to the repo's default branch; `fj search -L` documents its cap; and
  `fj tag list` truncates long annotated-tag messages.

## [0.2.0] - 2026-06-02

### Added

- **`fj work`**: local working-context commands. `work context` prints an
  offline-first snapshot of the repo (branch, dirty state, current PR and its
  CI checks, account, active stack); `work resume` / `work continue` restore a
  saved session. Session state is local at `<git-dir>/fj/session.json`.
- **`fj stack`**: stacked-branch / stacked-PR management, local and
  git-native (`<git-dir>/fj/stack.json`, never a committed manifest).
  - `stack new` authors a stack and its first branch.
  - `stack review` shows each item's PR number, review state, CI status, and
    mergeability; `--open` opens the PRs in a browser, `--request <user>`
    requests review across the stack.
  - `stack sync` pushes each branch and creates/updates its PR, chaining bases
    so each PR targets the branch below it; idempotent, with `--draft` /
    `--ready` controlling PR state and `--dry-run` for a plan.
  - `stack ship` merges the stack bottom-up behind a green-and-approved gate,
    re-pointing each next PR onto the trunk as the one below it merges, and
    stops on the first failure with recovery instructions (merged items are
    skipped on re-run). `--squash` / `--merge`, `--delete-branches`,
    `--force`, `--dry-run`.
- **`fj agent`**: AI-assisted code operations through an env-var provider
  abstraction (`FJ_AGENT_PROVIDER` / `FJ_AGENT_API_KEY` / `FJ_AGENT_MODEL`;
  null default, built-in `mock`; the Fjord token is never reused for AI).
  - `agent review` is a local diff-analysis command: it gathers the
    working-tree diff (or `--pr <n>`, or each `--stack` item), redacts secrets
    and sensitive files before anything leaves the machine, and prints
    severity-tagged findings with file references (secret-bearing files,
    source changed without tests, risky areas). `--security` focuses the pass;
    `--json` emits a structured report for editor/mobile clients.
  - `agent explain` explains a file, a git revision, or the working-tree diff.
  - `agent fix` emits a patch only and never mutates.
- **`fj pr merge --delete-branch`**: delete the head branch after the merge
  succeeds (matching `gh pr merge --delete-branch`).

### Changed

- `fj run view --log` / `--log-failed` now cover every job in a run by default
  (only the failed jobs for `--log-failed`) instead of silently defaulting to
  job 0, which hid failures in later jobs. Narrow to one job with
  `--job <index>`.
- `make install` signs the installed binary with a stable Developer ID, so the
  macOS keychain trust ("Always Allow") survives rebuilds.
- Internal: the large `cli/` and `api/` command modules were split into focused
  siblings with backfilled tests, the two JSON-path extractors converged onto
  one engine, and CI gained a strict region-level coverage gate. CI cache and
  release artifacts moved onto Fjord Actions.

### Fixed

- `fj run view` no longer errors with "invalid type: null, expected a sequence"
  when a run's job is still running or blocked. Forgejo returns `null` where the
  Actions UI sends arrays (jobs, steps, attempts, log lines); these now decode
  to empty.

## [0.1.3] - 2026-05-14

### Added

- **Linux x86_64 release binary.** `fj-v0.1.3-linux-x86_64.tar.gz`
  ships alongside the two macOS tarballs. Built via Docker
  cross-compile (rust:1.95-bookworm under linux/amd64 emulation on
  Apple Silicon). Statically linked against glibc 3.2+, ELF stripped.
- `scripts/build-linux.sh`: one-shot Docker-backed cross-build that
  drops `target/fj-linux-x86_64` into the host. Uses persistent
  named volumes for the cargo registry so incremental builds stay
  cheap.
- `scripts/notarize.sh`: submits a signed darwin tarball to Apple's
  notary service, polls, and surfaces the failure log if rejected.
  Requires a keychain profile created via `xcrun notarytool
  store-credentials fj-notary`.
- `scripts/sign.sh --notarize` (or `FJ_NOTARIZE=1`): chains signing
  → notarization in one step.

### Changed

- `docs/faq.md` Windows entry is now honest: "nobody's tried."
  Drops the implied claim of cross-platform support.

## [0.1.2] - 2026-05-14

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

## [0.1.1] - 2026-05-14

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

## [0.1.0] - 2026-05-13 - distribution & open-source ready

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
- `docs/gh-to-fj.md`: complete command-by-command mapping.
- `docs/faq.md`: common questions about tokens, hosts, debug,
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

## 0.1.0 - 2026-05-13

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
