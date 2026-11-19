#!/usr/bin/env bash
# Browser test: native DuckDB writes datasets and records expected results over HTTP,
# then DuckDB-Wasm in headless Chromium loads build/wasm_eh/geosquare_v2.duckdb_extension.wasm
# and must return the same results.
#   PYTHON=<python with duckdb>  CHROMIUM=<chromium binary, optional>  tests/wasm/run.sh
set -euo pipefail
cd "$(dirname "$0")"
EXT_DIR="$(cd ../.. && pwd)"
PYTHON="${PYTHON:-python3}"
PORT="${PORT:-8799}"
[ -d node_modules ] || npm install --silent
rm -rf www && mkdir -p www
cp "$EXT_DIR/build/wasm_eh/geosquare_v2.duckdb_extension.wasm" .
python3 -m http.server "$PORT" --bind 127.0.0.1 >/dev/null 2>&1 &
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
    c.execute(f"SELECT * FROM write_cell_dataset(?, ?, layout := '{layout}'{extra})", [src, f"{www}/{layout}"]).fetchall()
wkt = "POLYGON ((" + ", ".join(f"{106.85 + 0.02 * math.cos(2 * math.pi * (i % 48) / 48)} {-6.2 + 0.02 * math.sin(2 * math.pi * (i % 48) / 48)}" for i in range(49)) + "))"
open(f"{www}/query.wkt", "w").write(wkt)
s = lambda row: [None if v is None else str(v) for v in row]
exp = {"point_to_cell": c.execute("SELECT point_to_cell('ID', 106.894082, -6.26109, 12).gid").fetchone()[0]}
for layout in ("flat", "partitioned", "iceberg"):
    exp[f"{layout}_read"] = s(c.execute(f"SELECT count(*), sum(population) FROM read_cell_dataset('{url}/{layout}')").fetchone())
    exp[f"{layout}_query"] = s(c.execute(f"SELECT count(*), sum(population) FROM query_cell_dataset('{url}/{layout}', geometry := ?, min_coverage := 0.5)", [wkt]).fetchone())
    exp[f"{layout}_line"] = s(c.execute(f"SELECT count(*), sum(population), round(sum(length_ratio), 6)::VARCHAR FROM query_cell_dataset('{url}/{layout}', geometry := 'LINESTRING (106.82 -6.22, 106.86 -6.18)')").fetchone())
    exp[f"{layout}_points"] = s(c.execute(f"SELECT count(*), sum(point_count) FROM query_cell_dataset('{url}/{layout}', geometry := 'MULTIPOINT ((106.85 -6.2), (106.85 -6.2), (106.81 -6.24))')").fetchone())
json.dump(exp, open(f"{www}/expected.json", "w"), indent=1)
PY
node run_browser.cjs "http://127.0.0.1:$PORT" www/expected.json "${CHROMIUM:-}"
