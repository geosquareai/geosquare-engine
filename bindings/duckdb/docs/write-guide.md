# Writing cell datasets: create, insert, update, delete

How to create a cell dataset and change it later, for flat Parquet, `gid_prefix`-partitioned
Parquet and Iceberg. Every SQL block here runs in `tests/test_docs_examples.py` (paths under
`/data/` point at a temporary folder; blocks starting with `-- not run in tests` need S3).

| Operation | flat / partitioned | Iceberg |
|---|---|---|
| create | `mode := 'create'` (default) | `mode := 'create'` (default) |
| insert new cells | rewrite: old + new, `mode := 'overwrite'` | `mode := 'append'` (refuses cells already in the table) |
| update / upsert cells | rewrite: merged rows, `mode := 'overwrite'` | `mode := 'upsert'`: only the touched partitions are rewritten |
| delete cells | rewrite without them, `mode := 'overwrite'` | rows to keep, `mode := 'overwrite'` |
| add / drop / retype a column | `mode := 'overwrite'` | `alter_cell_dataset` (rename, drop, add: no data rewrite) or `mode := 'overwrite'` |
| overview at a coarser level | `rollup_level := L` | `rollup_level := L` |
| merge files / repartition, keep metadata | `compact_cell_dataset(path [, partition_level])` | same |
| drop old snapshots and their files | - | `expire_snapshots(path, retain_last := N, dry_run := false)` |
| change the partition level | `mode := 'overwrite', partition_level := ...` | same (new partition spec) |
| record statistics, parent rules, extent, governance and file hashes (opt-in) | `manifest_version := '1.1'`, see [manifest-1.1.md](manifest-1.1.md) | same |

A dataset holds one row per cell (unique `packed_id`), one domain and one level. The writer
sorts by `packed_id`, adds missing identity columns (`gid`, `x_idx`, `y_idx`), writes 0 for
NULL numeric values (`fill_nulls`), and checks all of this before it writes anything.

Rules of thumb:

- An `overwrite` reads its input completely before it changes anything, so the input may
  be the dataset being overwritten (that is how update and delete work below).
- Iceberg: every write is a new snapshot. Readers see either the old or the new snapshot,
  never a mix, and older snapshots stay readable by their metadata file. Flat/partitioned
  `overwrite` deletes the old files first, so readers can see a partial folder meanwhile;
  write to a new path and switch readers over if that matters.
- Iceberg has no catalog lock: a commit is refused ("changed during the write; retry") when
  another writer committed first. On S3 the version hint is replaced with a conditional
  PUT (If-Match), so of two writers committing at the same moment only one succeeds.
  A refused write leaves unreferenced data files; `expire_snapshots(path, dry_run := false)` removes them later.
- `append` and `overwrite` keep the dataset's metadata (per column by name: unit, semantics,
  aggregation rule, description; and source, coverage, notes, title, period, version).
  Values passed to the write replace them; new columns start without metadata.
- Large inserts into Iceberg: `append` in blocks of whole partitions (for example per 100 km
  or 500 km block of `packed_id`), so a partition is never split across appends.

## Create

```sql
-- the rows to write: packed_id (or gid with domain_code) + any value columns
CREATE TABLE src AS
SELECT packed_id, (packed_id % 97)::DOUBLE AS population, (packed_id % 13)::DOUBLE AS workers
FROM polygon_to_cells('ID',
  'POLYGON ((106.80 -6.22, 106.84 -6.22, 106.84 -6.18, 106.80 -6.18, 106.80 -6.22))', 12);

SELECT * FROM write_cell_dataset('SELECT * FROM src', '/data/w_flat', layout := 'flat',
  value_semantics := 'COUNT', aggregation_rule := 'sum');
SELECT * FROM write_cell_dataset('SELECT * FROM src', '/data/w_part', layout := 'partitioned',
  partition_level := 7, value_semantics := 'COUNT', aggregation_rule := 'sum');
SELECT * FROM write_cell_dataset('SELECT * FROM src', '/data/w_ice', layout := 'iceberg',
  partition_level := 7, value_semantics := 'COUNT', aggregation_rule := 'sum');
```

