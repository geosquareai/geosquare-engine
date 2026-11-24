# Manifest 1.1: statistics, parent rules, extent, governance and file hashes

Manifest 1.1 is an opt-in version of the cell manifest. A dataset written with it records
more facts about its own contents: how each column rolls up to a parent cell, which area it
covers, who owns it, and a hash of every data file. Readers use these facts to check a
dataset before they trust it.

The default is still manifest 1.0. Nothing changes for a write unless you pass
`manifest_version := '1.1'`. A later write to a 1.1 dataset stays 1.1 without asking.

Every SQL block on this page runs in `tests/test_docs_examples.py`. Paths under `/data/`
point at a temporary folder there.

## Write a 1.1 dataset

```sql
CREATE TABLE src AS
SELECT packed_id,
       (packed_id % 97)::BIGINT AS people,
       CASE WHEN packed_id % 5 = 0 THEN NULL ELSE (packed_id % 7)::DOUBLE / 10 END AS depth_m,
       (packed_id % 11)::DOUBLE / 100 AS unemployment_rate,
       CASE WHEN packed_id % 3 = 0 THEN 'low' ELSE 'high' END AS zone
FROM polygon_to_cells('ID', 'POLYGON ((106.78 -6.24, 106.84 -6.24, 106.84 -6.20, 106.78 -6.20, 106.78 -6.24))', 12);
```

```sql
SELECT * FROM write_cell_dataset('SELECT * FROM src', '/data/flat11', layout := 'flat',
    manifest_version := '1.1', fill_nulls := false,
    title := 'Test grid', owner := 'data team', source_name := 'synthetic', license := 'internal-test',
    value_semantics := '{"people": "COUNT", "unemployment_rate": "RATE", "zone": "CATEGORICAL"}',
    space_aggregation := '{"people": "sum", "unemployment_rate": "weighted_mean"}',
    space_weight := '{"unemployment_rate": "people"}')
```

The same call works for `layout := 'partitioned'` and `layout := 'iceberg'`. Iceberg also
accepts `mode := 'append'` later, and that append keeps 1.1.

The parameters that only manifest 1.1 records:

| Parameter | Default | Allowed values |
|---|---|---|
| `manifest_version` | 1.0 (or the version of the dataset being replaced) | `'1.0'`, `'1.1'` |
| `owner`, `source_name`, `license` | null | any text |
| `share_policy` | `internal` | `internal`, `k_anon_only`, `public` |
| `refresh` | `once` | `once`, `daily`, `weekly`, `monthly`, `yearly`, `irregular` |
| `derived_from` | empty list | a JSON list of text, or comma-separated text |
| `space_aggregation` | see the rules below | one word for every column, or `{column: rule}` |
| `space_weight` | null | `area`, or the name of a COUNT or TOTAL column, or `{column: name}` |

`title` and the other descriptive parameters work the same as in 1.0.

## Check a 1.1 dataset

```sql
SELECT manifest->>'manifest_version' AS manifest_version FROM cell_dataset_info('/data/flat11')
```

```sql
SELECT file, content_sha256 = manifest_content_sha256 AS hash_matches FROM cell_dataset_content_hashes('/data/flat11')
```

`cell_dataset_content_hashes` reads every data file again and computes its hash. The hash
it recorded is in `manifest_content_sha256`. A `false` means the file changed after the write.

The Python library in geosquare-v2 reads flat folders written this way and checks the same
hashes and statistics: `geosquare_v2.dataset.read_cell_dataset(path).validate()`. Version 1.3.0
or newer is needed. Older readers reject manifest 1.1.

## What the writer records

| Field | What it holds | How the writer computes it |
|---|---|---|
| `value_fields[].null_share` | Share of rows that are NULL in the column | NULL rows divided by all rows, over the values as written |
| `value_fields[].min_value`, `max_value` | Smallest and largest value of a numeric column | Over the values as written, NULLs left out. Null for text columns and for columns with no value |
| `value_fields[].space_aggregation`, `space_weight` | The parent rule and its weight | See the rules below |
| `extent.bbox_wgs84` | `[min lon, min lat, max lon, max lat]` | Smallest box around the outline of every cell |
| `extent.compact_set_path` | Path of a compact cell set | Always null for now |
| `storage.data_files[]` | One entry per Parquet file: `path`, `content_sha256`, `row_count` | Path is relative to the dataset folder. The hash is computed from the rows (section 3.1 of the dataset metadata spec) |
| `governance` | `title`, `owner`, `source_name`, `license`, `share_policy`, `refresh` | Parameters, else the values of the dataset being replaced, else the defaults |
| `provenance.spec_version` | Spec the writer followed | `"2.0"` |
| `provenance.derived_from` | Lineage: source dataset IDs or crosswalk IDs | Parameter, else the dataset being replaced |

The writer computes these from the rows before it writes any file. Each Parquet file embeds
the manifest, so the file hashes and the statistics must be known first.

## Parent rules

When a map shows a parent cell, its value comes from its children. Each value field has one
rule (`space_aggregation`). The rule must fit the column's semantics:

| Semantics | Allowed `space_aggregation` | Default | Weight |
|---|---|---|---|
| COUNT, TOTAL | `sum`, `none` | `sum` | none |
| DENSITY | `weighted_mean`, `none` | `weighted_mean` | `area` |
| MEASUREMENT | `weighted_mean`, `median`, `max`, `min`, `none` | `none` | `weighted_mean` needs one |
| RATE | `weighted_mean`, `none` | `none` | `weighted_mean` needs one |
| CATEGORICAL | `none` (`majority_weighted` is not implemented yet) | `none` | none |
| ORDINAL | `max`, `min`, `none` | `none` | none |
| RANGE | `none` | `none` | none |

