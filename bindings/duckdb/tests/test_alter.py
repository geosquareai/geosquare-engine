"""alter_cell_dataset (Iceberg): rename, drop and add columns without rewriting data; data
files are read by field id; later appends, upserts, compaction and time travel keep working.

    python tests/test_alter.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import json, math, sys, tempfile
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_alter_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'; LOAD iceberg")
p = f"{d}/t"
cells = lambda x: f"polygon_to_cells('ID', 'POLYGON (({x} -6.22, {x + 0.03} -6.22, {x + 0.03} -6.19, {x} -6.19, {x} -6.22))', 12, min_coverage := 0.99)"
c.execute(f"CREATE TABLE base AS SELECT packed_id, (packed_id % 97)::DOUBLE AS people, (packed_id % 7)::DOUBLE / 10 AS hazard, (packed_id % 5)::BIGINT AS n FROM {cells(106.80)}")
c.execute("""SELECT * FROM write_cell_dataset('SELECT * FROM base', ?, layout := 'iceberg', partition_level := 7,
  aggregation_rule := '{"people": "sum", "hazard": "mean", "n": "sum"}', units := '{"people": "people"}',
  column_descriptions := '{"people": "People count", "hazard": "Hazard index"}')""", [p]).fetchall()
snap_before = c.execute("SELECT max(snapshot_id) FROM iceberg_snapshots(?)", [p]).fetchone()[0]
before = c.execute("SELECT sum(people), sum(hazard), count(*) FROM read_cell_dataset(?)", [p]).fetchone()
nfiles = c.execute("SELECT count(*) FROM glob(?)", [f"{p}/data/**/*.parquet"]).fetchone()[0]

r = c.execute("""SELECT * FROM alter_cell_dataset(?, rename_columns := '{"people": "pop"}', drop_columns := 'n',
                 add_columns := '{"score": "DOUBLE"}')""", [p]).fetchone()
assert r[2] == "3", r
assert c.execute("SELECT count(*) FROM glob(?)", [f"{p}/data/**/*.parquet"]).fetchone()[0] == nfiles   # no data rewritten
cols = [x[0] for x in c.execute("DESCRIBE SELECT * FROM read_cell_dataset(?)", [p]).fetchall()]
assert cols == ["gid", "packed_id", "x_idx", "y_idx", "pop", "hazard", "score"], cols
assert c.execute("SELECT sum(pop), sum(hazard), count(*) FROM read_cell_dataset(?)", [p]).fetchone() == before
assert c.execute("SELECT min(score), max(score) FROM read_cell_dataset(?)", [p]).fetchone() == (0.0, 0.0)
AREA = "POLYGON ((106.805 -6.215, 106.825 -6.215, 106.825 -6.195, 106.805 -6.195, 106.805 -6.215))"
q = c.execute("SELECT sum(pop * coverage_ratio), sum(score) FROM query_cell_dataset(?, geometry := ?)", [p, AREA]).fetchone()
q0 = c.execute("SELECT sum(people * coverage_ratio) FROM query_cell_dataset(?, geometry := ?, snapshot_id := ?)", [p, AREA, snap_before]).fetchone()
assert math.isclose(q[0], q0[0]) and q[1] == 0, (q, q0)
roll = c.execute("SELECT sum(pop) FROM query_cell_dataset(?, geometry := ?, rollup_level := 9)", [p, AREA]).fetchone()[0]
assert math.isclose(roll, q[0]), (roll, q)
meta = {x[0]: x for x in c.execute("SELECT * FROM cell_dataset_columns(?)", [p]).fetchall()}
assert set(meta) == {"pop", "hazard", "score"} and meta["pop"][3:6] == ("sum", "people", "People count") and meta["hazard"][3] == "mean", meta
try:
    from geosquare_v2.cell_manifest import CellDatasetManifest
    CellDatasetManifest.from_dict(json.loads(c.execute("SELECT manifest FROM cell_dataset_info(?)", [p]).fetchone()[0]))
except ImportError:
    pass
# old snapshot keeps its names
assert [x[0] for x in c.execute("DESCRIBE SELECT * FROM read_cell_dataset(?, snapshot_id := ?)", [p, snap_before]).fetchall()][4:] == ["people", "hazard", "n"]
# append with the new names, upsert, stats, validate, compact
c.execute(f"CREATE TABLE more AS SELECT packed_id, 1.0 AS pop, 0.5 AS hazard, 2.0 AS score FROM {cells(106.84)}")
c.execute("SELECT * FROM write_cell_dataset('SELECT m.* FROM more m ANTI JOIN base USING (packed_id)', ?, layout := 'iceberg', mode := 'append')", [p]).fetchall()
assert c.execute("SELECT count(*), sum(score) > 0 FROM read_cell_dataset(?)", [p]).fetchone()[1]
up = c.execute("SELECT * FROM write_cell_dataset('SELECT packed_id, 12345.0 AS pop, hazard, 1.0 AS score FROM base LIMIT 50', ?, layout := 'iceberg', mode := 'upsert')", [p]).fetchone()
assert up[0] == "iceberg (upsert)", up
t = c.execute("SELECT count(*), count(DISTINCT packed_id), count(*) FILTER (WHERE pop = 12345.0) FROM read_cell_dataset(?)", [p]).fetchone()
assert t[0] == t[1] and t[2] == 50, t
assert all(ok for _, ok, _ in c.execute("SELECT * FROM validate_cell_dataset(?)", [p]).fetchall())
assert {x[0] for x in c.execute("SELECT * FROM cell_dataset_stats(?, exact := true)", [p]).fetchall()} == {"pop", "hazard", "score"}
before_compact = c.execute("SELECT count(*), sum(pop), sum(score) FROM read_cell_dataset(?)", [p]).fetchone()
c.execute("SELECT * FROM compact_cell_dataset(?)", [p]).fetchall()
assert c.execute("SELECT count(*), sum(pop), sum(score) FROM read_cell_dataset(?)", [p]).fetchone() == before_compact
# refusals
for args, msg in (("drop_columns := 'gid'", "cannot be dropped"), ("""rename_columns := '{"pop": "hazard"}'""", "already exists"),
                  ("drop_columns := 'nope'", "no column"), ("""add_columns := '{"x": "VARCHAR"}'""", "not supported")):
    try:
        c.execute(f"SELECT * FROM alter_cell_dataset(?, {args})", [p]).fetchall(); raise AssertionError(args)
    except duckdb.Error as e:
        assert msg in str(e), (args, e)
c.execute("SELECT * FROM write_cell_dataset('SELECT * FROM base', ?, layout := 'flat')", [f"{d}/flat"]).fetchall()
try:
    c.execute("SELECT * FROM alter_cell_dataset(?, drop_columns := 'n')", [f"{d}/flat"]).fetchall(); raise AssertionError("flat")
except duckdb.Error as e:
    assert "Iceberg" in str(e), e
print("alter OK", d)
