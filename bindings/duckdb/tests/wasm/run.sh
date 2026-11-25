#!/usr/bin/env bash
# Browser test: native DuckDB writes datasets and records expected results over HTTP, then
# DuckDB-Wasm in headless Chromium loads build/<platform>/geosquare_v2.duckdb_extension.wasm in
# every bundle and must return the same results:
#   DuckDB-Wasm dev (DuckDB 2.0):  base -> wasm_eh, threads -> wasm_threads
#   DuckDB-Wasm 1.31.0 (DuckDB 1.4.0) and 1.32.0 (DuckDB 1.4.3):
#                                  mvp -> wasm_mvp, eh -> wasm_eh, coi -> wasm_threads
# The page is served with COOP/COEP headers (the threads bundles need SharedArrayBuffer). In the
# threads bundles DuckDB's own parquet extension does not load from extensions.duckdb.org (a
# non-shared-memory build), so there only the checks without Parquet run.
#   PYTHON=<python with duckdb>  CHROMIUM=<chromium binary, optional>  tests/wasm/run.sh
set -euo pipefail
cd "$(dirname "$0")"
EXT_DIR="$(cd ../.. && pwd)"
PYTHON="${PYTHON:-python3}"
PORT="${PORT:-8799}"
OLD_PKGS="1.31.0 1.32.0"
[ -d node_modules ] || npm install --silent
for pkg in $OLD_PKGS; do
  if [ ! -d "pkgs/$pkg/package/dist" ]; then
    mkdir -p "pkgs/$pkg"
    (cd "pkgs/$pkg" && npm pack --silent "@duckdb/duckdb-wasm@$pkg" >/dev/null && tar xzf duckdb-duckdb-wasm-$pkg.tgz)
  fi
done
rm -rf www build && mkdir -p www build
for p in wasm_eh wasm_mvp wasm_threads; do
  [ -f "$EXT_DIR/build/$p/geosquare_v2.duckdb_extension.wasm" ] || { echo "missing build/$p (PLATFORM=all ./build_wasm.sh)" >&2; exit 1; }
  mkdir -p "build/$p" && cp "$EXT_DIR/build/$p/geosquare_v2.duckdb_extension.wasm" "build/$p/"
done
python3 coi_server.py "$PORT" --gzip &   # extensions served gzip-compressed, as published
SERVER=$!
trap 'kill $SERVER 2>/dev/null' EXIT
sleep 1
"$PYTHON" - "$EXT_DIR/build/linux_amd64/geosquare_v2.duckdb_extension" "$PWD/www" "http://127.0.0.1:$PORT/www" <<'PY'
import json, math, sys, duckdb
ext, www, url = sys.argv[1:]
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
src = ("SELECT packed_id, (packed_id % 97)::INTEGER AS population FROM polygon_to_cells('ID', "
       "'POLYGON ((106.80 -6.25, 106.90 -6.25, 106.90 -6.15, 106.80 -6.15, 106.80 -6.25))', 12)")
for layout in ("flat", "partitioned", "iceberg"):
    extra = "" if layout == "flat" else ", partition_level := 7"
    c.execute(f"SELECT * FROM write_cell_dataset(?, ?, layout := '{layout}', value_semantics := 'COUNT'{extra})", [src, f"{www}/{layout}"]).fetchall()
wkt = "POLYGON ((" + ", ".join(f"{106.85 + 0.02 * math.cos(2 * math.pi * (i % 48) / 48)} {-6.2 + 0.02 * math.sin(2 * math.pi * (i % 48) / 48)}" for i in range(49)) + "))"
open(f"{www}/query.wkt", "w").write(wkt)
s = lambda row: [None if v is None else str(v) for v in row]
exp = {"point_to_cell": c.execute("SELECT point_to_cell('ID', 106.894082, -6.26109, 12).gid").fetchone()[0],
       "cells": s(c.execute("SELECT count(*), round(sum(coverage_ratio), 6)::VARCHAR FROM polygon_to_cells('ID', 'POLYGON ((106.80 -6.25, 106.90 -6.25, 106.90 -6.15, 106.80 -6.15, 106.80 -6.25))', 12)").fetchone()),
       "v1_gid": s(c.execute("SELECT v1_gid_to_cell('ID', lonlat_to_v1_gid(106.8272, -6.1754, 12), 12).gid, octet_length(cell_to_wkb('ID', point_to_cell('ID', 106.8, -6.2, 12).gid))").fetchone())}
