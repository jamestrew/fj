# Choose an account for one invocation

Use `--account LOGIN` or `FJ_ACCOUNT` with an environment PAT or a named
PAT profile to act as a
non-default Forgejo account. This works for rasterhub identities and accounts
on independent Forgejo hosts, using each host's own PAT.

```sh
FJ_TOKEN="$QA_PAT" fj --account qa pr review 42 --approve
FJ_TOKEN="$QA_PAT" FJ_ACCOUNT=qa fj pr comment 42 --body 'Reviewed'
FJ_TOKEN="$QA_PAT" fj --account qa issue create --title 'Follow-up'
```

The flag overrides `FJ_ACCOUNT`. With neither set, authentication is unchanged.
Environment overrides are not persisted. Named profiles use the separate
verified login flow below.
Do not place token values in command-line arguments; the existing `--token`
on auth subcommands still controls persistent authentication.

Credential selection follows the resolved target host, including git remote
inference. A host-scoped `FJ_TOKEN_H<HEX>` takes precedence over `FJ_TOKEN`.
`HEX` is the uppercase hexadecimal UTF-8 encoding of the exact configured
host key. For example, `second.example` uses
`FJ_TOKEN_H7365636F6E642E6578616D706C65`. Generic `FJ_TOKEN` is eligible only
for the default host. To target another host, provide its scoped variable:

```sh
FJ_TOKEN_H7365636F6E642E6578616D706C65="$CUSTOMER_PAT" \
  fj --host second.example --account qa issue create --title 'Follow-up'
```

Explicit account selection uses an eligible environment PAT first, otherwise
the exact stored profile. It never falls back to another account. Before making API requests, fj checks the token's
`/user` identity against the selected login. A mismatch, failed identity read
or missing credential stops the command. Successful verification is shared
by clones of that client. The selected login is redacted in CLI debug output;
verification errors do not print either identity or the token. Existing
same-origin credential and redirect restrictions still apply.

Anonymous or stored web-session fallbacks are refused with account selection.
Alternate auth query parameters (`token`, `access_token`) and `sudo`
impersonation conflict with account selection, as do
custom authorization headers. Both URL query strings and separately supplied
query fields are checked before verification, without printing their values.
Custom URL-encoded form content types are also refused because Forgejo reads
form tokens before the authorization header.
Multipart issue/PR attachments and release uploads initiate identity
verification before constructing upload headers, even as the first request.
Synchronous direct HTTP helpers still require completed verification. Named PAT login, default switching and redacted status support profiles;
legacy credential maintenance rejects account overrides. Rotate a PAT through
named login. Fjord OIDC profiles support gateway requests and instance commands;
a stored Fjord gateway session cannot be replaced with an environment PAT.

## Named PAT profiles

```sh
fj auth login --host rasterhub.com --account qa --with-token
fj --account qa pr comment 42 --body 'Reviewed'
fj auth switch rasterhub.com --account qa
fj auth status
```

`--with-token` reads only stdin, even when `FJ_TOKEN` is set.
Login verifies the token and saves the named profile without changing another
account's default. With no token input, named login can migrate the matching
legacy account or re-verify an existing profile. `auth switch HOST --account`
verifies the stored credential before selecting that account and host as the
default. A first named login on a new host needs this explicit switch.

Descriptors/default pointers live under `[accounts.HOST]` in hosts.toml;
URL, API mount and proxy settings remain host-wide. Named login uses the
existing configured URL for a known host. Each PAT is stored in the
keychain or existing 0600 fallback under a distinct host/account/generation
key. Login stages and reads back a fresh generation before atomically
publishing metadata. Failure leaves the old active descriptor intact; retry
the same login. Old generations, legacy credential keys and legacy host
fields remain for at least one release, with no automatic cleanup.

Status reports configured counts, a redacted default marker and the number of
imported web sessions, not a fresh authentication result.

`fj auth token` needs `--account` (a bare token export on a profiled host stays
refused). `refresh`, `session` and `logout` act on the `--account` profile, or
the host's default profile, never on legacy credentials. A web
session cookie is bound to its profile and destination instance; if the
instance or URL changes the cookie is ignored until you import it again.
`session --with-cookie` reads stdin only. Logout removes the profile and its
secrets but keeps the default pointer, so a logged-out default fails instead of
using legacy credentials; recover with `auth switch HOST --account` or login.
`setup-git` stays refused on profiled hosts.

Remove an existing fj Git helper with `git config --global --unset-all
credential.https://HOST.helper` (replace HOST), then configure a standard Git
credential manager for Git authentication.

## Named Fjord OIDC profiles

Use `fj auth login --fjord --platform-url URL --account EMAIL` with loopback, `--manual`, or `--device`.
Select instances with `fj --host URL --account EMAIL instances use ID`; default switching and migration follow the rules above.

Named Fjord profiles support API, instance, status/web and mute
commands. Mute lists are independent per host, account and instance; rotating
credentials preserves the list. Retained legacy mute entries remain in their
legacy scope and are not implicitly applied to named profiles.

Work context/resume/continue remain legacy-host-based and are not yet profile-aware.
Work-session account/instance identity will be handled separately in 2b-iv.
