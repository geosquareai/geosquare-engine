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


def test_check_clean_file_exits_zero(tmp_path: Path) -> None:
    source = tmp_path / "cells.csv"
    write_csv(source, [{"gid": gid, "value": 1} for gid in GIDS])
    result = run_gsq("check", str(source), "--domain", "ID", "--json", extension_path=extension())
    assert result.returncode == 0, result.stderr
    report = json.loads(result.stdout)
    assert report["ok"] is True
    assert report["problems"] == []
    assert report["levels"] == [9]


def test_check_reports_bad_gid_duplicate_and_mixed_levels(tmp_path: Path) -> None:
    source = tmp_path / "cells.csv"
    write_csv(
        source,
        [
            {"gid": GIDS[0], "value": 1},
            {"gid": GIDS[0], "value": 1},
            {"gid": "NOTAGID!", "value": 2},
            {"gid": "H2X545V3", "value": 3},
        ],
    )
    result = run_gsq("check", str(source), "--domain", "ID", "--json", extension_path=extension())
    assert result.returncode == 1, result.stderr
    report = json.loads(result.stdout)
    assert report["ok"] is False
    assert report["invalid_gids"] == [[3, "NOTAGID!"]]
    assert report["duplicate_gids"] == [[GIDS[0], 2]]
    assert report["levels"] == [8, 9]
    assert len(report["problems"]) == 3


def test_check_source_total_mismatch_fails(tmp_path: Path) -> None:
    source = tmp_path / "cells.csv"
    other = tmp_path / "source.csv"
    write_csv(source, [{"gid": gid, "value": 1} for gid in GIDS])
    write_csv(other, [{"gid": gid, "value": 2} for gid in GIDS])
    ext = extension()
    result = run_gsq("check", str(source), "--domain", "ID", "--source", str(other), "--value", "value", extension_path=ext)
    assert result.returncode == 1, result.stdout
    assert "value total differs from source by -3.0" in result.stdout
    result = run_gsq("check", str(source), "--domain", "ID", "--source", str(source), "--value", "value", extension_path=ext)
    assert result.returncode == 0, result.stdout


def test_check_empty_file_fails(tmp_path: Path) -> None:
    source = tmp_path / "cells.csv"
    source.write_text("gid,value\n", encoding="utf-8")
    result = run_gsq("check", str(source), "--domain", "ID", extension_path=extension())
    assert result.returncode == 1
    assert "no rows with a GID value" in result.stdout


def test_check_without_gid_column_is_error(tmp_path: Path) -> None:
    source = tmp_path / "cells.csv"
    write_csv(source, [{"packed_id": 1, "value": 2}])
    result = run_gsq("check", str(source), "--domain", "ID", extension_path=extension())
    assert result.returncode == 1
    assert "unknown column(s): gid" in result.stderr


def test_check_parquet_input(tmp_path: Path) -> None:
    import duckdb

    source = tmp_path / "cells.parquet"
    rows = ", ".join(f"('{gid}', 1)" for gid in GIDS)
    duckdb.connect().execute(f"COPY (SELECT * FROM (VALUES {rows}) t(gid, value)) TO '{source}' (FORMAT PARQUET)")
    result = run_gsq("check", str(source), "--domain", "ID", "--json", extension_path=extension())
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["rows"] == 3


def test_export_geoparquet_keeps_values_and_nulls(tmp_path: Path) -> None:
    import duckdb

    source = tmp_path / "cells.csv"
    output = tmp_path / "cells.parquet"
    write_csv(source, [{"gid": gid, "value": None if i == 1 else i + 1} for i, gid in enumerate(GIDS)])
    result = run_gsq("export", str(source), "--domain", "ID", "-o", str(output), extension_path=extension())
    assert result.returncode == 0, result.stderr
    con = duckdb.connect()
    rows = con.execute(f"SELECT gid, value, CAST(geometry AS BLOB) FROM read_parquet('{output}') ORDER BY gid").fetchall()
    assert len(rows) == len(GIDS)
    assert [row[1] for row in sorted(rows, key=lambda r: GIDS.index(r[0]))] == [1, None, 3]
    for _, _, wkb in rows:
        # Little-endian WKB header, geometry type 3 (Polygon).
        assert wkb[:5] == b"\x01\x03\x00\x00\x00"
    geo = json.loads(con.execute(f"SELECT value FROM parquet_kv_metadata('{output}') WHERE key = 'geo'").fetchone()[0])
    assert geo["primary_column"] == "geometry"
    assert geo["columns"]["geometry"]["encoding"] == "WKB"
    assert geo["columns"]["geometry"]["geometry_types"] == ["Polygon"]


def test_export_refuses_bad_gid(tmp_path: Path) -> None:
    source = tmp_path / "cells.csv"
    output = tmp_path / "cells.parquet"
    write_csv(source, [{"gid": "H2X545V39", "value": 1}, {"gid": "NOTAGID", "value": 2}])
    result = run_gsq("export", str(source), "--domain", "ID", "-o", str(output), extension_path=extension())
    assert result.returncode == 1
    assert "invalid GID at row 2: NOTAGID" in result.stderr
    assert not output.exists()


def test_export_kepler_config_colors_by_column(tmp_path: Path) -> None:
    source = tmp_path / "cells.csv"
    output = tmp_path / "cells.parquet"
    config = tmp_path / "cells.kepler.json"
    write_csv(source, [{"gid": gid, "value": i + 1} for i, gid in enumerate(GIDS)])
    result = run_gsq(
        "export", str(source), "--domain", "ID", "-o", str(output),
        "--kepler", str(config), "--color", "value", extension_path=extension(),
    )
    assert result.returncode == 0, result.stderr
    layer = json.loads(config.read_text())["config"]["visState"]["layers"][0]["config"]
    assert layer["dataId"] == "cells"
    assert layer["columns"] == {"geojson": "geometry"}
    assert layer["colorField"] == {"name": "value", "type": "integer"}
    result = run_gsq("export", str(source), "--domain", "ID", "-o", str(output), "--color", "value", extension_path=extension())
    assert result.returncode == 2
    assert "--color needs --kepler" in result.stderr
