# Query examples: flat Parquet, partitioned Parquet and Iceberg

Every SQL block below runs in `tests/test_docs_examples.py` (blocks starting with
`-- not run in tests` need S3, a network install, or a real bucket). The examples use
`/data/...` paths; the test points them at a temporary folder.

Contents: [setup](#setup) · [sample data](#sample-data-in-the-three-layouts) ·
[reading](#reading-a-whole-dataset) · [area queries](#area-queries-polygon-bbox-line-point) ·
[aggregates](#aggregates-and-roll-ups) · [manual joins](#the-same-query-without-query_cell_dataset) ·
[cells](#cell-lookups-and-neighbours) · [writing](#writing) · [Iceberg](#iceberg-snapshots-and-time-travel) ·
[S3 and HTTP](#s3-http-and-the-browser) · [performance](#choosing-a-layout-and-performance)

## Setup

```sql
-- not run in tests: install from the extension repository, then load
SET allow_unsigned_extensions = true;
INSTALL geosquare_v2 FROM 'https://<host>/<prefix>';
LOAD geosquare_v2;
-- or a local build
LOAD 'build/linux_amd64/geosquare_v2.duckdb_extension';
```

Reading Iceberg needs DuckDB's `iceberg` extension for whole-table reads
(`read_cell_dataset`, `iceberg_scan`); the extension loads it on first use.

## Sample data in the three layouts

The same 50 m cells (level 12) over central Jakarta, with two synthetic value columns,
written as a flat dataset, a `gid_prefix`-partitioned dataset and an Iceberg table.

```sql
CREATE TABLE sample AS
SELECT packed_id,
       (packed_id % 97)::DOUBLE AS population,
       (packed_id % 13)::DOUBLE AS workers
FROM polygon_to_cells('ID',
  'POLYGON ((106.78 -6.24, 106.88 -6.24, 106.88 -6.14, 106.78 -6.14, 106.78 -6.24))', 12);

SELECT * FROM write_cell_dataset('SELECT * FROM sample', '/data/pop_flat',
  layout := 'flat', value_semantics := 'COUNT', aggregation_rule := 'sum');

SELECT * FROM write_cell_dataset('SELECT * FROM sample', '/data/pop_part',
  layout := 'partitioned', partition_level := 7, value_semantics := 'COUNT', aggregation_rule := 'sum');

SELECT * FROM write_cell_dataset('SELECT * FROM sample', '/data/pop_ice',
  layout := 'iceberg', partition_level := 7, value_semantics := 'COUNT', aggregation_rule := 'sum');
```

`write_cell_dataset` returns `(layout, path, files, rows)`. It sorts by `packed_id`, adds the
missing identity columns (`gid`, `x_idx`, `y_idx`), writes 0 for NULL numeric values
(`fill_nulls`), and refuses duplicate cells or mixed domains and levels.

What lands on disk:

| Layout | Files |
|---|---|
| flat | `pop_flat/manifest.json`, `pop_flat/data/part-00000.parquet` |
| partitioned | `pop_part/manifest.json`, `pop_part/gid_prefixes.json`, `pop_part/data/gid_prefix=<7 chars>/part-00000.parquet` |
| iceberg | `pop_ice/metadata/*.metadata.json`, `version-hint.text`, manifest list and manifests (`*.avro`), `pop_ice/data/pk_trunc=<value>/*.parquet` |

## Reading a whole dataset

The extension detects the layout, so one function reads all three:

```sql
SELECT count(*), sum(population) FROM read_cell_dataset('/data/pop_flat');
SELECT count(*), sum(population) FROM read_cell_dataset('/data/pop_part');
SELECT count(*), sum(population) FROM read_cell_dataset('/data/pop_ice');
```

The files are plain Parquet and Iceberg, so DuckDB (or Spark, pyarrow, pyiceberg, Trino)
reads them without the extension:

```sql
SELECT count(*) FROM read_parquet('/data/pop_flat/data/*.parquet');
SELECT count(*) FROM read_parquet('/data/pop_part/data/*/*.parquet', hive_partitioning = false);
LOAD iceberg;
SELECT count(*) FROM iceberg_scan('/data/pop_ice');
```

The cell manifest (value semantics, units, coverage) is in `manifest.json`, in every
Parquet footer under `geosquare.manifest`, and for Iceberg in the table property of the
same name:

```sql
SELECT value_fields FROM read_json('/data/pop_flat/manifest.json');
SELECT decode(value) AS manifest
FROM parquet_kv_metadata('/data/pop_flat/data/part-00000.parquet')
WHERE decode(key) = 'geosquare.manifest';
```

## Area queries: polygon, bbox, line, point

`query_cell_dataset(path, geometry := WKT | bbox := [xmin, ymin, xmax, ymax])` reads only
the files the area touches and returns the dataset's columns plus one measure of overlap:

| Geometry | Extra column | Meaning |
|---|---|---|
| `POLYGON` / `MULTIPOLYGON` / `bbox` | `coverage_ratio` | share of the cell inside the area (0..1] |
| `LINESTRING` / `MULTILINESTRING` | `length_ratio` | share of the whole line inside the cell |
| `POINT` / `MULTIPOINT` | `point_count` | number of points in the cell |

The SQL is the same for every layout; only the path changes.

```sql
-- all cells touching the polygon, area-weighted total (best estimate for a count)
SELECT count(*) AS cells,
       sum(population) AS population_touching,
       sum(population * coverage_ratio) AS population_weighted
FROM query_cell_dataset('/data/pop_ice',
  geometry := 'POLYGON ((106.8227 -6.1799, 106.8317 -6.1799, 106.8317 -6.1709, 106.8227 -6.1709, 106.8227 -6.1799))');

-- the same on the flat and the partitioned copy gives the same numbers
SELECT sum(population * coverage_ratio)
FROM query_cell_dataset('/data/pop_part',
  geometry := 'POLYGON ((106.8227 -6.1799, 106.8317 -6.1799, 106.8317 -6.1709, 106.8227 -6.1709, 106.8227 -6.1799))');

-- only cells at least half inside (like v1 polyfill with fullcover = false)
SELECT count(*), sum(population)
FROM query_cell_dataset('/data/pop_flat',
  geometry := 'POLYGON ((106.8227 -6.1799, 106.8317 -6.1799, 106.8317 -6.1709, 106.8227 -6.1709, 106.8227 -6.1799))',
  min_coverage := 0.5);

-- coverage measured in an equal-area CRS instead of the grid plane
SELECT sum(population * coverage_ratio)
FROM query_cell_dataset('/data/pop_ice',
  geometry := 'POLYGON ((106.8227 -6.1799, 106.8317 -6.1799, 106.8317 -6.1709, 106.8227 -6.1709, 106.8227 -6.1799))',
  coverage_mode := 'EQUAL_AREA');

-- polygon with a hole, and a multipolygon
SELECT sum(population * coverage_ratio)
FROM query_cell_dataset('/data/pop_ice', geometry :=
  'POLYGON ((106.80 -6.20, 106.84 -6.20, 106.84 -6.16, 106.80 -6.16, 106.80 -6.20),
            (106.81 -6.19, 106.83 -6.19, 106.83 -6.17, 106.81 -6.17, 106.81 -6.19))');
SELECT sum(population * coverage_ratio)
FROM query_cell_dataset('/data/pop_ice', geometry :=
  'MULTIPOLYGON (((106.80 -6.20, 106.81 -6.20, 106.81 -6.19, 106.80 -6.19, 106.80 -6.20)),
                 ((106.85 -6.17, 106.86 -6.17, 106.86 -6.16, 106.85 -6.16, 106.85 -6.17)))');

-- bounding box (lon/lat)
SELECT count(*), sum(population * coverage_ratio)
FROM query_cell_dataset('/data/pop_part', bbox := [106.82, -6.18, 106.83, -6.17]);

-- a road: cells along the line, with each cell's share of the line length
SELECT count(*) AS cells, sum(length_ratio) AS total_ratio, sum(workers) AS workers_along_road
FROM query_cell_dataset('/data/pop_ice',
  geometry := 'LINESTRING (106.80 -6.20, 106.82 -6.18, 106.85 -6.17)');

-- points of interest: the cells holding them
SELECT gid, point_count, population
FROM query_cell_dataset('/data/pop_ice',
  geometry := 'MULTIPOINT ((106.8272 -6.1754), (106.8456 -6.2088), (106.8273 -6.1755))')
ORDER BY gid;
```

Self-intersecting polygons are repaired (even-odd, like `shapely.make_valid`) before the
cells are computed.

## Aggregates and roll-ups

A level-L ancestor's gid is the first L characters of the gid, so roll-ups are a
`GROUP BY left(gid, L)`: level 9 = 1 km, 7 = 10 km, 6 = 50 km, 5 = 100 km.

```sql
-- 1 km totals inside an area, with the 1 km cell outline as GeoJSON (lon/lat)
SELECT left(gid, 9) AS gid_1km,
       sum(population * coverage_ratio) AS population,
       sum(workers * coverage_ratio) AS workers,
       cell_to_geometry('ID', left(gid, 9), 'EPSG:4326', 'geojson') AS geometry
FROM query_cell_dataset('/data/pop_ice',
  geometry := 'POLYGON ((106.80 -6.20, 106.84 -6.20, 106.84 -6.16, 106.80 -6.16, 106.80 -6.20))')
GROUP BY 1
ORDER BY 1;

-- 10 km totals over the whole dataset
SELECT left(gid, 7) AS gid_10km, count(*) AS cells, sum(population) AS population
FROM read_cell_dataset('/data/pop_part')
GROUP BY 1
ORDER BY population DESC;

-- cells with their outline straight from the query (no domain code needed)
SELECT gid, population, coverage_ratio, cell_geometry
FROM query_cell_dataset('/data/pop_ice', bbox := [106.82, -6.18, 106.83, -6.17], with_geometry := 'geojson')
LIMIT 3;

-- top 5 cells in an area, with WKT outlines (from packed_id the domain is implicit)
SELECT gid, population, cell_to_geometry(packed_id, 'EPSG:4326') AS wkt
FROM query_cell_dataset('/data/pop_flat', bbox := [106.82, -6.18, 106.83, -6.17])
ORDER BY population DESC
LIMIT 5;

-- several value columns at once, and a ratio
SELECT sum(population * coverage_ratio) AS population,
       sum(workers * coverage_ratio) AS workers,
       sum(workers * coverage_ratio) / nullif(sum(population * coverage_ratio), 0) AS workers_per_person
FROM query_cell_dataset('/data/pop_ice',
  geometry := 'POLYGON ((106.80 -6.20, 106.84 -6.20, 106.84 -6.16, 106.80 -6.16, 106.80 -6.20))');
```

## The same query without query_cell_dataset

`query_cell_dataset` is a polyfill joined on `packed_id` plus file pruning. Written out by
hand (useful in tools that only have the grid functions, or to see what it reads):

```sql
-- flat: one file, row groups are pruned by the packed_id range
SELECT sum(d.population * k.coverage_ratio)
FROM polygon_to_cells('ID', 'POLYGON ((106.8227 -6.1799, 106.8317 -6.1799, 106.8317 -6.1709, 106.8227 -6.1709, 106.8227 -6.1799))', 12) AS k
JOIN read_parquet('/data/pop_flat/data/*.parquet') AS d USING (packed_id);

-- partitioned: read only the gid_prefix folders of the cells
SELECT sum(d.population * k.coverage_ratio)
FROM polygon_to_cells('ID', 'POLYGON ((106.8227 -6.1799, 106.8317 -6.1799, 106.8317 -6.1709, 106.8227 -6.1709, 106.8227 -6.1799))', 12) AS k
JOIN read_parquet('/data/pop_part/data/*/*.parquet', hive_partitioning = true) AS d
  ON d.packed_id = k.packed_id AND d.gid_prefix = left(k.gid, 7);

-- iceberg: iceberg_scan prunes data files from the manifest bounds of the packed_id range
LOAD iceberg;
WITH k AS (
  SELECT * FROM polygon_to_cells('ID', 'POLYGON ((106.8227 -6.1799, 106.8317 -6.1799, 106.8317 -6.1709, 106.8227 -6.1709, 106.8227 -6.1799))', 12)
)
SELECT sum(d.population * k.coverage_ratio)
FROM iceberg_scan('/data/pop_ice') AS d
JOIN k USING (packed_id)
WHERE d.packed_id BETWEEN (SELECT min(packed_id) FROM k) AND (SELECT max(packed_id) FROM k);
```

For Iceberg, `query_cell_dataset` is faster than the hand-written `iceberg_scan` join:
it reads the manifests once per process (cached), picks data files from the cells'
partition values rather than one `packed_id` range (a polygon crossing a large grid
boundary has a wide range), and reads them with `read_parquet`.

## Cell lookups and neighbours

```sql
-- the cell holding a coordinate, then its values
SELECT d.gid, d.population, d.workers
FROM read_cell_dataset('/data/pop_flat') AS d
WHERE d.packed_id = point_to_cell('ID', 106.8272, -6.1754, 12).packed_id;

-- values for a list of coordinates (e.g. customer locations)
WITH pts(id, lon, lat) AS (VALUES (1, 106.8272, -6.1754), (2, 106.8456, -6.2088), (3, 106.8000, -6.1500))
SELECT pts.id, c.gid, d.population
FROM pts
CROSS JOIN LATERAL (SELECT point_to_cell('ID', pts.lon, pts.lat, 12) AS c) AS p(c)
LEFT JOIN read_cell_dataset('/data/pop_ice') AS d ON d.packed_id = p.c.packed_id
ORDER BY pts.id;

-- a cell and its 8 neighbours (k_disk = cell + ring)
SELECT n.gid, d.population
FROM (SELECT unnest(k_disk(point_to_cell('ID', 106.8272, -6.1754, 12).gid, 1)) AS gid) AS n
JOIN read_cell_dataset('/data/pop_part') AS d USING (gid)
ORDER BY n.gid;

-- distance from each cell in an area to a site, in metres
SELECT gid, round(distance_m(gid, point_to_cell('ID', 106.8272, -6.1754, 12).gid)) AS metres, population
FROM query_cell_dataset('/data/pop_ice', bbox := [106.82, -6.18, 106.83, -6.17])
ORDER BY metres
LIMIT 5;

-- join two datasets on packed_id (here the flat and the Iceberg copy)
SELECT count(*) AS cells, sum(abs(a.population - b.population)) AS difference
FROM read_cell_dataset('/data/pop_flat') AS a
JOIN read_cell_dataset('/data/pop_ice') AS b USING (packed_id);
```

## Writing

The full guide (insert, update/upsert, delete, schema and partition changes) is
[write-guide.md](write-guide.md); the basics:

```sql
-- append to Iceberg (new cells only; cells already in the table are refused)
CREATE TABLE more AS
SELECT packed_id, 1.0::DOUBLE AS population, 1.0::DOUBLE AS workers
FROM polygon_to_cells('ID',
  'POLYGON ((106.90 -6.24, 106.92 -6.24, 106.92 -6.22, 106.90 -6.22, 106.90 -6.24))', 12);
SELECT * FROM write_cell_dataset('SELECT * FROM more', '/data/pop_ice', layout := 'iceberg', mode := 'append');

-- overwrite with a new column and another partition level (new schema and spec; older
-- snapshots stay readable)
SELECT * FROM write_cell_dataset(
  'SELECT packed_id, population, workers, population * 2 AS households FROM sample',
  '/data/pop_ice', layout := 'iceberg', mode := 'overwrite', partition_level := 6);

-- overwrite a flat or partitioned folder (old files are deleted first)
SELECT * FROM write_cell_dataset('SELECT * FROM sample', '/data/pop_part',
  layout := 'partitioned', mode := 'overwrite', partition_level := 6);

-- keep NULL (only when NULL must mean "not measured"), per-column semantics
SELECT * FROM write_cell_dataset(
  'SELECT packed_id, CASE WHEN packed_id % 2 = 0 THEN NULL ELSE population END AS population, workers FROM sample',
  '/data/pop_nulls', layout := 'flat', fill_nulls := false,
  value_semantics := '{"population": "COUNT", "workers": "COUNT"}', aggregation_rule := 'sum');

-- a wide table (hundreds of value columns): keep manifest metrics to the id columns
SELECT * FROM write_cell_dataset('SELECT * FROM sample', '/data/pop_wide',
  layout := 'iceberg', partition_level := 7, column_metrics := 'key');

-- write from gid only (domain_code needed), at 1 km, from any query
SELECT * FROM write_cell_dataset(
  'SELECT left(gid, 9) AS gid, sum(population) AS population FROM read_cell_dataset(''/data/pop_flat'') GROUP BY 1',
  '/data/pop_1km', layout := 'flat', domain_code := 'ID', value_semantics := 'COUNT', aggregation_rule := 'sum');
SELECT count(*), sum(population) FROM read_cell_dataset('/data/pop_1km');
```

Options: `layout` (`flat` | `partitioned` | `iceberg`), `mode` (`create` | `overwrite` |
`append`, append is Iceberg only), `partition_level` (default 5), `row_group_size`
(default 8192), `identity` (`query` keeps gid, packed_id, x_idx, y_idx; `compact` keeps gid
only), `value_semantics`, `aggregation_rule`, `fill_nulls` (default true), `column_metrics`
(`auto` | `key` | `all`), `value_fields` / `coverage` / `source` / `notes`, and the `s3_*`
options below.

## Iceberg snapshots and time travel

Each write is a snapshot; the newest one is found through `metadata/version-hint.text`.

```sql
LOAD iceberg;
SELECT sequence_number, snapshot_id, manifest_list FROM iceberg_snapshots('/data/pop_ice') ORDER BY sequence_number;

-- read an older snapshot by its metadata file (table functions take constants, so pass
-- the path through a variable or write it out)
SET VARIABLE first_snapshot = (SELECT min(file) FROM glob('/data/pop_ice/metadata/*.metadata.json'));
SELECT count(*), sum(population) FROM read_cell_dataset(getvariable('first_snapshot'));

-- area query on an older snapshot
SELECT count(*)
FROM query_cell_dataset(getvariable('first_snapshot'), bbox := [106.82, -6.18, 106.83, -6.17]);
```

## S3, HTTP and the browser

```sql
-- not run in tests: read from S3 (any layout)
CREATE SECRET (TYPE s3, PROVIDER credential_chain, REGION 'ap-southeast-3');
SELECT count(*) AS cells,
       sum(people_counts * coverage_ratio) AS people,
       sum(people_millenials_female_night * coverage_ratio) AS millenial_women_at_night
FROM query_cell_dataset('s3://geosquare-data/apps-data/data_v2/data_name=50m_people_v2023',
  geometry := 'POLYGON ((106.8227 -6.1799, 106.8317 -6.1799, 106.8317 -6.1709, 106.8227 -6.1709, 106.8227 -6.1799))');

-- not run in tests: 1 km roll-up of a few columns of the people table
SELECT left(gid, 9) AS gid_1km,
       sum(people_counts * coverage_ratio) AS people,
       sum(people_gen_z * coverage_ratio) AS gen_z,
       sum(people_gen_z_night * coverage_ratio) AS gen_z_night
FROM query_cell_dataset('s3://geosquare-data/apps-data/data_v2/data_name=50m_people_v2023',
  bbox := [106.80, -6.20, 106.84, -6.16])
GROUP BY 1 ORDER BY 1;

-- not run in tests: write to S3 (keys from the secret's provider chain or AWS_* variables)
SELECT * FROM write_cell_dataset('SELECT * FROM sample', 's3://my-bucket/prefix/pop',
  layout := 'iceberg', partition_level := 7, s3_region := 'ap-southeast-3');

-- not run in tests: over plain HTTP(S) (CDN, static host): no listing, so the writer's
-- index files are used (gid_prefixes.json, metadata/version-hint.text)
SELECT sum(population * coverage_ratio)
FROM query_cell_dataset('https://cdn.example.com/pop_ice', bbox := [106.82, -6.18, 106.83, -6.17]);
```

In DuckDB-Wasm the same SQL runs after `LOAD geosquare_v2` (see the main README); the
data host must allow CORS, and S3 needs a secret with keys or a public or presigned URL.

## Choosing a layout and performance

| | flat | partitioned (`gid_prefix`) | iceberg (`truncate(packed_id)`) |
|---|---|---|---|
| Files | one | one per prefix cell | one per partition and write |
| Small-area query reads | footer + matching row groups of one big file | footers + row groups of the prefix folders touched | manifest list, the manifests covering the area (cached), footers + row groups of the files touched |
| Append / overwrite | rewrite | rewrite | snapshot (append, atomic overwrite, schema and partition changes, time travel) |
| Plain HTTP | yes | yes (`gid_prefixes.json`) | yes (`version-hint.text`) |
| Good for | small datasets | read-mostly datasets, simple tooling | datasets that change, many readers, other engines (Spark, Trino, pyiceberg) |

Partition level: for every layout, a file's footer grows with row groups × columns
(about 30 KB per row group of 8192 rows for 230 DOUBLE columns), and a query always reads
the footer of each file it touches. Measured on the 19.9 million cell, 232-column people
table, for a 1 km² area in Jakarta:

| Iceberg partition level | files | largest footer | bytes read |
|---|---|---|---|
| 5 (100 km) | 649 | 5.9 MB | 5.9 MB |
| 6 (50 km) | 2,116 | 3.1 MB | 3.1 MB |
| 7 (10 km) | 27,071 | 0.2 MB | 0.26 MB |

So use a finer partition level for wide tables, a coarser one for narrow tables (few
columns keep footers small, and fewer files mean fewer requests), and `column_metrics :=
'key'` when there are more than about 100 columns.
