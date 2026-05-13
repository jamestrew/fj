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
- `fj search code` (`/repos/search/code` is Forgejo 7+ only).

For older instances, the typed API still works via `fj api <path>` if
you know the right path.

## Forgejo-only endpoints

These are fully implemented and exposed through fj. They are
Forgejo extensions of the Gitea base.

- Branch protection rules (`fj protect`)
- Mirrors (`fj repo mirror`)
- Topics (`fj repo topics`)
- Webhooks (`fj hook`) — present in Gitea too, identical surface
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
