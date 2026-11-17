# geosquare-engine

Status: kerangka saja. Belum ada kode. Kode mulai ditulis di Fase 1.

## Fungsi repo ini

Repo ini adalah mesin hitung GeoSquare V2. Logikanya sama di Python, JavaScript (WASM), DuckDB, dan Postgres.

## Aturan utama

1. Aturan grid tidak ditulis di sini. Sumbernya ada di `geosquare-v2`, spec 2.0, folder `conformance/spec-2.0/`.
2. Semua binding harus lulus semua test vector. Tidak ada binding yang boleh lewat dengan sebagian test saja.
3. Engine mengunci tiga hal: versi spec, versi registry, dan SHA-256 dari `conformance/spec-2.0/manifest.json`.
4. Kalau butuh aturan baru, ubah spec di `geosquare-v2` dulu. Jangan menambah aturan di sini.

## Struktur rencana

```text
core/       Rust: codec, packing, hierarki, range, tetangga, detector
crs/        Rust: titik ke sel dan sel ke polygon
bindings/   python (PyO3), wasm (npm), duckdb, postgres, bigquery
cli/        perintah gsq
vectors/    salinan test vector yang dikunci ke versi spec
```

## Referensi

- Spec: `geosquare-v2/internal/architecture/V2SPECS.md`, bagian 13 sampai 16.
- Vector: `geosquare-v2/conformance/spec-2.0/`.
- Rencana: `plan/` di folder workspace.
