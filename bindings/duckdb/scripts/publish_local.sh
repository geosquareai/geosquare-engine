#!/usr/bin/env bash
# Build what this machine can build, lay out the extension repository for every DuckDB release
# from 1.4.0, and sync it to S3 (default s3://geosquare-static/geosquare-v2).
#   scripts/publish_local.sh [s3://bucket/prefix]
# Needs: rust targets x86_64/aarch64-unknown-linux-gnu, cargo-zigbuild + zig (Linux builds
# against glibc 2.28, so they load on RHEL 8 / Debian 10 / Ubuntu 18.10 and newer), emcc
# and Rust nightly with rust-src (wasm_eh, wasm_mvp, wasm_threads), the AWS CLI with write access to the bucket. macOS and Windows come from
# .github/workflows/duckdb-release.yml.
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
