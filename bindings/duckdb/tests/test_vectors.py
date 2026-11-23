"""Spec 2.0 vector tests for the geosquare_v2 DuckDB extension.

Run with any DuckDB Python package from 1.4 to 2.0:

    python tests/test_vectors.py build/linux_amd64/geosquare_v2.duckdb_extension

Only the `duckdb` package is needed. Exits non-zero on the first failure.
"""

import json
import sys
from pathlib import Path

import duckdb

ROOT = Path(__file__).resolve().parents[3]
VECTORS = ROOT / "vectors" / "spec-2.0"
EXT = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux_amd64/geosquare_v2.duckdb_extension").resolve()


def load(name):
    return json.loads((VECTORS / f"{name}.json").read_text())


def connect():
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{EXT}'")
    return con


def rows(con, sql, params=None):
    return con.execute(sql, params or []).fetchall()


def raises(con, sql, params=None):
    try:
        con.execute(sql, params or []).fetchall()
    except duckdb.Error:
        return True
    return False


def check(cond, msg):
    if not cond:
        raise AssertionError(msg)


def test_codec(con):
    codec = load("codec")
    cells = codec["cells"]
    for c in cells:
        gid, lvl, x, y, pk = c["gid"], c["level"], c["x_idx"], c["y_idx"], int(c["packed_id"])
        got = rows(con, "SELECT canonical_to_gid(?, ?, ?), gid_to_canonical('ID', ?), pack('ID', ?), pack(1, ?), "
                        "unpack_int64(?), pack_int64(1, ?, ?, ?)", [lvl, x, y, gid, gid, gid, pk, lvl, x, y])[0]
        check(got[0] == gid, f"canonical_to_gid {c}: {got[0]}")
        check(got[1] == {"domain_code": "ID", "level": lvl, "x_idx": x, "y_idx": y}, f"gid_to_canonical {c}: {got[1]}")
        check(got[2] == pk and got[3] == pk and got[5] == pk, f"pack {c}: {got[2]} {got[3]} {got[5]}")
        check(got[4] == {"domain_id": 1, "level": lvl, "x_idx": x, "y_idx": y}, f"unpack_int64 {c}: {got[4]}")
    for c in codec["packed_ids_by_domain"]["cells"]:
        for code, pk in c["packed_ids"].items():
            got = rows(con, "SELECT pack(?, ?), unpack_int64(?)", [code, c["gid"], int(pk)])[0]
            did = rows(con, "SELECT unpack_int64(pack(?, '')).domain_id", [code])[0][0]
            check(got[0] == int(pk) and got[1]["domain_id"] == did, f"domain {code} {c['gid']}: {got}")
    for bad in codec["invalid_gids"]:
        check(raises(con, "SELECT gid_to_canonical('ID', ?)", [bad["input"]]), f"invalid gid accepted: {bad}")
    for bad in codec["invalid_packed_ids"]:
        v = int(bad["packed_id"])
        if v < 2**63:
            check(raises(con, "SELECT unpack_int64(?::BIGINT)", [v]), f"invalid packed accepted: {bad}")
    # vectorised: many rows, constant and NULL arguments
    n = rows(con, "SELECT count(*) FROM (SELECT canonical_to_gid(12, i, i) g FROM range(5000) t(i)) WHERE g IS NULL")[0][0]
    check(n == 0, "constant level over many rows")
    check(rows(con, "SELECT canonical_to_gid(NULL::INT, 1, 1), parent(NULL), pack('ID', NULL)")[0] == (None, None, None), "NULL in -> NULL out")
    return len(cells)


def test_points(con):
    pts = load("points")["cases"]
    n = 0
    for c in pts:
        for lvl, gid in c["gids"].items():
            args = [c["domain_code"], c["lon"], c["lat"], int(lvl)]
            if c["production_ready"]:
                got = rows(con, "SELECT point_to_cell(?, ?, ?, ?)", args)[0][0]
            else:
                check(raises(con, "SELECT point_to_cell(?, ?, ?, ?)", args), f"candidate CRS accepted: {c['domain_code']}")
                got = rows(con, "SELECT point_to_cell(?, ?, ?, ?, true)", args)[0][0]
            check(got["gid"] == gid, f"point_to_cell {c['domain_code']} {lvl}: {got['gid']} != {gid}")
            check(got["uri"] == f"geosquare:v2:{c['domain_code']}:{gid}", f"uri {got}")
            check(rows(con, "SELECT pack(?, ?)", [c["domain_code"], gid])[0][0] == got["packed_id"], "packed_id")
            n += 1
    return n


