# Compact cell sets

A compact set is a list of cells that can mix levels. Large cells cover the middle of an area. Small cells cover the edges. The set stores the area with fewer rows than a single-level table.

The rules come from the `cell_compact_set` contract in `geosquare-v2`. The helpers live in `geosquare_engine.compact`. They use only `decode`, `parent`, and `children` from the Rust core.

## Rules in short

- Each row is a GID and its level. The level must equal the GID length.
- No row may sit inside another row.
- No complete group of children may be present. All 25 children of a 5 by 5 parent, or all 4 children of a 2 by 2 parent, must be replaced by the parent.
- Mixed levels are allowed in this one type.
- A reader rejects a non-canonical set. It does not fix the set for you. Call `canonicalize` first if you want the fixed version.

## Functions

| Function | What it does |
|---|---|
| `canonicalize(gids)` | Remove covered cells, then replace complete child groups with their parent. Returns a sorted tuple |
| `validate_compact_set(rows)` | Check rows of `(gid, level)`. Raises `CompactSetError` if a rule breaks |
| `expand(gids, level)` | All level-`level` cells covered by the set. A level below any row's level raises an error |
| `contains(gids, gid)` | `True` if the GID is a row, or lies under a row |
| `child_gids(gid)` | Direct children of a GID. Raises at level 14 |

Pass a `set` or `frozenset` to `contains` when you check many cells. The function then does not copy the set each time.

## Example

```python
import geosquare_engine as gs
from geosquare_engine import compact

parent = gs.encode(1, 0, 0)
children = [gs.encode(2, x, y) for y in range(2) for x in range(2)]

print(compact.canonicalize(children))    # ('2',)  the four children become their parent

rows = [(parent, 1), (children[0], 2)]
try:
    compact.validate_compact_set(rows)
except compact.CompactSetError as error:
    print("rejected:", error)            # a row lies inside another row
```

## Expansion and joins

- To join a compact set to a single-level table, use `contains` for each row. This is the default. It needs no extra memory.
- To reuse the set for many joins, use `expand` once, then join on equal GIDs.

Both ways give the same matched cells.

## Errors

| Error | When |
|---|---|
| `CompactSetError` | A rule breaks. It is a subclass of `ValueError` |
| `ValueError` | A GID is invalid |

## Tests

The vectors are in `vectors/spec-2.0/compact-set.json`. The tests run every case against the helpers. See `bindings/python/tests/test_compact.py`.
