"""Compare polygon_to_cells with the geosquare-v2 Python oracle (needs geosquare_v2 + shapely).

    python tests/test_polyfill_oracle.py <extension> <path to geosquare-v2 checkout>
"""

import base64
import json
import sys
from pathlib import Path

import duckdb
from shapely.geometry import LineString, MultiLineString, MultiPolygon, Point, Polygon

from geosquare_v2 import DbRegistryLoader, GeosquareService

EXT = Path(sys.argv[1]).resolve()
V2 = Path(sys.argv[2]).resolve()


def service():
    trust = {k: base64.b64decode(v, validate=True) for k, v in json.loads((V2 / "release/registry-trust.json").read_text()).items()}
    boundary = V2 / "src/geosquare_v2/data/registry"
    registry = DbRegistryLoader(trust, V2 / "src/geosquare_v2/db", boundary_root=boundary, verify_boundaries=True).load()
    return GeosquareService(registry, boundary_root=boundary, allow_candidate_crs=True)


def cases():
    ring = Point(106.85, -6.2).buffer(0.02, 48)
    hole = Point(106.85, -6.2).buffer(0.008, 24)
    yield "circle 2 km Jakarta", "ID", Point(106.97, -6.43).buffer(0.018, 64), 12
    yield "circle 1 km Semarang", "ID", Point(110.4, -7.0).buffer(0.01, 32), 12
    yield "circle 3 km Makassar", "ID", Point(119.4, -5.1).buffer(0.03, 64), 11
    yield "circle 600 m Medan lvl 14", "ID", Point(98.67, 3.59).buffer(0.003, 32), 14
    yield "polygon with hole", "ID", Polygon(ring.exterior.coords, [hole.exterior.coords]), 12
    yield "multipolygon", "ID", MultiPolygon([Point(106.8, -6.3).buffer(0.01, 16), Point(106.9, -6.3).buffer(0.01, 16)]), 12
    yield "concave L", "ID", Polygon([(106.8, -6.3), (106.85, -6.3), (106.85, -6.29), (106.81, -6.29), (106.81, -6.25), (106.8, -6.25)]), 12
    yield "circle Kuala Lumpur (MY)", "MY", Point(101.69, 3.14).buffer(0.01, 32), 12


def main():
    svc = service()
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{EXT}'")
    worst = 0.0
    total = 0
    for name, dom, geom, level in cases():
        for mc in (0.0, 0.5, 0.99):
            want = dict(svc.polyfill(dom, geom, "EPSG:4326", level, min_coverage=mc))
            got = dict(con.execute("SELECT gid, coverage_ratio FROM polyfill(?, ?, ?, min_coverage := ?)", [dom, geom.wkt, level, mc]).fetchall())
            extra, missing = set(got) - set(want), set(want) - set(got)
            # A cell can sit on the threshold within float noise; allow only those.
            border = [g for g in extra | missing if abs((got.get(g) or want.get(g)) - mc) > 1e-9]
            if border:
                raise AssertionError(f"{name} min_coverage={mc}: {len(extra)} extra, {len(missing)} missing, e.g. {border[:3]}")
            diff = max((abs(got[g] - want[g]) for g in set(got) & set(want)), default=0.0)
            worst = max(worst, diff)
            total += len(want)
            order = [g for g, _ in con.execute("SELECT gid, coverage_ratio FROM polyfill(?, ?, ?, min_coverage := ?)", [dom, geom.wkt, level, mc]).fetchall()]
            if order != [g for g, _ in svc.polyfill(dom, geom, "EPSG:4326", level, min_coverage=mc) if g in got]:
                raise AssertionError(f"{name}: row order differs from the oracle")
    print(f"polyfill oracle: OK, {total} cells over {len(list(cases())) * 3} cases, max |coverage diff| = {worst:.2e}")
    test_lines(svc, con)


def invalid_cases():
    from shapely.geometry import Polygon as P
    x, y, d = 106.85, -6.20, 0.01
    yield "bowtie", "ID", P([(x, y), (x + d, y + d), (x + d, y), (x, y + d)]), 12
    yield "self-overlapping loop", "ID", P([(x, y), (x + 4 * d, y), (x + 4 * d, y + 4 * d), (x + d, y + 4 * d), (x + d, y + d), (x + 3 * d, y + d), (x + 3 * d, y + 3 * d), (x, y + 3 * d)]), 11
    yield "hole outside shell", "ID", P([(x, y), (x + 2 * d, y), (x + 2 * d, y + 2 * d), (x, y + 2 * d)], [[(x + 3 * d, y), (x + 4 * d, y), (x + 4 * d, y + d), (x + 3 * d, y + d)]]), 11
    yield "overlapping parts", "ID", MultiPolygon([P([(x, y), (x + 2 * d, y), (x + 2 * d, y + 2 * d), (x, y + 2 * d)]), P([(x + d, y + d), (x + 3 * d, y + d), (x + 3 * d, y + 3 * d), (x + d, y + 3 * d)])]), 11


