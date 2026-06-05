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

- `fj pr ready` (`PATCH /repos/.../pulls/{n}` with `draft: false`).
- `fj repo mirror-sync` (`POST /repos/.../mirror-sync`).
- `fj milestone` group on instances that didn't expose
  `/repos/.../milestones/{id}` for editing.

For older instances, the typed API still works via `fj api <path>` if
you know the right path.

## Web-route capability map (verified against 15.0.2 / gitea-1.22)

Several GitHub/`gh` capabilities have no `/api/v1` endpoint but ARE served
by Forgejo's human-facing web routes (the ones the browser UI calls). We
use those where they work, exactly as `fj run --log` does. The decisive
rule, learned the hard way:

> **Read-only web endpoints accept a personal access token. State-changing
> web POSTs reject it (they require a browser session).**

So reads can borrow the web-route trick; writes cannot. Status of every
no-`/api/v1` capability we looked at:

| Capability | Web route? | Reachable with a PAT? | In `fj` |
| --- | --- | --- | --- |
| Run logs (`run view --log`, `watch`) | yes (read) | yes | ✅ implemented |
| Run summary (`run view`) | yes (read) | yes | ✅ implemented |
| Artifact **list** (`run download --list`) | yes, returns JSON (read) | yes | ✅ implemented |
| Artifact **download** (`run download`) | documented `.../artifacts/{name}` (read) | likely (unverified) | ⚠️ implemented, see note |
| Workflow **list** (`workflow list`) | n/a, read the files via the contents API | yes | ✅ implemented |
| Run rerun / cancel | yes (write) | **no**, 404s a token POST | degrades, prints run URL |
| Run delete, workflow enable/disable | write | no | not offered |
| Issue/PR lock/unlock, issue transfer, pr revert | write | no | not offered |
| Code search / commit search | web route returns **HTML only**, indexer-gated | scrape-only | not offered (fragile) |
| Actions cache list/delete | **no web route** (404) and no API | n/a | not offered |
| Gist / snippets | no entity at all (a GitLab feature, not Forgejo) | n/a | removed |

Note on artifact download: the list route (`GET .../runs/{run}/artifacts`)
is confirmed working and returns `{"artifacts":[{name,size,status}]}`.
The download route (`GET .../runs/{run}/artifacts/{name}`) is Forgejo's
documented path, but no run in development had artifacts to download, so
the exact behavior on a populated run is unverified. `fj run download`
lists first (always works) and then fetches each artifact; if the
download route misbehaves it surfaces a clear error rather than failing
silently.

`fj repo sync` is the one non-Actions case here: `POST
/repos/.../merge-upstream` is a real Gitea endpoint absent on some Forgejo
builds (15.0.x included). The command detects the 404 and tells you to
sync the fork manually; `fj repo mirror-sync` (a different endpoint) is
unaffected.

## Actions: read vs write over the API

Reads (`fj run list`, `fj run view`, `--log`, `watch`, `run download
--list`, `workflow list`) work, but most go through Forgejo's **web**
routes (or the contents API), not the Actions `/api/v1` surface, which
only exposes run list/get plus secrets, variables, and workflow dispatch.
See `docs/architecture.md` → "Actions run logs". A Fjord-Account gateway
proxies only `/api/v1`, so the whole `fj run` / `fj workflow` group needs
a personal-access-token host.

## Forgejo-only endpoints

These are fully implemented and exposed through fj. They are
Forgejo extensions of the Gitea base.

- Branch protection rules (`fj protect`)
- Mirrors (`fj repo mirror`)
- Topics (`fj repo topics`)
- Webhooks (`fj hook`), present in Gitea too, identical surface
- Actions runs / secrets / variables (`fj run`, `fj secret`,
  `fj variable`)

## How fj detects versions

On `fj auth login`, fj calls `/api/v1/version` once and stores the
version string. If the version looks pre-7.x, fj prints a warning to
stderr so you know what to expect. After login, the stored version
is consulted only by `fj auth status`.

If you want to know the version of a configured host:

```sh
fj api /version
# or
fj auth status | grep Version
```

## Bumping the supported floor

If we drop support for a Forgejo version, it'll be called out under
"BREAKING CHANGES" in CHANGELOG.md ahead of the release, and we'll
emit a stronger error (not just a warning) from the version probe.
