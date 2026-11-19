"""list_cell_datasets, validate_cell_dataset and cell_dataset_stats.

    python tests/test_inspect.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import glob, json, math, os, sys, tempfile
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_inspect_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
src = ("SELECT packed_id, (packed_id % 97)::DOUBLE AS people, (packed_id % 7)::DOUBLE / 10 AS hazard, (packed_id % 5)::BIGINT AS n "
       "FROM polygon_to_cells('ID', 'POLYGON ((106.79 -6.22, 106.85 -6.22, 106.85 -6.16, 106.79 -6.16, 106.79 -6.22))', 12)")
for layout in ("flat", "partitioned", "iceberg"):
    c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := ?, partition_level := 7, title := ?, data_version := 'v1', units := '{\"people\": \"people\"}')",
              [src, f"{d}/data_name={layout}_v1", layout, f"T {layout}"]).fetchall()
os.makedirs(f"{d}/not_a_dataset/x")
rows = c.execute("SELECT name, layout, domain_code, level, row_count, value_columns, title, data_version FROM list_cell_datasets(?)", [d]).fetchall()
assert [r[0] for r in rows] == ["flat_v1", "iceberg_v1", "partitioned_v1"], rows
assert all(r[2:4] == ("ID", 12) and r[5] == 3 and r[7] == "v1" and r[6] == f"T {r[1]}" for r in rows), rows
n = c.execute(f"SELECT count(*) FROM ({src})").fetchone()[0]
assert all(r[4] == n for r in rows), rows

# validate: all fine
for layout in ("flat", "partitioned", "iceberg"):
    v = c.execute("SELECT * FROM validate_cell_dataset(?)", [f"{d}/data_name={layout}_v1"]).fetchall()
    assert all(ok for _, ok, _ in v), (layout, v)
    names = {x[0] for x in v}
    assert {"manifest", "columns", "row_count", "unique_cells", "gid_matches_packed_id", "domain_and_level", "no_null_values"} <= names, names
    if layout == "iceberg":
        assert {"data_files_exist", "manifest_record_count"} <= names
# broken flat: duplicate rows and NULLs written behind the writer's back
f = glob.glob(f"{d}/data_name=flat_v1/data/*.parquet")[0]
c.execute(f"COPY (SELECT * REPLACE (CASE WHEN packed_id % 11 = 0 THEN NULL ELSE people END AS people) FROM read_parquet('{f}', hive_partitioning = false) "
          f"UNION ALL (SELECT * FROM read_parquet('{f}', hive_partitioning = false) LIMIT 5)) TO '{d}/tmp.parquet' (FORMAT parquet)")
os.replace(f"{d}/tmp.parquet", f)
v = dict((k, ok) for k, ok, _ in c.execute("SELECT * FROM validate_cell_dataset(?)", [f"{d}/data_name=flat_v1"]).fetchall())
assert not v["unique_cells"] and not v["no_null_values"] and not v["row_count"] and v["manifest"] and v["columns"], v
# broken iceberg: a data file deleted
victim = sorted(glob.glob(f"{d}/data_name=iceberg_v1/data/**/*.parquet", recursive=True))[0]
os.remove(victim)
v = c.execute("SELECT * FROM validate_cell_dataset(?)", [f"{d}/data_name=iceberg_v1"]).fetchall()
dv = {k: (ok, det) for k, ok, det in v}
assert not dv["data_files_exist"][0] and "1 missing" in dv["data_files_exist"][1] and dv["unique_cells"][1].startswith("skipped"), v

# stats: manifest bounds == exact scan
p = f"{d}/stats_ice"
c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', partition_level := 7, units := '{\"people\": \"people\"}')", [src, p]).fetchall()
fast = {r[0]: r for r in c.execute("SELECT * FROM cell_dataset_stats(?)", [p]).fetchall()}
exact = {r[0]: r for r in c.execute("SELECT * FROM cell_dataset_stats(?, exact := true)", [p]).fetchall()}
want = c.execute("SELECT min(people), max(people), min(hazard), max(hazard), min(n), max(n), count(*) FROM read_cell_dataset(?)", [p]).fetchone()
assert all(r[6] == "manifest" for r in fast.values()) and all(r[6] == "scan" for r in exact.values()), (fast, exact)
for col, (lo, hi) in (("people", want[0:2]), ("hazard", want[2:4]), ("n", want[4:6])):
    for r in (fast[col], exact[col]):
        assert math.isclose(r[2], lo) and math.isclose(r[3], hi) and r[4] == 0 and r[5] == want[6], (col, r, lo, hi)
assert fast["people"][1] == "people" and fast["hazard"][1] is None
# a wide table written with key-only metrics: values not in the manifests
c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', column_metrics := 'key')", [src, f"{d}/stats_key"]).fetchall()
k = {r[0]: r for r in c.execute("SELECT * FROM cell_dataset_stats(?)", [f"{d}/stats_key"]).fetchall()}
assert all(r[6] == "unavailable" and r[2] is None for r in k.values()), k
k2 = {r[0]: r for r in c.execute("SELECT * FROM cell_dataset_stats(?, exact := true)", [f"{d}/stats_key"]).fetchall()}
assert math.isclose(k2["people"][3], want[1]), k2
# folder layouts are scanned
s = {r[0]: r for r in c.execute("SELECT * FROM cell_dataset_stats(?)", [f"{d}/data_name=partitioned_v1"]).fetchall()}
assert s["people"][6] == "scan" and math.isclose(s["people"][3], want[1]), s
print("inspect OK", d)
