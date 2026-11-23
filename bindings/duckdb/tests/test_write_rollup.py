"""write_cell_dataset(..., rollup_level := R): overview datasets written straight from a finer
dataset, compared with the same aggregation written by hand; metadata (rules) carried by the
writer parameters.

    python tests/test_write_rollup.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import json, math, sys, tempfile
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_wroll_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
src = ("SELECT packed_id, (packed_id % 97)::DOUBLE AS people, (packed_id % 13)::BIGINT AS stores, "
       "(packed_id % 7)::DOUBLE / 10 AS hazard FROM polygon_to_cells('ID', "
       "'POLYGON ((106.79 -6.22, 106.85 -6.22, 106.85 -6.16, 106.79 -6.16, 106.79 -6.22))', 12)")
rules = json.dumps({"people": "sum", "stores": "sum", "hazard": "mean"})
c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', partition_level := 7, aggregation_rule := ?)",
          [src, f"{d}/fine", rules]).fetchall()
close = lambda a, b: all(x == y if not isinstance(x, float) else math.isclose(x, y, rel_tol=1e-9) for x, y in zip(a, b))
for layout, level in (("flat", 9), ("iceberg", 9), ("partitioned", 7)):
    out = f"{d}/{layout}_{level}"
    c.execute(f"SELECT * FROM write_cell_dataset('SELECT * FROM read_cell_dataset(''{d}/fine'')', ?, layout := ?, "
              f"partition_level := 5, rollup_level := {level}, aggregation_rule := ?)", [out, layout, rules]).fetchall()
    got = c.execute("SELECT gid, packed_id, people, stores, hazard FROM read_cell_dataset(?) ORDER BY packed_id", [out]).fetchall()
    want = c.execute(f"SELECT left(gid, {level}) g, pack('ID', g), sum(people), sum(stores)::DOUBLE, avg(hazard) "
                     f"FROM read_cell_dataset(?) GROUP BY 1 ORDER BY 2", [f"{d}/fine"]).fetchall()
    assert len(got) == len(want) > 0 and all(close(a, b) for a, b in zip(got, want)), (layout, got[:2], want[:2])
    info = c.execute("SELECT level, row_count FROM cell_dataset_info(?)", [out]).fetchone()
    assert info == (level, len(want)), info
    agg = {r[0]: r[3] for r in c.execute("SELECT * FROM cell_dataset_columns(?)", [out]).fetchall()}
    assert agg == {"people": "sum", "stores": "sum", "hazard": "mean"}, agg
# from an area query: coverage_ratio weights the cells and is not written
area = "POLYGON ((106.80 -6.21, 106.83 -6.21, 106.83 -6.18, 106.80 -6.18, 106.80 -6.21))"
c.execute(f"SELECT * FROM write_cell_dataset('SELECT * FROM query_cell_dataset(''{d}/fine'', geometry := ''{area}'')', ?, "
          "rollup_level := 9, aggregation_rule := ?)", [f"{d}/area_1km", rules]).fetchall()
a = c.execute("SELECT sum(people) FROM read_cell_dataset(?)", [f"{d}/area_1km"]).fetchone()[0]
b = c.execute("SELECT sum(people * coverage_ratio) FROM query_cell_dataset(?, geometry := ?)", [f"{d}/fine", area]).fetchone()[0]
assert math.isclose(a, b, rel_tol=1e-9), (a, b)
assert "coverage_ratio" not in [r[0] for r in c.execute("DESCRIBE SELECT * FROM read_cell_dataset(?)", [f"{d}/area_1km"]).fetchall()]
# without rules: COUNT columns summed, others averaged
c.execute(f"SELECT * FROM write_cell_dataset('SELECT * FROM read_cell_dataset(''{d}/fine'')', ?, rollup_level := 9, "
          "value_semantics := '{{\"people\": \"COUNT\", \"stores\": \"COUNT\", \"hazard\": \"MEASUREMENT\"}}')".replace("{{", "{").replace("}}", "}"), [f"{d}/sem"]).fetchall()
x = c.execute("SELECT sum(people), avg(hazard) FROM read_cell_dataset(?)", [f"{d}/sem"]).fetchone()
y = c.execute("SELECT sum(people), avg(h) FROM (SELECT left(gid, 9), sum(people) people, avg(hazard) h FROM read_cell_dataset(?) GROUP BY 1)", [f"{d}/fine"]).fetchone()
assert close(x, y), (x, y)
for bad, msg in (("rollup_level := 20", "rollup_level must be"),):
    try:
        c.execute(f"SELECT * FROM write_cell_dataset('SELECT * FROM read_cell_dataset(''{d}/fine'')', '{d}/bad', {bad})").fetchall()
        raise AssertionError(bad)
    except duckdb.Error as e:
        assert msg in str(e), (bad, e)
# median: the plain median of the children, NULL with fewer than 3 children
groups = {}
for (g,) in c.execute("SELECT gid FROM polygon_to_cells('ID', ?, 12)", [area]).fetchall():
    groups.setdefault(g[:9], []).append(g)
rich = next(k for k, v in groups.items() if len(v) >= 3)
sparse = next(k for k, v in groups.items() if k != rich and len(v) >= 2)
vals = {g: float(i) for i, g in enumerate(groups[rich][:3])}
vals.update({g: float(10 + i) for i, g in enumerate(groups[sparse][:2])})
src = ("SELECT pack('ID', gid) AS packed_id, hazard FROM (VALUES "
       + ", ".join(f"('{g}', {v})" for g, v in vals.items()) + ") AS t(gid, hazard)")
c.execute("SELECT * FROM write_cell_dataset(?, ?, rollup_level := 9, aggregation_rule := ?)",
          [src, f"{d}/median", json.dumps({"hazard": "median"})]).fetchall()
got = dict(c.execute("SELECT gid, hazard FROM read_cell_dataset(?)", [f"{d}/median"]).fetchall())
assert got[rich] == 1.0 and got[sparse] is None, (got, rich, sparse)
print("write rollup OK")
