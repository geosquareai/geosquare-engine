"""expire_snapshots: old snapshots and the files only they used are deleted, the current data
is unchanged and still readable by DuckDB's iceberg reader; orphans younger than
orphan_age_hours stay; dry_run changes nothing.

    python tests/test_expire.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import glob, os, sys, tempfile, time
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_expire_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'; LOAD iceberg")
p = f"{d}/t"
area = lambda x: f"polygon_to_cells(''ID'', ''POLYGON (({x} -6.22, {x + 0.02} -6.22, {x + 0.02} -6.20, {x} -6.20, {x} -6.22))'', 12)"
c.execute(f"SELECT * FROM write_cell_dataset('SELECT packed_id, 1.0 AS v FROM {area(106.80)}', ?, layout := 'iceberg', partition_level := 7)", [p]).fetchall()
c.execute(f"SELECT * FROM write_cell_dataset('SELECT packed_id, 2.0 AS v FROM {area(106.83)}', ?, layout := 'iceberg', mode := 'append')", [p]).fetchall()
for k in (3.0, 4.0):   # two full rewrites: their old files become garbage
    c.execute(f"SELECT * FROM write_cell_dataset('SELECT packed_id, v * {k} AS v FROM read_cell_dataset(''{p}'')', ?, layout := 'iceberg', mode := 'overwrite')", [p]).fetchall()
files = lambda: sorted(f for f in glob.glob(f"{p}/**/*", recursive=True) if os.path.isfile(f))
before = c.execute("SELECT count(*), sum(v) FROM read_cell_dataset(?)", [p]).fetchone()
nsnap = c.execute("SELECT count(*) FROM iceberg_snapshots(?)", [p]).fetchone()[0]
assert nsnap == 4, nsnap
n0 = len(files())
# an orphan from a "running" write (young) and an old one
young, old = f"{p}/data/young_orphan.parquet", f"{p}/data/old_orphan.parquet"
for f in (young, old):
    open(f, "wb").write(b"x")
os.utime(old, (time.time() - 3 * 86400, time.time() - 3 * 86400))

num = lambda r: tuple(int(x) for x in r[:5]) + tuple(r[5:])
r = num(c.execute("SELECT * FROM expire_snapshots(?, dry_run := true)", [p]).fetchone())
assert r[0] == 1 and r[1] == 3 and r[6] == "true" and len(files()) == n0 + 2, r
r = num(c.execute("SELECT * FROM expire_snapshots(?)", [p]).fetchone())
kept, expired, deleted, nbytes, orphans_kept = r[:5]
assert (kept, expired, orphans_kept) == (1, 3, 1) and deleted > 0 and nbytes > 0, r
after = c.execute("SELECT count(*), sum(v) FROM read_cell_dataset(?)", [p]).fetchone()
assert after == before, (before, after)
assert c.execute("SELECT count(*), sum(v) FROM iceberg_scan(?)", [p]).fetchone() == before
assert c.execute("SELECT count(*) FROM iceberg_snapshots(?)", [p]).fetchone()[0] == 1
left = files()
assert young in left and old not in left, left
assert len([f for f in left if f.endswith(".metadata.json")]) == 1, left
# every remaining data file is used: a second run deletes nothing but the old orphan rule
r2 = num(c.execute("SELECT * FROM expire_snapshots(?, orphan_age_hours := 0)", [p]).fetchone())
assert r2[1] == 0 and r2[2] == 2, r2          # the young orphan (now allowed) + the superseded metadata file
assert c.execute("SELECT count(*), sum(v) FROM read_cell_dataset(?)", [p]).fetchone() == before
# keep the last two snapshots
for k in (5.0, 6.0):
    c.execute(f"SELECT * FROM write_cell_dataset('SELECT packed_id, v + {k} AS v FROM read_cell_dataset(''{p}'')', ?, layout := 'iceberg', mode := 'overwrite')", [p]).fetchall()
r3 = num(c.execute("SELECT * FROM expire_snapshots(?, retain_last := 2)", [p]).fetchone())
assert r3[:2] == (2, 1), r3
snaps = c.execute("SELECT snapshot_id FROM iceberg_snapshots(?) ORDER BY sequence_number", [p]).fetchall()
assert len(snaps) == 2
assert c.execute("SELECT count(*) FROM iceberg_scan(?, snapshot_from_id := ?)", [p, snaps[0][0]]).fetchone()[0] == before[0]
for bad, msg in (("retain_last := 0", "retain_last"), ("older_than := 'yesterday-ish'", "")):
    try:
        c.execute(f"SELECT * FROM expire_snapshots(?, {bad})", [p]).fetchall()
        raise AssertionError(bad)
    except duckdb.Error as e:
        assert msg in str(e), (bad, e)
c.execute("SELECT * FROM write_cell_dataset('SELECT packed_id, 1.0 AS v FROM polygon_to_cells(''ID'', ''POLYGON ((106.80 -6.22, 106.81 -6.22, 106.81 -6.21, 106.80 -6.21, 106.80 -6.22))'', 12)', ?, layout := 'flat')", [f"{d}/flat"]).fetchall()
try:
    c.execute("SELECT * FROM expire_snapshots(?)", [f"{d}/flat"]).fetchall(); raise AssertionError("flat")
except duckdb.Error as e:
    assert "Iceberg" in str(e), e
print("expire_snapshots OK", d)