- `none` leaves the column out of roll-ups. It is the default for every semantics without a
  rule, so a column is never rolled up by accident.
- A weight is `area`, or the name of a COUNT or TOTAL column in the same dataset. A RATE
  column weighted by `people` gives the population-weighted rate of the parent. A plain mean
  of child rates gives the wrong parent rate when children differ in size.
- `median` takes no weight and needs at least 3 child values with data.
- A rule that does not fit the semantics is refused with the allowed list in the message.
- Appending to or overwriting a 1.1 dataset keeps its rules unless the write gives new ones.

## Content hash

The hash of a data file covers its rows, not its bytes. Each row becomes a JSON array:
`gid` first, then the value fields in manifest order. Numbers use Python's float notation,
and text uses Python's JSON escapes. The rows are sorted by bytes, and one SHA-256 covers them.
Column names and types are not hashed, so a column rename would not change the hash.

Rules and test vectors: section 3.1 of the dataset metadata spec in the geosquare-v2
repository, and `vectors/spec-2.0/content-hash.json` in this repository. The engine and the
Python library must agree on every vector.

## Extent

`extent.bbox_wgs84` covers the outline of every cell, not only the cell centres. The outline
comes from the same function as `cell_to_geometry(..., 'EPSG:4326')`. The extent is in lon/lat,
so a 1.1 write needs a domain whose CRS is released. For a candidate domain such as VN, the
write is refused with "not released for lon/lat". A 1.0 write to the same domain still works.

## Operations on 1.1 datasets

| Operation | Behaviour on a 1.1 dataset |
|---|---|
| `write_cell_dataset`, create, flat, partitioned or Iceberg | Supported with `manifest_version := '1.1'` |
| `overwrite` | Supported. Statistics, extent and hashes are recomputed. Governance and rules are kept unless the write gives new ones |
| `append` (Iceberg) | Supported. Statistics and extent are merged with the old ones. `data_files` lists the old files and the new ones |
| `append` with `manifest_version := '1.0'` | Refused: it would drop the hashes |
| `append` with `manifest_version := '1.1'` onto a 1.0 table | Refused. Overwrite the table to upgrade it |
| `mode := 'upsert'` | Refused for now. Use `append` for new cells, or `overwrite` |
| `compact_cell_dataset` | Supported. The dataset stays 1.1, and statistics and hashes are recomputed |
| `expire_snapshots` | Unchanged. It only removes old metadata and files |
| `alter_cell_dataset` (rename, add, drop) | Refused. The content hash reads columns by name, so a renamed column cannot be checked yet |
| `update_cell_metadata` on Iceberg | Supported. Only the table properties change |
| `update_cell_metadata` on a flat or partitioned folder | Refused. The Python reader needs the Parquet files to carry the same manifest as `manifest.json` |
| `query_cell_dataset` with `rollup_level` | Uses the stored rules |

To change a 1.1 folder's metadata, rewrite it with the new values:

```sql
SELECT * FROM write_cell_dataset('SELECT * FROM read_cell_dataset(''/data/flat11'')', '/data/flat11', layout := 'flat', mode := 'overwrite', title := 'Renamed grid')
```

To upgrade a 1.0 dataset, overwrite it with `manifest_version := '1.1'`. A 1.1 dataset goes
back to 1.0 the same way, with `manifest_version := '1.0'`.

```sql
SELECT * FROM write_cell_dataset('SELECT packed_id, (packed_id % 97)::BIGINT AS people FROM polygon_to_cells(''ID'', ''POLYGON ((106.78 -6.24, 106.84 -6.24, 106.84 -6.20, 106.78 -6.20, 106.78 -6.24))'', 12)', '/data/flat10', layout := 'flat', value_semantics := '{"people": "COUNT"}')
```

```sql
SELECT * FROM write_cell_dataset('SELECT * FROM read_cell_dataset(''/data/flat10'')', '/data/flat10', layout := 'flat', mode := 'overwrite', manifest_version := '1.1', owner := 'data team')
```

## Limits

- **Time and memory.** A write hashes each data file from the staged rows, and computes the
  statistics and the extent from them. Writes take longer and use more memory than 1.0 writes.
- **NULL numbers.** By default numeric NULLs are written as 0 (`fill_nulls`), so `null_share`
  is 0 for such a column. Pass `fill_nulls := false` to keep NULLs and record them.
- **Column types.** A value column must be an integer, FLOAT, DOUBLE or VARCHAR. BOOLEAN,
  DATE, DECIMAL, HUGEINT and other types are refused. A plain literal such as `0.5` is a
  DECIMAL, so cast it: `0.5::DOUBLE`.
- **Values.** NaN and infinity cannot be hashed, so such a write fails.
- **Compact.** `compact_cell_dataset` writes NULL numbers as 0 (its `fill_nulls` is on). This
  is the 1.0 behaviour too, so do not compact a dataset whose NULLs you need to keep.
- **Not yet implemented.** `majority_weighted` rules, `compact_set_path`, column changes
  in 1.1 tables, and upsert.
- **Readers.** The Python reader in version 1.3.0 reads flat folders only. For partitioned
  folders and Iceberg tables, use the engine's `cell_dataset_content_hashes` and
  `cell_dataset_info`.
