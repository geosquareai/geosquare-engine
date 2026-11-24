# Quick start

This page runs each example once. The output under each block is what we saw when we ran it.

Read [Concepts](concepts.md) if a GID or a level is new to you.

## 1. Python

Install the package:

```zsh
pip install geosquare-engine
```

Run the examples:

```python
import geosquare_engine as gs

gid = gs.encode(12, 475168, 491792)
print(gid)                           # H2X545M5X4E2
print(gs.decode(gid))                # (12, 475168, 491792)
print(gs.cell_side_m(12))            # 50.0
```

Every function is listed in [Python API](python-api.md).

## 2. pandas and Polars: `df.gsq`

Import the accessor once. pandas or Polars must be installed for this part. The engine itself has no
dependency on them.

```python
import pandas as pd
import geosquare_engine.pandas_gsq  # registers df.gsq

df = pd.DataFrame({"gid": ["H2X545M5X4E2", "H2X545M5X4E3"], "value": [1, 2]})
print(df.gsq.parent(9).tolist())     # ['H2X545M5X', 'H2X545M5X']
print(df.gsq.level().tolist())       # [12, 12]
```

Polars works the same way:

```python
import polars as pl
import geosquare_engine.polars_gsq  # registers df.gsq

pdf = pl.DataFrame({"gid": ["H2X545M5X4E2"]})
print(pdf.gsq.parent(9).to_list())   # ['H2X545M5X']
```

`to_geo(domain_id)` adds a `geometry` column with the cell polygon in WGS 84. If geopandas is
installed, the result is a GeoDataFrame. Otherwise the geometry is GeoJSON text.

## 3. DuckDB SQL

Build the extension first. See [the DuckDB README](../bindings/duckdb/README.md) for the
build steps. This example uses a local build on macOS. Start the `duckdb` shell from `bindings/duckdb`,
so the relative path works:

```zsh
cd bindings/duckdb
duckdb -unsigned
```

```sql
LOAD 'build/osx_arm64/geosquare_v2.duckdb_extension';

SELECT point_to_cell('ID', 106.8, -6.2, 12).gid AS gid;
SELECT parent('H2X545M5X4E2') AS parent_gid;
SELECT count(*) AS cells
FROM polyfill('ID', 'POLYGON ((106.8 -6.2, 106.81 -6.2, 106.81 -6.19, 106.8 -6.19, 106.8 -6.2))', 12);
```

Output, trimmed to the values:

```text
gid           H2X545V2H495
parent_gid    H2X545M5X4E
cells         529
```

`point_to_cell` returns a struct. Use `.gid` to read the GID. `polyfill` returns one row per cell
that covers the polygon.

Candidate domains need one extra argument. `point_to_cell('VN', ...)` without it fails with
`domain VN is not released`. Add `true` only for technical work. It does not make the domain released:

```sql
SELECT point_to_cell('VN', 107.868144, 15.981051, 5, true).gid AS vn;
```

```text
vn    H3T4T
```

For the full function list, see [the DuckDB README](../bindings/duckdb/README.md).

## 4. Command line: `gsq`

Install the CLI and point it at the extension:

```zsh
python -m pip install ./cli
export GSQ_EXTENSION="$PWD/bindings/duckdb/build/osx_arm64/geosquare_v2.duckdb_extension"
```

Make a small file:

```zsh
printf "gid,population\nH2X545M5X4E2,10\nH2X545M5X4E3,20\n" > cells.csv
printf "gid,population\nH2X545M5X4E2,10\nH2X545M5X4E2,5\nbad!,1\n" > bad.csv
```

### `gsq check`

Checks GIDs before you upload a file. It needs `--domain`.

```zsh
gsq check cells.csv --domain ID
```

Output:

```text
cells.csv: 2 rows, domain ID, levels [12]
OK
```

On the bad file, the command exits with status 1:

```zsh
gsq check bad.csv --domain ID
```

```text
bad.csv: 3 rows, domain ID, levels [12]
  invalid GID at row 3: bad!
  duplicate GID H2X545M5X4E2: 2 rows
2 problem(s) found
```

### `gsq export`

Writes one GeoParquet polygon per row. It also writes a Kepler.gl config when you pass `--kepler`.

```zsh
gsq export cells.csv --domain ID -o cells.parquet --kepler cells.kepler.json --color population
```

The command writes `cells.parquet` and `cells.kepler.json`. It prints nothing when it works.

### `gsq pyramid`

Writes levels 12, 10, and 8 for a map. The input must be a level 12 dataset folder with a
`manifest.json` (manifest 1.1). A plain CSV is refused, so this example is not run here:

```zsh
gsq pyramid dataset12/ --domain ID -o out/pyramid --value population=sum
```

See [the CLI README](../cli/README.md) for every option.

## 5. Browser

The npm package is not published yet. Build it from this repo:

```zsh
cargo install wasm-bindgen-cli --version 0.2.130 --locked   # once
cd bindings/wasm
npm run build
```

The build writes `pkg/`. Import from it in your page:

```js
import init, { encode, decode, pointToCell, cellPolygonWgs84 } from "./pkg/geosquare_wasm.js";

await init(); // call once before any other function

const gid = encode(12, 475168, 491792);
console.log(gid, decode(gid).level);                 // H2X545M5X4E2 12
const cell = pointToCell(1, 118.01521445, -2.55029565, 5, false);
console.log(cell, cellPolygonWgs84(1, cell, 25000, false).length);   // H5222 17
```

Rollups use `geosquare-engine/rollup` (`js/rollup.mjs`) and the map layer uses `js/deck.mjs`.
See [the WebAssembly README](../bindings/wasm/README.md).

Run the deck.gl example from a local web server:

```zsh
cd bindings/wasm
python3 -m http.server 8000
# open http://localhost:8000/examples/deck-basic.html
```

## Next steps

- [Concepts](concepts.md)
- [Python API](python-api.md)
- [Compact cell sets](compact-sets.md)
- [CLI README](../cli/README.md)
- [WebAssembly README](../bindings/wasm/README.md)
