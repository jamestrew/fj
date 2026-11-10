# Choose an account for one invocation

Use `--account LOGIN` or `FJ_ACCOUNT` with an environment PAT to act as a
non-default Forgejo account. This works for rasterhub identities and accounts
on independent Forgejo hosts, using each host's own PAT.

```sh
FJ_TOKEN="$QA_PAT" fj --account qa pr review 42 --approve
FJ_TOKEN="$QA_PAT" FJ_ACCOUNT=qa fj pr comment 42 --body 'Reviewed'
FJ_TOKEN="$QA_PAT" fj --account qa issue create --title 'Follow-up'
```

The flag overrides `FJ_ACCOUNT`. With neither set, authentication is unchanged.
The account and token are not written to hosts.toml, keychain or tokens.toml.
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

Explicit account selection requires an environment PAT. It never falls back
to a stored credential. Before making API requests, fj checks the token's
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
Synchronous direct HTTP helpers still require completed verification. Stored-auth commands (`fj auth`, `fj instances`) reject
account overrides. This step supports direct Forgejo PAT hosts; a stored
Fjord Account gateway session cannot be replaced with a PAT. Named profiles
and separate Fjord Account bearer sessions require a later storage design.
