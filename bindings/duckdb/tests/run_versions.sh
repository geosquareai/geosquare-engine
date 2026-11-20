#!/usr/bin/env bash
# Run the vector tests against every DuckDB release line the extension supports.
# Venvs are cached in $DUCKDB_VENVS (default: ./.venvs). Override versions with DUCKDB_VERSIONS.
set -uo pipefail
cd "$(dirname "$0")/.."
EXT="${1:-build/linux_amd64/geosquare_v2.duckdb_extension}"
VERSIONS="${DUCKDB_VERSIONS:-1.4.0 1.4.1 1.4.2 1.4.3 1.4.4 1.4.5 1.5.0 1.5.1 1.5.2 1.5.3 1.5.4 1.5.5 1.5.6 2.0.0}"
VENVS="${DUCKDB_VENVS:-.venvs}"
TESTS="${TESTS:-tests/test_vectors.py}"
fail=0
for v in $VERSIONS; do
  venv="$VENVS/duckdb-$v"
  if [ ! -x "$venv/bin/python" ]; then
    python3 -m venv "$venv" && "$venv/bin/pip" install -q "duckdb==$v" >/dev/null || { echo "duckdb $v: install failed"; fail=1; continue; }
  fi
  for t in $TESTS; do
    if ! "$venv/bin/python" "$t" "$EXT"; then echo "duckdb $v: FAILED ($t)"; fail=1; fi
  done
done
exit $fail
