# geosquare_v2 DuckDB extension

A loadable DuckDB extension for the GeoSquare V2 grid (spec 2.0). It wraps the Rust core in
`core/` through the DuckDB C extension API, so **one binary loads in DuckDB 1.4 to 2.0**
(tested on 1.4.0, 1.4.5, 1.5.0, 1.5.6 and 2.0.0). The same code builds for DuckDB-Wasm
(`wasm_eh`) for use in the browser.

SQL names follow `geosquare-v2` and the engine bindings. It is not related to the V1
extension `geosquare_grid_duckdb`.

## Install

Published repository (public, CORS enabled) with builds for every DuckDB release from 1.4.0
to 2.0.0: `linux_amd64` and `linux_arm64` (glibc 2.28+), `wasm_eh` (DuckDB-Wasm):

```sql
SET allow_unsigned_extensions = true;   -- the build is not signed by DuckDB
INSTALL geosquare_v2 FROM 'https://geosquare-static.s3.ap-southeast-3.amazonaws.com/geosquare-v2';
LOAD geosquare_v2;
```

DuckDB-Wasm: open the database with `allowUnsignedExtensions: true`, then the same `INSTALL`
and `LOAD` (or `GeosquareCellsWasm.open({ extension: '<that url>' })` from `examples/`).

DuckDB 1.4.5 cannot `INSTALL` from any `https://` repository: a regression in that release, it
also fails for `https://extensions.duckdb.org` and `https://community-extensions.duckdb.org`, and
makes no network request at all (1.4.4 and 1.5.0 work). Its default repositories use plain
`http://`, which still works, and so does `INSTALL geosquare_v2 FROM 'http://geosquare-static.s3.ap-southeast-3.amazonaws.com/geosquare-v2'`
(but this build is unsigned, so plain HTTP can be tampered with on the way). Safer on 1.4.5:
download over HTTPS and load the file, or upgrade DuckDB:

```sh
curl -sO https://geosquare-static.s3.ap-southeast-3.amazonaws.com/geosquare-v2/v1.4.5/linux_amd64/geosquare_v2.duckdb_extension.gz
gunzip geosquare_v2.duckdb_extension.gz   # then: INSTALL './geosquare_v2.duckdb_extension'; LOAD geosquare_v2;
```

macOS and Windows builds are not in the repository yet; `.github/workflows/duckdb-release.yml`
builds them. `scripts/publish_local.sh` rebuilds and republishes what a Linux workstation can
build. Other repositories: see `community/README.md`.

## Build and load

```zsh
cd bindings/duckdb
./build.sh            # -> build/linux_amd64/geosquare_v2.duckdb_extension
```

```sql
-- unsigned local build
SET allow_unsigned_extensions = true;   -- or start DuckDB with -unsigned
LOAD 'build/linux_amd64/geosquare_v2.duckdb_extension';
SELECT geosquare_engine_version();
```

`DUCKDB_PLATFORM` sets the platform written into the extension footer (default
`linux_amd64`); `CARGO_BUILD_TARGET` cross-compiles.

## DuckDB-Wasm (browser)

```zsh
source <emsdk>/emsdk_env.sh      # Emscripten 6.0.9, as in DuckDB's extension CI
rustup target add wasm32-unknown-emscripten
./build_wasm.sh                  # -> build/wasm_eh/geosquare_v2.duckdb_extension.wasm
```

```js
const db = new duckdb.AsyncDuckDB(logger, worker);   // @duckdb/duckdb-wasm, "base" bundle
await db.instantiate(bundle.mainModule);
await db.open({ allowUnsignedExtensions: true });
const c = await db.connect();
await c.query("LOAD 'https://<host>/geosquare_v2.duckdb_extension.wasm'");
await c.query("SELECT count(*) FROM query_cell_dataset('https://<host>/population', bbox := [106.8, -6.3, 106.9, -6.2])");
```

