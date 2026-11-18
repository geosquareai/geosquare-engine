# geosquare-engine

Status: Phase 1 is done. Phase 3 CRS work now uses explicit datum operations. Ten domains have an engine operation. VN remains blocked until its operation review closes. See "CRS status" below.

| Part | Status | Location |
|---|---|---|
| Rust core: GID codec, Int64 packing, topology, column detection | Done. Passes the spec 2.0 vectors | `core/` |
| Python binding (PyO3) | Done | `bindings/python/` |
| WebAssembly / npm binding | Done | `bindings/wasm/` |
| CRS: point to cell, cell to polygon | Phase 3 E8. Explicit datum operations for 10 domains; VN blocked | `core/src/projection.rs`, `core/src/domain.rs`, `core/src/geo.rs` |
| `points.json` and `projected.json` vectors | Pass for released domains. Blocked domains must refuse lon/lat | `vectors/spec-2.0/` |
| DuckDB, Postgres, BigQuery bindings | Planned | - |

## What this repo is

The calculation engine for GeoSquare V2. One set of rules runs in Rust, Python, and JavaScript. The rules come from `geosquare-v2`. Every binding must pass the same test vectors.

## Layout

```text
core/                 Rust core: codec, packing, topology, detector, grid CRS
bindings/python/      Python package geosquare-engine (PyO3)
bindings/wasm/        npm package geosquare-engine (WebAssembly)
vectors/spec-2.0/     Copy of the spec 2.0 test vectors
.github/workflows/    CI: vectors, core, Python, WASM
```

## CRS status

The CRS code is in `core/src/projection.rs` (Lambert Conic Conformal 2SP and Transverse
Mercator), `core/src/domain.rs` (the 11 domains from registry 2.0.0), and `core/src/geo.rs`
(point to cell, cell to WGS84 polygon). The reference is PROJ 9.5.1 via pyproj 3.7.1.

Measured on 300 spread-out points inside each boundary. The explicit pipelines match pinned PROJ for ID, LA, and PH.

| Domain | Written operation | Status |
|---|---|---|
| BN, KH, MM, MY, SG, TH, TL | None; explicit projection pipeline | Available |
| ID | `INVERSE(EPSG:15912) + EPSG:9472` | Available |
| LA | `INVERSE(EPSG:1065)` | Available |
| PH | `INVERSE(EPSG:15708)` | Available |
| VN | `INVERSE(EPSG:6960)` | Blocked. Operation review remains open |

The Rust engine applies the profile-owned operation before projection. It does not query a CRS database. VN stays blocked because EPSG offers competing operations. Independent control points are still needed before any production claim.

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

---

# Bahasa Indonesia

Status: Fase 1 selesai. Bagian CRS Fase 3 sekarang memakai datum operation yang tertulis. Sepuluh domain punya operation di engine. VN masih diblokir sampai review operation selesai. Lihat bagian "CRS status" di atas.

Tabel status ada di atas.

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
