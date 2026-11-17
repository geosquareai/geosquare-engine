# geosquare-engine

Status: Phase 1 is done for the scope without CRS. CRS work and two CRS test vector files move to Phase 3.

| Part | Status | Location |
|---|---|---|
| Rust core: GID codec, Int64 packing, topology, column detection | Done. Passes the spec 2.0 vectors | `core/` |
| Python binding (PyO3) | Done | `bindings/python/` |
| WebAssembly / npm binding | Done | `bindings/wasm/` |
| CRS: point to cell, cell to polygon | Phase 3 | - |
| `points.json` and `projected.json` vectors | Phase 3. They need CRS | `vectors/spec-2.0/` |
| DuckDB, Postgres, BigQuery bindings | Planned | - |

## What this repo is

The calculation engine for GeoSquare V2. One set of rules runs in Rust, Python, and JavaScript. The rules come from `geosquare-v2`. Every binding must pass the same test vectors.

## Layout

```text
core/                 Rust core: codec, packing, topology, detector
bindings/python/      Python package geosquare-engine (PyO3)
bindings/wasm/        npm package geosquare-engine (WebAssembly)
vectors/spec-2.0/     Copy of the spec 2.0 test vectors
.github/workflows/    CI: vectors, core, Python, WASM
```

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

Status: Fase 1 selesai untuk bagian tanpa CRS. Bagian CRS dan dua file vector (`points.json`, `projected.json`) pindah ke Fase 3.

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