Tested with DuckDB-Wasm `1.33.1-dev118.0` (DuckDB v2.0.0, platform `wasm_eh`) in Node and
in headless Chromium 154: grid functions, polyfill, and reading flat, partitioned and
Iceberg datasets over HTTP give the same results as native DuckDB (`tests/wasm/run.sh`).
Over plain HTTP, folders cannot be listed, so the reader uses `manifest.json`,
`data/part-00000.parquet`, the partition index `gid_prefixes.json` (written by
`write_cell_dataset`) and `metadata/version-hint.text`. Iceberg tables are scanned with
`allow_moved_paths`, so a table written locally can be served from another location.
Writing is meant for the backend.

## Grid functions

| Function | Returns |
|---|---|
| `point_to_cell(domain_code, longitude, latitude, level)` | `STRUCT(domain_code, level, x_idx, y_idx, gid, uri, packed_id)` |
| `pack(domain, gid)` (domain = code or id) | `BIGINT` packed id |
| `pack_int64(domain_id, level, x_idx, y_idx)` | `BIGINT` |
| `unpack_int64(packed_id)` | `STRUCT(domain_id, level, x_idx, y_idx)` |
| `canonical_to_gid(level, x_idx, y_idx)` | `VARCHAR` |
| `gid_to_canonical(domain_code, gid)` | `STRUCT(domain_code, level, x_idx, y_idx)` |
| `parent(gid)` / `children(gid)` | `VARCHAR` / `VARCHAR[]` |
| `k_ring(gid, k)` / `k_disk(gid, k)` | `VARCHAR[]` |
| `distance_m(gid_a, gid_b)` / `cell_distance(uri_a, uri_b)` | `DOUBLE` metres |
| `cell_side_m(level)` | `DOUBLE` |
| `descendant_range(domain, parent_gid, child_level)` | `STRUCT(lo, hi)` packed range (spec 8.1) |
| `parse_uri(uri)` | `STRUCT(domain_code, gid)` |
| `cell_to_geometry(domain_code, gid [, output_crs [, format]])`, `cell_to_geometry(packed_id [, output_crs [, format]])` (domain from the packed id) | WKT or GeoJSON; `output_crs` `'grid'` (default, as in v2) or `'EPSG:4326'` |
| `polygon_to_cells(domain_code, wkt, level, ...)` | table: `domain_code, level, x_idx, y_idx, gid, uri, packed_id, coverage_ratio` |
| `polyfill(domain_code, wkt, level, ...)` | table: `gid, coverage_ratio` |
| `line_to_cells(domain_code, wkt, level, source_crs := ..., max_candidate_limit := ...)` | table: same cell columns + `length_ratio` (share of the whole line inside the cell, as in v2) |
| `packed_to_gid(packed_id)` | `VARCHAR` gid of a packed id |
| `detect_table(query, domain_code := ..., level := ..., max_rows := 100000)` | table: `column, kind, status, reason, valid_ratio, domain_codes, domain_ids, levels, invalid_examples` (spec 2.0 section 14; x_idx/y_idx as one pair; `max_rows` rows are read, 0 = all) |
| `v1_gid_to_cell(domain_code, gid_v1, level)` | the v2 cell holding the centre of a GeoSquare v1 cell, same `STRUCT` as `point_to_cell` |
| `v1_gid_center(gid_v1)` / `v1_gid_to_geometry(gid_v1 [, format])` | `STRUCT(lon, lat)` / the v1 cell box in lon/lat as WKT or GeoJSON |
| `lonlat_to_v1_gid(longitude, latitude, level_v1)` | `VARCHAR` v1 gid (v1 level 12 = 50 m, 9 = 1 km), identical to `@geosquare/grid` `lonLatToGid` |

`polygon_to_cells` / `polyfill` take `source_crs := 'EPSG:4326' | 'grid'`,
`coverage_mode := 'GRID_PLANAR' | 'EQUAL_AREA'`, `min_coverage := 0.0` and
`max_candidate_limit := 100000`, and follow `geosquare_v2.polyfill_stream`: vertices are
projected without densification, the intersection is taken in the grid plane, cells come in
row-major order, and a cell is kept when `ratio > 0 and ratio >= min_coverage`.
`EQUAL_AREA` measures both areas in Equal Earth (EPSG:8857), the profiles' equal-area CRS.
Invalid polygons (self-intersections, holes outside the shell, overlapping parts) are
repaired like `shapely.make_valid` (even-odd over all rings); valid input is untouched.

