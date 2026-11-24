"""radius_m (a circle around a lon/lat point, true distance) and WKB geometry output
(with_geometry := 'wkb', cell_to_wkb).

    python tests/test_radius_wkb.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import math, re, struct, sys, tempfile
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_radius_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
p = f"{d}/t"
c.execute("""SELECT * FROM write_cell_dataset('SELECT packed_id, 1.0 AS one FROM polygon_to_cells(''ID'',
  ''POLYGON ((106.78 -6.24, 106.88 -6.24, 106.88 -6.14, 106.78 -6.14, 106.78 -6.24))'', 12)', ?, layout := 'iceberg',
  aggregation_rule := 'sum', value_semantics := 'COUNT')""", [p]).fetchall()
lon, lat = 106.8272, -6.1754
side = c.execute("SELECT cell_side_m(12)").fetchone()[0]
# true area of a cell near the point (the grid cell is 50 m in the grid plane, ~2525 m2 here)
wkt = c.execute("SELECT cell_to_geometry('ID', point_to_cell('ID', ?, ?, 12).gid, 'EPSG:4326')", [lon, lat]).fetchone()[0]
pts = [tuple(map(float, xy.split())) for xy in re.findall(r"(-?[\d.]+ -?[\d.]+)", wkt)][:-1]
R, lat0 = 6371008.8, math.radians(lat)
xy = [(math.radians(x) * R * math.cos(lat0), math.radians(y) * R) for x, y in pts]
cell_area = abs(sum(xy[i][0] * xy[i - 1][1] - xy[i - 1][0] * xy[i][1] for i in range(len(xy)))) / 2
for r in (300.0, 1000.0, 2500.0):
    cells, cov = c.execute("SELECT count(*), sum(coverage_ratio) FROM query_cell_dataset(?, geometry := ?, radius_m := ?)",
                           [p, f"POINT ({lon} {lat})", r]).fetchone()
    area = cov * cell_area
    polygon64 = 64 / 2 * math.sin(2 * math.pi / 64) * r * r      # area of the 64-gon
    assert abs(area / polygon64 - 1) < 0.002, (r, area, polygon64)
    # cells whose centre is well inside are fully covered, cells far outside are absent
    far = c.execute("""SELECT count(*) FROM query_cell_dataset(?, geometry := ?, radius_m := ?)
                       WHERE distance_m(gid, point_to_cell('ID', ?, ?, 12).gid) > ? + 2 * ?""",
                    [p, f"POINT ({lon} {lat})", r, lon, lat, r, side]).fetchone()[0]
    inner = c.execute("""SELECT min(coverage_ratio) FROM query_cell_dataset(?, geometry := ?, radius_m := ?)
                         WHERE distance_m(gid, point_to_cell('ID', ?, ?, 12).gid) < ? - 2 * ?""",
                      [p, f"POINT ({lon} {lat})", r, lon, lat, r, side]).fetchone()[0]
    assert far == 0 and inner == 1.0, (r, far, inner)
# with rollup and several datasets
s = c.execute("SELECT sum(one) FROM query_cell_dataset(?, geometry := ?, radius_m := 1000, rollup_level := 9)", [p, f"POINT ({lon} {lat})"]).fetchone()[0]
t = c.execute("SELECT sum(one * coverage_ratio) FROM query_cell_dataset(?, geometry := ?, radius_m := 1000)", [p, f"POINT ({lon} {lat})"]).fetchone()[0]
assert math.isclose(s, t, rel_tol=1e-9), (s, t)
for bad, msg in (("geometry := 'POINT (106.8 -6.2)', radius_m := -1", "radius_m must be"),
                 ("geometry := 'LINESTRING (106.8 -6.2, 106.81 -6.2)', radius_m := 100", "exactly one POINT"),
                 ("bbox := [106.8, -6.2, 106.81, -6.19], radius_m := 100", "radius_m needs geometry")):
    try:
        c.execute(f"SELECT * FROM query_cell_dataset(?, {bad})", [p]).fetchall(); raise AssertionError(bad)
    except duckdb.Error as e:
        assert msg in str(e), (bad, e)

# WKB: same coordinates as the WKT outline
def wkb_ring(b):
    order, typ, nrings, npts = struct.unpack_from("<BIII", b, 0)
    assert (order, typ, nrings) == (1, 3, 1)
    return [struct.unpack_from("<dd", b, 13 + 16 * i) for i in range(npts)]
rows = c.execute("""SELECT packed_id, cell_to_wkb(packed_id), cell_to_geometry(packed_id, 'EPSG:4326'),
                    cell_to_wkb('ID', gid, 'grid'), cell_to_geometry('ID', gid, 'grid') FROM read_cell_dataset(?) LIMIT 50""", [p]).fetchall()
for pid, wkb, wkt, wkb_g, wkt_g in rows:
    for b, t in ((wkb, wkt), (wkb_g, wkt_g)):
        pts = [tuple(map(float, xy.split())) for xy in re.findall(r"(-?[\d.eE+-]+ -?[\d.eE+-]+)", t)]
        ring = wkb_ring(bytes(b))
        assert ring[0] == ring[-1] and len(ring) == len(pts) and all(math.isclose(a, x, abs_tol=1e-12) and math.isclose(b2, y, abs_tol=1e-12) for (a, b2), (x, y) in zip(ring, pts)), (pid, ring[:2], pts[:2])
g = c.execute("SELECT typeof(cell_geometry), octet_length(cell_geometry) FROM query_cell_dataset(?, bbox := [106.82, -6.18, 106.83, -6.17], with_geometry := 'wkb') LIMIT 1", [p]).fetchone()
assert g[0] == "BLOB" and g[1] > 13, g
try:
    c.execute("INSTALL spatial; LOAD spatial")
    a = c.execute("SELECT ST_Area(ST_GeomFromWKB(cell_geometry)) > 0, ST_GeometryType(ST_GeomFromWKB(cell_geometry))::VARCHAR FROM query_cell_dataset(?, bbox := [106.82, -6.18, 106.83, -6.17], with_geometry := 'wkb') LIMIT 1", [p]).fetchone()
    assert a == (True, "POLYGON"), a
    spatial = "spatial ST_GeomFromWKB OK"
except duckdb.IOException:
    spatial = "spatial not installable here"
print("radius + wkb OK;", spatial)
