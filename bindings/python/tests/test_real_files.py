"""Runs the detector on real project files.

These tests are skipped unless `GEOSQUARE_REAL_DATA_ROOT` is set to the folder that holds
`geosquare-risk/`, `pandaigadai/` and `dukcapil/`. They need `pyarrow` to read the files.
If the variable is set and a file is missing, the test fails. It does not skip.
"""

import os
from pathlib import Path

import pytest

import geosquare_engine as gs

ROOT = os.environ.get("GEOSQUARE_REAL_DATA_ROOT")
pytestmark = pytest.mark.skipif(
    not ROOT, reason="GEOSQUARE_REAL_DATA_ROOT is not set"
)

PARCELS = "geosquare-risk/kyl/data/parcels_jakbar.parquet"
KYL_CELLS = "geosquare-risk/kyl/showcase/build/cell_features.parquet"
PANDAI_GRID = "pandaigadai/whitespace/grid/output/grid_whitespace_jabodetabek.parquet"
DUKCAPIL_CSV = "dukcapil/raw/2024S1_rheyhan/all_observations.csv"
NEEDS_INPUT = {"needs_domain", "needs_level", "needs_confirmation"}


def _file(relative):
    path = Path(ROOT) / relative
    if not path.is_file():
        pytest.fail(f"real-data file is missing: {path}")
    return path


def _parquet(relative, columns):
    pq = pytest.importorskip("pyarrow.parquet", reason="pyarrow is needed for real files")
    table = pq.read_table(_file(relative), columns=columns)
    return {name: table.column(name).to_pylist() for name in columns}


def _by_column(results):
    return {result["column"]: result for result in results}


def test_kyl_parcel_gid10km_is_rejected_as_v1():
    # Every value is a 7-character V1 GID. Its sixth character is "7", which is not a
    # valid V2 character at that position (spec 2.0, section 14).
    columns = _parquet(PARCELS, ["gid10km"])
    (result,) = gs.detect_table({"gid10km": ("string", columns["gid10km"])}, domain="ID")
    assert result["status"] == "rejected"
    assert result["valid_ratio"] == 0.0
    assert result["invalid_examples"], "a rejected column must name failing values"
    assert all(len(value) == 7 and value[5] == "7" for value in result["invalid_examples"])


def test_kyl_parcel_id_columns_are_not_spatial_keys():
    columns = _parquet(PARCELS, ["NIB", "NUMERIKID", "WILAYAHID", "KANTORID"])
    table = {name: ("string", values) for name, values in columns.items()}
    for result in gs.detect_table(table, domain="ID"):
        assert result["status"] == "rejected", result["column"]


def test_kyl_50m_cell_gids_are_certain_level_12():
    columns = _parquet(KYL_CELLS, ["gid", "cell_uri"])
    table = {
        "gid": ("string", columns["gid"]),
        "cell_uri": ("string", columns["cell_uri"]),
    }

    with_domain = _by_column(gs.detect_table(table, domain="ID"))
    assert with_domain["gid"]["status"] == "certain"
    assert with_domain["gid"]["levels"] == [12]
    assert with_domain["cell_uri"]["kind"] == "uri"
    assert with_domain["cell_uri"]["status"] == "certain"

    without_domain = _by_column(gs.detect_table({"gid": table["gid"]}))
    assert without_domain["gid"]["status"] == "needs_domain"

    # The URI and the bare GID must name the same cell on every row.
    assert all(
        uri == f"geosquare:v2:ID:{gid}"
        for uri, gid in zip(columns["cell_uri"], columns["gid"])
    )


def test_pandai_grid_gid_and_xy_agree_on_every_row():
    columns = _parquet(PANDAI_GRID, ["gid", "x_idx", "y_idx", "level"])
    assert set(columns["level"]) == {9}

    table = {
        "gid": ("string", columns["gid"]),
        "x_idx": ("int64", columns["x_idx"]),
        "y_idx": ("int64", columns["y_idx"]),
    }
    results = _by_column(gs.detect_table(table, domain="ID", level=9))
    assert results["gid"]["status"] == "certain"
    assert results["gid"]["levels"] == [9]
    assert results["x_idx+y_idx"]["kind"] == "xy_indices"
    assert results["x_idx+y_idx"]["status"] == "certain"

    # Decoding each GID must give back its own x_idx and y_idx.
    for gid, x_idx, y_idx in zip(columns["gid"], columns["x_idx"], columns["y_idx"]):
        assert gs.decode(gid) == (9, x_idx, y_idx), gid


def test_dukcapil_csv_has_no_spatial_key():
    # Desa-level population files carry names and counts, not a grid key. Joining them to
    # cells is a crosswalk job (Phase 3), so this file must not look like a spatial key.
    csv = pytest.importorskip("pyarrow.csv", reason="pyarrow is needed for real files")
    table = csv.read_csv(_file(DUKCAPIL_CSV))
    columns = {}
    for field in table.schema:
        text = str(field.type)
        if text in ("string", "large_string"):
            dtype = "string"
        elif text.startswith("int"):
            dtype = "int64"
        else:
            dtype = "other"
        columns[field.name] = (dtype, table.column(field.name).to_pylist())

    results = gs.detect_table(columns, domain="ID")
    assert results, "the table has columns"
    for result in results:
        assert result["status"] not in NEEDS_INPUT | {"certain"}, result["column"]
        assert result["kind"] != "uri", result["column"]