### Names compared with the Python and Wasm bindings

Every function of `bindings/python` (`geosquare_engine`) and `bindings/wasm` is here. Three
have another name in SQL because the binding name is taken by DuckDB, and one returns a
different shape:

| Python / Wasm | DuckDB | Why |
|---|---|---|
| `encode(level, x_idx, y_idx)` | `canonical_to_gid(level, x_idx, y_idx)` | `encode` is DuckDB's BLOB function |
| `decode(gid)` | `gid_to_canonical(domain_code, gid)` | `decode` is DuckDB's BLOB function (DuckDB 1.4 resolves `decode('<gid>')` to it) |
| `unpack(packed)` → `(domain_id, gid)` | `packed_to_gid(packed_id)` → gid, `unpack_int64(packed_id)` → `(domain_id, level, x_idx, y_idx)` | `UNPACK(...)` is DuckDB syntax |
| `version()` (Wasm), `__version__` (Python) | `geosquare_engine_version()` | a plain `version()` is DuckDB's own |
| `detectTable` (Wasm), `detect_table` (Python) | `detect_table(query, ...)` | takes a query instead of a dict of columns; dtype comes from the column type (VARCHAR → string, integers → int64, others → other) |

Same names: `pack`, `parent`, `children`, `descendant_range`, `k_ring`, `k_disk`,
`distance_m`, `cell_side_m`. Only in DuckDB: `point_to_cell`, `pack_int64`, `cell_distance`,
`parse_uri`, `cell_to_geometry`, `polygon_to_cells`, `polyfill`, `line_to_cells` and the dataset
functions.

Not registered: the engine names `encode`, `decode` and `unpack`. `encode`/`decode` are DuckDB
BLOB built-ins (DuckDB 1.4 resolves `decode('<gid>')` to the built-in) and `UNPACK(...)` is
DuckDB syntax. Use `canonical_to_gid`, `gid_to_canonical` and `unpack_int64`.

## Cell datasets: flat, partitioned, Iceberg

Worked examples for all three layouts (polygon, bbox, line and point queries, roll-ups,
joins, writes, snapshots, S3/HTTP, choosing a partition level):
[docs/query-examples.md](docs/query-examples.md). Every example there is run by
`tests/test_docs_examples.py`.
Creating and changing datasets (create, insert, update/upsert, delete, schema and partition
changes, for every layout): [docs/write-guide.md](docs/write-guide.md).
Ready-to-use wrappers for Node (`@duckdb/node-api`), the browser (DuckDB-Wasm) and Python,
with one API (`summarize`, `cells`, `rollup`, `atPoints`, `featureCollection`, `stream`,
`write`): [examples/](examples/README.md).

These follow the geosquare-v2 cell dataset contract (`manifest.json`, the manifest embedded
in every Parquet file under `geosquare.manifest`, files sorted by `packed_id`).

| Layout | Files |
|---|---|
| `flat` | `<path>/manifest.json`, `<path>/data/part-00000.parquet` |
| `partitioned` | `<path>/manifest.json`, `<path>/data/gid_prefix=<L>/part-00000.parquet` (spec 2.0 section 15) |
| `iceberg` | Iceberg format v2 table at `<path>`, partitioned by `truncate(W, packed_id)`; cell manifest in the table property `geosquare.manifest` |

```sql
-- write: the query needs packed_id or gid; missing identity columns are derived
SELECT * FROM write_cell_dataset(
  'SELECT packed_id, population FROM read_parquet(''src/*.parquet'')',
  '/data/population', layout := 'partitioned', partition_level := 5,
  value_semantics := 'COUNT', aggregation_rule := 'sum');

-- read the whole dataset (layout detected; Iceberg uses the newest metadata)
SELECT count(*) FROM read_cell_dataset('/data/population');

-- area query: polygon, line or point WKT (lon/lat) or bbox. Adds coverage_ratio (polygon,
-- bbox), length_ratio (LINESTRING/MULTILINESTRING) or point_count (POINT/MULTIPOINT)
SELECT sum(population * coverage_ratio)
FROM query_cell_dataset('/data/population', geometry := 'POLYGON ((...))', min_coverage := 0.0);
SELECT * FROM query_cell_dataset('s3://bucket/population', bbox := [106.8, -6.3, 106.9, -6.2]);
```

