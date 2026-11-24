"""gsq pyramid: every coarser level must equal a rollup of the base level with the declared rules.

The base is a cell dataset folder with manifest 1.1 (pyramid spec section 5). The oracle is plain
Python on the base rows. It does not use the writer's SQL.
"""

from __future__ import annotations

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
SEMANTICS = '{"pop": "COUNT", "rate": "RATE", "flood": "CATEGORICAL", "share_in_aoi": "COUNT"}'
sys.path.insert(0, str(CLI_ROOT))

from gsq_cli.main import content_sha256  # noqa: E402


def extension() -> Path:
    value = os.environ.get("GSQ_EXTENSION")
    path = Path(value) if value else DEFAULT_EXTENSION
    if not path.exists():
        pytest.skip(f"extension not built: {path}")
    return path


def duck(ext: Path):
    import duckdb

    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{ext}'")
    return con


def run_pyramid(*args: str, ext: Path) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["PYTHONPATH"] = str(CLI_ROOT)
    env["GSQ_EXTENSION"] = str(ext)
    return subprocess.run(
        [sys.executable, "-m", "gsq_cli.main", "pyramid", *args],
        cwd=CLI_ROOT, env=env, text=True, capture_output=True,
    )


def base_cells(ext: Path, level: int = 12) -> list[str]:
    """Distinct GIDs on a 40 x 40 grid of points about 50 m apart in Jakarta, at one level."""

    con = duck(ext)
    rows = con.execute(
        f"SELECT DISTINCT point_to_cell('ID', 106.80 + i * 0.0005, -6.20 - j * 0.0005, {level}).gid "
        "FROM range(40) r(i), range(40) s(j)"
    ).fetchall()
    return sorted(r[0] for r in rows)


def base_values(gids: list[str]) -> dict[str, dict[str, float | None]]:
    """Deterministic values for each GID. Returns the rows by GID for the oracle."""

    data: dict[str, dict[str, float | None]] = {}
    for n, gid in enumerate(gids):
        rate = None if n % 6 == 0 else float((n * 7) % 11)
        share = 0.25 if n % 7 == 0 else 1.0
        data[gid] = {
            "pop": float(1 + n % 5),
            "rate": rate,
            "flood": float((n * 3) % 9) / 2,
            "share": share,
        }
    return data


def write_dataset(ext: Path, folder: Path, data: dict[str, dict[str, float | None]],
                  level_gids: bool = True, manifest_version: str = "1.1", domain: str = "ID") -> Path:
    """Write the base rows as a flat cell dataset with manifest 1.1 (or another version)."""

    con = duck(ext)
    staged = folder.parent / f"{folder.name}.rows.parquet"
    con.execute("CREATE TEMP TABLE base (gid VARCHAR, pop DOUBLE, rate DOUBLE, flood DOUBLE, share_in_aoi DOUBLE)")
    con.executemany(
        "INSERT INTO base VALUES (?, ?, ?, ?, ?)",
        [(gid, v["pop"], v["rate"], v["flood"], v["share"]) for gid, v in data.items()],
    )
    con.execute(f"COPY base TO '{staged}' (FORMAT parquet)")
    con.execute(
        f"SELECT * FROM write_cell_dataset('SELECT * FROM read_parquet(''{staged}'')', ?, layout := 'flat', "
        f"domain_code := '{domain}', manifest_version := '{manifest_version}', fill_nulls := false, value_semantics := ?)",
        [str(folder), SEMANTICS],
    ).fetchall()
    staged.unlink()
    return folder


def base_dataset(tmp_path: Path, ext: Path) -> tuple[Path, dict[str, dict[str, float | None]]]:
    base = base_values(base_cells(ext))
    return write_dataset(ext, tmp_path / "base12", base), base


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
    con = duck(Path(os.environ.get("GSQ_EXTENSION", DEFAULT_EXTENSION)))
    out: dict[str, dict[str, float | None]] = {}
    for file in sorted((root / f"level={level}").glob("gid_prefix=*/part-00000.parquet")):
        for row in con.execute(f"SELECT * FROM read_parquet('{file}')").fetchall():
            out[row[0]] = dict(zip(columns, row[1:]))
    return out


def run_ok(base: Path, out: Path, ext: Path, *extra: str) -> subprocess.CompletedProcess[str]:
    return run_pyramid(
        str(base), "--domain", "ID", "-o", str(out),
        "--value", "pop=sum", "--value", "rate=weighted_mean:pop",
        "--value", "flood=max", "--share-column", "share_in_aoi", *extra,
        ext=ext,
    )


def test_coarse_levels_equal_declared_rollup(tmp_path: Path) -> None:
    ext = extension()
    base_path, base = base_dataset(tmp_path, ext)
    assert len(base) > 500
    out = tmp_path / "pyramid"
    result = run_ok(base_path, out, ext)
    assert result.returncode == 0, result.stderr

    manifest = json.loads((out / "pyramid.json").read_text(encoding="utf-8"))
    assert [lv["level"] for lv in manifest["levels"]] == [12, 10, 8]
    assert manifest["source"]["row_count"] == len(base)
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


def test_source_copies_the_base_manifest(tmp_path: Path) -> None:
    ext = extension()
    base_path, _ = base_dataset(tmp_path, ext)
    out = tmp_path / "pyramid"
    assert run_ok(base_path, out, ext).returncode == 0
    base = json.loads((base_path / "manifest.json").read_text(encoding="utf-8"))
    manifest = json.loads((out / "pyramid.json").read_text(encoding="utf-8"))
    assert manifest["source"]["manifest_version"] == "1.1"
    assert manifest["source"]["dataset_id"] == "base12"
    for key in ("grid_system", "grid_version", "domain_code", "profile_version", "registry_version"):
        assert manifest[key] == base[key], key


def test_refuses_raw_csv_with_the_spec_rule(tmp_path: Path) -> None:
    ext = extension()
    csv = tmp_path / "base.csv"
    csv.write_text("gid,pop\n" + f"{base_cells(ext)[0]},1.0\n", encoding="utf-8")
    result = run_pyramid(str(csv), "--domain", "ID", "-o", str(tmp_path / "p"), "--value", "pop=sum", ext=ext)
    assert result.returncode == 1
    assert "cell dataset folder" in result.stderr and "section 5" in result.stderr
    assert not (tmp_path / "p").exists()


def test_refuses_manifest_other_than_1_1(tmp_path: Path) -> None:
    ext = extension()
    folder = write_dataset(ext, tmp_path / "old", base_values(base_cells(ext)), manifest_version="1.0")
    result = run_pyramid(str(folder), "--domain", "ID", "-o", str(tmp_path / "p"), "--value", "pop=sum", ext=ext)
    assert result.returncode == 1
    assert "manifest 1.1" in result.stderr


def test_refuses_weight_that_is_not_count_or_total(tmp_path: Path) -> None:
    ext = extension()
    base_path, _ = base_dataset(tmp_path, ext)
    result = run_pyramid(
        str(base_path), "--domain", "ID", "-o", str(tmp_path / "p"),
        "--value", "rate=sum", "--value", "pop=weighted_mean:rate", ext=ext,
    )
    assert result.returncode == 2
    assert "COUNT or TOTAL" in result.stderr


def test_refuses_base_level_other_than_12(tmp_path: Path) -> None:
    ext = extension()
    folder = write_dataset(ext, tmp_path / "l10", base_values(base_cells(ext, level=10)))
    result = run_pyramid(str(folder), "--domain", "ID", "-o", str(tmp_path / "p"), "--value", "pop=sum", ext=ext)
    assert result.returncode == 1
    assert "pyramid base must be level 12" in result.stderr


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
