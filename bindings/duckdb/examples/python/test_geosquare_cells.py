"""Python helper vs the Node results: same datasets (written by examples/ts/test/node.test.ts), same answers.

    python test_geosquare_cells.py <extension> <dir written by node.test.ts>
"""
import json, sys, tempfile, os
from geosquare_cells import GeosquareCells

ext, data = sys.argv[1], sys.argv[2]
expected = json.load(open(f"{data}/expected.json"))
expected.pop("columns", None)

def close(a, b):
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        return abs(a - b) <= 1e-9 * max(1, abs(b))
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(close(x, y) for x, y in zip(a, b))
    if isinstance(a, dict) and isinstance(b, dict):
        return a.keys() == b.keys() and all(close(a[k], b[k]) for k in a)
    return a == b

polygon = {"type": "Polygon", "coordinates": [[[106.8227, -6.1799], [106.8317, -6.1799], [106.8317, -6.1709], [106.8227, -6.1709], [106.8227, -6.1799]]]}
points = [(106.8272, -6.1754), (106.8456, -6.2088), (106.83, -6.2)]
with GeosquareCells(ext) as gs:
    for layout in ("flat", "partitioned", "iceberg"):
        p = f"{data}/{layout}"
        got = {
            "summary": gs.summarize(p, {"geojson": polygon}, ["population", "workers"]),
            "summaryBbox": gs.summarize(p, {"bbox": [106.82, -6.18, 106.83, -6.17]}, ["population"]),
            "majority": gs.summarize(p, {"geojson": polygon}, ["population"], min_coverage=0.5, weighted=False),
            "line": gs.summarize(p, {"wkt": "LINESTRING (106.81 -6.21, 106.85 -6.17)"}, ["population"]),
            "rollup": gs.rollup(p, {"geojson": polygon}, ["population"], 9),
            "cells": gs.cells(p, {"geojson": polygon}, ["population"], limit=5),
            "points": gs.at_points(p, points, ["population", "workers"]),
            "fc": len(gs.feature_collection(p, {"bbox": [106.825, -6.176, 106.826, -6.175]}, ["population"])["features"]),
        }
        for k, v in expected.items():
            assert close(got[k], v), (layout, k, str(got[k])[:200], str(v)[:200])
    # stream + write round trip
    n = gs.stream(f"{data}/iceberg", lambda rows: None, columns=["population"], batch=5000)
    assert n == gs.query(f"SELECT count(*) AS n FROM read_cell_dataset('{data}/flat')")[0]["n"]
    out = tempfile.mkdtemp()
    r = gs.write(f"SELECT packed_id, population, NULL::DOUBLE AS empty FROM read_cell_dataset('{data}/flat')", f"{out}/copy",
                 layout="iceberg", value_semantics="COUNT", aggregation_rule="sum")
    assert r["rows"] == str(n) or r["rows"] == n, r
    assert gs.query(f"SELECT count(*) AS n FROM read_cell_dataset('{out}/copy') WHERE empty IS NULL")[0]["n"] == 0
print("python OK: same results as Node for flat, partitioned and iceberg; stream", n, "rows; write round trip")
