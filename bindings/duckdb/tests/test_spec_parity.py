"""Spec 2.0 parity with the geosquare-v2 reference: cell_compact_set (functions, datasets, the
conformance/spec-2.0/compact-set.json vectors), CATEGORICAL majority_weighted roll-ups,
geometry allocation (geometry_to_cells, aggregate_geometries_to_cells), operational boundary
predicates, Crosswalk 1.0, time_values on any query and manifest 1.1 compact extents.
Needs only duckdb; tests/test_parity_oracles.py compares the same functions with geosquare_v2.

    python tests/test_spec_parity.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import json, os, sys, tempfile
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_parity_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
W = "POLYGON ((106.80 -6.22, 106.84 -6.22, 106.84 -6.18, 106.80 -6.18, 106.80 -6.22))"


def fails(sql, params=(), match=""):
    try:
        c.execute(sql, list(params)).fetchall()
    except duckdb.Error as e:
        assert match in str(e), (match, str(e))
        return
    raise AssertionError(f"accepted: {sql}")


# --- cell_compact_set: the spec vectors -------------------------------------------------
cases = json.load(open(os.path.join(os.path.dirname(__file__), "data", "compact-set.json")))["cases"]
for case in cases:
    op, e = case["operation"], case["expect"]
    try:
        if op == "canonicalize":
            ok = c.execute("SELECT compact_canonicalize(?)", [case["input"]]).fetchone()[0] == e["gids"]
        elif op == "expand":
            r = c.execute("SELECT compact_expand(?, ?)", [case["input"], case["level"]]).fetchone()[0]
            ok = "error" not in e and len(r) == e["count"] and ("gids" not in e or r == e["gids"])
        elif op == "contains":
            ok = c.execute("SELECT compact_contains(?, ?)", [case["input"], case["gid"]]).fetchone()[0] == e["member"]
        else:
            why = c.execute("SELECT compact_check(?)", [[x["gid"] for x in case["input"]]]).fetchone()[0]
            ok = (why is None and all(len(x["gid"]) == x["level"] for x in case["input"])) == e["valid"]
    except duckdb.Error:
        ok = "error" in e or e.get("valid") is False
    assert ok, case["name"]
assert len(cases) == 18

# compact set datasets
c.execute(f"SELECT * FROM write_compact_set('SELECT gid FROM polygon_to_cells(''ID'', ''{W}'', 12)', '{d}/cs', domain_code := 'ID', canonicalize := true)").fetchall()
n12 = c.execute(f"SELECT count(*) FROM polygon_to_cells('ID', '{W}', 12)").fetchone()[0]
rows, levels = c.execute(f"SELECT count(*), list(DISTINCT level ORDER BY level) FROM read_compact_set('{d}/cs')").fetchone()
assert rows < n12 and len(levels) > 1, (rows, levels)
assert c.execute(f"SELECT count(*) FROM read_compact_set('{d}/cs', level := 12) JOIN polygon_to_cells('ID', '{W}', 12) USING (packed_id)").fetchone()[0] == n12
assert c.execute(f"SELECT count(*) FROM read_cell_dataset('{d}/cs')").fetchone()[0] == rows
assert all(x[1] for x in c.execute(f"SELECT * FROM validate_cell_dataset('{d}/cs')").fetchall())
fails(f"SELECT * FROM write_compact_set('SELECT unnest([''22'', ''23'', ''24'', ''25'']) AS gid', '{d}/cs2', domain_code := 'ID')", match="not canonical")
fails(f"SELECT * FROM read_compact_set('{d}/cs', level := 8)", match="does not roll up")

# --- majority_weighted ------------------------------------------------------------------
c.execute(f"CREATE TABLE lc AS SELECT packed_id, gid, ['forest', 'urban', 'water', 'crop'][1 + (packed_id % 7) % 4] AS lc, (packed_id % 11)::DOUBLE AS people FROM polygon_to_cells('ID', '{W}', 12)")
c.execute("SELECT * FROM write_cell_dataset('SELECT packed_id, lc, people FROM lc', ?, layout := 'iceberg', partition_level := 7, value_semantics := ?)",
          [f"{d}/lc", json.dumps({"lc": "CATEGORICAL", "people": "COUNT"})]).fetchall()
truth = lambda w: c.execute(f"""SELECT g, (SELECT c FROM (SELECT lc AS c, sum({w}) AS s FROM lc x WHERE left(x.gid, 9) = y.g GROUP BY 1) ORDER BY s DESC, c DESC LIMIT 1)
    FROM (SELECT DISTINCT left(gid, 9) AS g FROM lc) y ORDER BY g""").fetchall()
mw = '{"lc": "majority_weighted"}'
assert c.execute(f"SELECT gid, lc FROM read_cell_dataset('{d}/lc', rollup_level := 9, rollup_rule := '{mw}')").fetchall() == truth("1")
assert c.execute(f"SELECT gid, lc FROM read_cell_dataset('{d}/lc', rollup_level := 9, rollup_rule := '{mw}', space_weight := '{{\"lc\": \"people\"}}')").fetchall() == truth("people")
c.execute(f"SELECT * FROM write_cell_dataset('SELECT packed_id, lc, people FROM lc', '{d}/lc9', value_semantics := ?, space_aggregation := '{mw}', rollup_level := 9)",
          [json.dumps({"lc": "CATEGORICAL", "people": "COUNT"})]).fetchall()
assert c.execute(f"SELECT gid, lc FROM read_cell_dataset('{d}/lc9') ORDER BY gid").fetchall() == truth("1")

# --- geometry allocation ----------------------------------------------------------------
P1 = "POLYGON ((106.800 -6.210, 106.812 -6.210, 106.812 -6.199, 106.800 -6.199, 106.800 -6.210))"
P2 = "POLYGON ((106.806 -6.205, 106.818 -6.205, 106.818 -6.194, 106.806 -6.194, 106.806 -6.205))"
for g in (P1, "LINESTRING (106.801 -6.209, 106.817 -6.195)"):
    assert abs(c.execute("SELECT list_sum(list_transform(geometry_to_cells('ID', ?, 12), lambda x: x.share))", [g]).fetchone()[0] - 1) < 1e-9
assert c.execute("SELECT list_sum(list_transform(geometry_to_cells('ID', 'MULTIPOINT ((106.8 -6.2), (106.8 -6.2), (106.81 -6.2))', 12), lambda x: x.records))").fetchone()[0] == 3
c.execute("CREATE TABLE f (fid VARCHAR, geometry VARCHAR, pop DOUBLE, dens DOUBLE, lc VARCHAR, risk VARCHAR, num DOUBLE, den DOUBLE, lo DOUBLE, hi DOUBLE)")
c.executemany("INSERT INTO f VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", [["a", P1, 120.0, 3.5, "forest", "low", 10, 2, 1, 3], ["b", P2, 80.0, 7.0, "urban", "high", 20, 4, 2, 9]])
agg = c.execute("""SELECT count(*), round(sum(pop), 9), max(feature_count), count(DISTINCT lc), max(lc_share) <= 1, min(rate), max(rate), min(lo), max(hi)
  FROM aggregate_geometries_to_cells('SELECT * FROM f', domain_code := 'ID', level := 12, id_column := 'fid',
    value_semantics := '{"pop": "TOTAL", "dens": "DENSITY", "lc": "CATEGORICAL", "risk": "ORDINAL"}', ordinal_order := '{"risk": ["low", "high"]}',
    rate_parts := '{"rate": ["num", "den"]}', range_columns := '{"r": ["lo", "hi"]}')""").fetchone()
assert agg[1] == 200.0 and agg[2] == 2 and agg[3] == 2 and agg[4] and abs(agg[5] - 5) < 1e-9 and abs(agg[6] - 5) < 1e-9 and agg[7:] == (1.0, 9.0), agg
dens = c.execute("""SELECT round(sum(abs(a.dens - m.dens)), 9) FROM aggregate_geometries_to_cells('SELECT geometry, dens FROM f', domain_code := 'ID', level := 12, value_semantics := 'DENSITY') a
  JOIN (SELECT x.c.gid AS gid, sum(dens * x.c.share) / sum(x.c.share) AS dens FROM (SELECT dens, unnest(geometry_to_cells('ID', geometry, 12)) AS c FROM f) x GROUP BY 1) m USING (gid)""").fetchone()[0]
assert dens == 0, dens
fails("SELECT * FROM aggregate_geometries_to_cells('SELECT geometry, risk FROM f', domain_code := 'ID', level := 12)", match="has no value_semantics")

# --- operational boundary ---------------------------------------------------------------
B = "POLYGON ((106.80 -6.22, 106.84 -6.22, 106.84 -6.18, 106.80 -6.18, 106.80 -6.22), (106.815 -6.205, 106.825 -6.205, 106.825 -6.195, 106.815 -6.195, 106.815 -6.205))"
gj = json.dumps({"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {}, "geometry": {"type": "Polygon", "coordinates": [
    [[106.80, -6.22], [106.84, -6.22], [106.84, -6.18], [106.80, -6.18], [106.80, -6.22]],
    [[106.815, -6.205], [106.825, -6.205], [106.825, -6.195], [106.815, -6.195], [106.815, -6.205]]]}}]})
open(f"{d}/b.geojson", "w").write(gj)
for spec in (B, gj, f"{d}/b.geojson"):
    pts = c.execute("SELECT boundary_covers_point('ID', 106.81, -6.21, ?), boundary_covers_point('ID', 106.82, -6.20, ?), boundary_covers_point('ID', 106.85, -6.20, ?)", [spec] * 3).fetchone()
    assert pts == (True, False, False), pts
# coverage equals polygon_to_cells' coverage_ratio, for cells along both rings
diff, n = c.execute("""SELECT max(abs(boundary_coverage('ID', k.gid, ?) - k.coverage_ratio)), count(*) FROM polygon_to_cells('ID', ?, 11) k""", [f"{d}/b.geojson", B]).fetchone()
assert diff < 1e-9 and n > 100, (diff, n)
m = c.execute("""SELECT count(*) FILTER (WHERE boundary_cell_matches('ID', gid, ?, 'MIN_COVERAGE', 0.5) <> (coverage_ratio >= 0.5)),
   count(*) FILTER (WHERE NOT boundary_cell_matches('ID', gid, ?, 'INTERSECTS')) FROM polygon_to_cells('ID', ?, 11)""", [B, B, B]).fetchone()
assert m == (0, 0), m
sha = c.execute("SELECT file_sha256(?)", [f"{d}/b.geojson"]).fetchone()[0]
assert c.execute(f"SELECT count(*) FROM read_cell_dataset('{d}/lc', boundary := ?, boundary_predicate := 'MIN_COVERAGE', boundary_min_coverage := 0.5, boundary_sha256 := ?)", [f"{d}/b.geojson", sha]).fetchone()[0] == \
    c.execute("SELECT count(*) FROM lc WHERE boundary_coverage('ID', gid, ?) >= 0.5", [B]).fetchone()[0]
fails(f"SELECT * FROM read_cell_dataset('{d}/lc', boundary := ?, boundary_sha256 := 'abc')", [f"{d}/b.geojson"], "SHA-256 mismatch")

# --- Crosswalk 1.0 ----------------------------------------------------------------------
M = {"format_version": "1.0", "spec_version": "2.0", "domain_code": "ID", "level": 2, "weight_kind": "luas",
     "source_description": "Batas desa", "created_at": "2026-11-19T10:00:00Z", "registry_version": "2.0.0"}
c.execute("CREATE TABLE x (sumber_id VARCHAR, gid VARCHAR, bobot DOUBLE)")
expect = [  # rows, manifest change, result or the reference's message
    ([("DESA-001", "H2", 0.5), ("DESA-001", "H3", 0.3), ("DESA-001", "H4", 0.2), ("v1:abc", "H2", 1.0)], {}, (2, 4, 3)),
    ([("A", "H24", 1.0)], {}, "row 1 for sumber_id 'A' has gid 'H24' at level 3; manifest requires level 2"),
    ([("A", "2A", 1.0)], {}, "row 1 for sumber_id 'A' has an invalid gid '2A': character 'A' is invalid at level 2 (base 2)"),
    ([("A", "H2", 0.0)], {}, "row 1 for sumber_id 'A' has bobot <= 0"),
    ([("A", "H2", 0.5), ("A", "H2", 0.5)], {}, "row 2 duplicates (sumber_id, gid) pair ('A', 'H2')"),
    ([("A", "H2", 0.5), ("B", "H2", 1.0), ("A", "H3", 0.4)], {}, "weights for sumber_id 'A' sum to 0.90000000000000002, expected 1 within 1e-9"),
    ([], {}, "crosswalk has no rows"),
    ([("A", "H2", 1.0)], {"weight_kind": "x"}, "manifest weight_kind must be one of bangunan, lainnya, luas, populasi"),
]
for rows, change, want in expect:
    c.execute("DELETE FROM x")
    if rows:
        c.executemany("INSERT INTO x VALUES (?, ?, ?)", [list(r) for r in rows])
    try:
        got = c.execute("SELECT * FROM validate_crosswalk('SELECT * FROM x', manifest := ?)", [json.dumps({**M, **change})]).fetchone()
    except duckdb.Error as e:
        got = str(e).split("Error: ", 1)[-1].split("\n")[0]
    assert got == want, (got, want)
c.execute("CREATE TABLE p (sumber_id VARCHAR, geometry VARCHAR)")
c.executemany("INSERT INTO p VALUES (?, ?)", [["DESA-1", P1], ["DESA-2", P2]])
assert c.execute("SELECT max(abs(s - 1)) < 1e-9 FROM (SELECT sum(bobot) AS s FROM polygon_crosswalk('SELECT * FROM p', domain_code := 'ID', level := 11) GROUP BY sumber_id)").fetchone()[0]
assert c.execute("SELECT sources FROM validate_crosswalk('SELECT * FROM polygon_crosswalk(''SELECT * FROM p'', domain_code := ''ID'', level := 11)', manifest := ?)",
                 [json.dumps({**M, "level": 11})]).fetchone()[0] == 2

# --- time_values on any query -----------------------------------------------------------
c.execute(f"""COPY (SELECT gid, strftime(DATE '2026-01-25' + INTERVAL (i) DAY, '%Y-%m-%d') AS period, (packed_id % 13 + i)::DOUBLE AS rain, (packed_id % 3)::BIGINT AS trips
  FROM polygon_to_cells('ID', '{W}', 11), range(14) t(i)) TO '{d}/t.csv'""")
got = c.execute(f"""SELECT period, count(*), round(sum(rain), 4), sum(trips), sum(period_count) FROM time_values('SELECT * FROM read_csv(''{d}/t.csv'')', period_type := 'day',
  value_semantics := '{{"rain": "MEASUREMENT", "trips": "COUNT"}}', time_aggregation := 'max', time_rollup := 'month', period_from := '2026-01-28') GROUP BY 1 ORDER BY 1""").fetchall()
want = c.execute(f"""SELECT p, count(*), round(sum(r), 4), sum(t), sum(n) FROM (SELECT gid, left(CAST(period AS VARCHAR), 7) AS p, max(rain) AS r, sum(trips) AS t, count(*) AS n
  FROM read_csv('{d}/t.csv') WHERE CAST(period AS VARCHAR) >= '2026-01-28' GROUP BY 1, 2) GROUP BY 1 ORDER BY 1""").fetchall()
assert got == want, (got, want)
fails("SELECT * FROM time_values('SELECT ''H2X545V2'' AS gid, ''2026-02-30'' AS period, 1.0 AS v', period_type := 'day')", match="is not a day label")

# --- manifest 1.1 compact extent --------------------------------------------------------
W2 = "POLYGON ((106.84 -6.22, 106.86 -6.22, 106.86 -6.20, 106.84 -6.20, 106.84 -6.22))"
src = lambda w: f"SELECT packed_id, 1.0::DOUBLE AS v FROM polygon_to_cells(''ID'', ''{w}'', 12)"
c.execute(f"SELECT * FROM write_cell_dataset('{src(W)}', '{d}/ce', layout := 'iceberg', partition_level := 7, value_semantics := 'COUNT', manifest_version := '1.1', compact_extent := true)").fetchall()
c.execute(f"SELECT * FROM write_cell_dataset('{src(W2)} EXCEPT {src(W)}', '{d}/ce', layout := 'iceberg', mode := 'append', manifest_version := '1.1', compact_extent := true)").fetchall()
m = json.loads(c.execute(f"SELECT manifest FROM cell_dataset_info('{d}/ce')").fetchone()[0])
assert m["extent"]["compact_set_path"] == "compact/cells.parquet"
cells = c.execute(f"SELECT count(*) FROM read_cell_dataset('{d}/ce')").fetchone()[0]
assert c.execute(f"SELECT len(compact_expand((SELECT list(gid) FROM read_parquet('{d}/ce/compact/cells.parquet')), 12)), compact_check((SELECT list(gid) FROM read_parquet('{d}/ce/compact/cells.parquet')))").fetchone() == (cells, None)
print("spec parity OK", d)
