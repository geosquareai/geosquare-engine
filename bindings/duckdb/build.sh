#!/usr/bin/env bash
# Build the loadable extension: build/<platform>/geosquare_v2.duckdb_extension
# One binary targets the DuckDB C API struct v1.2.0, so it loads in DuckDB 1.4 to 2.0.
set -euo pipefail
cd "$(dirname "$0")"
PLATFORM="${DUCKDB_PLATFORM:-linux_amd64}"
TARGET="${CARGO_BUILD_TARGET:-}"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
cargo build --release ${TARGET:+--target "$TARGET"}
LIBDIR="target/${TARGET:+$TARGET/}release"
LIB=""
for f in "$LIBDIR"/libgeosquare_v2.so "$LIBDIR"/libgeosquare_v2.dylib "$LIBDIR"/geosquare_v2.dll; do
  if [ -f "$f" ]; then LIB="$f"; break; fi
done
[ -n "$LIB" ] || { echo "library not found in $LIBDIR" >&2; exit 1; }
mkdir -p "build/$PLATFORM"
python3 scripts/append_extension_metadata.py -l "$LIB" -n geosquare_v2 -p "$PLATFORM" \
  -dv v1.2.0 -ev "v$VERSION" -o "build/$PLATFORM/geosquare_v2.duckdb_extension" >/dev/null
echo "build/$PLATFORM/geosquare_v2.duckdb_extension"
