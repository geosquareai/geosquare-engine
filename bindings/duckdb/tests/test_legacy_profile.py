"""Profile 2.1.0 datasets (geosquare-v2 1.0.0) read back with the GIDs they were written with.

tests/data/legacy_v1_0_0/ holds synthetic datasets that geosquare-v2 1.0.0 wrote under PROJ 9.5.1,
at levels 12 and 14. points.json keeps each sample point with the GID 1.0.0 gave it. A point query
on a legacy dataset must return exactly the stored cells. VN is the domain where profile 2.2.0 and
profile 2.1.0 differ, so it is the case that would fail if the legacy datum rule were dropped.

    python tests/test_legacy_profile.py build/linux_amd64/geosquare_v2.duckdb_extension
"""

import json
import shutil
import sys
import tempfile
from pathlib import Path

import duckdb

EXT = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux_amd64/geosquare_v2.duckdb_extension").resolve()
DATA = Path(__file__).resolve().parent / "data" / "legacy_v1_0_0"
POINTS = json.loads((DATA / "points.json").read_text())


def check(cond, msg):
    if not cond:
        raise AssertionError(msg)


con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute(f"LOAD '{EXT}'")

for code in ("ID", "LA", "VN"):
    for level in (12, 14):
        path = DATA / f"{code}-L{level}"
        rows = con.execute(f"SELECT count(*) FROM read_cell_dataset('{path}')").fetchone()[0]
        check(rows > 0, f"{code} L{level}: read_cell_dataset returned no rows")

        # Every sample point must come back on exactly the cells that 1.0.0 stored for it.
        key = f"gid_l{level}"
        multipoint = ", ".join(f"({p['lon']} {p['lat']})" for p in POINTS[code])
        got = {
            r[0]
            for r in con.execute(
                "SELECT gid FROM query_cell_dataset(?, geometry := ?, allow_candidate_crs := 1)",
                [str(path), f"MULTIPOINT ({multipoint})"],
            ).fetchall()
        }
        want = {p[key] for p in POINTS[code]}
        check(got == want, f"{code} L{level}: query gave {len(got)} cells, 1.0.0 stored {len(want)}; "
                           f"missing {sorted(want - got)[:3]}, extra {sorted(got - want)[:3]}")
        print(f"ok {code} L{level}: {len(want)} stored cells returned")

# A profile this engine does not know must still be refused, with a clear message.
with tempfile.TemporaryDirectory() as tmp:
    bad = Path(tmp) / "ID-L12"
    shutil.copytree(DATA / "ID-L12", bad)
    manifest = json.loads((bad / "manifest.json").read_text())
    manifest["profile_version"] = "9.9.9-unknown"
    (bad / "manifest.json").write_text(json.dumps(manifest))
    try:
        con.execute(f"SELECT count(*) FROM read_cell_dataset('{bad}')").fetchall()
    except duckdb.Error as e:
        check("not supported" in str(e) and "9.9.9-unknown" in str(e), f"unclear error: {e}")
        print("ok unknown profile refused")
    else:
        raise AssertionError("a dataset with an unknown profile_version was accepted")

print("legacy profile tests passed")
