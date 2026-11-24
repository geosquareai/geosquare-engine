# Ready-to-use readers and writers

Wrappers around the `geosquare_v2` extension with one API for backend and frontend. They
read every layout (flat, `gid_prefix`-partitioned, Iceberg) the same way; the layout is
detected from the path.

| Where | File | DuckDB |
|---|---|---|
| Node backend | [ts/src/node.ts](ts/src/node.ts) `GeosquareCells` | `@duckdb/node-api` (1.4.5+) |
| Browser frontend | [ts/src/web.ts](ts/src/web.ts) `GeosquareCellsWasm` | `@duckdb/duckdb-wasm` + `geosquare_v2.duckdb_extension.wasm` |
| Python (pipelines, notebooks) | [python/geosquare_cells.py](python/geosquare_cells.py) `GeosquareCells` | `duckdb` (1.4+) |
| Shared SQL | [ts/src/sql.ts](ts/src/sql.ts) | builders, `plain()`, GeoJSON → WKT |

| Method | Returns |
|---|---|
| `summarize(path, area, columns, {weighted, minCoverage, coverageMode})` | `{cells, <column>: total}`; weighted by `coverage_ratio` by default (best estimate for counts) |
| `cells(path, area, {columns, minCoverage, withGeometry, limit})` | one row per cell: `gid`, `packed_id` (text), `coverage_ratio` / `length_ratio` / `point_count`, columns |
| `rollup(path, area, columns, level)` | totals per parent cell (9 = 1 km, 7 = 10 km) with a GeoJSON outline |
| `atPoints(path, [[lon, lat], ...], columns)` | the values of the cell under each point, in input order |
| `featureCollection(path, area, columns)` | GeoJSON FeatureCollection of the cells (for MapLibre, Leaflet, deck.gl) |
| `stream(path, onBatch, {area, columns, batch})` | Node/Python: batches of rows, for exports |
| `write(query, path, {layout, mode, partitionLevel, ...})` | Node/Python: `{layout, path, files, rows}` |
| `columns(path)`, `query(sql)` | column types; any SQL with the extension loaded |

`area` is `{ wkt }`, `{ geojson }` (a GeoJSON geometry in lon/lat) or `{ bbox: [xmin, ymin, xmax, ymax] }`.
Polygons give `coverage_ratio`, lines `length_ratio`, points `point_count`.

## Node (backend)

```ts
import { GeosquareCells } from './src/node';

const gs = await GeosquareCells.open({
  extension: '/opt/geosquare_v2.duckdb_extension',            // or the repository URL: INSTALL ... FROM
  s3: { provider: 'credential_chain', region: 'ap-southeast-3' },
});
const PEOPLE = 's3://geosquare-data/apps-data/data_v2/data_name=people_v2023';
const total = await gs.summarize(PEOPLE, { geojson: polygon }, ['people_counts', 'people_gen_z_night']);
const km = await gs.rollup(PEOPLE, { geojson: polygon }, ['people_counts'], 9);
await gs.write("SELECT packed_id, sales FROM read_parquet('sales.parquet')", 's3://bucket/v2/sales',
  { layout: 'iceberg', partitionLevel: 7, valueSemantics: 'COUNT', aggregationRule: 'sum' });
gs.close();
```

Keep one instance per process: Iceberg metadata and manifests are cached in the process,
so later requests skip them. Ready CLIs:

```sh
cd ts && npm install
GEOSQUARE_EXTENSION=/opt/geosquare_v2.duckdb_extension npx tsx examples/node-query.ts area.geojson people_counts people_gen_z_night
npx tsx examples/node-write.ts points.csv s3://bucket/v2/points iceberg      # lon, lat, value columns -> 50 m sums
```

## Browser (frontend, DuckDB-Wasm)

```ts
import { GeosquareCellsWasm } from './src/web';

const gs = await GeosquareCellsWasm.open({ extension: 'https://cdn.example.com/geosquare/geosquare_v2.duckdb_extension.wasm' });
const total = await gs.summarize('https://cdn.example.com/v2/people', { geojson: drawnPolygon }, ['people_counts']);
map.getSource('cells').setData(await gs.featureCollection(url, { bbox: map.getBounds().toArray().flat() }, ['people_counts']));
```

- Data over HTTP(S)/CDN needs no listing: the writer leaves `gid_prefixes.json` (partitioned)
  and `metadata/version-hint.text` (Iceberg). The host must send CORS headers and support
  `Range` requests.
- S3 directly: pass temporary keys (`s3: { keyId, secret, sessionToken, region }`, e.g. from STS
  via your backend) and allow CORS on the bucket.
- Only the columns a query uses are fetched, so ask for the columns you need.
- Demo page: `cd ts && node examples/web/serve.mjs <dataset dir>` then open http://127.0.0.1:8798/www/.

## Python

```python
from geosquare_cells import GeosquareCells

with GeosquareCells("/opt/geosquare_v2.duckdb_extension", s3_region="ap-southeast-3") as gs:
    gs.summarize(PEOPLE, {"bbox": [106.80, -6.20, 106.84, -6.16]}, ["people_counts"])
    gs.df(gs.cells_sql(PEOPLE, {"wkt": "POLYGON ((...))"}, ["people_counts"]))   # pandas
    gs.write("SELECT packed_id, sales FROM read_parquet('sales.parquet')", "s3://bucket/v2/sales", layout="iceberg")
```

## Tests

```sh
cd ts && npm install
EXT=../../build/linux_amd64/geosquare_v2.duckdb_extension npx tsx test/node.test.ts /tmp/gs_examples   # writes 3 layouts, reads them
CHROMIUM=<chrome-headless-shell> node test/web.test.mjs /tmp/gs_examples                                # same results in DuckDB-Wasm
cd ../python && python test_geosquare_cells.py ../../build/linux_amd64/geosquare_v2.duckdb_extension /tmp/gs_examples
```

Tested: Node with `@duckdb/node-api` 1.4.5-r.1, DuckDB-Wasm 1.33.1-dev118.0 in Chromium,
Python with duckdb 1.4.0 and 2.0.0; results agree across the three layouts and the three
runtimes (Wasm and native differ only after ~10 significant digits).
