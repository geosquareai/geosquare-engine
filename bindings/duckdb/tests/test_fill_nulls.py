"""write_cell_dataset fills NULL with 0 in numeric value columns by default (fill_nulls := false keeps NULL).

    python tests/test_fill_nulls.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import sys, tempfile
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_fill_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
q = ("SELECT packed_id, CASE WHEN packed_id % 2 = 0 THEN NULL ELSE 1.5 END::DOUBLE AS a, "
     "CASE WHEN packed_id % 3 = 0 THEN NULL ELSE 7 END::BIGINT AS b, "
     "CASE WHEN packed_id % 5 = 0 THEN NULL ELSE 2.25 END::DECIMAL(10,2) AS c, "
     "CASE WHEN packed_id % 2 = 0 THEN NULL ELSE 'x' END AS label "
     "FROM polygon_to_cells('ID', 'POLYGON ((106.80 -6.25, 106.82 -6.25, 106.82 -6.23, 106.80 -6.23, 106.80 -6.25))', 12)")
want = c.execute(f"SELECT count(*), count(*) - count(a), count(*) - count(b), count(*) - count(c), count(*) - count(label) FROM ({q})").fetchone()
assert min(want[1:4]) > 0, want
for layout in ("flat", "partitioned", "iceberg"):
    for fill in (None, False):
        p = f"{d}/{layout}_{fill}"
        opt = "" if fill is None else ", fill_nulls := false"
        c.execute(f"SELECT * FROM write_cell_dataset(?, ?, layout := '{layout}', partition_level := 7{opt})", [q, p])
        r = c.execute(f"SELECT count(*), count(*) - count(a), count(*) - count(b), count(*) - count(c), count(*) - count(label), "
                      f"sum(a), sum(b), sum(c) FROM read_cell_dataset(?)", [p]).fetchone()
        types = dict(c.execute(f"SELECT column_name, column_type FROM (DESCRIBE SELECT * FROM read_cell_dataset(?))", [p]).fetchall())
        assert (types["a"], types["b"], types["c"]) == ("DOUBLE", "BIGINT", "DECIMAL(10,2)"), types
        if fill is None:
            assert r[:5] == (want[0], 0, 0, 0, want[4]), (layout, r)   # text column untouched
        else:
            assert r[:5] == want, (layout, r)
print("fill_nulls OK", d)
