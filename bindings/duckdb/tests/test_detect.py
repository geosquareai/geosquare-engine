"""detect_table on the spec 2.0 detection vectors, and packed_to_gid against point_to_cell.

    python tests/test_detect.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import json, random, sys
from pathlib import Path
import duckdb

ext = sys.argv[1]
root = Path(__file__).resolve().parents[3]
cases = json.loads((root / "vectors" / "spec-2.0" / "detect.json").read_text())["cases"]
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
TYPES = {"string": "VARCHAR", "int64": "BIGINT", "float64": "DOUBLE", "other": "DOUBLE"}
for case in cases:
    cols = case["columns"]
    n = max(len(v["values"]) for v in cols.values())
    selects = []
    for name, spec in cols.items():
        vals = spec["values"] + [None] * (n - len(spec["values"]))
        lit = [("NULL" if v is None else ("'" + str(v).replace("'", "''") + "'")) for v in vals]
        selects.append(f"unnest([{', '.join(lit)}]::VARCHAR[])::{TYPES[spec['dtype']]} AS \"{name}\"")
    c.execute(f"CREATE OR REPLACE TABLE t AS SELECT {', '.join(selects)}")
    opts = "".join(f", {k} := {repr(case[k]) if isinstance(case[k], str) else case[k]}" for k in ("domain_code", "level") if k in case)
    got = {r[0]: r for r in c.execute(f"SELECT * FROM detect_table('SELECT * FROM t'{opts})").fetchall()}
    for col, want in case["expect"].items():
        assert col in got, (case["name"], col, list(got))
        _, kind, status, reason, ratio, codes, ids, levels, examples = got[col]
        have = {"kind": kind, "status": status, "domain_codes": codes, "domain_ids": ids, "levels": levels}
        for k, v in want.items():
            assert have[k] == v, (case["name"], col, k, have[k], v, reason)
# packed_to_gid = the gid of point_to_cell for the same cell, at several levels
rng = random.Random(1)
pts = [(rng.uniform(95.5, 140.5), rng.uniform(-10.5, 5.5), rng.choice([5, 7, 9, 12, 14])) for _ in range(2000)]
c.execute("CREATE OR REPLACE TABLE p AS SELECT * FROM (VALUES " + ", ".join(f"({x}, {y}, {l})" for x, y, l in pts) + ") v(lon, lat, lvl)")
bad = c.execute("SELECT count(*) FROM (SELECT point_to_cell('ID', lon, lat, lvl) AS c FROM p) WHERE packed_to_gid(c.packed_id) <> c.gid").fetchone()[0]
assert bad == 0, bad
assert c.execute("SELECT packed_to_gid(NULL::BIGINT)").fetchone()[0] is None
# cell_to_geometry(packed_id, ...) takes the domain from the packed id; same output as ('ID', gid, ...)
bad = c.execute("""SELECT count(*) FROM (SELECT point_to_cell('ID', lon, lat, lvl) AS c FROM p)
  WHERE cell_to_geometry(c.packed_id) <> cell_to_geometry('ID', c.gid)
     OR cell_to_geometry(c.packed_id, 'EPSG:4326') <> cell_to_geometry('ID', c.gid, 'EPSG:4326')
     OR cell_to_geometry(c.packed_id, 'EPSG:4326', 'geojson') <> cell_to_geometry('ID', c.gid, 'EPSG:4326', 'geojson')""").fetchone()[0]
assert bad == 0, bad
assert c.execute("SELECT cell_to_geometry(NULL::BIGINT, 'EPSG:4326')").fetchone()[0] is None
print(f"detect OK: {len(cases)} detection vectors; packed_to_gid and cell_to_geometry(packed_id) match on 2000 cells")
