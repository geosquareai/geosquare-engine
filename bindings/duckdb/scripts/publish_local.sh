#!/usr/bin/env bash
# Build every platform on this Linux machine, lay out the extension repository for every
# DuckDB release from 1.4.0, and sync it to S3 (default s3://geosquare-static/geosquare-v2).
#   scripts/publish_local.sh [s3://bucket/prefix]
# Needs: cargo-zigbuild + zig, Rust targets x86_64/aarch64-unknown-linux-gnu,
# x86_64-pc-windows-gnu, x86_64/aarch64-apple-darwin, emcc and Rust nightly with rust-src
# (Wasm), the AWS CLI with write access to the bucket.
#   linux_amd64/arm64  glibc 2.28 (RHEL 8 / Debian 10 / Ubuntu 18.10 and newer)
#   windows_amd64      MinGW build; imports only Windows system DLLs (Windows 10+)
#   osx_amd64/arm64    macOS 11+, linked against a CoreFoundation stub (scripts/macos-stubs,
#                      the 7 functions chrono/iana-time-zone use; macOS has the real
#                      framework). Built without the native S3 client (no macOS SDK here):
#                      reading S3 through httpfs works, writing/maintaining S3 tables does not.
set -euo pipefail
cd "$(dirname "$0")/.."
DEST="${1:-s3://geosquare-static/geosquare-v2}"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
rm -rf dist repo
for pair in "x86_64-unknown-linux-gnu.2.28 linux_amd64" "aarch64-unknown-linux-gnu.2.28 linux_arm64"; do
  set -- $pair
  cargo zigbuild --release --target "$1"
  mkdir -p "dist/$2"
  python3 scripts/append_extension_metadata.py -l "target/${1%.2.28}/release/libgeosquare_v2.so" \
    -n geosquare_v2 -p "$2" -dv v1.2.0 -ev "v$VERSION" -o "dist/$2/geosquare_v2.duckdb_extension" >/dev/null
done
cargo zigbuild --release --target x86_64-pc-windows-gnu
mkdir -p dist/windows_amd64
python3 scripts/append_extension_metadata.py -l target/x86_64-pc-windows-gnu/release/geosquare_v2.dll \
  -n geosquare_v2 -p windows_amd64 -dv v1.2.0 -ev "v$VERSION" -o dist/windows_amd64/geosquare_v2.duckdb_extension >/dev/null
for pair in "aarch64-apple-darwin osx_arm64" "x86_64-apple-darwin osx_amd64"; do
  set -- $pair
  env MACOSX_DEPLOYMENT_TARGET=11.0 \
    "CARGO_TARGET_$(echo "$1" | tr 'a-z-' 'A-Z_')_RUSTFLAGS=-C link-arg=-F$PWD/scripts/macos-stubs" \
    cargo zigbuild --release --target "$1" --no-default-features
  mkdir -p "dist/$2"
  python3 scripts/append_extension_metadata.py -l "target/$1/release/libgeosquare_v2.dylib" \
    -n geosquare_v2 -p "$2" -dv v1.2.0 -ev "v$VERSION" -o "dist/$2/geosquare_v2.duckdb_extension" >/dev/null
done
PLATFORM=all ./build_wasm.sh
for p in wasm_eh wasm_mvp wasm_threads; do
  mkdir -p "dist/$p" && cp "build/$p/geosquare_v2.duckdb_extension.wasm" "dist/$p/"
done
python3 scripts/make_repository.py --artifacts dist --out repo --versions auto
aws s3 sync repo "$DEST/" --exclude "*" --include "*.duckdb_extension.gz" --content-type application/octet-stream
# Wasm builds are stored gzip-compressed with Content-Encoding: gzip (about 60% smaller);
# browsers decompress them transparently, the path and name stay the same.
rm -rf repo_wasm_gz && mkdir repo_wasm_gz
(cd repo && find . -name '*.wasm' | while read -r f; do mkdir -p "../repo_wasm_gz/$(dirname "$f")"; gzip -9 -n -c "$f" > "../repo_wasm_gz/$f"; done)
aws s3 sync repo_wasm_gz "$DEST/" --exclude "*" --include "*.wasm" --content-type application/wasm --content-encoding gzip
rm -rf repo_wasm_gz
aws s3 cp repo/index.json "$DEST/index.json" --content-type application/json
echo "published $(find repo -type f | wc -l) files to $DEST"