From points (lon/lat) summed per 50 m cell, or from gids:

```sql
CREATE TABLE stores(lon DOUBLE, lat DOUBLE, sales DOUBLE);
INSERT INTO stores VALUES (106.8272, -6.1754, 120), (106.8273, -6.1755, 80), (106.8456, -6.2088, 50);
SELECT * FROM write_cell_dataset(
  'SELECT point_to_cell(''ID'', lon, lat, 12).packed_id AS packed_id, sum(sales) AS sales, count(*) AS stores
   FROM stores GROUP BY 1',
  '/data/w_stores', layout := 'flat', value_semantics := 'COUNT', aggregation_rule := 'sum');

CREATE TABLE src_gid AS SELECT gid, population FROM read_cell_dataset('/data/w_flat');
SELECT * FROM write_cell_dataset(
  'SELECT left(gid, 9) AS gid, sum(population) AS population FROM src_gid GROUP BY 1',
  '/data/w_1km', layout := 'flat', domain_code := 'ID', value_semantics := 'COUNT', aggregation_rule := 'sum');
```

## Insert new cells

Iceberg: append. Only cells that are not in the table yet are accepted.

```sql
CREATE TABLE more AS
SELECT packed_id, 5.0::DOUBLE AS population, 1.0::DOUBLE AS workers
FROM polygon_to_cells('ID',
  'POLYGON ((106.85 -6.22, 106.87 -6.22, 106.87 -6.20, 106.85 -6.20, 106.85 -6.22))', 12);

SELECT * FROM write_cell_dataset('SELECT * FROM more', '/data/w_ice', layout := 'iceberg', mode := 'append');
```

When some of the new cells may already exist, append only the missing ones:

```sql
CREATE TABLE more2 AS
SELECT packed_id, 7.0::DOUBLE AS population, 2.0::DOUBLE AS workers
FROM polygon_to_cells('ID',
  'POLYGON ((106.86 -6.22, 106.88 -6.22, 106.88 -6.20, 106.86 -6.20, 106.86 -6.22))', 12);
SELECT * FROM write_cell_dataset(
  'SELECT m.* FROM more2 m ANTI JOIN read_cell_dataset(''/data/w_ice'') t USING (packed_id)',
  '/data/w_ice', layout := 'iceberg', mode := 'append');
```

Flat / partitioned: rewrite with the old and the new rows.

```sql
SELECT * FROM write_cell_dataset(
  'SELECT packed_id, population, workers FROM read_cell_dataset(''/data/w_part'')
   UNION ALL SELECT * FROM more',
  '/data/w_part', layout := 'partitioned', mode := 'overwrite', partition_level := 7);
```

## Update or upsert cells

Merge the current rows with the changes (new values win, new cells are added), then
overwrite. Same SQL for every layout; Iceberg makes it one atomic snapshot.

```sql
-- changes: new values for some existing cells and for some new cells
CREATE TABLE changes AS
SELECT packed_id, 1000.0::DOUBLE AS population
FROM polygon_to_cells('ID',
  'POLYGON ((106.83 -6.21, 106.86 -6.21, 106.86 -6.19, 106.83 -6.19, 106.83 -6.21))', 12);

SELECT * FROM write_cell_dataset(
  'SELECT packed_id,
          coalesce(c.population, t.population) AS population,   -- updated column
          t.workers                                             -- untouched column (0 for new cells)
   FROM read_cell_dataset(''/data/w_ice'') t
   FULL JOIN changes c USING (packed_id)',
  '/data/w_ice', layout := 'iceberg', mode := 'overwrite');
```

To update in place without adding cells, use `LEFT JOIN changes` instead of `FULL JOIN`.

