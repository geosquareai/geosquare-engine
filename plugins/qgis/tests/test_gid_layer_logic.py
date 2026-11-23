"""Unit tests for the QGIS-independent E14 operations."""

from pathlib import Path
import sys

import pytest

PLUGIN_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PLUGIN_DIR))

from gid_layer_logic import (  # noqa: E402
    GidLayerError,
    build_cell_features,
    rollup_cell_features,
)

import geosquare_engine as gs  # noqa: E402


POINTS = Path(__file__).resolve().parents[3] / "vectors" / "spec-2.0" / "points.json"


def _points():
    import json

    return json.loads(POINTS.read_text(encoding="utf-8"))["cases"]


def test_build_uses_point_vectors_and_copies_numeric_fields():
    case = next(case for case in _points() if case["domain_code"] == "ID")
    gid = case["gids"]["5"]
    features = build_cell_features(
        [{"gid": gid, "population": 12, "rate": 0.5}],
        gid_field="gid",
        numeric_fields=["population", "rate"],
        domain_id=1,
        max_segment_length_m=25_000,
    )

    assert len(features) == 1
    assert features[0].gid == gid
    assert features[0].level == 5
    assert features[0].attributes == {"population": 12, "rate": 0.5}
    assert features[0].geometry[0] == features[0].geometry[-1]


def test_invalid_gid_fails_before_output_is_built():
    with pytest.raises(GidLayerError, match="Invalid GID"):
        build_cell_features(
            [{"gid": "not-a-gid", "population": 1}],
            gid_field="gid",
            numeric_fields=["population"],
            domain_id=1,
        )


def test_mixed_levels_are_rejected():
    case = next(case for case in _points() if case["domain_code"] == "ID")
    with pytest.raises(GidLayerError, match="one level"):
        build_cell_features(
            [{"gid": case["gids"]["5"], "population": 1}, {"gid": case["gids"]["7"], "population": 2}],
            gid_field="gid",
            numeric_fields=["population"],
            domain_id=1,
        )


def test_rollup_walks_engine_parents_and_sums_numeric_fields():
    parent_gid = "H2X54"
    child_gids = gs.children(parent_gid)[:2]
    children = build_cell_features(
        [
            {"gid": child_gids[0], "population": 10, "missing": None},
            {"gid": child_gids[1], "population": 7, "missing": None},
        ],
        gid_field="gid",
        numeric_fields=["population", "missing"],
        domain_id=1,
        max_segment_length_m=25_000,
    )

    rolled = rollup_cell_features(
        children,
        target_level=len(parent_gid),
        domain_id=1,
        max_segment_length_m=25_000,
    )

    assert [(feature.gid, feature.attributes) for feature in rolled] == [
        (parent_gid, {"population": 17, "missing": None})
    ]
    assert rolled[0].geometry[0] == rolled[0].geometry[-1]


def test_rollup_rejects_a_deeper_target_level():
    features = build_cell_features(
        [{"gid": "H2X54", "population": 1}],
        gid_field="gid",
        numeric_fields=["population"],
        domain_id=1,
        max_segment_length_m=25_000,
    )
    with pytest.raises(GidLayerError, match="between 0 and 5"):
        rollup_cell_features(features, target_level=6, domain_id=1)
