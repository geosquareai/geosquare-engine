"""query_cell_datasets: several datasets over one area in one call, compared with one
query_cell_dataset per dataset (cells, values, clashing names, fill_nulls, rollup_level).

    python tests/test_multi_query.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import math, sys, tempfile
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_multi_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
cells = lambda wkt, level=12: f"polygon_to_cells('ID', '{wkt}', {level})"
A = "POLYGON ((106.79 -6.22, 106.83 -6.22, 106.83 -6.18, 106.79 -6.18, 106.79 -6.22))"
B = "POLYGON ((106.81 -6.21, 106.85 -6.21, 106.85 -6.17, 106.81 -6.17, 106.81 -6.21))"  # overlaps A partly
c.execute(f"SELECT * FROM write_cell_dataset('SELECT packed_id, (packed_id % 97)::DOUBLE AS people, (packed_id % 5)::DOUBLE AS pop FROM {cells(A).replace(chr(39), chr(39)*2)}', '{d}/people', layout := 'iceberg', aggregation_rule := 'sum')").fetchall()
c.execute(f"SELECT * FROM write_cell_dataset('SELECT packed_id, (packed_id % 7)::DOUBLE / 10 AS hazard, (packed_id % 3)::DOUBLE AS pop FROM {cells(B).replace(chr(39), chr(39)*2)}', '{d}/data_name=hazard_v1', layout := 'flat', aggregation_rule := '{{\"hazard\": \"mean\", \"pop\": \"sum\"}}')").fetchall()
c.execute(f"SELECT * FROM write_cell_dataset('SELECT left(gid, 9) AS gid, sum(people) AS people FROM read_cell_dataset(''{d}/people'') GROUP BY 1', '{d}/people_1km', layout := 'flat', domain_code := 'ID', aggregation_rule := 'sum')").fetchall()
Q = "POLYGON ((106.80 -6.215, 106.84 -6.215, 106.84 -6.175, 106.80 -6.175, 106.80 -6.215))"
paths = [f"{d}/people", f"{d}/data_name=hazard_v1"]

def close(a, b):
    return math.isclose(a, b, rel_tol=1e-9, abs_tol=1e-9)

for roll, extra in (("", "coverage_ratio"), (", rollup_level := 9", "coverage_ratio")):
    got = c.execute(f"SELECT gid, people, hazard, people__pop, hazard_v1__pop, {extra} FROM query_cell_datasets(?, geometry := ? {roll})", [paths, Q]).fetchall()
    one = lambda p, cols: {r[0]: r[1:] for r in c.execute(f"SELECT gid, {cols} FROM query_cell_dataset(?, geometry := ? {roll})", [p, Q]).fetchall()}
    a, b = one(paths[0], f"people, pop, {extra}"), one(paths[1], f"hazard, pop, {extra}")
    assert len(got) == len(set(a) | set(b)) and len(set(a) - set(b)) > 0 and len(set(b) - set(a)) > 0, (len(got), len(a), len(b))
    for g, people, hazard, pop_a, pop_b, cov in got:
        ra, rb = a.get(g, (0, 0, None)), b.get(g, (0, 0, None))
        assert close(people, ra[0]) and close(pop_a, ra[1]) and close(hazard, rb[0]) and close(pop_b, rb[1]), (g, roll)
        assert close(cov, ra[2] if ra[2] is not None else rb[2]), (g, cov)
assert c.execute("SELECT count(*) FILTER (WHERE hazard IS NULL) FROM query_cell_datasets(?, geometry := ?, fill_nulls := false)", [paths, Q]).fetchone()[0] > 0
# levels differ: only with rollup_level
try:
    c.execute("SELECT * FROM query_cell_datasets(?, geometry := ?)", [[paths[0], f"{d}/people_1km"], Q]); raise AssertionError("levels")
except duckdb.Error as e:
    assert "rollup_level" in str(e), e
r = c.execute("SELECT sum(people__people), sum(people_1km__people) FROM query_cell_datasets(?, bbox := [106.70, -6.30, 106.95, -6.10], rollup_level := 9, max_candidate_limit := 1000000)", [[paths[0], f"{d}/people_1km"]]).fetchone()
assert close(r[0], r[1]), r
g = c.execute("SELECT cell_geometry FROM query_cell_datasets(?, geometry := ?, rollup_level := 9, with_geometry := 'geojson') LIMIT 1", [paths, Q]).fetchone()[0]
assert g.startswith('{"type":"Polygon"'), g
print("multi-dataset query OK")
