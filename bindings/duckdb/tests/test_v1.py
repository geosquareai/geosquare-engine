"""GeoSquare v1 grid helpers against reference values of @geosquare/grid (gidToBBox /
lonLatToGid of the v1 apps): tests/data/v1_grid_ref.json = [lon, lat, level, gid, bbox].

    python tests/test_v1.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import json, os, sys
import duckdb

ext = sys.argv[1]
ref = json.load(open(os.path.join(os.path.dirname(__file__), "data", "v1_grid_ref.json")))
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
for lon, lat, level, gid, b in ref:
    assert c.execute("SELECT lonlat_to_v1_gid(?, ?, ?)", [lon, lat, level]).fetchone()[0] == gid, gid
    ctr = c.execute("SELECT v1_gid_center(?)", [gid]).fetchone()[0]
    assert (ctr["lon"], ctr["lat"]) == ((b[0] + b[2]) / 2, (b[1] + b[3]) / 2), (gid, ctr, b)
    pts = json.loads(c.execute("SELECT v1_gid_to_geometry(?, 'geojson')", [gid]).fetchone()[0])["coordinates"][0]
    assert [min(p[0] for p in pts), min(p[1] for p in pts), max(p[0] for p in pts), max(p[1] for p in pts)] == b, gid
    cell = c.execute("SELECT v1_gid_to_cell('ID', ?, 12)", [gid]).fetchone()[0]
    want = c.execute("SELECT point_to_cell('ID', ?, ?, 12)", [(b[0] + b[2]) / 2, (b[1] + b[3]) / 2]).fetchone()[0]
    assert cell == want, (gid, cell, want)

# v1 data keyed by v1 gids onto the v2 grid, e.g. v1 50 m values summed per v2 50 m cell
c.execute("""CREATE TABLE v1 AS SELECT lonlat_to_v1_gid(106.80 + i * 0.0004, -6.20 + j * 0.0004, 12) AS gid, 1.0 AS people
             FROM range(20) a(i), range(20) b(j)""")
rows, v2cells, people = c.execute("""SELECT count(*), count(DISTINCT packed_id), sum(people) FROM (
    SELECT v1_gid_to_cell('ID', gid, 12).packed_id AS packed_id, people FROM v1)""").fetchone()
assert rows == 400 and 300 < v2cells <= 400 and people == 400, (rows, v2cells, people)
assert c.execute("SELECT v1_gid_center(NULL), lonlat_to_v1_gid(NULL, 1, 12)").fetchone() == (None, None)
for bad, msg in (("v1_gid_to_cell('ID', 'H2X5', 12)", "invalid v1 gid"), ("lonlat_to_v1_gid(106.8, -6.2, 16)", "v1 level"),
                 ("v1_gid_to_geometry('H2', 'svg')", "format must be")):
    try:
        c.execute(f"SELECT {bad}").fetchall()
        raise AssertionError(bad)
    except duckdb.Error as e:
        assert msg in str(e), (bad, e)
print(f"v1 grid OK: {len(ref)} reference cells")