`write_cell_dataset(query, path, ...)` options: `layout`, `mode := 'create' | 'overwrite' |
'append'` (append is Iceberg only), `domain_code`, `level`, `partition_level` (default 5),
`row_group_size` (default 8192), `identity := 'query' | 'compact'`, `value_semantics`,
`aggregation_rule`, `value_fields` / `coverage` / `source` (JSON text overriding those
manifest sections) and `notes`. `value_semantics` / `aggregation_rule` take one word for all
numeric columns or a JSON object per column, e.g. `'{"population": "COUNT", "score":
"MEASUREMENT"}'`. Value columns keep their own names and types; any number of them.
Before writing it checks one domain, one level, unique cells, and that gid, packed_id, x_idx
and y_idx agree; an Iceberg append also refuses cells already in the table.

Descriptive metadata, at write time or later with `update_cell_metadata(path, ...)` (no data
rewrite; Iceberg gets a new metadata file with the same snapshot):

- `units` — text for every value column, or `'{"people_*": "people", "hazard_eq": "index 0-1"}'`
  (`*` patterns; exact names win) → `value_fields[].unit` of the cell manifest
- `source := '{"source_dataset": ..., "source_crs": ..., "source_row_count": ..., "format": ...}'`
  and `coverage := '{"assignment_method": ..., "boundary_policy": ..., ...}'` — merged into the
  manifest sections; keys outside the geosquare-v2 contract are refused
- `title`, `description`, `data_period`, `data_version`, `column_descriptions` (same form as
  `units`) — not in the v2 contract, so kept in `dataset_info` (Iceberg table property
  `geosquare.dataset_info` and each column's schema `doc`; `dataset_info.json` for flat and
  partitioned); `notes` → `provenance.notes`

Read it back with `cell_dataset_info(path)` (one row: title, description, period, version,
source, coverage, provenance, manifest) and `cell_dataset_columns(path)` (one row per value
column: type, semantics, aggregation rule, unit, description).

Numeric value columns are written with 0 in place of NULL (`fill_nulls`, default `true`;
the column type is kept, text columns are left alone). Pass `fill_nulls := false` when NULL
means "not measured" and must stay distinct from 0.

Iceberg manifests are written with the Avro `deflate` codec, one manifest per 128 data
files (in packed_id order) with each manifest's partition range in the manifest list.

Reading an Iceberg table with `query_cell_dataset`: the table is found through
`metadata/version-hint.text` (no listing), the manifest list and only the manifests whose
partition range covers the query are read once per process and cached (Iceberg metadata
files are never rewritten), the data files are picked from the query cells' partition
values or packed_id bounds, and read with `read_parquet`. Tables with delete files or
nested column types fall back to `iceberg_scan`. `read_cell_dataset` (whole table) uses
`iceberg_scan`. `column_metrics` sets which
columns get per-file counts and lower/upper bounds in the manifest: `'auto'` (default,
Iceberg's rule: the first 100 columns plus the cell id columns), `'key'` (gid, packed_id,
x_idx, y_idx only; use it for wide tables, where bounds on hundreds of value columns make
every manifest, and so every query plan, large) or `'all'`.

For an `s3://` target DuckDB writes the data files to a local staging folder and the
extension uploads them in parallel (`GEOSQUARE_V2_UPLOAD_CONCURRENCY`, default 32), since
one remote COPY per partition is slow with thousands of partitions. The folder is
`$GEOSQUARE_V2_STAGING_DIR` or the system temp folder, needs room for the written data,
and is removed afterwards. Metadata is written only after every data file is uploaded.

