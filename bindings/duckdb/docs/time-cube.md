# Time cubes: cell values per period

A time cube (`dataset_type: cell_time_values`, geosquare-v2
[docs/time-cube.md](https://github.com/geosquareai/geosquare-v2/blob/main/docs/time-cube.md))
holds one row per cell and period instead of one row per cell: daily rainfall, hourly
traffic, monthly night lights. Every SQL block below runs in `tests/test_docs_examples.py`;
`/data/...` points at a temporary folder there.

Contents: [periods](#periods) · [writing](#writing) · [reading](#reading-a-period-or-a-range) ·
[roll-up in time](#roll-up-in-time) · [with space](#time-and-space-together) ·
[several datasets](#several-datasets) · [changing](#append-upsert-compact) ·
[checks](#checks-and-metadata) · [storage](#how-it-is-stored)

## Periods

One period type per dataset, all in UTC, written as text labels:

| `period_type` | Label | Example |
|---|---|---|
| `year` | `YYYY` | `2026` |
| `month` | `YYYY-MM` | `2026-11` |
| `day` | `YYYY-MM-DD` | `2026-11-19` |
| `hour` with `bucket_hours` 1, 2, 3, 4, 6, 8 or 12 | `YYYY-MM-DDTHH`, HH a multiple of the bucket | `2026-11-19T04` |

A label is the half-open interval `[start, end)`. The extension can check and compare them:

```sql
SELECT period_valid('2026-02-29', 'day') AS leap_day_2026,          -- false
       period_valid('2026-11-19T04', 'hour:4') AS four_hour_bucket,  -- true
       period_interval('2026-11', 'month') AS month_interval,
       period_relation('2026-11-19', 'day', '2026-11', 'month') AS relation,  -- contained
       period_rollup('2026-11-19T05', 'hour:1', 'day') AS day;      -- 2026-11-19
```

`period_type` can carry the bucket (`'hour:4'`) or take it from `bucket_hours`.

## Writing

`write_cell_dataset` writes a time cube when it gets `period_type`. The query needs a
`period` column (text labels) next to the cell id and the value columns.

```sql
CREATE TABLE rain AS
SELECT packed_id, d.period,
       ((packed_id % 97) + d.i)::DOUBLE AS rain_mm,
       (packed_id % 5)::BIGINT AS storms
FROM polygon_to_cells('ID',
  'POLYGON ((106.78 -6.24, 106.84 -6.24, 106.84 -6.20, 106.78 -6.20, 106.78 -6.24))', 12),
  (SELECT strftime(DATE '2026-01-01' + INTERVAL (i) DAY, '%Y-%m-%d') AS period, i
   FROM range(90) t(i)) d;

SELECT * FROM write_cell_dataset('SELECT * FROM rain', '/data/rain_flat',
  layout := 'flat', period_type := 'day',
  value_semantics := '{"rain_mm": "MEASUREMENT", "storms": "COUNT"}',
  time_aggregation := 'mean');

SELECT * FROM write_cell_dataset('SELECT * FROM rain', '/data/rain_part',
  layout := 'partitioned', partition_level := 7, period_type := 'day',
  value_semantics := '{"rain_mm": "MEASUREMENT", "storms": "COUNT"}',
  time_aggregation := 'mean');

SELECT * FROM write_cell_dataset('SELECT * FROM rain', '/data/rain_ice',
  layout := 'iceberg', partition_level := 7, period_type := 'day',
  value_semantics := '{"rain_mm": "MEASUREMENT", "storms": "COUNT"}',
  time_aggregation := 'mean');
```

Time options of `write_cell_dataset`:

- `period_type` — `year`, `month`, `day`, `hour` (with `bucket_hours`) or `hour:N`
- `period_start`, `period_end` — the declared range `[start, end)`; by default the first
  period of the data and the period after the last one. Every row must be inside it.
- `time_aggregation` — how a MEASUREMENT column rolls up in time: `mean`, `min` or `max`,
  one word for all MEASUREMENT columns or `'{"rain_mm": "max"}'`. COUNT and TOTAL always
  sum; CATEGORICAL cannot be rolled up in time.
- `time_chunk` — `month` (default), `year`, `day` or `none`: how many periods one data file
  holds (partitioned and Iceberg). Smaller chunks skip more files for a short range; larger
  ones make fewer files.

The writer refuses labels that are not of the period type (`2026-02-30`, `T03` in a 4-hour
cube), periods outside the declared range, two rows for one cell and period, and a
`period` value column.

## Reading a period or a range

```sql
-- the whole cube: one row per cell and day
SELECT count(*), min(period), max(period) FROM read_cell_dataset('/data/rain_ice');

-- one day, one month (a coarser label means every period inside it), a half-open range
SELECT count(*) FROM read_cell_dataset('/data/rain_ice', period := '2026-02-03');
SELECT count(*) FROM read_cell_dataset('/data/rain_part', period := '2026-02');
SELECT count(*) FROM read_cell_dataset('/data/rain_flat',
  period_from := '2026-01-15', period_to := '2026-02-10');

-- an area and a range
SELECT period, sum(rain_mm * coverage_ratio) / sum(coverage_ratio) AS mean_rain
FROM query_cell_dataset('/data/rain_ice',
  geometry := 'POLYGON ((106.79 -6.23, 106.80 -6.23, 106.80 -6.22, 106.79 -6.22, 106.79 -6.23))',
  period_from := '2026-03-01', period_to := '2026-03-08')
GROUP BY 1 ORDER BY 1;
```

`period` and `period_from` / `period_to` take a label of the dataset's period type or a
coarser one (`'2026'`, `'2026-02'`), never a finer one. Partitioned and Iceberg datasets
read only the files whose periods overlap the range: Iceberg files by the `period` bounds
in the table manifests, partitioned files by the period chunk in their name.

## Roll-up in time

`time_rollup` gives one row per cell and coarser period (`'year'`, `'month'`, `'day'`,
`'hour:N'`), or per cell over the whole range (`'all'`). COUNT and TOTAL are summed,
MEASUREMENT uses its `time_aggregation`; `time_rule` overrides it
(`'max'` or `'{"rain_mm": "max"}'`). `period_count` is the number of periods in each row.

```sql
-- monthly values per cell
SELECT period, count(*) AS cells, avg(rain_mm) AS mean_rain, sum(storms) AS storms
FROM read_cell_dataset('/data/rain_ice', time_rollup := 'month')
GROUP BY 1 ORDER BY 1;

-- the wettest day of February per cell
SELECT gid, rain_mm, period_count
FROM read_cell_dataset('/data/rain_part', period := '2026-02',
  time_rollup := 'all', time_rule := '{"rain_mm": "max"}')
ORDER BY rain_mm DESC LIMIT 5;
```

## Time and space together

With `rollup_level` the time roll-up runs first, per cell, then the space roll-up per
period: one row per ancestor cell and period.

```sql
-- 1 km cells (level 9) per month inside an area
SELECT gid, period, rain_mm, storms, cell_count
FROM query_cell_dataset('/data/rain_ice',
  geometry := 'POLYGON ((106.79 -6.23, 106.80 -6.23, 106.80 -6.22, 106.79 -6.22, 106.79 -6.23))',
  time_rollup := 'month', rollup_level := 9,
  rollup_rule := '{"rain_mm": "max"}')
ORDER BY gid, period;
```

## Several datasets

`query_cell_dataset([...])` joins time cubes on (cell, period) and datasets without periods
on the cell alone, so a static value repeats for every period. Period parameters apply to
the time cubes.

```sql
SELECT * FROM write_cell_dataset(
  'SELECT packed_id, 1::DOUBLE AS people FROM polygon_to_cells(''ID'',
     ''POLYGON ((106.78 -6.24, 106.84 -6.24, 106.84 -6.20, 106.78 -6.20, 106.78 -6.24))'', 12)',
  '/data/people', layout := 'flat', value_semantics := 'COUNT');

SELECT period, sum(rain_mm * people) / sum(people) AS rain_per_person
FROM query_cell_dataset(['/data/rain_ice', '/data/people'],
  geometry := 'POLYGON ((106.79 -6.23, 106.80 -6.23, 106.80 -6.22, 106.79 -6.22, 106.79 -6.23))',
  period := '2026-02')
GROUP BY 1 ORDER BY 1;
```

## Append, upsert, compact

Iceberg cubes take new periods with `mode := 'append'` (rows already in the table for the
same cell and period are refused) and replace rows with `mode := 'upsert'`, keyed on
(cell, period). The period type is kept; the declared range grows to cover both.

```sql
CREATE TABLE april AS
SELECT packed_id, strftime(DATE '2026-04-01' + INTERVAL (i) DAY, '%Y-%m-%d') AS period,
       ((packed_id % 97) + 90 + i)::DOUBLE AS rain_mm, (packed_id % 5)::BIGINT AS storms
FROM polygon_to_cells('ID',
  'POLYGON ((106.78 -6.24, 106.84 -6.24, 106.84 -6.20, 106.78 -6.20, 106.78 -6.24))', 12), range(3) t(i);

SELECT * FROM write_cell_dataset('SELECT * FROM april', '/data/rain_ice',
  layout := 'iceberg', mode := 'append');

SELECT * FROM write_cell_dataset(
  'SELECT packed_id, period, rain_mm + 1 AS rain_mm, storms FROM april WHERE period = ''2026-04-03''',
  '/data/rain_ice', layout := 'iceberg', mode := 'upsert');

-- the range is now 2026-01-01 to 2026-04-04
SELECT period_start, period_end FROM cell_dataset_info('/data/rain_ice');

SELECT * FROM compact_cell_dataset('/data/rain_ice');
```

Flat and partitioned cubes are rewritten whole (`mode := 'overwrite'`), as for cell values.

## Checks and metadata

```sql
SELECT dataset_type, period_type, period_start, period_end
FROM cell_dataset_info('/data/rain_part');

SELECT "column", semantics, time_aggregation FROM cell_dataset_columns('/data/rain_part');

-- time_manifest, unique_cell_periods, periods (labels inside the declared range) and,
-- for flat cubes, sorted_by_gid_period
SELECT * FROM validate_cell_dataset('/data/rain_flat');
```

## How it is stored

| Layout | Files |
|---|---|
| `flat` | `manifest.json`, `data/time_values.parquet` with exactly `gid`, `period` and the value columns, sorted by gid then period: the file `geosquare_v2.time_store.read_time_values` reads, and the extension reads what `write_time_values` writes |
| `partitioned` | `data/gid_prefix=<P>/period=<chunk>.parquet` (one file per prefix and month by default), `gid_prefixes.json`, `period_chunks.json` (the chunk files per prefix, for HTTP readers that cannot list folders) |
| `iceberg` | partition spec `truncate(W, packed_id)` as for cell values; one data file per partition and month by default, with `period` lower and upper bounds in the manifests |

The manifest has `dataset_type: cell_time_values`, `period_type`, `bucket_hours`,
`timezone: UTC`, `period_start`, `period_end`, `identity.unique_key: [gid, period]`,
`identity.unique_gid: false`, `storage.sorted_by: [gid, period]`, `storage.time_chunk` and
`time_aggregation` per value field. Time cubes are written as manifest 1.0.

Cell-value datasets are read and written exactly as before; the time options only apply
to a dataset whose manifest says `cell_time_values`.
