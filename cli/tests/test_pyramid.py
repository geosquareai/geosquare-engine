"""gsq pyramid: every coarser level must equal a rollup of the base level with the declared rules.

The oracle is plain Python on the base rows. It does not use the writer's SQL.
"""

from __future__ import annotations

import csv
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

CLI_ROOT = Path(__file__).resolve().parents[1]
REPO_ROOT = CLI_ROOT.parent
DEFAULT_EXTENSION = REPO_ROOT / "bindings/duckdb/build/osx_arm64/geosquare_v2.duckdb_extension"
sys.path.insert(0, str(CLI_ROOT))

from gsq_cli.main import content_sha256  # noqa: E402


def extension() -> Path:
    value = os.environ.get("GSQ_EXTENSION")
    path = Path(value) if value else DEFAULT_EXTENSION
    if not path.exists():
        pytest.skip(f"extension not built: {path}")
    return path


def run_pyramid(*args: str, ext: Path) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["PYTHONPATH"] = str(CLI_ROOT)
    env["GSQ_EXTENSION"] = str(ext)
    return subprocess.run(
        [sys.executable, "-m", "gsq_cli.main", "pyramid", *args],
        cwd=CLI_ROOT, env=env, text=True, capture_output=True,
    )


def base_cells(ext: Path) -> list[str]:
    """Distinct level-12 GIDs on a 40 x 40 grid of points at about 50 m spacing in Jakarta."""

    import duckdb

    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{ext}'")
    rows = con.execute(
        "SELECT DISTINCT point_to_cell('ID', 106.80 + i * 0.0005, -6.20 - j * 0.0005, 12).gid "
        "FROM range(40) r(i), range(40) s(j)"
    ).fetchall()
    return sorted(r[0] for r in rows)


def write_base(path: Path, gids: list[str]) -> dict[str, dict[str, float | None]]:
    """Write a base CSV with deterministic values. Returns the rows by GID for the oracle."""

    data: dict[str, dict[str, float | None]] = {}
    with open(path, "w", newline="", encoding="utf-8") as handle:
        writer = csv.writer(handle, lineterminator="\n")
        writer.writerow(["gid", "pop", "rate", "flood", "share_in_aoi"])
        for n, gid in enumerate(gids):
            pop = float(1 + n % 5)
            rate = None if n % 6 == 0 else float((n * 7) % 11)
            flood = float((n * 3) % 9) / 2
            share = 0.25 if n % 7 == 0 else 1.0
            data[gid] = {"pop": pop, "rate": rate, "flood": flood, "share": share}
            writer.writerow([gid, pop, "" if rate is None else rate, flood, share])
    return data


def median(values: list[float]) -> float:
    values = sorted(values)
    mid = len(values) // 2
    return values[mid] if len(values) % 2 else (values[mid - 1] + values[mid]) / 2


def oracle(base: dict[str, dict[str, float | None]], level: int) -> dict[str, dict[str, float | None]]:
    fan_in = {10: 100, 8: 10000}[level]
    groups: dict[str, list[str]] = {}
    for gid in base:
        groups.setdefault(gid[:level], []).append(gid)
    out: dict[str, dict[str, float | None]] = {}
    for parent, children in groups.items():
        rates = [(base[c]["rate"], base[c]["pop"]) for c in children if base[c]["rate"] is not None]
        flood = [base[c]["flood"] for c in children]
        pop = [base[c]["pop"] for c in children]
        total_w = sum(w for _, w in rates)
        out[parent] = {
            "pop": sum(pop),
            "rate": sum(v * w for v, w in rates) / total_w if total_w else None,
            "flood": max(flood),
            "share": sum(base[c]["share"] for c in children) / fan_in,
        }
    return out


def read_level(root: Path, level: int, columns: list[str]) -> dict[str, dict[str, float | None]]:
    import duckdb

    out: dict[str, dict[str, float | None]] = {}
    files = sorted((root / f"level={level}").glob("gid_prefix=*/part-00000.parquet"))
    con = duckdb.connect()
    for file in files:
        for row in con.execute(f"SELECT * FROM read_parquet('{file}')").fetchall():
            out[row[0]] = dict(zip(columns, row[1:]))
    return out


