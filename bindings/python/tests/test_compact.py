"""Runs the cell_compact_set vectors against the Python binding's compact helpers.

The vector file is the same one the spec repo generates
(`conformance/spec-2.0/compact-set.json` in geosquare-v2). Expected values come
from the spec rules, so the two implementations cannot drift apart silently.
"""

import json
from pathlib import Path

import pytest

from geosquare_engine import compact
from geosquare_engine import encode

VECTORS = Path(__file__).resolve().parents[3] / "vectors" / "spec-2.0"


def load_cases():
    return json.loads((VECTORS / "compact-set.json").read_text(encoding="utf-8"))["cases"]


def evaluate(case):
    """Run one case and return the observed fields, using the names the vectors use."""
    operation = case["operation"]
    try:
        if operation == "canonicalize":
            return {"gids": list(compact.canonicalize(case["input"]))}
        if operation == "expand":
            cells = compact.expand(case["input"], case["level"])
            observed = {"count": len(cells)}
            if "gids" in case["expect"]:
                observed["gids"] = sorted(cells)
            return observed
        if operation == "contains":
            return {"member": compact.contains(case["input"], case["gid"])}
        if operation == "validate":
            compact.validate_compact_set((row["gid"], row["level"]) for row in case["input"])
            return {"valid": True}
    except compact.CompactSetError:
        name = "CompactSetError"
    except ValueError:
        # The Rust core raises ValueError for an invalid GID. The spec name is InvalidGIDError.
        name = "InvalidGIDError"
    else:
        raise AssertionError(f"unknown compact-set operation {operation!r}")
    if operation == "expand":
        return {"error": name}
    return {"valid": False, "error": name}


@pytest.mark.parametrize("case", load_cases(), ids=lambda case: case["name"])
def test_vector_case_matches_spec(case):
    assert evaluate(case) == case["expect"]


def test_root_and_leaf_edges_follow_the_core():
    root = encode(0, 0, 0)
    assert root == ""
    assert len(compact.expand([root], 2)) == 100
    with pytest.raises(compact.CompactSetError):
        compact.child_gids("2" * 14)