Iceberg: `mode := 'upsert'` does the merge itself and rewrites only the partitions the changes
fall in (other files stay as they are), so updating a few areas of a large table is cheap.
The changes need the table's columns; values are cast to the table's numeric types.

```sql
CREATE TABLE fixes AS
SELECT packed_id, 7.0 AS population, 3.0 AS workers
FROM read_cell_dataset('/data/w_ice') LIMIT 20;
SELECT * FROM write_cell_dataset('SELECT * FROM fixes', '/data/w_ice', layout := 'iceberg', mode := 'upsert');
```

## Delete cells

```sql
-- remove the cells inside an area
SELECT * FROM write_cell_dataset(
  'SELECT packed_id, population, workers FROM read_cell_dataset(''/data/w_ice'') t
   ANTI JOIN polygon_to_cells(''ID'', ''POLYGON ((106.80 -6.22, 106.81 -6.22, 106.81 -6.21, 106.80 -6.21, 106.80 -6.22))'', 12) d
   USING (packed_id)',
  '/data/w_ice', layout := 'iceberg', mode := 'overwrite');
```

## Add, drop or change columns

```sql
SELECT * FROM write_cell_dataset(
  'SELECT packed_id, population, workers, population - workers AS non_workers FROM read_cell_dataset(''/data/w_ice'')',
  '/data/w_ice', layout := 'iceberg', mode := 'overwrite',
  value_semantics := 'COUNT', aggregation_rule := 'sum');

-- the history: one row per snapshot
LOAD iceberg;
SELECT sequence_number, snapshot_id FROM iceberg_snapshots('/data/w_ice') ORDER BY sequence_number;
```

Iceberg tables can also rename, drop or add columns without rewriting any data file:
`alter_cell_dataset` writes a new schema with the same field ids, and the files are read by
field id (an added column reads 0 until the data is rewritten). Units, rules and
descriptions follow the renamed column.

```sql
SELECT * FROM alter_cell_dataset('/data/w_ice',
  rename_columns := '{"non_workers": "dependents"}', add_columns := '{"score": "DOUBLE"}');
SELECT "column", unit FROM cell_dataset_columns('/data/w_ice');
SELECT * FROM alter_cell_dataset('/data/w_ice',
  drop_columns := 'score', rename_columns := '{"dependents": "non_workers"}');
```

## Change the partition level

```sql
SELECT * FROM write_cell_dataset('SELECT * EXCLUDE (gid, x_idx, y_idx) FROM read_cell_dataset(''/data/w_part'')',
  '/data/w_part', layout := 'partitioned', mode := 'overwrite', partition_level := 6);
```

## Overview datasets (roll-up)

`rollup_level := L` aggregates the input cells to their level-L ancestors before writing:
one dataset of 1 km (9) or 10 km (7) cells from a 50 m one, for fast overviews of large
areas. Each numeric column is aggregated by its parent rule, `space_aggregation` (one word or
`{column: rule}`), weighted by `space_weight` (a COUNT or TOTAL column, or `area`). The rules
and weights follow spec 2.0, dataset metadata section 4. COUNT and TOTAL columns sum by
default; other semantics need `space_aggregation`. `aggregation_rule` is not used here: it
records how the source became a cell value. An input `coverage_ratio` (from
`query_cell_dataset`) weights the cells and is not written.

```sql
SELECT * FROM write_cell_dataset('SELECT * FROM read_cell_dataset(''/data/w_ice'')',
  '/data/w_ice_1km', layout := 'iceberg', partition_level := 5, rollup_level := 9,
  value_semantics := '{"population": "COUNT", "workers": "COUNT", "non_workers": "COUNT"}',
  space_aggregation := '{"population": "sum", "workers": "sum", "non_workers": "sum"}');
SELECT level, row_count FROM cell_dataset_info('/data/w_ice_1km');
```

## Compact a dataset