Partitions are written in parallel on the internal connections (`GEOSQUARE_V2_WRITE_THREADS`,
default 8; 1 in DuckDB-Wasm). The staged rows live in an in-memory database attached as
`gs_stage_mem` for the duration of a write, so every connection can read them.

Modes: `create` refuses an existing target; `append` adds an Iceberg snapshot;
`overwrite` replaces a flat/partitioned folder (old objects deleted) or writes an Iceberg
`overwrite` snapshot holding only the new rows. An Iceberg overwrite may add or drop
columns (new schema id, unchanged columns keep their field ids) and change
`partition_level` (new partition spec); older snapshots stay readable by metadata path.

### Writing to S3

Every layout writes straight to `s3://bucket/prefix`. Parquet goes through DuckDB `COPY`, so
DuckDB needs an S3 secret (`CREATE SECRET (TYPE s3, PROVIDER credential_chain)` or with
keys). The Iceberg metadata, manifest reads and `overwrite` deletes use the extension's
native S3 client, which takes endpoint, region, URL style and SSL from that same DuckDB
secret (DuckDB never reveals the secret key) and the keys from, in order: `s3_access_key_id`
/ `s3_secret_access_key` / `s3_session_token` parameters, `AWS_ACCESS_KEY_ID` /
`AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN`, the `~/.aws/credentials` profile
(`AWS_PROFILE`), then container/instance/web-identity credentials. `s3_region`,
`s3_endpoint`, `s3_url_style` and `s3_use_ssl` override the secret. Tested against an S3
emulator (`tests/test_s3.py`); pyiceberg reads the result.

Lines follow `geosquare_v2.line_to_cells` (identical on test lines); a segment lying exactly on
an internal cell edge goes to the cell on its high side (spec 2.0 half-open rule), so the
ratios of a line always sum to 1, whereas the Shapely oracle can count such a segment twice.

`snapshot_id := <id>` / `as_of := <timestamp>` on `read_cell_dataset`, `query_cell_dataset` and
`query_cell_datasets` read an older Iceberg snapshot.

`compact_cell_dataset(path, partition_level := <current>)` rewrites a dataset with one file per
partition and keeps its metadata.

