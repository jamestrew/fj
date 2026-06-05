# Stack annotation in PR descriptions

`fj stack sync` mirrors the stack into each PR description so the stack is
visible from anything that renders a Forgejo PR (the web UI, the Fjord mobile
apps) without reading any local fj state. The annotation is a convenience, not
the source of truth: the authoritative stack lives in `<git-dir>/fj/stack.json`
on the author's machine, and the server-side truth of which PRs chain together
is always the PR base/head graph.

This document is the contract for consumers that want the richer fj-authored
grouping (stack name, ordering, membership including entries that have no PR
yet) instead of, or in addition to, inferring the chain from the PR graph.

## Where it lives

Inside every PR that belongs to the stack, the body contains a block delimited
by two HTML-comment markers:

```
<!-- fj-stack:start -->
...
<!-- fj-stack:end -->
```

Everything outside those markers is human-authored and preserved across
re-syncs. Everything inside is regenerated on each `fj stack sync` (unless the
user passes `--no-annotate`). To locate the block, search for the literal
`<!-- fj-stack:start -->` and `<!-- fj-stack:end -->`.

## Machine-readable payload

The first line inside the block is a one-line HTML comment carrying the stack as
JSON:

```
<!-- fj-stack-data: {"version":1,"name":"Login flow","base":"main","current":2,"entries":[...]} -->
```

To read it: find `<!-- fj-stack-data: ` and the next ` -->`; the text between
them is a JSON document. Parse it with any JSON parser.

### Schema (version 1)

```json
{
  "version": 1,
  "name": "Login flow",
  "base": "main",
  "current": 2,
  "entries": [
    { "position": 1, "branch": "stephen/login-1", "title": "Add form", "pr": 7 },
    { "position": 2, "branch": "stephen/login-2", "title": "Wire API", "pr": 8 },
    { "position": 3, "branch": "stephen/login-3", "title": "Add tests", "pr": null }
  ]
}
```

| Field              | Type            | Meaning                                                        |
| ------------------ | --------------- | -------------------------------------------------------------- |
| `version`          | integer         | Schema version. Check it; ignore the block if it's newer.      |
| `name`             | string          | Human-readable stack name.                                     |
| `base`             | string          | Trunk branch the stack ultimately targets.                     |
| `current`          | integer         | 1-based position of the entry whose PR this payload lives in.  |
| `entries`          | array           | Stack entries, ordered bottom (base) to top.                   |
| `entries[].position` | integer       | 1-based position from the base upward.                         |
| `entries[].branch` | string          | The entry's branch name.                                       |
| `entries[].title`  | string          | The entry's title.                                             |
| `entries[].pr`     | integer \| null | PR number, or `null` when the entry has no PR yet.             |

Notes:

- The same stack payload appears in every PR of the stack; only `current`
  differs per PR. Reading any one PR tells you the whole stack and which entry
  you're looking at.
- If a string value (e.g. a title) contains the three characters that close an
  HTML comment, the closing angle bracket is emitted as its JSON unicode escape
  so the comment can't terminate early. A standard JSON parser decodes it back,
  so the value round-trips. Just parse the JSON; no special-casing needed.
- `version` is bumped only on incompatible changes. Treat an unknown (newer)
  version as "ignore the payload and fall back to the PR graph."

## Recommended consumption

Prefer the PR base/head graph as the primary source: it's tool-agnostic, always
present, and reflects server truth. Use this payload to enrich that view with
the stack's name, intended order, and any entries that don't have PRs yet. If
the payload is absent (the author never ran `sync`, or used `--no-annotate`),
the graph alone still yields a usable lane.