for layout in ("flat", "partitioned", "iceberg"):
    p = f"{url}/{layout}"
    exp[f"{layout}_read"] = s(c.execute(f"SELECT count(*), sum(population) FROM read_cell_dataset('{p}')").fetchone())
    exp[f"{layout}_query"] = s(c.execute(f"SELECT count(*), sum(population) FROM query_cell_dataset('{p}', geometry := ?, min_coverage := 0.5)", [wkt]).fetchone())
    exp[f"{layout}_line"] = s(c.execute(f"SELECT count(*), sum(population), round(sum(length_ratio), 6)::VARCHAR FROM query_cell_dataset('{p}', geometry := 'LINESTRING (106.82 -6.22, 106.86 -6.18)')").fetchone())
    exp[f"{layout}_points"] = s(c.execute(f"SELECT count(*), sum(point_count) FROM query_cell_dataset('{p}', geometry := 'MULTIPOINT ((106.85 -6.2), (106.85 -6.2), (106.81 -6.24))')").fetchone())
    exp[f"{layout}_rollup"] = s(c.execute(f"SELECT count(*), round(sum(population), 6)::VARCHAR FROM query_cell_dataset('{p}', geometry := ?, rollup_level := 9)", [wkt]).fetchone())
    exp[f"{layout}_radius"] = s(c.execute(f"SELECT count(*), round(sum(population * coverage_ratio), 6)::VARCHAR FROM query_cell_dataset('{p}', geometry := 'POINT (106.85 -6.2)', radius_m := 800)").fetchone())
# time cubes: one row per cell and day over 40 days
tsrc = ("SELECT packed_id, strftime(DATE '2026-01-20' + INTERVAL (i) DAY, '%Y-%m-%d') AS period, "
        "((packed_id % 97) + i)::DOUBLE AS rain FROM polygon_to_cells('ID', "
        "'POLYGON ((106.80 -6.25, 106.90 -6.25, 106.90 -6.15, 106.80 -6.15, 106.80 -6.25))', 12), range(40) t(i)")
for layout in ("flat", "partitioned", "iceberg"):
    extra = "" if layout == "flat" else ", partition_level := 7"
    c.execute(f"SELECT * FROM write_cell_dataset(?, ?, layout := '{layout}', period_type := 'day', value_semantics := 'MEASUREMENT', time_aggregation := 'max'{extra})", [tsrc, f"{www}/time_{layout}"]).fetchall()
exp["period_labels"] = s(c.execute("SELECT period_valid('2026-02-29', 'day')::VARCHAR, period_relation('2026-11-19', 'day', '2026-11', 'month'), period_rollup('2026-11-19T05', 'hour:1', 'day'), strftime(period_interval('2026-11', 'month')['end'], '%Y-%m-%d')").fetchone())
for layout in ("flat", "partitioned", "iceberg"):
    p = f"{url}/time_{layout}"
    exp[f"time_{layout}_month"] = s(c.execute(f"SELECT count(*), round(sum(rain), 6)::VARCHAR FROM read_cell_dataset('{p}', period := '2026-02')").fetchone())
    exp[f"time_{layout}_query"] = s(c.execute(f"SELECT count(*), round(sum(rain), 6)::VARCHAR FROM query_cell_dataset('{p}', geometry := ?, time_rollup := 'month', period_from := '2026-01-25')", [wkt]).fetchone())
exp["multi"] = s(c.execute(f"SELECT count(*), round(sum(flat__population + iceberg__population), 6)::VARCHAR FROM query_cell_datasets(['{url}/flat', '{url}/iceberg'], geometry := ?, rollup_level := 9)", [wkt]).fetchone())
json.dump(exp, open(f"{www}/expected.json", "w"), indent=1)
PY
fail=0
COMBOS=("dev base wasm_eh" "dev threads wasm_threads")
for pkg in $OLD_PKGS; do COMBOS+=("$pkg mvp wasm_mvp" "$pkg eh wasm_eh" "$pkg coi wasm_threads"); done
for combo in "${COMBOS[@]}"; do
  set -- $combo
  echo "== DuckDB-Wasm $1, bundle $2, extension $3"
  # one retry: the coi/threads bundles occasionally stall while starting their pthread workers
  node run_browser.cjs "http://127.0.0.1:$PORT/test.html?pkg=$1&bundle=$2&ext=$3" www/expected.json "${CHROMIUM:-}" ||
    { echo "retrying once"; node run_browser.cjs "http://127.0.0.1:$PORT/test.html?pkg=$1&bundle=$2&ext=$3" www/expected.json "${CHROMIUM:-}"; } || fail=1
done
exit $fail
