# Compact sets, geometry allocation, boundaries and crosswalks

The parts of geosquare-v2 spec 2.0 beyond cell values and time cubes, as SQL. Every block
runs in `tests/test_docs_examples.py`; `/data/...` points at a temporary folder there.
`tests/test_parity_oracles.py` checks the same functions cell by cell against the Python
reference (`geosquare_v2`).

Contents: [compact sets](#compact-sets-cell_compact_set) ·
[class roll-ups](#categorical-roll-up-majority_weighted) ·
[geometry to cells](#features-to-cells-by-value-semantics) ·
[boundaries](#operational-boundary) · [crosswalks](#crosswalk-10) ·
[time values from anything](#time-values-from-any-query) ·
[compact extent](#manifest-11-compact-extent)

## Compact sets (`cell_compact_set`)

A set of GIDs that may mix levels: an area stored as few cells. The canonical form has no
cell inside another and no complete child group (it becomes the parent).

```sql
SELECT compact_canonicalize(['22', '23', '24', '25', '32']) AS canonical,   -- [2, 32]
       compact_contains(['2'], '25') AS inside,                              -- true
       compact_check(['2', '22']) AS why_not,                                -- GID "22" is inside row "2"
       len(compact_expand(['2', '32'], 2)) AS cells_at_level_2;              -- 5
```

A polygon's level-12 cells, stored canonically, then expanded back:

```sql
SELECT * FROM write_compact_set(
  'SELECT gid FROM polygon_to_cells(''ID'', ''POLYGON ((106.80 -6.21, 106.82 -6.21, 106.82 -6.19, 106.80 -6.19, 106.80 -6.21))'', 12)',
  '/data/jakarta_block', domain_code := 'ID', canonicalize := true);

SELECT level, count(*) FROM read_compact_set('/data/jakarta_block') GROUP BY 1 ORDER BY 1;

SELECT count(*) FROM read_compact_set('/data/jakarta_block', level := 12);   -- every level-12 cell

SELECT * FROM validate_cell_dataset('/data/jakarta_block');
```

The folder holds `manifest.json` and `data/compact.parquet` (exactly `gid`, `level`), as
`geosquare_v2.compact_store` writes; each reads the other's. Without `canonicalize` a
non-canonical set is refused; reads refuse a stored set that breaks a rule.

## CATEGORICAL roll-up: `majority_weighted`

A class column rolls up to the class with the largest summed weight: each child counts 1,
or by its share of a query area, or by a COUNT column (`space_weight`). A tie goes to the
greatest class as text, as `geosquare_v2.aggregation` does.

```sql
CREATE TABLE landcover AS
SELECT packed_id, ['forest', 'urban', 'water', 'crop'][1 + (packed_id % 7) % 4] AS lc,
       (packed_id % 11)::DOUBLE AS people
FROM polygon_to_cells('ID', 'POLYGON ((106.80 -6.22, 106.84 -6.22, 106.84 -6.18, 106.80 -6.18, 106.80 -6.22))', 12);

-- manifest 1.1 records the rule (space_aggregation) with the column
SELECT * FROM write_cell_dataset('SELECT * FROM landcover', '/data/landcover',
  value_semantics := '{"lc": "CATEGORICAL", "people": "COUNT"}', manifest_version := '1.1',
  space_aggregation := '{"lc": "majority_weighted"}');

-- the main class per 1 km cell, children weighted by people
SELECT gid, lc, people FROM read_cell_dataset('/data/landcover', rollup_level := 9,
  space_weight := '{"lc": "people"}') ORDER BY gid LIMIT 5;

-- any dataset, rule given at read time
SELECT gid, lc FROM query_cell_dataset('/data/landcover',
  geometry := 'POLYGON ((106.805 -6.215, 106.83 -6.215, 106.83 -6.19, 106.805 -6.19, 106.805 -6.215))',
  rollup_level := 9, rollup_rule := '{"lc": "majority_weighted", "people": "sum"}') ORDER BY gid;
```

geosquare-v2 lists `majority_weighted` in dataset-metadata section 4, and marks it as an open
question for pyramids (cell-pyramid.md section 11).

## Features to cells by value semantics

`geometry_to_cells(domain, wkt, level [, source_crs])` gives one feature's cells, each with
`share`: a polygon's area share, a line's length ratio, or the points in the cell.
`aggregate_geometries_to_cells` allocates a table of features and aggregates per cell:

| Semantics | Per cell |
|---|---|
| COUNT | records (features; each point counts) |
| TOTAL | sum of value × share |
| DENSITY, MEASUREMENT, RATE | share-weighted mean; RATE with `rate_parts` sums numerator and denominator × share, then divides |
| CATEGORICAL | class of the largest summed share (`category_rule := 'centroid_category'`: most records), plus `<column>_share` |
| ORDINAL | first class of `ordinal_order` present |
| RANGE | `range_columns` pairs: lowest lower bound, highest upper bound |

```sql
CREATE TABLE villages AS SELECT * FROM (VALUES
  ('POLYGON ((106.800 -6.210, 106.812 -6.210, 106.812 -6.199, 106.800 -6.199, 106.800 -6.210))', 1200.0, 'low'),
  ('POLYGON ((106.806 -6.205, 106.818 -6.205, 106.818 -6.194, 106.806 -6.194, 106.806 -6.205))', 800.0, 'high')
) AS t(geometry, population, flood);

SELECT count(*) AS cells, sum(population) AS population   -- 2000: TOTAL is kept
FROM aggregate_geometries_to_cells('SELECT * FROM villages', domain_code := 'ID', level := 12,
  value_semantics := '{"population": "TOTAL", "flood": "ORDINAL"}',
  ordinal_order := '{"flood": ["high", "low"]}');
```

The geometry column (`geometry_column`, default `geometry`) is WKT text, or a GEOMETRY of
DuckDB spatial (cast to WKT).

## Operational boundary

A cell can be valid in the grid yet outside the country. The four rules of
`docs/boundary-policy.md`: `COVERS_POINT` (points), `CENTROID_COVERED`, `INTERSECTS` and
`MIN_COVERAGE` (cells). The boundary is GeoJSON or WKT text in lon/lat, or the path of a
GeoJSON file (local, http or s3); it is parsed once per process.

```sql
-- a toy boundary: a square with a hole
CREATE TABLE b AS SELECT 'POLYGON ((106.80 -6.22, 106.84 -6.22, 106.84 -6.18, 106.80 -6.18, 106.80 -6.22), (106.815 -6.205, 106.825 -6.205, 106.825 -6.195, 106.815 -6.195, 106.815 -6.205))' AS wkt;

SELECT boundary_covers_point('ID', 106.81, -6.21, wkt) AS land,          -- true
       boundary_covers_point('ID', 106.82, -6.20, wkt) AS in_the_hole    -- false
FROM b;

SELECT count(*) FILTER (WHERE boundary_cell_matches('ID', gid, (SELECT wkt FROM b), 'MIN_COVERAGE', 0.5)) AS half_inside,
       count(*) FILTER (WHERE boundary_cell_matches('ID', gid, (SELECT wkt FROM b), 'CENTROID_COVERED')) AS centre_inside
FROM read_cell_dataset('/data/landcover');
```

The same rule filters datasets and allocations:

```sql
SELECT count(*) FROM read_cell_dataset('/data/landcover',
  boundary := 'POLYGON ((106.80 -6.22, 106.83 -6.22, 106.83 -6.19, 106.80 -6.19, 106.80 -6.22))',
  boundary_predicate := 'MIN_COVERAGE', boundary_min_coverage := 0.5);
```

```sql
-- not run in tests: a country boundary file, checked against its SHA-256
SELECT file_sha256('boundaries/ID_full.geojson');
SELECT * FROM query_cell_dataset('s3://bucket/people', geometry := '<WKT>',
  boundary := 'boundaries/ID_full.geojson', boundary_sha256 := 'b2e253c4...',
  boundary_predicate := 'CENTROID_COVERED');
```

`boundary_coverage(domain, gid, boundary)` gives the share of a cell inside. Country
boundaries are edge-indexed: a cell the boundary line does not cross costs one point test
(about 300,000 cells a second against the 18 MB Indonesia boundary).

## Crosswalk 1.0

A table `(sumber_id, gid, bobot)` linking source units (villages, V1 GIDs) to cells; each
unit's `bobot` sums to 1 (docs/crosswalk-format.md).

```sql
CREATE TABLE desa AS SELECT * FROM (VALUES
  ('DESA-1', 'POLYGON ((106.800 -6.210, 106.812 -6.210, 106.812 -6.199, 106.800 -6.199, 106.800 -6.210))'),
  ('DESA-2', 'POLYGON ((106.806 -6.205, 106.818 -6.205, 106.818 -6.194, 106.806 -6.194, 106.806 -6.205))')
) AS t(sumber_id, geometry);

-- area-share rows per village
CREATE TABLE cw AS SELECT * FROM polygon_crosswalk('SELECT * FROM desa', domain_code := 'ID', level := 11);

SELECT * FROM validate_crosswalk('SELECT * FROM cw', manifest := '{"format_version": "1.0", "spec_version": "2.0",
  "domain_code": "ID", "level": 11, "weight_kind": "luas", "source_description": "desa",
  "created_at": "2026-11-19T10:00:00Z", "registry_version": "2.0.0"}');

-- village values onto cells
SELECT cw.gid, sum(v.households * cw.bobot) AS households
FROM cw JOIN (VALUES ('DESA-1', 300.0), ('DESA-2', 200.0)) AS v(sumber_id, households) USING (sumber_id)
GROUP BY 1 ORDER BY 1 LIMIT 5;
```

`validate_crosswalk` raises the reference's message for the first broken rule (row number,
sumber_id, gid) and otherwise returns `(sources, rows, cells)`. `manifest` is JSON text or a
file path.

## Time values from any query

`time_values` gives the time cube reads (`period`, `period_from` / `period_to`,
`time_rollup`, `time_rule`) on any query with a cell, a `period` column and values: CSV,
Parquet without a manifest, Delta, a database.

```sql
COPY (SELECT gid, strftime(DATE '2026-01-25' + INTERVAL (i) DAY, '%Y-%m-%d') AS period,
             (packed_id % 13 + i)::DOUBLE AS rain
      FROM polygon_to_cells('ID', 'POLYGON ((106.80 -6.21, 106.82 -6.21, 106.82 -6.19, 106.80 -6.19, 106.80 -6.21))', 11),
           range(14) t(i))
TO '/data/rain.csv';

SELECT period, count(*), avg(rain) FROM time_values('SELECT * FROM read_csv(''/data/rain.csv'')',
  period_type := 'day', value_semantics := 'MEASUREMENT', time_aggregation := 'max',
  time_rollup := 'month') GROUP BY 1 ORDER BY 1;
```

A raster per period: `read_raster(path, bbox := [...], period := '2026-03-01')` adds the
`period` column, so daily GeoTIFFs become a time cube:

```sql
-- not run in tests: needs GeoTIFFs
SELECT * FROM write_cell_dataset('
  SELECT point_to_cell(''ID'', lon, lat, 10).packed_id AS packed_id, period, avg(value) AS rain FROM (
    SELECT * FROM read_raster(''rain_2026-03-01.tif'', bbox := [106.7, -6.3, 106.9, -6.1], period := ''2026-03-01'')
    UNION ALL
    SELECT * FROM read_raster(''rain_2026-03-02.tif'', bbox := [106.7, -6.3, 106.9, -6.1], period := ''2026-03-02''))
  GROUP BY 1, 2', '/data/rain_cube', period_type := 'day', value_semantics := 'MEASUREMENT', time_aggregation := 'max');
```

Time cubes are written as manifest 1.0: spec 2.0 defines manifest 1.1 for `cell_values`
and `cell_contributions` only.

## Manifest 1.1 compact extent

`compact_extent := true` writes the compact set of the covered cells to
`compact/cells.parquet` and names it in `extent.compact_set_path`; appends extend it.

```sql
SELECT * FROM write_cell_dataset('SELECT packed_id, lc, people FROM landcover', '/data/landcover_11',
  value_semantics := '{"lc": "CATEGORICAL", "people": "COUNT"}', manifest_version := '1.1',
  compact_extent := true, owner := 'data team', license := 'internal-test');

SELECT manifest->'extent'->>'compact_set_path' FROM cell_dataset_info('/data/landcover_11');
```
