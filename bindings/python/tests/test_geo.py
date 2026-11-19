"""CRS binding tests mirrored from the WASM polygon API tests."""

import json
from pathlib import Path

import pytest

import geosquare_engine as gs

VECTORS = Path(__file__).resolve().parents[3] / "vectors" / "spec-2.0"


def _vector(name):
    return json.loads((VECTORS / name).read_text(encoding="utf-8"))


def _domain_ids():
    return {
        record["domain_code"]: record["domain_id"]
        for record in _vector("manifest.json")["domains"]
    }


def test_point_vectors_produce_closed_wgs84_rings():
    domain_ids = _domain_ids()
    for case in _vector("points.json")["cases"]:
        code = case["domain_code"]
        domain_id = domain_ids[code]
        candidate = not case["production_ready"]
        gid = case["gids"]["5"]
        ring = gs.cell_polygon_wgs84(
            domain_id, gid, 25_000.0, allow_candidate_crs=candidate
        )
        assert len(ring) >= 5, code
        assert ring[0] == ring[-1], code
        assert all(len(point) == 2 for point in ring)
        assert all(-180.0 <= lon <= 180.0 and -90.0 <= lat <= 90.0 for lon, lat in ring)


def test_candidate_polygon_requires_explicit_opt_in():
    case = next(case for case in _vector("points.json")["cases"] if case["domain_code"] == "VN")
    with pytest.raises(ValueError, match="not released"):
        gs.cell_polygon_wgs84(2, case["gids"]["5"], 25_000.0)
    assert gs.cell_polygon_wgs84(2, case["gids"]["5"], 25_000.0, True)


def test_polygon_arguments_fail_loudly():
    with pytest.raises(ValueError, match="unknown domain"):
        gs.cell_polygon_wgs84(99, "2345")
    with pytest.raises(ValueError, match="positive and finite"):
        gs.cell_polygon_wgs84(1, "2345", 0.0)
