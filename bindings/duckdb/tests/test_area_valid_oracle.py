"""EQUAL_AREA coverage and make_valid repair against the geosquare-v2 oracle.

The oracle builds a pyproj transformer per cell in EQUAL_AREA mode, so polygons are small.

    python tests/test_area_valid_oracle.py <extension> <geosquare-v2 checkout>
"""

import sys
import time

import duckdb
from shapely.geometry import MultiPolygon, Point, Polygon

sys.path.insert(0, __import__("os").path.dirname(__file__))
from test_polyfill_oracle import EXT, service  # noqa: E402


def cases():
    x, y, d = 106.85, -6.20, 0.004
    yield "circle 400 m", "ID", Point(110.4, -7.0).buffer(0.004, 32), 12
    yield "polygon with hole", "ID", Polygon(Point(x, y).buffer(0.005, 24).exterior.coords, [Point(x, y).buffer(0.002, 12).exterior.coords]), 12
    yield "Kuala Lumpur (MY)", "MY", Point(101.69, 3.14).buffer(0.004, 24), 12
    yield "bowtie (invalid)", "ID", Polygon([(x, y), (x + d, y + d), (x + d, y), (x, y + d)]), 12
    yield "self-overlapping loop (invalid)", "ID", Polygon([(x, y), (x + 4 * d, y), (x + 4 * d, y + 4 * d), (x + d, y + 4 * d), (x + d, y + d), (x + 3 * d, y + d), (x + 3 * d, y + 3 * d), (x, y + 3 * d)]), 11
    yield "hole outside shell (invalid)", "ID", Polygon([(x, y), (x + 2 * d, y), (x + 2 * d, y + 2 * d), (x, y + 2 * d)], [[(x + 3 * d, y), (x + 4 * d, y), (x + 4 * d, y + d), (x + 3 * d, y + d)]]), 12
    yield "overlapping parts (invalid)", "ID", MultiPolygon([Polygon([(x, y), (x + 2 * d, y), (x + 2 * d, y + 2 * d), (x, y + 2 * d)]), Polygon([(x + d, y + d), (x + 3 * d, y + d), (x + 3 * d, y + 3 * d), (x + d, y + 3 * d)])]), 12


def main():
    svc = service()
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{EXT}'")
    for mode in ("GRID_PLANAR", "EQUAL_AREA"):
        worst, total = 0.0, 0
        for name, dom, geom, level in cases():
            if mode == "GRID_PLANAR" and "invalid" not in name:
                continue
            t = time.time()
            for mc in (0.0, 0.5):
                want = dict(svc.polyfill(dom, geom, "EPSG:4326", level, min_coverage=mc, coverage_mode=mode))
                got = dict(con.execute("SELECT gid, coverage_ratio FROM polyfill(?, ?, ?, min_coverage := ?, coverage_mode := ?)",
                                       [dom, geom.wkt, level, mc, mode]).fetchall())
                odd = [g for g in set(got) ^ set(want) if abs((got.get(g) or want.get(g)) - mc) > 1e-6]
                assert not odd, f"{mode} {name} min_coverage={mc}: {len(set(got) - set(want))} extra, {len(set(want) - set(got))} missing"
                worst = max([worst] + [abs(got[g] - want[g]) for g in set(got) & set(want)])
                total += len(want)
            print(f"  {mode:11s} {name:32s} ok ({time.time() - t:.0f}s)", flush=True)
        assert worst < 1e-6, f"{mode}: coverage differs by {worst}"
        print(f"{mode} oracle: OK, {total} cells, max |coverage diff| = {worst:.2e}", flush=True)


if __name__ == "__main__":
    main()
