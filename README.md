# geosquare-engine

geosquare-engine is the grid engine for GeoSquare V2. It turns coordinates into cell codes (GIDs), and it rolls cells up to coarser cells. The same rules run in Python, in the browser, and in DuckDB.

Read the [quick start](docs/quickstart.md) to run your first example in a few minutes.

## Why use it

- **Stable IDs across versions.** GIDs from profile 1.0.0 and 2.1.0 still match. See [profile compatibility](https://github.com/geosquareai/geosquare-v2/blob/main/docs/profile-compatibility.md).
- **Near-square cells.** Cells are about 50 m at level 12. The comparison doc tests the cell shape in all 11 ASEAN countries. See [comparison](https://github.com/geosquareai/geosquare-v2/blob/main/docs/comparison.md).
- **Explicit datum per country.** Each country profile names its datum and projection. See [country profiles](https://github.com/geosquareai/geosquare-v2/blob/main/docs/country-profiles.md).
- **Same rules in browser and server.** The browser build and the server use the same Rust core. See [WebAssembly package](bindings/wasm/README.md).
- **Correct rollups.** Weighted means and NULL handling follow the spec. See [data and aggregation](https://github.com/geosquareai/geosquare-v2/blob/main/docs/data-and-aggregation.md).
- **Strict input.** Bad values raise an error. Nothing is rounded or cut silently.

Trade-offs: [H3](https://github.com/geosquareai/geosquare-v2/blob/main/docs/comparison.md#6-h3) and [S2](https://github.com/geosquareai/geosquare-v2/blob/main/docs/comparison.md#5-s2) cover the whole globe. GeoSquare covers the 11 ASEAN domains. [Geohash cells](https://github.com/geosquareai/geosquare-v2/blob/main/docs/comparison.md#4-asean-result) are about twice as wide as they are tall. Admin boundaries suit official reports better than grid cells.

Only 6 domains are released: BN, ID, LA, MY, PH, and SG. KH, MM, TH, TL, and VN are candidate domains. Candidate use needs an explicit opt-in flag. See [CRS status](#crs-status).

## Install

### Python

```zsh
pip install geosquare-engine
```

Version 0.3.0 is on PyPI. It needs Python 3.11 to 3.14.

### DuckDB extension

The DuckDB extension is not on PyPI. Build it from this repo:

```zsh
cd bindings/duckdb
DUCKDB_PLATFORM=osx_arm64 ./build.sh   # on Linux, use linux_amd64
```

Load it in DuckDB:

```sql
LOAD 'build/osx_arm64/geosquare_v2.duckdb_extension';
```

Start DuckDB with `duckdb -unsigned` for a local build. Linux users can also install a published build. See [Install in the DuckDB README](bindings/duckdb/README.md#install).

### Browser and npm

The npm package is not published yet. Build the WebAssembly package from this repo:

```zsh
cargo install wasm-bindgen-cli --version 0.2.130 --locked   # once
cd bindings/wasm
npm run build
```

The build writes `bindings/wasm/pkg/`. See [the WebAssembly README](bindings/wasm/README.md).

### gsq command-line tool

The `gsq` tool is not on PyPI yet. Install it from this repo. It needs the DuckDB extension from the step above.

```zsh
python -m pip install ./cli
export GSQ_EXTENSION="$PWD/bindings/duckdb/build/osx_arm64/geosquare_v2.duckdb_extension"
gsq check cells.csv --domain ID
```

See [the CLI README](cli/README.md).

## Quick start

```python
import geosquare_engine as gs

gid = gs.encode(12, 475168, 491792)  # level 12 cell, about 50 m
print(gid)                           # H2X545M5X4E2
print(gs.decode(gid))                # (12, 475168, 491792)
print(gs.parent(gid))                # H2X545M5X4E
packed = gs.pack(1, gid)             # 1 is the ID domain
print(packed)                        # 31528546977439304
print(gs.unpack(packed))             # (1, 'H2X545M5X4E2')
ring = gs.cell_polygon_wgs84(1, gid, 1000.0, False)
print(len(ring))                     # 5: the first point repeats at the end
```

More examples are in [docs/quickstart.md](docs/quickstart.md).

## Status

Phase 1 is done. Phase 3 CRS work now uses explicit datum operations. All 11 domains have an engine operation. VN remains non-production and provisional. See "CRS status" below.

| Part | Status | Location |
|---|---|---|
| Rust core: GID codec, Int64 packing, topology, column detection | Done. Passes the spec 2.0 vectors | `core/` |
| Python binding (PyO3) | Done | `bindings/python/` |
| WebAssembly / npm binding | Done | `bindings/wasm/` |
| CRS: point to cell, cell to polygon | Phase 3 E8. Explicit datum operations for all 11 domains; VN provisional | `core/src/projection.rs`, `core/src/domain.rs`, `core/src/geo.rs` |
| `points.json` and `projected.json` vectors | Pass. Released domains work by default; candidate domains require explicit opt-in | `vectors/spec-2.0/` |
| DuckDB extension (C API, DuckDB 1.4 to 2.0): grid functions, polyfill, cell datasets (flat, partitioned, Iceberg) | Done | `bindings/duckdb/` |
| `gsq` command-line tool: detect, rollup, join, check, and geometry export | Done | `cli/` |
| `gsq pyramid`: cell levels 12, 10, 8 with static files and `pyramid.json` | Done. Follows `docs/cell-pyramid.md` in geosquare-v2 | `cli/` |
| deck.gl layer `GeoSquareLayer` and zoom rollup in the browser | Done. Peer dependency deck.gl | `bindings/wasm/js/deck.mjs` |
| Signed registry verification | Done. Checks SHA-256, Ed25519 signature, and trusted key | `bindings/duckdb/README.md` |
| Postgres, BigQuery bindings | Planned | - |

## What this repo is

The calculation engine for GeoSquare V2. One set of rules runs in Rust, Python, and JavaScript. The rules come from `geosquare-v2`. Every binding must pass the same test vectors.

## Layout

```text
core/                 Rust core: codec, packing, topology, detector, grid CRS
bindings/python/      Python package geosquare-engine (PyO3)
bindings/wasm/        npm package geosquare-engine (WebAssembly)
bindings/duckdb/      DuckDB loadable extension geosquare_v2 (C API)
vectors/spec-2.0/     Copy of the spec 2.0 test vectors
.github/workflows/    CI: vectors, core, Python, WASM
```

## CRS status

The CRS code is in `core/src/projection.rs` (Lambert Conic Conformal 2SP and Transverse
Mercator), `core/src/domain.rs` (the 11 domains from registry 2.0.0), and `core/src/geo.rs`
(point to cell, cell to WGS84 polygon). The reference is PROJ 9.5.1 via pyproj 3.7.1.

The explicit pipelines match pinned PROJ for ID, LA, and PH.

Six domains are released under the GeoSquare support policy. Five domains remain candidate-only. Candidate CRS use must be explicit.

| Domain | Written operation | Status |
|---|---|---|
| BN, MY, SG | None; explicit projection pipeline | Released |
| ID | `INVERSE(EPSG:15912) + EPSG:9472` | Released |
| LA | `INVERSE(EPSG:1065)` | Released |
| PH | `INVERSE(EPSG:15708)` | Released |
| KH, MM, TH, TL | None; explicit projection pipeline | Candidate; explicit opt-in |
| VN | `INVERSE(EPSG:6960)` | Candidate; explicit opt-in |

The Rust engine applies each profile-owned operation before projection. It does not query a CRS database. Candidate domains require explicit opt-in in APIs that accept candidate CRS use. Production-ready means released under GeoSquare's support policy, not official national geodetic certification.

## Rules

1. The spec lives in `geosquare-v2`. Change the spec there first. Do not add grid rules here.
2. Every binding must pass every test vector.
3. Files in `vectors/spec-2.0/` are copies. Do not edit them by hand. CI checks each file's SHA-256 against `manifest.json`.
4. Invalid input raises an error. Nothing is repaired, cast, or wrapped silently.

## Tests

```zsh
cd core && cargo test --locked
cd bindings/wasm && npm run build && npm test
```

For Python, see `bindings/python/README.md`.

## References

- Spec: `geosquare-v2/internal/architecture/V2SPECS.md`, sections 13 to 16.
- Vectors: `geosquare-v2/conformance/spec-2.0/`.
- Phase plan: the `plan/` folder in the workspace.

## Documentation

The full guide is in [`docs/`](docs/README.md):

- [Quick start](docs/quickstart.md): Python, pandas and Polars, DuckDB, CLI, and browser.
- [Concepts](docs/concepts.md): GIDs, levels, domains, packed IDs, errors.
- [Python API](docs/python-api.md): every function, with examples.
- [Compact cell sets](docs/compact-sets.md): mixed-level areas.
- [Development](docs/development.md): build, test, and CI.
- [Release](docs/release.md): how to publish a Python version.

---

# Bahasa Indonesia

Status: Fase 1 selesai. Bagian CRS Fase 3 memakai datum operation yang tertulis. Enam domain sudah released. Lima domain masih candidate dan butuh opt-in jelas. Lihat bagian "CRS status" di atas.

Tabel status ada di atas.

Fase 5 menambah tiga hal. `gsq pyramid` menulis piramida sel (level 12, 10, dan 8) untuk peta.
`GeoSquareLayer` untuk deck.gl memberi rollup sesuai zoom di browser. `read_raster` sekarang hanya
mengembalikan piksel yang titik tengahnya ada di dalam `bbox`. Detail ada di `cli/README.md` dan
`bindings/wasm/README.md`.

## Fungsi repo ini

Repo ini adalah mesin hitung GeoSquare V2. Aturannya sama di Rust, Python, dan JavaScript. Aturannya datang dari `geosquare-v2`. Semua binding harus lulus test vector yang sama.

## Struktur

Lihat bagian Layout di atas. Isinya sama.

## Aturan

1. Spec ada di `geosquare-v2`. Ubah spec di sana dulu. Jangan tambah aturan grid di sini.
2. Setiap binding harus lulus semua test vector.
3. File di `vectors/spec-2.0/` adalah salinan. Jangan diedit dengan tangan. CI mengecek SHA-256 tiap file terhadap `manifest.json`.
4. Input salah harus melempar error. Tidak ada nilai yang diperbaiki, dipotong, atau dibungkus diam-diam.

## Test

Perintahnya sama dengan bagian Tests di atas. Untuk Python, lihat `bindings/python/README.md`.

## Referensi

- Spec: `geosquare-v2/internal/architecture/V2SPECS.md`, bagian 13 sampai 16.
- Vector: `geosquare-v2/conformance/spec-2.0/`.
- Rencana fase: folder `plan/` di workspace.