def compare_cases(svc, con, label, items, mode, thresholds, tol):
    worst, total = 0.0, 0
    for name, dom, geom, level in items:
        for mc in thresholds:
            want = dict(svc.polyfill(dom, geom, "EPSG:4326", level, min_coverage=mc, coverage_mode=mode))
            got = dict(con.execute("SELECT gid, coverage_ratio FROM polyfill(?, ?, ?, min_coverage := ?, coverage_mode := ?)",
                                   [dom, geom.wkt, level, mc, mode]).fetchall())
            odd = [g for g in set(got) ^ set(want) if abs((got.get(g) or want.get(g)) - mc) > tol]
            if odd:
                raise AssertionError(f"{label} / {name} min_coverage={mc}: {len(set(got) - set(want))} extra, {len(set(want) - set(got))} missing, e.g. {odd[:3]}")
            worst = max([worst] + [abs(got[g] - want[g]) for g in set(got) & set(want)])
            total += len(want)
    if worst > tol:
        raise AssertionError(f"{label}: coverage differs by {worst}")
    print(f"{label} oracle: OK, {total} cells, max |coverage diff| = {worst:.2e}")


def line_cases(con):
    yield "diagonal 3 km", "ID", LineString([(106.80, -6.30), (106.82, -6.28)]), "EPSG:4326", 12
    yield "polyline with turns", "ID", LineString([(106.80, -6.30), (106.81, -6.29), (106.83, -6.295), (106.84, -6.27)]), "EPSG:4326", 12
    yield "multiline", "ID", MultiLineString([[(106.80, -6.30), (106.805, -6.29)], [(106.90, -6.20), (106.91, -6.21)]]), "EPSG:4326", 12
    yield "road 15 km level 11", "ID", LineString([(106.70, -6.40), (106.75, -6.35), (106.80, -6.33), (106.84, -6.30)]), "EPSG:4326", 11
    yield "short line level 14", "ID", LineString([(98.670, 3.590), (98.6705, 3.5903), (98.6702, 3.5908)]), "EPSG:4326", 14
    yield "Kuala Lumpur (MY)", "MY", LineString([(101.68, 3.13), (101.70, 3.15)]), "EPSG:4326", 12
    # Lines exactly on cell edges, in grid CRS: horizontal, vertical, and an L along both.
    cell = con.execute("SELECT gid_to_canonical('ID', 'H2X545P49342')").fetchone()[0]
    side = con.execute("SELECT cell_side_m(12)").fetchone()[0]
    x0, y0 = -25_000_000 + cell["x_idx"] * side, -25_000_000 + cell["y_idx"] * side
    yield "on horizontal edge (grid)", "ID", LineString([(x0 + 10, y0), (x0 + 3 * side + 7, y0)]), "grid", 12
    yield "on vertical edge (grid)", "ID", LineString([(x0, y0 - 20), (x0, y0 + 2 * side)]), "grid", 12
    yield "L along edges (grid)", "ID", LineString([(x0, y0), (x0 + 2 * side, y0), (x0 + 2 * side, y0 + side)]), "grid", 12


def test_lines(svc, con):
    worst, total, n = 0.0, 0, 0
    edge_note = []
    for name, dom, geom, crs, level in line_cases(con):
        v2_crs = svc.registry.get(dom).crs_wkt2 if crs == "grid" else crs  # v2 needs a real CRS definition
        want = [(r.gid, r.length_ratio) for r in svc.line_to_cells(dom, geom, v2_crs, level)]
        got = con.execute("SELECT gid, length_ratio FROM line_to_cells(?, ?, ?, source_crs := ?)", [dom, geom.wkt, level, crs]).fetchall()
        if crs == "grid":
            # Lines exactly on cell edges. The engine applies the spec 2.0 half-open rule
            # (an edge belongs to the cell on its high side), so ratios sum to 1. The Shapely
            # oracle counts some edge segments in both neighbours (its ratios can sum to more
            # than 1), so here only the cells and the sum are checked.
            assert {g for g, _ in got} <= {g for g, _ in want}, f"line {name}: cells outside the oracle set"
            assert abs(sum(r for _, r in got) - 1) < 1e-9, f"line {name}: ratios sum to {sum(r for _, r in got)}"
            edge_note.append(f"{name}: oracle sum {sum(r for _, r in want):.3f}")
            n += 1
            continue
        if [g for g, _ in got] != [g for g, _ in want]:
            raise AssertionError(f"line {name}: cells differ\n got  {[g for g, _ in got][:8]}\n want {[g for g, _ in want][:8]} ({len(got)} vs {len(want)})")
        diff = max(abs(a[1] - b[1]) for a, b in zip(got, want))
        worst = max(worst, diff)
        total += len(want)
        n += 1
    if worst > 1e-9:
        raise AssertionError(f"line length_ratio differs by {worst}")
    print(f"line oracle: OK, {total} cells identical over {n - len(edge_note)} lines, max |length_ratio diff| = {worst:.2e}; "
          f"edge-exact lines follow the half-open rule ({'; '.join(edge_note)})")


if __name__ == "__main__":
    main()
