# `fj api --jq` syntax

`fj api` ships a small jq-flavored JSON projector. It's not a full jq
implementation; it covers the 95% of cases gh users use `--jq` for.

If you need anything more complex, pipe the raw JSON through real `jq`:

```sh
fj api /repos/foo/bar | jq '.<full expression>'
```

## Stages

Expressions are split on `|` into stages. Each stage runs against the
output of the previous stage.

```
fj api /repos/foo/bar | head
# vs
fj api /repos/foo/bar -q '.owner | .login'
```

## Path segments

Within a stage, dot-separated segments traverse the JSON tree. Each
segment is one of:

| Segment      | Meaning                                       |
| ------------ | --------------------------------------------- |
| `.field`     | Object field lookup                           |
| `.0`         | Array index (positional)                      |
| `.[0]`       | Same as `.0`, bracket form                    |
| `.[-1]`      | Last element of an array                      |
| `.[-2]`      | Second-to-last element                        |
| `.[]`        | Iterate: map the rest of the path over every element |

Bracket form and dot form are interchangeable: `.data.0.name` and
`.data[0].name` mean the same thing.

`.[]` fans out. Whatever follows it is applied to every element of the
array (or every value of an object) and the results are collected back
into an array. So `.[].id` over a list of objects yields a list of ids,
and a bare `.[]` yields the elements themselves. It pairs naturally with
`--paginate`:

```sh
fj api /repos/foo/bar/pulls --paginate -q '.[].number'
```

## Examples

Given:

```json
{
  "data": [
    {"id": 1, "owner": {"login": "alice"}},
    {"id": 2, "owner": {"login": "bob"}}
  ],
  "total": 2
}
```

| Query                          | Output             |
| ------------------------------ | ------------------ |
| `.total`                       | `2`                |
| `.data.0.id`                   | `1`                |
| `.data[0].owner.login`         | `"alice"`          |
| `.data[-1].owner.login`        | `"bob"`            |
| `.data | .[0]`                 | first element      |
| `.data[0] | .owner | .login`   | `"alice"`          |
| `.data[].id`                   | `[1, 2]`           |
| `.data[].owner.login`          | `["alice", "bob"]` |

## Errors

- A missing field returns a hard error with the field name.
- An out-of-range index returns a hard error with the index.
- Negative indices that overshoot (e.g. `[-5]` on a 3-element array)
  also error.

## Not supported

The following are intentionally NOT implemented; reach for real jq:

- Filters: `select(.state == "open")`
- Functions: `map`, `length`, `keys`, etc.
- String interpolation
- Iterating across a pipe boundary (`.[] | .title`); keep the iterate and
  its path in one stage instead (`.[].title`)
