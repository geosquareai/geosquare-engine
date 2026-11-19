"""Several databases in one process: each runs dataset functions on its own connections.

    python tests/test_multi_db.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import gc, sys, tempfile
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_multidb_")
def db():
    c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    c.execute(f"LOAD '{ext}'")
    return c
poly = "POLYGON ((106.80 -6.21, 106.81 -6.21, 106.81 -6.20, 106.80 -6.20, 106.80 -6.21))"
a, b = db(), db()
a.execute(f"CREATE TABLE only_in_a AS SELECT packed_id, 1.0::DOUBLE AS v FROM polygon_to_cells('ID', '{poly}', 12)")
b.execute(f"CREATE TABLE only_in_b AS SELECT packed_id, 2.0::DOUBLE AS v FROM polygon_to_cells('ID', '{poly}', 12)")
# each write must see its own database's table
b.execute(f"SELECT * FROM write_cell_dataset('SELECT * FROM only_in_b', '{d}/b', layout := 'iceberg', partition_level := 9)").fetchall()
a.execute(f"SELECT * FROM write_cell_dataset('SELECT * FROM only_in_a', '{d}/a', layout := 'flat')").fetchall()
n = a.execute("SELECT count(*) FROM only_in_a").fetchone()[0]
for c, path, v in ((a, f"{d}/a", 1.0), (b, f"{d}/b", 2.0), (a, f"{d}/b", 2.0), (b, f"{d}/a", 1.0)):
    r = c.execute(f"SELECT count(*), sum(v) FROM query_cell_dataset('{path}', geometry := '{poly}')").fetchone()
    assert r == (n, n * v), (path, r)
# a table name that exists only in the other database is an error, not a silent cross-read
try:
    a.execute(f"SELECT * FROM write_cell_dataset('SELECT * FROM only_in_b', '{d}/x', layout := 'flat')").fetchall()
    raise SystemExit("database a read a table of database b")
except duckdb.Error as e:
    assert "only_in_b" in str(e), e
# closing the first database leaves the second working
a.close(); del a; gc.collect()
r = b.execute(f"SELECT count(*), sum(v) FROM read_cell_dataset('{d}/b')").fetchone()
assert r == (n, n * 2.0), r
c = db()
assert c.execute(f"SELECT count(*) FROM query_cell_dataset('{d}/a', geometry := '{poly}')").fetchone()[0] == n
print("multi-database OK", d)
