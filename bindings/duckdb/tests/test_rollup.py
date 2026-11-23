"""rollup_level: query_cell_dataset / read_cell_dataset aggregated to an ancestor level by the
aggregation_rule of each column, compared with the same GROUP BY written by hand, for every layout.

    python tests/test_rollup.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import json, math, sys, tempfile
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_rollup_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
src = ("SELECT packed_id, (packed_id % 97)::DOUBLE AS people, (packed_id % 13)::BIGINT AS stores, "
       "(packed_id % 7)::DOUBLE / 10 AS hazard, (packed_id % 5)::DOUBLE AS peak FROM polygon_to_cells('ID', "
       "'POLYGON ((106.79 -6.22, 106.85 -6.22, 106.85 -6.16, 106.79 -6.16, 106.79 -6.22))', 12)")
rules = json.dumps({"people": "sum", "stores": "sum", "hazard": "mean", "peak": "max"})
AREA = "POLYGON ((106.8027 -6.2099, 106.8317 -6.2099, 106.8317 -6.1809, 106.8027 -6.1809, 106.8027 -6.2099))"


def close(a, b):
    return all((x == y) if not isinstance(x, float) else math.isclose(x, y, rel_tol=1e-9, abs_tol=1e-9) for x, y in zip(a, b))


for layout in ("flat", "partitioned", "iceberg"):
    p = f"{d}/{layout}"
    c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := ?, partition_level := 7, aggregation_rule := ?)",
              [src, p, layout, rules]).fetchall()
    for level, children in ((9, 400), (7, 40000)):
        got = c.execute(f"SELECT gid, packed_id, people, stores, hazard, peak, cell_count, coverage_ratio "
                        f"FROM query_cell_dataset(?, geometry := ?, rollup_level := {level})", [p, AREA]).fetchall()
        want = c.execute(f"""SELECT left(gid, {level}) g, pack('ID', g), sum(people * coverage_ratio), sum(stores * coverage_ratio),
                   sum(hazard * coverage_ratio) / sum(coverage_ratio), max(peak), count(*), sum(coverage_ratio) / {children}
                 FROM query_cell_dataset(?, geometry := ?) GROUP BY 1 ORDER BY 2""", [p, AREA]).fetchall()
        assert len(got) == len(want) >= (2 if level == 9 else 1) and all(close(a, b) for a, b in zip(got, want)), (layout, level, got[:2], want[:2])
    # whole dataset (weight 1), with geometry and a rule override
    got = c.execute("SELECT gid, people, hazard, cell_count, cell_geometry FROM read_cell_dataset(?, rollup_level := 9, "
                    "rollup_rule := '{\"hazard\": \"min\"}', with_geometry := 'wkt')", [p]).fetchall()
    want = c.execute("SELECT left(gid, 9), sum(people), min(hazard), count(*), cell_to_geometry('ID', left(gid, 9), 'EPSG:4326', 'wkt') "
                     "FROM read_cell_dataset(?) GROUP BY 1 ORDER BY 1", [p]).fetchall()
    assert len(got) == len(want) and all(close(a, b) for a, b in zip(got, want)), (layout, got[:1], want[:1])
    assert "coverage_ratio" not in [r[0] for r in c.execute("DESCRIBE SELECT * FROM read_cell_dataset(?, rollup_level := 9)", [p]).fetchall()]
    # types: sums and means are DOUBLE, min/max keep the column type
    types = dict(c.execute("SELECT column_name, column_type FROM (DESCRIBE SELECT * FROM query_cell_dataset(?, geometry := ?, rollup_level := 9))", [p, AREA]).fetchall())
    assert types["stores"] == "DOUBLE" and types["peak"] == "DOUBLE" and types["packed_id"] == "BIGINT" and types["cell_count"] == "BIGINT", types
    # rollup to the dataset level itself = the plain query
    a = c.execute("SELECT sum(people), count(*) FROM query_cell_dataset(?, geometry := ?, rollup_level := 12)", [p, AREA]).fetchone()
    b = c.execute("SELECT sum(people * coverage_ratio), count(*) FROM query_cell_dataset(?, geometry := ?)", [p, AREA]).fetchone()
    assert close(a, b), (a, b)
    # an area without data: no rows, same columns
    assert c.execute("SELECT count(*) FROM query_cell_dataset(?, bbox := [110.0, -7.0, 110.01, -6.99], rollup_level := 9)", [p]).fetchone()[0] == 0

for bad, msg in (("rollup_level := 13", "between 0 and the dataset level"), ("rollup_rule := 'sum'", "needs rollup_level"),):
    try:
        c.execute(f"SELECT * FROM query_cell_dataset(?, geometry := ?, {bad})", [f"{d}/flat", AREA])
        raise AssertionError(f"{bad}: no error")
    except duckdb.Error as e:
        assert msg in str(e), (bad, e)
# median is a valid rule for a query rollup: one row per parent, values from its children
med = c.execute("SELECT count(*), count(people) FROM query_cell_dataset(?, geometry := ?, rollup_level := 9, "
                "rollup_rule := '{\"people\": \"median\"}')", [f"{d}/flat", AREA]).fetchone()
assert med[0] > 0 and med[1] <= med[0], med
try:
    c.execute("SELECT * FROM query_cell_dataset(?, geometry := 'POINT (106.81 -6.20)', rollup_level := 9)", [f"{d}/flat"])
    raise AssertionError("points: no error")
except duckdb.Error as e:
    assert "not with points" in str(e), e
print("rollup ok")
