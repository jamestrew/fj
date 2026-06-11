# Exit codes

`fj` keeps command success at `0` and uses stable non-zero exit codes for failure classes that scripts commonly need to branch on. Clap usage errors keep clap's own exit code.

| Code | Class | Meaning |
| ---: | --- | --- |
| 1 | Generic failure | Any failure that does not match a more specific class. |
| 3 | Not found | Forgejo returned HTTP 404 for the requested resource. |
| 4 | Auth or forbidden | Authentication failed or Forgejo returned HTTP 401/403. |
| 5 | Rate limit or transient | Forgejo returned HTTP 429 or a 5xx server error. |

For HTTP 403, 404, and 429, normal error output starts with a short human-readable headline and avoids raw `/api/v1` URLs. Use `--debug` or `FJ_DEBUG=1` to include the underlying API error chain with the full request URL.
