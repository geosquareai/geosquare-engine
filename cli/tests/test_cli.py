from __future__ import annotations

import csv
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest


CLI_ROOT = Path(__file__).resolve().parents[1]
REPO_ROOT = CLI_ROOT.parent
DEFAULT_EXTENSION = REPO_ROOT / "bindings/duckdb/build/osx_arm64/geosquare_v2.duckdb_extension"
GIDS = ["H2X545V38", "H2X545V39", "H2X545V3C"]


def extension() -> Path:
    value = os.environ.get("GSQ_EXTENSION")
    path = Path(value) if value else DEFAULT_EXTENSION
    if not path.exists():
        pytest.skip(f"extension not built: {path}")
    return path


def run_gsq(*args: str, extension_path: Path | None = None) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["PYTHONPATH"] = str(CLI_ROOT)
    if extension_path is not None:
        env["GSQ_EXTENSION"] = str(extension_path)
    else:
        env.pop("GSQ_EXTENSION", None)
    return subprocess.run(
        [sys.executable, "-m", "gsq_cli.main", *args],
        cwd=CLI_ROOT,
        env=env,
        text=True,
        capture_output=True,
    )


def write_csv(path: Path, rows: list[dict[str, object]]) -> None:
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def read_csv(path: Path) -> list[dict[str, str]]:
    with path.open(newline="", encoding="utf-8") as handle:
        return list(csv.DictReader(handle))


def test_detect_finds_certain_gid_column(tmp_path: Path) -> None:
    write_csv(tmp_path / "cells.csv", [{"gid": "H2X545V39", "value": 2}])
    result = run_gsq(
        "detect", str(tmp_path / "cells.csv"), "--domain", "ID", "--format", "json", extension_path=extension()
    )
    assert result.returncode == 0, result.stderr
    rows = {row["column"]: row for row in json.loads(result.stdout)}
    assert rows["gid"]["status"] == "certain"


def test_rollup_preserves_totals_and_parent_prefixes(tmp_path: Path) -> None:
    source = tmp_path / "cells.csv"
    output = tmp_path / "rollup.csv"
    write_csv(source, [{"gid": gid, "value": i + 1} for i, gid in enumerate(GIDS)])
    result = run_gsq(
        "rollup", str(source), "--level", "8", "--sum", "value", "--out", str(output), extension_path=extension()
    )
    assert result.returncode == 0, result.stderr
    rows = read_csv(output)
    assert sum(int(row["value"]) for row in rows) == 6
    assert all(all(row["parent_gid"] == gid[:8] for gid in GIDS if gid.startswith(row["parent_gid"])) for row in rows)


def test_join_inner_and_left_row_counts(tmp_path: Path) -> None:
    left = tmp_path / "left.csv"
    right = tmp_path / "right.csv"
    write_csv(left, [{"gid": GIDS[0], "left_value": 1}, {"gid": GIDS[1], "left_value": 2}])
    write_csv(right, [{"gid": GIDS[1], "right_value": 9}, {"gid": GIDS[2], "right_value": 10}])
    ext = extension()
    inner = tmp_path / "inner.csv"
    result = run_gsq("join", str(left), str(right), "--on", "gid", "--how", "inner", "--out", str(inner), extension_path=ext)
    assert result.returncode == 0, result.stderr
    assert len(read_csv(inner)) == 1
    left_out = tmp_path / "left-join.csv"
    result = run_gsq("join", str(left), str(right), "--on", "gid", "--how", "left", "--out", str(left_out), extension_path=ext)
    assert result.returncode == 0, result.stderr
    assert len(read_csv(left_out)) == 2


def test_to_geo_polygon_and_candidate_gate(tmp_path: Path) -> None:
    source = tmp_path / "cells.csv"
    write_csv(source, [{"gid": "H2X545V39"}])
    ext = extension()
    geojson = tmp_path / "cells.geojson"
    result = run_gsq("to-geo", str(source), "--domain", "ID", "--out", str(geojson), extension_path=ext)
    assert result.returncode == 0, result.stderr
    document = json.loads(geojson.read_text())
    assert document["features"][0]["geometry"]["type"] == "Polygon"
    candidate = tmp_path / "candidate.csv"
    result = run_gsq("to-geo", str(source), "--domain", "VN", "--out", str(candidate), extension_path=ext)
    assert result.returncode == 1
    result = run_gsq("to-geo", str(source), "--domain", "VN", "--out", str(candidate), "--allow-candidate-crs", extension_path=ext)
    assert result.returncode == 0, result.stderr


def test_rollup_rejects_higher_level(tmp_path: Path) -> None:
    source = tmp_path / "cells.csv"
    write_csv(source, [{"gid": "H2X545V39", "value": 2}])
    result = run_gsq("rollup", str(source), "--level", "10", "--out", str(tmp_path / "bad.csv"), extension_path=extension())
    assert result.returncode == 1
    assert "greater than input level" in result.stderr


def test_missing_extension_path_is_usage_error(tmp_path: Path) -> None:
    write_csv(tmp_path / "cells.csv", [{"gid": "H2X545V39"}])
    result = run_gsq("detect", str(tmp_path / "cells.csv"), extension_path=None)
    result_env = result
    assert result_env.returncode == 2
    assert "GSQ_EXTENSION" in result_env.stderr
