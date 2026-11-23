# Python API

Package: `geosquare-engine`. Import it as `geosquare_engine`.

```python
import geosquare_engine as gs
```

Requires Python 3.11 or newer. Read [Concepts](concepts.md) first.

## Grid functions

| Function | What it does | Returns |
|---|---|---|
| `encode(level, x_idx, y_idx)` | Make the GID for one cell from its indices | `str` |
| `decode(gid)` | Read the level and indices from a GID | `(level, x_idx, y_idx)` |
| `parent(gid)` | Get the parent cell | `str`, or `None` for the root |
| `children(gid)` | Get the direct children | `list[str]` |
| `descendant_range(domain_id, parent_gid, child_level)` | Get the Int64 range that covers the descendants at a level. Some values inside the range are not valid codes | `(low, high)` |
| `k_ring(gid, k)` | Cells at exactly `k` steps away | `list[str]` |
| `k_disk(gid, k)` | Cells at most `k` steps away, including the cell itself | `list[str]` |
| `distance_m(a, b)` | Distance in metres between two cells at the same level | `float` |
| `cell_side_m(level)` | Side length of a cell at a level, in metres | `float` |

Example:

```python
gid = gs.encode(9, 0, 0)
level, x_idx, y_idx = gs.decode(gid)
print(gs.parent(gid))          # the level 8 cell above it
print(len(gs.children(gid)))   # 25 for a 5 by 5 split
print(gs.cell_side_m(9))       # 1000.0
```

`children` raises `ValueError` for a level 14 cell, because it has no children.

## Packing

| Function | What it does |
|---|---|
| `pack(domain_id, gid)` | Join a domain ID and a GID into one signed Int64 |
| `unpack(packed)` | Split a packed value back into `(domain_id, gid)` |

Domain IDs are 1 to 511. An inconsistent packed value raises an error.

## Polygons

```python
ring = gs.cell_polygon_wgs84(1, gid, 1000.0, False)
```

| Argument | Meaning |
|---|---|
| `domain_id` | Domain ID, for example 1 for ID |
| `gid` | The cell |
| `max_segment_length_m` | Longest edge piece, in metres. Default `1000.0` |
| `allow_candidate_crs` | Set to `True` only for candidate domains. Default `False` |

Returns a closed ring of `(longitude, latitude)` pairs in WGS 84.

## Detecting columns

`detect_table` checks which columns hold GIDs, packed IDs, or x and y indices.

```python
results = gs.detect_table(
    {"gid": ("string", ["2345", "2346"]), "share": ("other", [0.1, 0.2])},
    domain="ID",
)
for result in results:
    print(result["column"], result["kind"], result["status"])
```

Each column is `(dtype, values)`. Allowed dtypes are `"string"`, `"int64"`, and `"other"`. A bool in an int column, a float in a string column, or an int beyond Int64 raises an error.

Each result is a dict with at least `column`, `kind`, `status`, and `levels`. A bare GID with no domain has status `needs_domain`.

## Compact cell sets

The `compact` module works on compact sets. See [Compact cell sets](compact-sets.md).

```python
from geosquare_engine import compact
```

## Version

```python
print(gs.__version__)
```
