#!/usr/bin/env bash
# Build the extension for DuckDB-Wasm: build/<platform>/geosquare_v2.duckdb_extension.wasm
# Needs Emscripten (emcc on PATH; DuckDB's CI uses emsdk 6.0.9) and the Rust target
# wasm32-unknown-emscripten. PLATFORM is wasm_eh (default; the DuckDB-Wasm "base"
# bundle) or wasm_mvp. Same C API footer as the native build.
set -euo pipefail
cd "$(dirname "$0")"
PLATFORM="${PLATFORM:-wasm_eh}"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
command -v emcc >/dev/null || { echo "emcc not found: source <emsdk>/emsdk_env.sh" >&2; exit 1; }
cargo rustc --release --target wasm32-unknown-emscripten --no-default-features --lib --crate-type staticlib
mkdir -p "build/$PLATFORM"
emcc target/wasm32-unknown-emscripten/release/libgeosquare_v2.a -o "build/$PLATFORM/geosquare_v2.no_metadata.wasm" \
  -O3 -sSIDE_MODULE=2 -sEXPORTED_FUNCTIONS=_geosquare_v2_init_c_api
python3 scripts/append_extension_metadata.py -l "build/$PLATFORM/geosquare_v2.no_metadata.wasm" -n geosquare_v2 \
  -p "$PLATFORM" -dv v1.2.0 -ev "v$VERSION" -o "build/$PLATFORM/geosquare_v2.duckdb_extension.wasm" >/dev/null
rm -f "build/$PLATFORM/geosquare_v2.no_metadata.wasm"
echo "build/$PLATFORM/geosquare_v2.duckdb_extension.wasm"