def test_coarse_levels_equal_declared_rollup(tmp_path: Path) -> None:
    ext = extension()
    gids = base_cells(ext)
    assert len(gids) > 500
    source = tmp_path / "base.csv"
    base = write_base(source, gids)
    out = tmp_path / "pyramid"
    result = run_pyramid(
        str(source), "--domain", "ID", "-o", str(out),
        "--value", "pop=sum", "--value", "rate=weighted_mean:pop",
        "--value", "flood=max", "--share-column", "share_in_aoi",
        ext=ext,
    )
    assert result.returncode == 0, result.stderr

    manifest = json.loads((out / "pyramid.json").read_text(encoding="utf-8"))
    assert [lv["level"] for lv in manifest["levels"]] == [12, 10, 8]
    assert manifest["source"]["row_count"] == len(gids)
    for lv in manifest["levels"]:
        assert lv["row_count"] == sum(f["row_count"] for f in lv["files"])

    for level in (10, 8):
        expected = oracle(base, level)
        got = read_level(out, level, ["pop", "rate", "flood", "share_in_aoi"])
        assert set(got) == set(expected), f"level {level} parents differ"
        for parent, values in expected.items():
            for name, value in values.items():
                key = "share_in_aoi" if name == "share" else name
                actual = got[parent][key]
                if value is None:
                    assert actual is None, (level, parent, name)
                else:
                    assert actual == pytest.approx(value, rel=1e-12, abs=1e-12), (level, parent, name)


def test_rejects_non_numeric_value(tmp_path: Path) -> None:
    ext = extension()
    gids = base_cells(ext)[:20]
    source = tmp_path / "base.csv"
    write_base(source, gids)
    text = source.read_text(encoding="utf-8").replace(",1.0,", ",abc,", 1)
    source.write_text(text, encoding="utf-8")
    result = run_pyramid(str(source), "--domain", "ID", "-o", str(tmp_path / "p"), "--value", "pop=sum", ext=ext)
    assert result.returncode == 1
    assert "not numbers" in result.stderr
    assert not (tmp_path / "p").exists()


def test_rejects_mixed_levels(tmp_path: Path) -> None:
    ext = extension()
    gids = base_cells(ext)[:20]
    source = tmp_path / "base.csv"
    write_base(source, gids)
    with open(source, "a", encoding="utf-8") as handle:
        handle.write(f"{gids[0][:10]},1.0,1.0,1.0,1.0\n")
    result = run_pyramid(str(source), "--domain", "ID", "-o", str(tmp_path / "p"), "--value", "pop=sum", ext=ext)
    assert result.returncode == 1
    assert "pyramid base must be level 12" in result.stderr or "mixed levels" in result.stderr


def test_profile_constants_match_core() -> None:
    from gsq_cli.main import PYRAMID_PROFILE_VERSION, PYRAMID_REGISTRY_VERSION

    source = (REPO_ROOT / "core/src/domain.rs").read_text(encoding="utf-8")
    assert f'PROFILE_VERSION: &str = "{PYRAMID_PROFILE_VERSION}"' in source
    assert f'REGISTRY_VERSION: &str = "{PYRAMID_REGISTRY_VERSION}"' in source


def test_content_hash_matches_conformance_vectors() -> None:
    from gsq_cli.main import encode_element

    vectors = json.loads((REPO_ROOT / "vectors/spec-2.0/content-hash.json").read_text(encoding="utf-8"))
    checked = 0
    for case in vectors["cases"]:
        types = [f["parquet_type"] for f in case["value_fields"]]
        if any(t not in ("double", "string") for t in types):
            continue  # int64 rows are not written by pyramid; the Rust side checks them.
        rows = [
            [row[0], *[float(v) if t == "double" and v is not None else v for v, t in zip(row[1:], types)]]
            for row in case["rows"]
        ]
        assert content_sha256(rows) == case["sha256"], case["name"]
        checked += 1
    assert checked >= 4
    assert encode_element(0.5) == "0.5"
    assert hashlib.sha256(b"").hexdigest() == content_sha256([])
