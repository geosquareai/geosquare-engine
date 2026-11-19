"""Iceberg column metrics (column_metrics := 'key' | 'all') and deflate manifests, read by pyiceberg.

    python tests/test_iceberg_metrics.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import glob, sys, tempfile
import duckdb
from pyiceberg.table import StaticTable

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_metrics_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
q = ("SELECT packed_id, 1.0::DOUBLE AS a, 2.0::DOUBLE AS b, 3::BIGINT AS c FROM polygon_to_cells('ID', "
     "'POLYGON ((106.80 -6.25, 106.85 -6.25, 106.85 -6.20, 106.80 -6.20, 106.80 -6.25))', 12)")
key = {"gid", "packed_id", "x_idx", "y_idx"}
for mode, want in [("key", key), ("all", key | {"a", "b", "c"}), ("auto", key | {"a", "b", "c"})]:
    p = f"{d}/{mode}"
    c.execute(f"SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', partition_level := 7, column_metrics := '{mode}')", [q, p])
    meta = sorted(glob.glob(f"{p}/metadata/*.metadata.json"))[-1]
    assert open(sorted(glob.glob(f"{p}/metadata/*-m0.avro"))[-1], "rb").read().find(b"deflate") > 0
    t = StaticTable.from_metadata(meta)
    names = {f.field_id: f.name for f in t.schema().fields}
    for task in t.scan().plan_files():
        got = {names[i] for i in task.file.lower_bounds}
        assert got == want, (mode, got)
    n = c.execute("SELECT count(*) FROM read_cell_dataset(?)", [p]).fetchone()[0]
    assert n == t.scan().to_arrow().num_rows == 12396, n
try:
    c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', column_metrics := 'x')", [q, f"{d}/bad"])
    raise SystemExit("bad column_metrics accepted")
except duckdb.InvalidInputException:
    pass
print("iceberg metrics OK", d)
