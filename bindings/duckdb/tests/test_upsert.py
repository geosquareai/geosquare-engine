"""mode := 'upsert' (Iceberg): only the partitions the input touches are rewritten; staged rows
win, other cells of those partitions are kept, untouched partitions keep their files.

    python tests/test_upsert.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import glob, sys, tempfile
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_upsert_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'; LOAD iceberg")
p = f"{d}/t"
area = lambda x0, x1: f"polygon_to_cells('ID', 'POLYGON (({x0} -6.25, {x1} -6.25, {x1} -6.14, {x0} -6.14, {x0} -6.25))', 12, min_coverage := 0.99, max_candidate_limit := 1000000)"
c.execute(f"CREATE TABLE base AS SELECT packed_id, (packed_id % 97)::DOUBLE AS people, (packed_id % 7)::DOUBLE AS hazard FROM {area(106.70, 106.95)}")
c.execute("SELECT * FROM write_cell_dataset('SELECT * FROM base', ?, layout := 'iceberg', partition_level := 7, units := '{\"people\": \"people\"}')", [p]).fetchall()
parts = c.execute("SELECT DISTINCT left(gid, 7) FROM read_cell_dataset(?) ORDER BY 1", [p]).fetchall()
assert len(parts) >= 3, parts
files_before = set(glob.glob(f"{p}/data/**/*.parquet", recursive=True))
snap0 = c.execute("SELECT max(snapshot_id) FROM iceberg_snapshots(?)", [p]).fetchone()[0]
# changes: update half the cells of one 10 km partition, add cells east of the table (new partition too)
target = parts[1][0]
c.execute(f"""CREATE TABLE changes AS
  SELECT packed_id, 1000.0 AS people, -1.0 AS hazard FROM read_cell_dataset('{p}') WHERE left(gid, 7) = '{target}' AND packed_id % 2 = 0
  UNION ALL SELECT packed_id, 5.0, 5.0 FROM {area(106.95, 107.05)}""")
r = c.execute("SELECT * FROM write_cell_dataset('SELECT * FROM changes', ?, layout := 'iceberg', mode := 'upsert')", [p]).fetchone()
assert r[0] == "iceberg (upsert)", r
want = c.execute("""SELECT count(*), sum(people), sum(hazard) FROM (
    SELECT coalesce(ch.packed_id, b.packed_id) packed_id, coalesce(ch.people, b.people) people, coalesce(ch.hazard, b.hazard) hazard
    FROM base b FULL JOIN changes ch USING (packed_id))""").fetchone()
got = c.execute("SELECT count(*), sum(people), sum(hazard) FROM read_cell_dataset(?)", [p]).fetchone()
ice = c.execute("SELECT count(*), sum(people), sum(hazard) FROM iceberg_scan(?)", [p]).fetchone()
assert got == want == ice, (got, want, ice)
assert c.execute("SELECT count(*) - count(DISTINCT packed_id) FROM read_cell_dataset(?)", [p]).fetchone()[0] == 0
# only the touched partitions got new files; the others are the same files
files_after = set(glob.glob(f"{p}/data/**/*.parquet", recursive=True))
live = {r[0].replace("file://", "") for r in c.execute("SELECT file_path FROM iceberg_metadata(?) WHERE status <> 'DELETED' AND manifest_content = 'DATA'", [p]).fetchall()}
kept_old = {f for f in files_before if f in live}
touched = c.execute("SELECT count(DISTINCT left(packed_to_gid(c.packed_id), 7)) FROM changes c WHERE left(packed_to_gid(c.packed_id), 7) IN (SELECT DISTINCT left(packed_to_gid(packed_id), 7) FROM base)").fetchone()[0]
assert 1 <= touched < len(files_before) and len(kept_old) == len(files_before) - touched, (len(kept_old), len(files_before), touched)
import json, os
hint = open(f"{p}/metadata/version-hint.text").read().strip()
meta = json.load(open(f"{p}/metadata/{hint}.metadata.json"))
snap = next(x for x in meta["snapshots"] if x["snapshot-id"] == meta["current-snapshot-id"])
assert int(snap["summary"]["total-records"]) == got[0] and snap["summary"]["operation"] == "overwrite", snap["summary"]
assert c.execute("SELECT row_count FROM cell_dataset_info(?)", [p]).fetchone()[0] == got[0]
assert all(ok for _, ok, _ in c.execute("SELECT * FROM validate_cell_dataset(?)", [p]).fetchall())
assert c.execute("SELECT count(*), sum(people) FROM read_cell_dataset(?, snapshot_id := ?)", [p, snap0]).fetchone() == c.execute("SELECT count(*), sum(people) FROM base").fetchone()
assert c.execute("SELECT unit FROM cell_dataset_columns(?) WHERE \"column\" = 'people'", [p]).fetchone()[0] == "people"
# a second upsert on the same partition, then compact and expire still work
c.execute("SELECT * FROM write_cell_dataset('SELECT packed_id, 1.0 AS people, 1.0 AS hazard FROM changes LIMIT 10', ?, layout := 'iceberg', mode := 'upsert')", [p]).fetchall()
assert c.execute("SELECT count(*) FROM read_cell_dataset(?)", [p]).fetchone()[0] == got[0]
assert all(ok for _, ok, _ in c.execute("SELECT * FROM validate_cell_dataset(?)", [p]).fetchall())
# refusals
for q, msg in (("SELECT packed_id, people FROM changes", "columns do not match the table schema"),):
    try:
        c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'upsert')", [q, p]).fetchall(); raise AssertionError(q)
    except duckdb.Error as e:
        assert msg in str(e), e
try:
    c.execute("SELECT * FROM write_cell_dataset('SELECT * FROM changes', ?, layout := 'flat', mode := 'upsert')", [f"{d}/flat"]).fetchall(); raise AssertionError("flat")
except duckdb.Error as e:
    assert "only for layout 'iceberg'" in str(e), e
try:
    c.execute("SELECT * FROM write_cell_dataset('SELECT * FROM changes LIMIT 5', ?, layout := 'iceberg', mode := 'append')", [p]).fetchall(); raise AssertionError("dup")
except duckdb.Error as e:
    assert "already in the table" in str(e), e
print("upsert OK", d)
