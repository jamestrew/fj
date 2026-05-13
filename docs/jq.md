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

Bracket form and dot form are interchangeable: `.data.0.name` and
`.data[0].name` mean the same thing.

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

## Errors

- A missing field returns a hard error with the field name.
- An out-of-range index returns a hard error with the index.
- Negative indices that overshoot (e.g. `[-5]` on a 3-element array)
  also error.

## Not supported

The following are intentionally NOT implemented; reach for real jq:

- Filters like `.[] | select(.state == "open")`
- Functions: `map`, `length`, `keys`, etc.
- String interpolation
- Multiple outputs from a single expression
