"""query_cell_dataset on Iceberg reads files chosen from the (split, cached) manifests.
Checks it against iceberg_scan for random polygons, across several manifests, after an
append, and for an area with no data.

    python tests/test_iceberg_planned.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import glob, random, sys, tempfile
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_planned_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'; LOAD iceberg")
def area(x0, y0, x1, y1):
    return (f"SELECT packed_id, (packed_id % 97)::DOUBLE AS v FROM polygon_to_cells('ID', "
            f"'POLYGON (({x0} {y0}, {x1} {y0}, {x1} {y1}, {x0} {y1}, {x0} {y0}))', 12, max_candidate_limit := 2000000)")
p = f"{d}/t"
c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', partition_level := 9)", [area(106.70, -6.40, 107.00, -6.10), p])
c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'append')", [area(107.01, -6.40, 107.10, -6.30), p])
n_manifests = len(glob.glob(f"{p}/metadata/*-m*.avro"))
assert n_manifests > 2, n_manifests
meta = sorted(glob.glob(f"{p}/metadata/*.metadata.json"))[-1]
rng = random.Random(7)
for i in range(40):
    x, y = rng.uniform(106.65, 107.15), rng.uniform(-6.45, -6.05)
    r = rng.uniform(0.002, 0.03)
    pts = [(x + r * rng.uniform(0.5, 1) * __import__("math").cos(a), y + r * rng.uniform(0.5, 1) * __import__("math").sin(a))
           for a in sorted(rng.uniform(0, 6.283) for _ in range(rng.randint(4, 10)))]
    wkt = "POLYGON ((" + ", ".join(f"{a} {b}" for a, b in pts + pts[:1]) + "))"
    got = c.execute("SELECT count(*), sum(v), sum(v * coverage_ratio) FROM query_cell_dataset(?, geometry := ?)", [p, wkt]).fetchone()
    want = c.execute(f"SELECT count(*), sum(d.v), sum(d.v * k.coverage_ratio) FROM iceberg_scan('{meta}') d "
                     f"JOIN polygon_to_cells('ID', ?, 12) k USING (packed_id)", [wkt]).fetchone()
    assert got[0] == want[0] and (got[1] or 0) == (want[1] or 0) and abs((got[2] or 0) - (want[2] or 0)) < 1e-6, (i, got, want)
# projection pushdown: subsets of columns, reordered, count(*) only, filter-only columns
wkt = "POLYGON ((106.80 -6.25, 106.83 -6.25, 106.83 -6.22, 106.80 -6.22, 106.80 -6.25))"
full = c.execute("SELECT gid, v, coverage_ratio FROM (SELECT * FROM query_cell_dataset(?, geometry := ?)) ORDER BY gid", [p, wkt]).fetchall()
assert c.execute("SELECT gid, v, coverage_ratio FROM query_cell_dataset(?, geometry := ?) ORDER BY gid", [p, wkt]).fetchall() == full
assert c.execute("SELECT coverage_ratio, v, gid FROM query_cell_dataset(?, geometry := ?) ORDER BY gid", [p, wkt]).fetchall() == [(r[2], r[1], r[0]) for r in full]
assert c.execute("SELECT count(*) FROM query_cell_dataset(?, geometry := ?)", [p, wkt]).fetchone()[0] == len(full)
assert c.execute("SELECT count(*) FROM query_cell_dataset(?, geometry := ?) WHERE v > 50", [p, wkt]).fetchone()[0] == sum(1 for r in full if r[1] > 50)
assert c.execute("SELECT sum(v) FROM read_cell_dataset(?)", [p]).fetchone()[0] == c.execute("SELECT sum(v) FROM (SELECT * FROM read_cell_dataset(?))", [p]).fetchone()[0]
# with_geometry: a cell_geometry column, same as cell_to_geometry on the gid
g = c.execute("SELECT count(*), count(*) FILTER (WHERE cell_geometry = cell_to_geometry('ID', gid, 'EPSG:4326', 'geojson')), "
              "max(column_index) FROM (SELECT *, 0 AS column_index FROM query_cell_dataset(?, geometry := ?, with_geometry := 'geojson'))", [p, wkt]).fetchone()
assert g[0] == g[1] == len(full), g
assert c.execute("SELECT count(*) FROM query_cell_dataset(?, geometry := ?, with_geometry := 'wkt', geometry_crs := 'grid') "
                 "WHERE cell_geometry = cell_to_geometry('ID', gid)", [p, wkt]).fetchone()[0] == len(full)
cols_g = [r[0] for r in c.execute("DESCRIBE SELECT * FROM query_cell_dataset(?, geometry := ?, with_geometry := 'wkt')", [p, wkt]).fetchall()]
assert cols_g[-1] == "cell_geometry" and "coverage_ratio" in cols_g, cols_g
assert c.execute("SELECT count(*) FROM read_cell_dataset(?, with_geometry := 'wkt') WHERE cell_geometry LIKE 'POLYGON%'", [p]).fetchone()[0] \
    == c.execute("SELECT count(*) FROM read_cell_dataset(?)", [p]).fetchone()[0]
# no data in the area: empty result with the table's columns
empty = c.execute("SELECT count(*) FROM query_cell_dataset(?, geometry := 'POLYGON ((110 -7.5, 110.01 -7.5, 110.01 -7.49, 110 -7.49, 110 -7.5))')", [p]).fetchone()
cols = [r[0] for r in c.execute("DESCRIBE SELECT * FROM query_cell_dataset(?, geometry := 'POLYGON ((110 -7.5, 110.01 -7.5, 110.01 -7.49, 110 -7.49, 110 -7.5))')", [p]).fetchall()]
assert empty == (0,) and "v" in cols and "coverage_ratio" in cols, (empty, cols)
print(f"iceberg planned reads OK ({n_manifests} manifests, 40 polygons)", d)
# overlap columns keep their declared types (point_count BIGINT, ratios DOUBLE) on the planned path
mp = "MULTIPOINT ((106.80 -6.20), (106.80 -6.20), (106.85 -6.25), (107.05 -6.35))"
for _ in range(20):   # a wrong width would show as garbage that changes between runs
    r = c.execute("SELECT count(*), sum(point_count), typeof(any_value(point_count)) FROM query_cell_dataset(?, geometry := ?)", [p, mp]).fetchone()
    assert r == (3, 4, "BIGINT"), r
print("planned overlap types OK")
