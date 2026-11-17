"""Runs the spec 2.0 conformance vectors against the Python binding.

The vectors are the same files the Rust core reads (`core/tests/vectors.rs`). Every
expected value here comes from those files, so the two bindings cannot drift apart
without a failing test.
"""

import json
from pathlib import Path

import pytest

import geosquare_engine as gs

VECTORS = Path(__file__).resolve().parents[3] / "vectors" / "spec-2.0"


def load(name):
    return json.loads((VECTORS / name).read_text(encoding="utf-8"))


def domain_ids():
    return load("codec.json")["packed_ids_by_domain"]["domains"]


def descendant_gids(parent_gid, child_level):
    gids = [parent_gid]
    for _ in range(len(parent_gid), child_level):
        gids = [child for gid in gids for child in gs.children(gid)]
    return gids


def test_vector_set_is_complete():
    assert len(domain_ids()) == 11
    assert len(load("detect.json")["cases"]) >= 15


def test_codec_cells_round_trip_and_pack_for_id():
    cells = load("codec.json")["cells"]
    assert cells
    for record in cells:
        gid = record["gid"]
        assert gs.encode(record["level"], record["x_idx"], record["y_idx"]) == gid
        assert gs.decode(gid) == (record["level"], record["x_idx"], record["y_idx"])
        packed = int(record["packed_id"])
        assert gs.pack(1, gid) == packed, gid
        assert gs.unpack(packed) == (1, gid)


def test_packed_ids_match_for_every_domain():
    codec = load("codec.json")["packed_ids_by_domain"]
    for record in codec["cells"]:
        gid = record["gid"]
        assert gs.encode(record["level"], record["x_idx"], record["y_idx"]) == gid
        for code, domain_id in codec["domains"].items():
            expected = int(record["packed_ids"][code])
            assert gs.pack(domain_id, gid) == expected, f"domain {code}"
            assert gs.unpack(expected) == (domain_id, gid), f"domain {code}"


def test_invalid_gids_are_rejected():
    for record in load("codec.json")["invalid_gids"]:
        with pytest.raises(ValueError):
            gs.decode(record["input"])


def test_invalid_packed_ids_are_rejected():
    for record in load("codec.json")["invalid_packed_ids"]:
        value = int(record["packed_id"])
        if value >= 2**63:
            # Outside the signed Int64 range, so pyo3 refuses the argument.
            with pytest.raises(OverflowError):
                gs.unpack(value)
        else:
            with pytest.raises(ValueError):
                gs.unpack(value)


def test_parents_and_children_match():
    topology = load("topology.json")
    for record in topology["parent_of"]:
        parent = gs.parent(record["gid"])
        assert parent == record["parent_gid"]
        assert len(parent) == record["level"] - 1
    for record in topology["children_of"]:
        assert sorted(gs.children(record["gid"])) == sorted(record["child_gids"])


def test_descendant_ranges_match_and_contain_every_descendant():
    for record in load("topology.json")["descendant_ranges"]:
        parent = record["parent_gid"]
        child_level = record["child_level"]
        lo, hi = gs.descendant_range(1, parent, child_level)
        assert lo == int(record["lo"])
        assert hi == int(record["hi"])
        descendants = descendant_gids(parent, child_level)
        assert len(descendants) == record["descendant_count"]
        for gid in descendants:
            assert lo <= gs.pack(1, gid) <= hi


def test_descendant_ranges_match_for_every_domain():
    domains = domain_ids()
    records = load("topology.json")["descendant_ranges_by_domain"]["records"]
    assert records
    for record in records:
        for code, bounds in record["ranges"].items():
            lo, hi = gs.descendant_range(
                domains[code], record["parent_gid"], record["child_level"]
            )
            assert lo == int(bounds["lo"]), code
            assert hi == int(bounds["hi"]), code


def test_neighbour_sets_match():
    for record in load("topology.json")["neighbours"]:
        gid = gs.encode(record["level"], record["x_idx"], record["y_idx"])
        assert sorted(gs.k_ring(gid, 1)) == sorted(record["k_ring_1"])
        assert sorted(gs.k_ring(gid, 2)) == sorted(record["k_ring_2"])
        assert sorted(gs.k_disk(gid, 1)) == sorted(record["k_disk_1"])


def test_distances_match_the_square_root_formula():
    for record in load("topology.json")["distances"]:
        a, b = record["a_gid"], record["b_gid"]
        assert gs.cell_side_m(len(a)) == record["cell_side_m"]
        assert gs.distance_m(a, b) == pytest.approx(record["distance_m"], rel=1e-12)


def _detect_dtype(dtype):
    if dtype in ("string", "int64"):
        return dtype
    return "other"


def _detect_value(dtype, value):
    if dtype == "int64" and value is not None:
        return int(value)
    return value


def test_detection_cases_match_the_reference():
    cases = load("detect.json")["cases"]
    for case in cases:
        table = {}
        for name, spec in case["columns"].items():
            dtype = _detect_dtype(spec["dtype"])
            values = [_detect_value(dtype, value) for value in spec["values"]]
            table[name] = (dtype, values)
        results = gs.detect_table(
            table, domain=case.get("domain_code"), level=case.get("level")
        )
        by_column = {result["column"]: result for result in results}
        for key, expected in case["expect"].items():
            result = by_column[key]
            label = f"{case['name']}/{key}"
            assert result["kind"] == expected["kind"], label
            assert result["status"] == expected["status"], label
            if "domain_codes" in expected:
                assert result["domain_codes"] == expected["domain_codes"], label
            if "domain_ids" in expected:
                assert result["domain_ids"] == expected["domain_ids"], label
            if "levels" in expected:
                assert result["levels"] == expected["levels"], label
