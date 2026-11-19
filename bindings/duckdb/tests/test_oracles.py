"""Cross-check datasets written by the extension with independent readers.

Needs geosquare_v2 (contract validator), pyiceberg and pyarrow besides duckdb:

    python tests/test_oracles.py <extension> <geosquare-v2 checkout> [pyiceberg table dir to append to]
"""

import base64
import json
import shutil
import sys
import tempfile
from pathlib import Path

import duckdb
import pyarrow.parquet as pq
from geosquare_v2 import CellDataset, CellDatasetManifest, DbRegistryLoader, read_cell_dataset
from pyiceberg.table import StaticTable

EXT, V2 = Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve()
PYICE = Path(sys.argv[3]).resolve() if len(sys.argv) > 3 else None


def registry():
    trust = {k: base64.b64decode(v, validate=True) for k, v in json.loads((V2 / "release/registry-trust.json").read_text()).items()}
    boundary = V2 / "src/geosquare_v2/data/registry"
    return DbRegistryLoader(trust, V2 / "src/geosquare_v2/db", boundary_root=boundary, verify_boundaries=True).load()


def main():
    reg = registry()
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{EXT}'")
    tmp = Path(tempfile.mkdtemp(prefix="gs_oracle_"))
    src = ("SELECT packed_id, (packed_id % 97)::UTINYINT AS population, (packed_id % 1000) / 10.0 AS score "
           "FROM polygon_to_cells('ID', 'POLYGON ((106.8 -6.25, 106.9 -6.25, 106.9 -6.15, 106.8 -6.15, 106.8 -6.25))', 12)")
    n = con.execute(f"SELECT count(*) FROM ({src})").fetchone()[0]

    # 1. flat folder: the geosquare-v2 reader validates manifest, schema and every row.
    con.execute("SELECT * FROM write_cell_dataset(?, ?, value_semantics := 'COUNT', aggregation_rule := 'sum')", [src, str(tmp / "flat")]).fetchall()
    ds = read_cell_dataset(tmp / "flat", registry=reg)
    assert ds.num_rows == n, ds.num_rows
    print(f"geosquare-v2 read_cell_dataset(flat): OK, {ds.num_rows} rows, value fields {ds.manifest.value_field_names}")

    # 2. partitioned folder: each partition validated with the v2 rules (row_count set per file).
    con.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'partitioned', partition_level := 7)", [src, str(tmp / "part")]).fetchall()
    sidecar = CellDatasetManifest.from_file(tmp / "part" / "manifest.json", registry=reg)
    total = 0
    for f in sorted((tmp / "part" / "data").glob("*/*.parquet")):
        t = pq.ParquetFile(f).read()
        emb = json.loads(t.schema.metadata[b"geosquare.manifest"])
        assert CellDatasetManifest.from_dict(emb, registry=reg).to_json() == sidecar.to_json(), f"embedded manifest differs in {f}"
        part = sidecar.to_dict()
        part["storage"]["row_count"] = part["storage"]["unique_cell_count"] = t.num_rows
        CellDataset.from_table(t, part, registry=reg)
        prefix = f.parent.name.split("=", 1)[1]
        assert all(g.startswith(prefix) for g in t["gid"].to_pylist())
        total += t.num_rows
    assert total == n
    print(f"geosquare-v2 rules per partition: OK, {len(list((tmp / 'part' / 'data').iterdir()))} partitions, {total} rows")

    # 3. iceberg: pyiceberg reads the newest metadata, with and without a filter.
    con.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', partition_level := 7)", [src, str(tmp / "ice")]).fetchall()
    latest = sorted((tmp / "ice" / "metadata").glob("*.metadata.json"))[-1]
    table = StaticTable.from_metadata(str(latest))
    arrow = table.scan().to_arrow()
    assert arrow.num_rows == n, arrow.num_rows
    lo, hi = con.execute(f"SELECT min(packed_id), max(packed_id) FROM ({src}) WHERE packed_id % 5 = 0").fetchone()
    from pyiceberg.expressions import And, GreaterThanOrEqual, LessThanOrEqual
    files = list(table.scan(row_filter=And(GreaterThanOrEqual("packed_id", lo), LessThanOrEqual("packed_id", lo))).plan_files())
    assert len(files) == 1, f"partition pruning: {len(files)} files"
    print(f"pyiceberg: OK, {arrow.num_rows} rows, schema {[f.name for f in table.schema().fields]}, point lookup plans {len(files)} file")

    # 4. append to a table written by pyiceberg (deflate-compressed manifests).
    if PYICE:
        copy = tmp / "pyice"
        shutil.copytree(PYICE, copy)
        meta = sorted((copy / "metadata").glob("*.metadata.json"))[-1]
        txt = meta.read_text().replace(str(PYICE), str(copy))
        meta.write_text(txt)
        before = con.execute(f"SELECT count(*) FROM read_cell_dataset('{copy}', domain_code := 'ID', level := 12)").fetchone()[0]
        extra = ("SELECT packed_id, 1::INTEGER AS value, 1::INTEGER AS n_v1 FROM polygon_to_cells('ID', "
                 "'POLYGON ((110.30 -7.05, 110.35 -7.05, 110.35 -7.00, 110.30 -7.00, 110.30 -7.05))', 12)")
        m = con.execute(f"SELECT count(*) FROM ({extra})").fetchone()[0]
        con.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'append')", [extra, str(copy)]).fetchall()
        after_duck = con.execute(f"SELECT count(*) FROM read_cell_dataset('{copy}')").fetchone()[0]
        newest = max((copy / "metadata").glob("*.metadata.json"), key=lambda p: json.loads(p.read_text())["last-updated-ms"])
        after_py = StaticTable.from_metadata(str(newest)).scan().to_arrow().num_rows
        assert after_duck == after_py == before + m, (before, m, after_duck, after_py)
        print(f"append to pyiceberg table: OK, {before} + {m} = {after_py} rows (duckdb and pyiceberg agree)")
    print(f"oracles OK in {tmp}")


if __name__ == "__main__":
    main()
