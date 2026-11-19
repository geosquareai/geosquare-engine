"""Lay out a DuckDB extension repository for `INSTALL geosquare_v2 FROM '<url>'`.

DuckDB downloads `<url>/<duckdb version>/<platform>/geosquare_v2.duckdb_extension.gz`
(`.duckdb_extension.wasm` for the Wasm platforms). The extension uses the stable C API,
so one build per platform serves every DuckDB release; this script copies it under each
version folder.

    python scripts/make_repository.py --artifacts dist --out repo --versions auto
    python scripts/make_repository.py --artifacts dist --out repo --versions v1.4.0,v2.0.0

`--artifacts` holds one folder per platform (linux_amd64, osx_arm64, wasm_eh, ...) with the
built `geosquare_v2.duckdb_extension` or `geosquare_v2.duckdb_extension.wasm`.
`--versions auto` reads the DuckDB releases from PyPI, from `--min-version` (default 1.4.0).
"""

import argparse
import gzip
import hashlib
import json
import shutil
import urllib.request
from pathlib import Path

NAME = "geosquare_v2"


def pypi_versions(minimum):
    data = json.load(urllib.request.urlopen("https://pypi.org/pypi/duckdb/json", timeout=60))
    key = lambda v: tuple(int(x) for x in v.split("."))
    out = []
    for v, files in data["releases"].items():
        if not files or not all(p.isdigit() for p in v.split(".")):
            continue  # skip yanked/empty and pre-releases
        if any(f.get("yanked") for f in files):
            continue
        if key(v) >= key(minimum):
            out.append(v)
    return [f"v{v}" for v in sorted(out, key=key)]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--artifacts", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--versions", default="auto", help="'auto' or a comma list like v1.4.0,v2.0.0")
    ap.add_argument("--min-version", default="1.4.0")
    args = ap.parse_args()

    versions = pypi_versions(args.min_version) if args.versions == "auto" else [v.strip() for v in args.versions.split(",") if v.strip()]
    versions = [v if v.startswith("v") else f"v{v}" for v in versions]
    builds = {}
    for platform_dir in sorted(p for p in args.artifacts.iterdir() if p.is_dir()):
        wasm = platform_dir / f"{NAME}.duckdb_extension.wasm"
        native = platform_dir / f"{NAME}.duckdb_extension"
        if wasm.exists():
            builds[platform_dir.name] = (wasm.read_bytes(), f"{NAME}.duckdb_extension.wasm", False)
        elif native.exists():
            builds[platform_dir.name] = (native.read_bytes(), f"{NAME}.duckdb_extension.gz", True)
    if not builds:
        raise SystemExit(f"no builds under {args.artifacts}")

    shutil.rmtree(args.out, ignore_errors=True)
    index = {"extension": NAME, "versions": versions, "platforms": {}}
    for platform, (data, filename, compress) in builds.items():
        payload = gzip.compress(data, mtime=0) if compress else data
        index["platforms"][platform] = {"file": filename, "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
        for v in versions:
            target = args.out / v / platform / filename
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(payload)
    (args.out / "index.json").write_text(json.dumps(index, indent=1))
    print(f"{len(builds)} platforms x {len(versions)} DuckDB versions -> {args.out}")
    print("platforms:", ", ".join(builds), "| versions:", ", ".join(versions))


if __name__ == "__main__":
    main()
