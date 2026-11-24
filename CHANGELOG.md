# Changelog

## 0.3.0 (Python package `geosquare-engine`)

Tag: `py-v0.3.0`. Requires Python 3.11 to 3.14, same as 0.2.0.

### Added

- pandas accessor `gsq` for GID columns and Series. Import once with
  `import geosquare_engine.pandas_gsq`, then use `df.gsq.parent(...)`.
  Covers parent, ring, level and geometry conversion.
- Polars accessor `gsq` with the same methods. Import once with
  `import geosquare_engine.polars_gsq`.
- pandas and Polars stay optional. The package has no new hard dependencies.

### Changed

- The core crate reads profile 2.1.0 datasets (written by geosquare-v2 1.0.0)
  with their own datum rule. This is internal to the Rust core. The Python GID
  functions give the same results as 0.2.0 for the current profile.

### Not in this release

These ship in other packages or later releases, not in the Python wheel:

- The `gsq` command line tool (`cli/`).
- The DuckDB extension (`bindings/duckdb/`).
- The npm WebAssembly package and deck.gl layer (`bindings/wasm/`).
