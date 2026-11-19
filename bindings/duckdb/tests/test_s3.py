"""Writes to S3 (flat, partitioned, Iceberg with append and overwrite) against an S3 endpoint.

Use an emulator, never a real bucket you care about:

    moto_server -H 127.0.0.1 -p 5555 &          # pip install "moto[server]"
    S3_TEST_ENDPOINT=127.0.0.1:5555 S3_TEST_BUCKET=geo-test \\
    AWS_ACCESS_KEY_ID=testing AWS_SECRET_ACCESS_KEY=testing \\
    python tests/test_s3.py build/linux_amd64/geosquare_v2.duckdb_extension

DuckDB uses its S3 secret for Parquet; the extension's native client takes the endpoint,
region and URL style from that secret and the keys from the AWS environment.
"""

import json
import os
import sys
import uuid
from pathlib import Path

import duckdb

EXT = Path(sys.argv[1]).resolve()
ENDPOINT = os.environ.get("S3_TEST_ENDPOINT", "127.0.0.1:5555")
BUCKET = os.environ.get("S3_TEST_BUCKET", "geo-test")
ROOT = f"s3://{BUCKET}/gs-test-{uuid.uuid4().hex[:8]}"

AREA_A = "POLYGON ((106.80 -6.25, 106.84 -6.25, 106.84 -6.21, 106.80 -6.21, 106.80 -6.25))"
AREA_B = "POLYGON ((106.90 -6.25, 106.93 -6.25, 106.93 -6.22, 106.90 -6.22, 106.90 -6.25))"


def src(area, extra=""):
    return f"SELECT packed_id, (packed_id % 97)::INTEGER AS population{extra} FROM polygon_to_cells('ID', '{area}', 12)"


def main():
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{EXT}'")
    con.execute("INSTALL httpfs; LOAD httpfs; INSTALL iceberg; LOAD iceberg")
    con.execute(f"""CREATE SECRET gs_test (TYPE s3, KEY_ID '{os.environ['AWS_ACCESS_KEY_ID']}', SECRET '{os.environ['AWS_SECRET_ACCESS_KEY']}',
                    REGION 'us-east-1', ENDPOINT '{ENDPOINT}', URL_STYLE 'path', USE_SSL false)""")
    q = lambda sql, p=None: con.execute(sql, p or []).fetchall()
    n_a = q(f"SELECT count(*) FROM ({src(AREA_A)})")[0][0]
    n_b = q(f"SELECT count(*) FROM ({src(AREA_B)})")[0][0]
    out = {}

    # flat and partitioned straight to S3, then overwrite (old objects removed)
    for layout, extra in (("flat", ""), ("partitioned", ", partition_level := 9")):
        path = f"{ROOT}/{layout}"
        r = q(f"SELECT * FROM write_cell_dataset(?, ?, layout := '{layout}'{extra})", [src(AREA_A), path])
        assert r[0][3] == str(n_a), r
        assert q(f"SELECT count(*) FROM read_cell_dataset('{path}')")[0][0] == n_a
        r = q(f"SELECT * FROM write_cell_dataset(?, ?, layout := '{layout}', mode := 'overwrite'{extra})", [src(AREA_B), path])
        files = q(f"SELECT count(*) FROM glob('{path}/data/**')")[0][0]
        got = q(f"SELECT count(*), sum(population) FROM read_cell_dataset('{path}')")[0]
        want = q(f"SELECT count(*), sum(population) FROM ({src(AREA_B)})")[0]
        assert got == want, (layout, got, want)
        out[layout] = f"{n_a} -> overwrite {n_b} rows, {files} data files"

    # Iceberg on S3: create, append, overwrite with a new column and new partitioning
    ice = f"{ROOT}/iceberg"
    r1 = q("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', partition_level := 7)", [src(AREA_A), ice])
    q("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'append')", [src(AREA_B), ice])
    assert q(f"SELECT count(*) FROM read_cell_dataset('{ice}')")[0][0] == n_a + n_b
    assert q(f"SELECT count(*) FROM iceberg_scan('{ice}')")[0][0] == n_a + n_b  # version-hint on S3
    try:
        q("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'append')", [src(AREA_A), ice])
        raise AssertionError("duplicate append accepted")
    except duckdb.Error as e:
        assert "already in the table" in str(e)
    r3 = q("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'overwrite', partition_level := 9)",
           [src(AREA_B, ", (packed_id % 7)::DOUBLE AS score"), ice])
    assert r3[0][0] == "iceberg (overwrite)", r3
    got = q(f"SELECT count(*), sum(population), sum(score) FROM read_cell_dataset('{ice}')")[0]
    assert got[0] == n_b and got[2] is not None, got
    meta = json.loads(q(f"SELECT content FROM read_text('{r3[0][1]}')")[0][0])
    snaps = meta["snapshots"]
    assert [s["summary"]["operation"] for s in snaps] == ["append", "append", "overwrite"], snaps
    assert len(meta["schemas"]) == 2 and len(meta["partition-specs"]) == 2, (len(meta["schemas"]), len(meta["partition-specs"]))
    # time travel: the first metadata file still reads the first snapshot
    assert q(f"SELECT count(*) FROM read_cell_dataset('{r1[0][1]}')")[0][0] == n_a
    out["iceberg"] = f"create {n_a}, append {n_b}, overwrite -> {n_b} rows with new column 'score', 3 snapshots, 2 schemas, 2 specs"
    for k, v in out.items():
        print(f"{k:12s} {v}")
    print(f"S3 OK ({ROOT})")


if __name__ == "__main__":
    main()
