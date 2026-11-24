"""snapshot_id / as_of: read_cell_dataset, query_cell_dataset and query_cell_datasets read an
older Iceberg snapshot, the same rows as DuckDB's iceberg_scan at that snapshot.

    python tests/test_time_travel.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import sys, tempfile, time
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_tt_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'; LOAD iceberg")
p = f"{d}/t"
cells = lambda x: f"polygon_to_cells(''ID'', ''POLYGON (({x} -6.22, {x + 0.02} -6.22, {x + 0.02} -6.20, {x} -6.20, {x} -6.22))'', 12, min_coverage := 0.99)"
steps = [
    f"SELECT * FROM write_cell_dataset('SELECT packed_id, 1.0 AS v FROM {cells(106.80)}', '{p}', layout := 'iceberg', partition_level := 7)",
    f"SELECT * FROM write_cell_dataset('SELECT packed_id, 2.0 AS v FROM {cells(106.83)}', '{p}', layout := 'iceberg', mode := 'append')",
    f"SELECT * FROM write_cell_dataset('SELECT packed_id, v * 10 AS v FROM read_cell_dataset(''{p}'')', '{p}', layout := 'iceberg', mode := 'overwrite')",
    f"SELECT * FROM write_cell_dataset('SELECT packed_id, v, v + 1 AS w FROM read_cell_dataset(''{p}'')', '{p}', layout := 'iceberg', mode := 'overwrite')",
]
stamps = []
for s in steps:
    c.execute(s).fetchall()
    time.sleep(0.01)
    stamps.append(int(time.time() * 1000))
    time.sleep(0.01)
snaps = [r[0] for r in c.execute("SELECT snapshot_id FROM iceberg_snapshots(?) ORDER BY sequence_number", [p]).fetchall()]
assert len(snaps) == 4, snaps
AREA = "POLYGON ((106.81 -6.215, 106.84 -6.215, 106.84 -6.205, 106.81 -6.205, 106.81 -6.215))"
for i, sid in enumerate(snaps):
    want = c.execute("SELECT count(*), sum(v) FROM iceberg_scan(?, snapshot_from_id = ?)", [p, sid]).fetchone()
    got = c.execute("SELECT count(*), sum(v) FROM read_cell_dataset(?, snapshot_id := ?)", [p, sid]).fetchone()
    assert got == want, (i, got, want)
    asof = c.execute("SELECT count(*), sum(v) FROM read_cell_dataset(?, as_of := ?)", [p, str(stamps[i])]).fetchone()
    assert asof == want, (i, asof, want)
    q = c.execute("SELECT count(*), sum(v) FROM query_cell_dataset(?, geometry := ?, snapshot_id := ?)", [p, AREA, sid]).fetchone()
    qw = c.execute("""SELECT count(*), sum(v) FROM iceberg_scan(?, snapshot_from_id = ?) t
                      SEMI JOIN polygon_to_cells('ID', ?, 12) k USING (packed_id)""", [p, sid, AREA]).fetchone()
    assert q == qw, (i, q, qw)
    cols = [r[0] for r in c.execute("DESCRIBE SELECT * FROM query_cell_dataset(?, geometry := ?, snapshot_id := ?)", [p, AREA, sid]).fetchall()]
    assert ("w" in cols) == (i == 3), (i, cols)
# current by default; a timestamp text works too
assert c.execute("SELECT sum(v) FROM read_cell_dataset(?)", [p]).fetchone() == c.execute("SELECT sum(v) FROM iceberg_scan(?)", [p]).fetchone()
ts = c.execute("SELECT make_timestamptz(?::BIGINT * 1000)::VARCHAR", [stamps[1]]).fetchone()[0]
assert c.execute("SELECT count(*) FROM read_cell_dataset(?, as_of := ?)", [p, ts]).fetchone() == c.execute("SELECT count(*) FROM iceberg_scan(?, snapshot_from_id = ?)", [p, snaps[1]]).fetchone()
cols = [r[0] for r in c.execute("DESCRIBE SELECT * FROM query_cell_datasets([?, ?], geometry := ?, snapshot_id := ?)", [p, p, AREA, snaps[0]]).fetchall()]
assert cols == ["packed_id", "gid", "t_1__v", "t_2__v", "coverage_ratio"], cols
r = c.execute("SELECT sum(t_1__v), sum(v) FROM query_cell_dataset(?, geometry := ?, snapshot_id := ?, rollup_level := 9) "
              "JOIN query_cell_datasets([?, ?], geometry := ?, snapshot_id := ?, rollup_level := 9) USING (gid)", [p, AREA, snaps[0], p, p, AREA, snaps[0]]).fetchone()
assert r[0] == r[1] and r[0] > 0, r
for bad, msg in (("snapshot_id := 123", "not in the table"), ("as_of := '2001-01-01'", "no snapshot at or before")):
    try:
        c.execute(f"SELECT * FROM read_cell_dataset(?, {bad})", [p]).fetchall(); raise AssertionError(bad)
    except duckdb.Error as e:
        assert msg in str(e), (bad, e)
print("time travel OK")
