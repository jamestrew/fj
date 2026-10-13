# Forgejo compatibility

fj is built against the **Forgejo 7.x API surface**. We test against
`7.0.16+gitea-1.21.11` on rasterhub.com. Most endpoints are inherited
unchanged from Gitea, so old Gitea instances will largely work too,
but there are sharp edges:

## Tested

| Server               | Version                          | Status         |
| -------------------- | -------------------------------- | -------------- |
| Forgejo              | 7.0.x (we test 7.0.16)           | full support   |
| Forgejo              | 8.x                              | expected to work |
| Forgejo              | 9.x / 10.x                       | expected to work |
| Gitea                | 1.21.x (the base of Forgejo 7.0) | mostly works   |
| Gitea                | 1.20.x and earlier               | YMMV, see below |

If you run fj against an untested version and find something broken,
please open an [issue](../.forgejo/issue_template/bug.md) with
`fj api /version` output.

## Known caveats on older Gitea (≤ 1.19)

These features rely on endpoints introduced in or after Gitea 1.20.
`fj` will return an HTTP 404 with the endpoint path.

- `fj repo mirror-sync` (`POST /repos/.../mirror-sync`).
- `fj milestone` group on instances that didn't expose
  `/repos/.../milestones/{id}` for editing.

For older instances, the typed API still works via `fj api <path>` if
you know the right path.

## Actions capability map

Actions support differs by forge flavor and by version, so fj gates each
optional route on the pair (flavor, version). The table lives in
`src/client/capability.rs`. A Forgejo version number is never read as a Gitea
one: Forgejo 16 is not "at least Gitea 1.26".

| Capability | Route | Forgejo | Gitea |
| --- | --- | --- | --- |
| Run jobs (`run view`, `run logs`) | `GET actions/runs/{id}/jobs` | 16.0+ | 1.25+ |
| Per-job logs (`run logs`, `run view --log`, `run watch`) | `GET actions/jobs/{id}/logs` | 16.0+ | 1.24+ |
| Artifacts (`run download`) | `GET actions/runs/{id}/artifacts`, `GET actions/artifacts/{id}/zip` | 16.0+ | 1.24+ |
| Cancel (`run cancel`) | `POST actions/runs/{id}/cancel` | 16.0+ | not offered (absent on 1.27.3) |
| Rerun (`run rerun`, `--failed`, `--job`) | `POST actions/runs/{id}/rerun`, `/rerun-failed-jobs`, `/jobs/{job}/rerun` | not offered (no API route) | 1.26+ |
| Run-level log archive | `GET actions/runs/{id}/logs` | 16.0+ | absent on 1.27.3 |

fj reads logs per job and never asks for the run-level archive, so per-job logs
stay usable on a Gitea that lacks it.

What happens when a route is not there:

- **Known absent** (for example cancel on Gitea, rerun on Forgejo, anything
  Actions on Forgejo 15): fj makes no request and reports the limitation with
  the run URL, so you can act in the web UI.
- **Unidentified server**: fj tries the API route and treats a 404 as the
  evidence.
- **Permission denied** (403) is reported as a permission problem, not as a
  missing feature.

The Forgejo rows were checked against the Forgejo 16.0.4 API description. The
Gitea rows come from a route audit and were not exercised against a live Gitea.

### Web routes and the token

Forgejo's human-facing web routes (the ones the browser UI calls) authenticate
with a session cookie and a CSRF token, not a personal access token. fj never
sends the token to a web route. Where a server has no API route, fj falls back
to the web route **without credentials** for three reads: the run overview
(`run view`), the live tail (`run watch`), and the artifact list and download.
That works for publicly readable runs. For a private repository on such a
server fj reports the limitation and the run URL instead.

State-changing web actions (rerun, cancel) are never attempted: they need a
browser session.

Other capabilities with no `/api/v1` route:

| Capability | In `fj` |
| --- | --- |
| Workflow **list** (`workflow list`) | implemented, reads the files via the contents API |
| Run delete, workflow enable/disable | not offered |
| Issue/PR lock/unlock | implemented |
| Issue transfer, pr revert | not offered |
| Code search / commit search | not offered (web route returns HTML only) |
| Actions cache list/delete | not offered (no web route, no API) |
| Gist / snippets | removed (no such entity in Forgejo) |

`fj repo sync` is the one non-Actions case here: `POST
/repos/.../merge-upstream` is a real Gitea endpoint absent on some Forgejo
builds (15.0.x included). The command detects the 404 and tells you to
sync the fork manually; `fj repo mirror-sync` (a different endpoint) is
unaffected.

## Actions on older servers

On a server without the token log routes (Forgejo before 16, or a server that
answers 404), `fj run logs` and `fj run view --log` use a separately authorized
log source if you configured one (`FJ_RUN_LOG_LOKI_URL` or `FJ_LOKI_URL`), and
otherwise report the limitation with the run URL. See `docs/architecture.md`,
"Actions run logs".

A Fjord Account gateway proxies only `/api/v1`. API-backed commands work
through it when the server has the routes; the credential-free web fallback is
not available there.

## Forgejo-only endpoints

These are fully implemented and exposed through fj. They are
Forgejo extensions of the Gitea base.

- Branch protection rules (`fj protect`)
- Mirrors (`fj repo mirror`)
- Topics (`fj repo topics`, with `add` / `remove` for single topics)
- Repository collaborators (`fj repo collaborator`)
- Repository transfers (`fj repo transfer`). Forgejo has no endpoint that
  lists incoming transfers, so `fj repo transfer list` scans one repository
  listing for repositories marked pending.
- Webhooks (`fj hook`), present in Gitea too, identical surface
- Actions runs / secrets / variables (`fj run`, `fj secret`,
  `fj variable`)

## How fj detects the forge flavor and version

fj establishes the flavor from route evidence before it reads a version number:

1. `GET /api/forgejo/v1/version` answers 200 only on Forgejo. If it does, the
   host is Forgejo and that body is the version.
2. If that route answers 404 and `GET /api/v1/version` answers 200, the host is
   Gitea.
3. Anything else (both missing, a 401 or 403 on the first route, a network
   error) leaves the host **unidentified**. It is not assumed to be either.

The Forgejo floor (7.0) applies only to a host identified as Forgejo. A Gitea
1.x host is never warned or refused for being "pre-7", and an unidentified host
gets a note saying fj could not tell, not a version warning.

`fj auth login` and `fj auth refresh` run the probe and store the result in
`hosts.toml` (`flavor`, `server_version`). Commands that need a gate use the
stored value, and probe once per process (at most two requests) when nothing is
stored. A `hosts.toml` written by an older fj has neither field and still
loads. Run `fj auth refresh` after a server upgrade so the stored version
catches up.

To see what a configured host reports:

```sh
fj api /version
fj auth status
```

## Bumping the supported floor

If we drop support for a Forgejo version, it'll be called out under
"BREAKING CHANGES" in CHANGELOG.md ahead of the release, and we'll
emit a stronger error (not just a warning) from the version probe.