`expire_snapshots(path, retain_last := 1, older_than := NULL, orphan_age_hours := 24, dry_run := false)`
drops old Iceberg snapshots and deletes the files only they used (see
[docs/write-guide.md](docs/write-guide.md#clean-up-old-snapshots-iceberg)).

`query_cell_datasets([path, ...], ...)` queries several datasets over one area and joins them per
cell (same parameters as `query_cell_dataset`, plus `fill_nulls`).

`read_cell_dataset` / `query_cell_dataset` take `rollup_level := L` for one row per level-L
ancestor cell (1 km = 9, 10 km = 7), each value column aggregated by its manifest
`aggregation_rule` and weighted by `coverage_ratio`; `rollup_rule` overrides the rule. See
[docs/query-examples.md](docs/query-examples.md#aggregates-and-roll-ups).

`read_cell_dataset` / `query_cell_dataset` take `with_geometry := 'wkt' | 'geojson'` (and
`geometry_crs := 'EPSG:4326'` (default) `| 'grid'`) to add each cell's outline as a
`cell_geometry` column; the domain comes from the dataset, so no `domain_code` is needed.

`read_cell_dataset` / `query_cell_dataset` accept `domain_code` and `level` for tables
without a cell manifest. `query_cell_dataset` reads only the `gid_prefix` folders that the
area touches, and for Iceberg only the data files of the area's partitions (see above).

Newest Iceberg metadata: `metadata/version-hint.text` when present, else the three
highest-numbered `*.metadata.json` files ranked by `last-updated-ms`. Writes update the
version hint, so `iceberg_scan('<table dir>')` also sees the newest snapshot.

## Limits

- Iceberg tables have no catalog: a commit refuses to land if another writer moved the
  table head during the write, but there is no lock. Remote writes support `s3://` only
  (MinIO, R2 and other S3-compatible stores through an endpoint).
- Flat/partitioned `overwrite` deletes then writes, so readers can see a partial folder
  during the write; Iceberg overwrite is atomic per snapshot.
- Boundary predicates (operational country boundary) are not in the engine.
- Dataset scans and writes run on internal connections opened at LOAD: 32 by default,
  `GEOSQUARE_V2_CONNECTIONS` (1 to 1024) before LOAD changes it. Each database that loads
  the extension gets its own connections, so several databases in one process stay
  separate (`tests/test_multi_db.py`); those connections keep the database open until
  the process ends.
- The Wasm build has no native S3 client (writing is for the backend).

## Tests

```zsh
tests/wasm/run.sh                                       # DuckDB-Wasm in headless Chromium vs native
./build.sh
tests/run_versions.sh                                   # spec 2.0 vectors on DuckDB 1.4.0 ... 2.0.0
TESTS="tests/test_vectors.py tests/test_datasets.py tests/test_iceberg_planned.py tests/test_fill_nulls.py tests/test_docs_examples.py tests/test_multi_db.py tests/test_detect.py" tests/run_versions.sh
python tests/test_metadata.py build/linux_amd64/geosquare_v2.duckdb_extension   # needs pyiceberg, geosquare_v2
python tests/test_iceberg_metrics.py build/linux_amd64/geosquare_v2.duckdb_extension   # needs pyiceberg
# needs geosquare_v2, shapely, pyiceberg:
python tests/test_polyfill_oracle.py build/linux_amd64/geosquare_v2.duckdb_extension ../geosquare-v2
python tests/test_oracles.py build/linux_amd64/geosquare_v2.duckdb_extension ../geosquare-v2 [pyiceberg table dir]
python tests/test_area_valid_oracle.py build/linux_amd64/geosquare_v2.duckdb_extension ../geosquare-v2   # EQUAL_AREA, make_valid (slow oracle)
# S3 (use an emulator such as moto_server):
S3_TEST_ENDPOINT=127.0.0.1:5555 AWS_ACCESS_KEY_ID=testing AWS_SECRET_ACCESS_KEY=testing python tests/test_s3.py build/linux_amd64/geosquare_v2.duckdb_extension
```

## Bahasa Indonesia

Ekstensi DuckDB untuk grid GeoSquare V2. Pasang dengan `INSTALL geosquare_v2 FROM
'https://geosquare-static.s3.ap-southeast-3.amazonaws.com/geosquare-v2'` (Linux amd64/arm64 dan
DuckDB-Wasm, DuckDB 1.4.0–2.0.0; untuk 1.4.5 unduh file dulu, lihat bagian Install). Satu binary bisa di-load di DuckDB 1.4 sampai 2.0,
dan kode yang sama dibuild untuk DuckDB-Wasm (browser) lewat `build_wasm.sh`.
Nama fungsi mengikuti `geosquare-v2`. Fungsi dataset bisa membaca dan menulis folder flat,
folder berpartisi `gid_prefix`, dan tabel Iceberg (metadata terbaru dipakai otomatis).
Contoh query lengkap untuk flat, partitioned, dan Iceberg (polygon, bbox, garis, titik,
roll-up ke 1 km/10 km, join, penulisan, snapshot, S3/HTTP, pemilihan level partisi) ada di
[docs/query-examples.md](docs/query-examples.md); semua contohnya dijalankan oleh tes.
Panduan membuat dan mengubah dataset (create, insert, update/upsert, delete, ubah kolom dan
partisi) untuk ketiga layout ada di [docs/write-guide.md](docs/write-guide.md).
Skrip siap pakai untuk backend Node, frontend (DuckDB-Wasm) dan Python ada di
[examples/](examples/README.md).
Beberapa nama fungsi berbeda dari binding Python/Wasm karena bentrok dengan DuckDB (`encode` → `canonical_to_gid`,
`decode` → `gid_to_canonical`, `unpack` → `packed_to_gid` / `unpack_int64`, `version` → `geosquare_engine_version`);
daftarnya di bagian "Names compared with the Python and Wasm bindings".
Batasan dan cara uji ada di bagian Limits dan Tests di atas.
