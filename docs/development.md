# Development

This page is for people who change the engine.

## Layout

| Folder | What it holds |
|---|---|
| `core/` | Rust core: codec, packing, topology, detector, grid CRS |
| `bindings/python/` | Python package, built with PyO3 and maturin |
| `bindings/wasm/` | npm package, built to WebAssembly |
| `bindings/duckdb/` | DuckDB extension. See its own README |
| `cli/` | `gsq` command-line tool. See `cli/README.md` |
| `plugins/qgis/` | QGIS plugin. See `plugins/qgis/README.md` |
| `vectors/spec-2.0/` | Copies of the spec test vectors |

## Rules

1. The spec lives in `geosquare-v2`. Change the spec there first. Do not add grid rules here.
2. Every binding must pass every test vector.
3. Do not edit the files in `vectors/spec-2.0/` by hand. They are copies. CI checks each file's SHA-256 against `manifest.json`.
4. Invalid input raises an error. Do not repair, cast, or wrap input silently.

## Rust core

```zsh
cd core
cargo test --locked
```

`--locked` makes Cargo use the lock file as it is. Update the lock file on purpose, not by accident.

## Python binding

Build a wheel with the same command CI uses:

```zsh
cd bindings/python
uvx maturin build --release --locked --out dist
```

Install the wheel into a clean Python 3.11 or newer environment, then run the tests against that wheel:

```zsh
uv run --no-project --python 3.11 --with dist/geosquare_engine-*.whl --with "pytest>=8" \
  python -m pytest -q bindings/python/tests
```

The release workflow also checks that the imported `_core` module is the installed wheel, not a file from the source tree.

## WebAssembly binding

```zsh
cd bindings/wasm
npm run build
npm test
```

## Test vectors

`vectors/spec-2.0/` holds files copied from `geosquare-v2`. To add a case, add it to the spec repository first. Then copy the file here in the same pull request, and update `manifest.json`.

## CI

| Workflow | Checks |
|---|---|
| `ci.yml` | Vector and Rust core checks. See the file for the exact steps |
| `python.yml` | Python binding on Python 3.11 to 3.14 |
| `wasm.yml` | WebAssembly binding |
| `duckdb.yml` | DuckDB extension |
| `release.yml` | Python wheels and sdist, and publishing. See [Release](release.md) |

## Before you open a pull request

- Run the Rust tests and the Python tests for the parts you changed.
- Keep the docs in this folder up to date with the code.
- Write the change in English first. The Indonesian summary goes at the bottom of `README.md`.
