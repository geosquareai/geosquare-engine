"""The engine's content hash (docs/dataset-metadata.md, section 3.1) matches the Python library.

tests/data/v2_1_1/ID-L12 was written by the Python library (manifest 1.1). Its manifest records the
content hash that Python computed. The engine must reproduce it from the same file. A copy with
one changed value must not match.

    python tests/test_content_hash.py build/linux_amd64/geosquare_v2.duckdb_extension
"""

import shutil
import sys
import tempfile
from pathlib import Path

import duckdb

EXT = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux_amd64/geosquare_v2.duckdb_extension").resolve()
DATASET = Path(__file__).resolve().parent / "data" / "v2_1_1" / "ID-L12"


def check(cond, msg):
    if not cond:
        raise AssertionError(msg)


con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute(f"LOAD '{EXT}'")

rows = con.execute(
    "SELECT file, row_count, content_sha256, manifest_content_sha256 FROM cell_dataset_content_hashes(?)",
    [str(DATASET)],
).fetchall()
check(len(rows) == 1, f"expected one data file, got {rows}")
file, row_count, engine_hash, recorded = rows[0]
check(row_count == 60, f"row count {row_count}")
check(recorded is not None, "the manifest should record the hash")
check(engine_hash == recorded, f"engine {engine_hash} != Python {recorded}")
print("ok: engine hash matches the Python-written manifest")

# A copy whose depth_m value changed must not match the recorded hash.
with tempfile.TemporaryDirectory() as tmp:
    copy = Path(tmp) / "ID-L12"
    shutil.copytree(DATASET, copy)
    data_file = copy / "data" / "part-00000.parquet"
    changed = Path(tmp) / "changed.parquet"
    con.execute(
        f"COPY (SELECT gid, population, CASE WHEN gid = (SELECT min(gid) FROM read_parquet('{data_file}')) "
        f"THEN 999.5 ELSE depth_m END AS depth_m, zone FROM read_parquet('{data_file}')) "
        f"TO '{changed}' (FORMAT PARQUET)"
    )
    shutil.move(str(changed), str(data_file))
    tampered = con.execute(
        "SELECT content_sha256, manifest_content_sha256 FROM cell_dataset_content_hashes(?)", [str(copy)]
    ).fetchone()
    check(tampered[0] != tampered[1], "a changed value must change the hash")
    print("ok: a changed value is detected")

# A value field whose declared type the engine does not hash is refused, with a clear message.
try:
    con.execute("SELECT * FROM cell_dataset_content_hashes(?)", [str(Path(__file__).resolve().parent)]).fetchall()
    raise AssertionError("a folder without a manifest should be refused")
except duckdb.Error as e:
    check("manifest" in str(e).lower(), f"unclear error: {e}")
    print("ok: a folder without a manifest is refused")

print("content hash tests passed")