def test_topology(con):
    t = load("topology")
    for c in t["parent_of"]:
        got = rows(con, "SELECT parent(?)", [c["gid"]])[0][0]
        check(got == c["parent_gid"], f"parent {c}: {got}")
    for c in t["children_of"]:
        got = rows(con, "SELECT children(?)", [c["gid"]])[0][0]
        check(got == c["child_gids"], f"children {c['gid']}: {got}")
    for c in t["descendant_ranges"]:
        got = rows(con, "SELECT descendant_range(1, ?, ?)", [c["parent_gid"], c["child_level"]])[0][0]
        check(got == {"lo": int(c["lo"]), "hi": int(c["hi"])}, f"descendant_range {c}: {got}")
    for c in t["neighbours"]:
        for key, fn in (("k_disk_1", "k_disk(?, 1)"), ("k_ring_1", "k_ring(?, 1)"), ("k_ring_2", "k_ring(?, 2)")):
            if key in c:
                got = rows(con, f"SELECT {fn}", [c["gid"]])[0][0]
                check(sorted(got) == sorted(c[key]), f"{key} {c['gid']}: {got}")
    for c in t["distances"]:
        got = rows(con, "SELECT distance_m(?, ?), cell_side_m(?), cell_distance(?, ?)",
                   [c["a_gid"], c["b_gid"], c["level"], "geosquare:v2:ID:" + c["a_gid"], "geosquare:v2:ID:" + c["b_gid"]])[0]
        check(abs(got[0] - c["distance_m"]) < 1e-6 and got[1] == c["cell_side_m"] and abs(got[2] - c["distance_m"]) < 1e-6, f"distance {c}: {got}")
    return len(t["parent_of"]) + len(t["children_of"]) + len(t["descendant_ranges"])


def test_polygon(con):
    # A 3x2 block of level-12 cells in grid CRS, shifted by half a cell in x.
    side = rows(con, "SELECT cell_side_m(12)")[0][0]
    cell = rows(con, "SELECT gid_to_canonical('ID', 'H2X545P49342')")[0][0]
    ox, oy = -25_000_000 + cell["x_idx"] * side, -25_000_000 + cell["y_idx"] * side
    x0, x1, y0, y1 = ox + side / 2, ox + 3.5 * side, oy, oy + 2 * side
    wkt = f"POLYGON (({x0} {y0}, {x1} {y0}, {x1} {y1}, {x0} {y1}, {x0} {y0}))"
    got = rows(con, "SELECT x_idx - ?, y_idx - ?, coverage_ratio FROM polygon_to_cells('ID', ?, 12, source_crs := 'grid')",
               [cell["x_idx"], cell["y_idx"], wkt])
    want = [(0, 0, 0.5), (1, 0, 1.0), (2, 0, 1.0), (3, 0, 0.5), (0, 1, 0.5), (1, 1, 1.0), (2, 1, 1.0), (3, 1, 0.5)]
    check([(a, b, round(c, 9)) for a, b, c in got] == want, f"polygon_to_cells grid square: {got}")
    half = rows(con, "SELECT count(*) FROM polyfill('ID', ?, 12, source_crs := 'grid', min_coverage := 0.6)", [wkt])[0][0]
    check(half == 4, f"min_coverage filter: {half}")
    check(raises(con, "SELECT * FROM polygon_to_cells('ID', ?, 12, source_crs := 'grid', max_candidate_limit := 3)", [wkt]), "candidate limit")
    candidate_wkt = "POLYGON ((107.86 15.97, 107.87 15.97, 107.87 15.98, 107.86 15.98, 107.86 15.97))"
    check(
        raises(con, "SELECT * FROM polygon_to_cells('VN', ?, 12)", [candidate_wkt]),
        "candidate polygon CRS accepted",
    )
    candidate_cells = rows(
        con,
        "SELECT * FROM polygon_to_cells('VN', ?, 12, allow_candidate_crs := true)",
        [candidate_wkt],
    )
    check(candidate_cells, "candidate polygon opt-in returned no cells")
    check(raises(con, "SELECT * FROM polygon_to_cells('ID', 'POINT (1 2)', 12)"), "non-polygon rejected")
    geom = rows(con, "SELECT cell_to_geometry('ID', 'H2X545P49342'), cell_to_geometry('ID', 'H2X545P49342', 'EPSG:4326', 'geojson')")[0]
    check(geom[0].startswith("POLYGON ((") and '"Polygon"' in geom[1], f"cell_to_geometry: {geom}")
    return len(got)


def main():
    con = connect()
    version = rows(con, "SELECT version()")[0][0]
    results = {name: fn(con) for name, fn in [("codec", test_codec), ("points", test_points), ("topology", test_topology), ("polygon", test_polygon)]}
    print(f"duckdb {version}: OK {results} | {rows(con, 'SELECT geosquare_engine_version()')[0][0]}")


if __name__ == "__main__":
    main()
