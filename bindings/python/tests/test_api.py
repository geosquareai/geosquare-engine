"""Checks the Python API surface: types, error classes, and loud failures.

The vector tests cover the values. These tests cover what a caller sees when input is
wrong: the exception type and a message that names the problem.
"""

import pytest

import geosquare_engine as gs


def test_version_is_exposed():
    assert isinstance(gs.__version__, str)


def test_return_types_are_plain_python():
    assert isinstance(gs.encode(3, 1, 2), str)
    assert gs.decode(gs.encode(4, 15, 1)) == (4, 15, 1)
    assert isinstance(gs.pack(1, "2345"), int)
    domain_id, gid = gs.unpack(gs.pack(7, "2345"))
    assert (domain_id, gid) == (7, "2345")


def test_root_parent_is_none_and_level_14_has_no_children():
    assert gs.parent("") is None
    with pytest.raises(ValueError, match="outside 0..=14"):
        gs.children("2" * 14)


def test_out_of_range_indices_raise():
    with pytest.raises(ValueError, match="outside the level 1 grid"):
        gs.encode(1, 5, 0)


def test_level_above_14_raises():
    with pytest.raises(ValueError, match="outside 0..=14"):
        gs.encode(15, 0, 0)


def test_negative_index_is_refused_by_the_argument_parser():
    with pytest.raises(OverflowError):
        gs.encode(3, -1, 0)


def test_domain_zero_is_rejected():
    with pytest.raises(ValueError, match="domain ID 0"):
        gs.pack(0, "2345")


def test_distance_between_levels_names_the_mismatch():
    with pytest.raises(ValueError, match="different levels"):
        gs.distance_m("2345", "23456")


def test_child_below_parent_is_named():
    with pytest.raises(ValueError, match="above parent level"):
        gs.descendant_range(1, "23456", 3)


def test_k_disk_with_huge_k_clips_to_the_grid():
    gid = gs.encode(2, 3, 4)
    assert len(gs.k_disk(gid, 2**64 - 1)) == 100
    assert gs.k_ring(gid, 2**64 - 1) == []


def test_detect_rejects_bool_values():
    with pytest.raises(TypeError, match="bool"):
        gs.detect_table({"flag": ("int64", [True])})


def test_detect_rejects_float_values_in_a_string_column():
    with pytest.raises(TypeError, match="not float"):
        gs.detect_table({"gid": ("string", ["2345", float("nan")])})


def test_detect_accepts_float_values_in_an_other_column():
    result = gs.detect_table({"ratio": ("other", [0.5, None])})[0]
    assert result["kind"] == "none"
    assert result["status"] == "rejected"


def test_detect_rejects_int_beyond_int64():
    with pytest.raises(OverflowError, match="Int64"):
        gs.detect_table({"packed": ("int64", [2**63])})


def test_detect_rejects_unknown_dtype():
    with pytest.raises(ValueError, match="dtype"):
        gs.detect_table({"x": ("float64", [1.0])})


def test_detect_needs_a_domain_for_bare_gids():
    result = gs.detect_table({"gid": ("string", ["23456", "H2X54"])})[0]
    assert result["kind"] == "gid"
    assert result["status"] == "needs_domain"
    assert result["levels"] == [5]


def test_detect_xy_pair_needs_a_level():
    columns = {
        "x_idx": ("int64", [1, 2]),
        "y_idx": ("int64", [3, 4]),
    }
    results = gs.detect_table(columns, domain="ID")
    assert [r["column"] for r in results] == ["x_idx+y_idx"]
    assert results[0]["status"] == "needs_level"


def test_detect_xy_pair_with_level_is_certain():
    columns = {
        "x_idx": ("int64", [1, 2]),
        "y_idx": ("int64", [3, 4]),
    }
    results = gs.detect_table(columns, domain="ID", level=2)
    assert results[0]["kind"] == "xy_indices"
    assert results[0]["status"] == "certain"
    assert results[0]["domain_codes"] == ["ID"]
    assert results[0]["levels"] == [2]


def test_detect_result_keys_are_stable():
    result = gs.detect_table({"gid": ("string", ["2345"])}, domain="ID")[0]
    assert isinstance(result["levels"], list)
    assert isinstance(result["domain_ids"], list)
    assert set(result) == {
        "column",
        "kind",
        "status",
        "reason",
        "valid_ratio",
        "domain_codes",
        "domain_ids",
        "levels",
        "invalid_examples",
    }
