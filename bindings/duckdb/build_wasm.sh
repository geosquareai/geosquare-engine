#!/usr/bin/env bash
# Build the extension for DuckDB-Wasm: build/<platform>/geosquare_v2.duckdb_extension.wasm
# Needs Emscripten (emcc on PATH; DuckDB's CI uses emsdk 6.0.9) and the Rust target
# wasm32-unknown-emscripten. PLATFORM picks the DuckDB-Wasm bundle it loads in:
#   wasm_eh      (default) the "eh" / "base" bundle of current browsers
#   wasm_mvp     the "mvp" bundle for browsers without Wasm exception handling. Built with
#                panic=abort the module uses no exceptions: the wasm_eh binary, mvp metadata.
# Every variant needs Rust nightly with rust-src (std is rebuilt); WASM_TOOLCHAIN picks it
# (default `nightly`, CI pins nightly-2026-11-18).
#   wasm_threads the "coi" / "threads" bundle (cross-origin isolated, SharedArrayBuffer).
#                Needs a shared-memory side module: Rust nightly with rust-src
#                (`rustup toolchain install nightly --component rust-src`), std rebuilt with
#                atomics, linked with -pthread.
#   all          the three.
set -euo pipefail
cd "$(dirname "$0")"
PLATFORM="${PLATFORM:-wasm_eh}"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
command -v emcc >/dev/null || { echo "emcc not found: source <emsdk>/emsdk_env.sh" >&2; exit 1; }

package() { # <platform> <side module>
  mkdir -p "build/$1"
  python3 scripts/append_extension_metadata.py -l "$2" -n geosquare_v2 \
    -p "$1" -dv v1.2.0 -ev "v$VERSION" -o "build/$1/geosquare_v2.duckdb_extension.wasm" >/dev/null
  echo "build/$1/geosquare_v2.duckdb_extension.wasm"
}

# std is rebuilt with panic=abort (Rust nightly + rust-src, as for wasm_threads): no unwinding
# imports, so the module links into every DuckDB-Wasm bundle, also mvp (no Wasm exceptions).
single() { # one side module, packaged for each given platform
  local tdir=target/wasm-single
  RUSTFLAGS="-C panic=abort" \
    cargo "+${WASM_TOOLCHAIN:-nightly}" rustc -Z build-std=std,panic_abort --release --target wasm32-unknown-emscripten \
      --target-dir "$tdir" --no-default-features --lib --crate-type staticlib
  local side=build/geosquare_v2.single.wasm
  mkdir -p build
  emcc "$tdir/wasm32-unknown-emscripten/release/libgeosquare_v2.a" -o "$side" \
    -O3 -sSIDE_MODULE=2 -sEXPORTED_FUNCTIONS=_geosquare_v2_init_c_api
  for p in "$@"; do package "$p" "$side"; done
  rm -f "$side"
}

threads() {
  local tdir=target/wasm-threads
  RUSTFLAGS="-C panic=abort -C target-feature=+atomics,+bulk-memory,+mutable-globals" \
    cargo "+${WASM_TOOLCHAIN:-nightly}" rustc -Z build-std=std,panic_abort --release --target wasm32-unknown-emscripten \
      --target-dir "$tdir" --no-default-features --lib --crate-type staticlib
  local side=build/geosquare_v2.threads.wasm
  mkdir -p build
  emcc "$tdir/wasm32-unknown-emscripten/release/libgeosquare_v2.a" -o "$side" \
    -O3 -pthread -sSIDE_MODULE=2 -sEXPORTED_FUNCTIONS=_geosquare_v2_init_c_api
  package wasm_threads "$side"
  rm -f "$side"
}

case "$PLATFORM" in
  wasm_eh) single wasm_eh ;;
  wasm_mvp) single wasm_mvp ;;
  wasm_threads) threads ;;
  all) single wasm_eh wasm_mvp; threads ;;
  *) echo "PLATFORM must be wasm_eh, wasm_mvp, wasm_threads or all" >&2; exit 1 ;;
esac