`compact_cell_dataset(path, partition_level := <current>)` rewrites a dataset in place with
one sorted file per partition, merging the files many appends left, and keeps all its
metadata. `partition_level` moves it to coarser partitions (fewer, larger files). The whole
dataset is staged in memory, as for any write. Iceberg: one new snapshot; the old files stay
until `expire_snapshots`.

```sql
SELECT * FROM compact_cell_dataset('/data/w_ice');
SELECT * FROM compact_cell_dataset('/data/w_part', partition_level := 6);
SELECT * FROM expire_snapshots('/data/w_ice', dry_run := false);
```

## Clean up old snapshots (Iceberg)

Every write keeps the previous snapshots and their files, so a table rewritten often grows.
`expire_snapshots` keeps the current snapshot and the last `retain_last` ones (and any newer
than `older_than`), writes new metadata without the others, and deletes the files only they
used. Files no metadata refers to (left by a failed or running write) are deleted only when
older than `orphan_age_hours` (default 24), so a write in progress is safe. Dry run is the
default: the call only reports what it would delete. Pass `dry_run := false` to delete. Older
snapshots are no longer readable after a real run.

```sql
SELECT * FROM expire_snapshots('/data/w_ice');
SELECT * FROM expire_snapshots('/data/w_ice', retain_last := 2, dry_run := false);
SELECT count(*) FROM iceberg_snapshots('/data/w_ice');
```

## From a raster (GeoTIFF / COG)

`read_raster(path, bbox := [min_lon, min_lat, max_lon, max_lat])` returns the valid pixels of
a tiled single-band GeoTIFF in EPSG:4326 inside the box (`lon`, `lat` of the pixel centre,
`value`; nodata skipped). Only the tiles the box touches are read, with range requests on
S3, so a multi-GB COG is sampled by area. The mean of the pixels whose centre lies in a cell
is the method the hazard tables use:

```sql
-- not run in tests: needs a COG on S3
SELECT * FROM write_cell_dataset(
  'SELECT point_to_cell(''ID'', lon, lat, 12).packed_id AS packed_id, avg(value) AS hazard_flood
   FROM read_raster(''s3://geosquare-data/apps-data/raster/hazard/hazard_flood_cog.tif'',
                    bbox := [106.6, -6.4, 107.1, -6.0])
   GROUP BY 1',
  's3://bucket/prefix/flood_jakarta', layout := 'iceberg', aggregation_rule := 'mean');
```

For a whole country, loop over boxes (e.g. per 1 degree) and `append` each, or use
`mode := 'upsert'`. Supported: classic TIFF and BigTIFF, tiled, uncompressed or Deflate,
predictor 1/2/3, 8-64 bit integers and 32/64 bit floats; `max_pixels` (default 200 million)
guards against huge boxes.

## Check what was written

```sql
SELECT count(*) AS cells, count(DISTINCT packed_id) AS unique_cells,
       sum(population) AS population, count(*) FILTER (WHERE population IS NULL) AS nulls
FROM read_cell_dataset('/data/w_ice');
SELECT value_fields FROM read_json('/data/w_flat/manifest.json');
```

## S3

```sql
-- not run in tests: the same calls with an s3:// path. DuckDB needs an S3 secret; the
-- extension uploads the data files in parallel and writes the metadata last.
CREATE SECRET (TYPE s3, PROVIDER credential_chain, REGION 'ap-southeast-3');
SELECT * FROM write_cell_dataset('SELECT * FROM src', 's3://bucket/prefix/my_dataset',
  layout := 'iceberg', partition_level := 7, value_semantics := 'COUNT', aggregation_rule := 'sum');
SELECT * FROM write_cell_dataset('SELECT * FROM more', 's3://bucket/prefix/my_dataset',
  layout := 'iceberg', mode := 'append');
```

From code, the same through the wrappers in [../examples](../examples/README.md):
`gs.write(query, path, { layout, mode, partitionLevel, ... })` (Node) or
`gs.write(query, path, layout=..., mode=...)` (Python).
