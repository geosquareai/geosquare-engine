"""compact_cell_dataset: a table built by many appends is rewritten with one file per
partition (optionally at another partition level); data and metadata stay the same.

    python tests/test_compact.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import glob, json, os, sys, tempfile
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_compact_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'; LOAD iceberg")
meta = dict(units=json.dumps({"people": "people", "hazard": "index 0-1"}), column_descriptions=json.dumps({"people": "People", "hazard": "Hazard"}),
            title="T", description="D", data_period="2023", data_version="v1", aggregation_rule=json.dumps({"people": "sum", "hazard": "mean"}),
            source=json.dumps({"format": "parquet", "source_dataset": "s3://x/y"}), coverage=json.dumps({"boundary_policy": "CENTROID_COVERED"}), notes="n")
args = ", ".join(f"{k} := ?" for k in meta)
# 6 appends of thin vertical strips: every 10 km partition gets several files
strips = [f"POLYGON (({x:.3f} -6.24, {x + 0.01:.3f} -6.24, {x + 0.01:.3f} -6.16, {x:.3f} -6.16, {x:.3f} -6.24))" for x in [106.78 + 0.01 * i for i in range(6)]]
def src(w):
    return f"SELECT packed_id, (packed_id % 97)::DOUBLE AS people, (packed_id % 7)::DOUBLE / 10 AS hazard FROM polygon_to_cells('ID', '{w}', 12)"
for layout in ("iceberg", "partitioned", "flat"):
    p = f"{d}/{layout}"
    if layout == "iceberg":
        c.execute(f"SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', partition_level := 7, {args})", [src(strips[0]), p, *meta.values()]).fetchall()
        for w in strips[1:]:
            q = f"SELECT s.* FROM ({src(w)}) s ANTI JOIN read_cell_dataset('{p}') t USING (packed_id)"
            c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'append')", [q, p]).fetchall()
    else:
        q = "SELECT packed_id, any_value(people) AS people, any_value(hazard) AS hazard FROM (" + " UNION ALL ".join(src(w) for w in strips) + ") GROUP BY 1"
        c.execute(f"SELECT * FROM write_cell_dataset(?, ?, layout := ?, partition_level := 7, {args})", [q, p, layout, *meta.values()]).fetchall()
    data = lambda: c.execute("SELECT count(*), sum(people), round(sum(hazard), 6), min(packed_id), max(packed_id) FROM read_cell_dataset(?)", [p]).fetchone()
    info = lambda: c.execute("SELECT title, description, data_period, data_version, source, coverage, provenance->>'notes', level FROM cell_dataset_info(?)", [p]).fetchone()
    cols = lambda: c.execute("SELECT * FROM cell_dataset_columns(?) ORDER BY 1", [p]).fetchall()
    parts = lambda: c.execute("SELECT manifest->'storage'->'partitioning' FROM cell_dataset_info(?)", [p]).fetchone()[0]
    before = (data(), info(), cols(), parts())
    assert before[1][4] and json.loads(before[1][4])["source_dataset"] == "s3://x/y" and before[1][6] == "n", (layout, before[1])
    assert {r[0]: (r[3], r[4]) for r in before[2]} == {"hazard": ("mean", "index 0-1"), "people": ("sum", "people")}, before[2]
    nfiles = len(glob.glob(f"{p}/data/**/*.parquet", recursive=True))
    r = c.execute("SELECT * FROM compact_cell_dataset(?)", [p]).fetchone()
    after = (data(), info(), cols(), parts())
    assert after == before, (layout, before, after)
    if layout == "iceberg":
        live = c.execute("SELECT count(*) FROM iceberg_metadata(?) WHERE status <> 'DELETED' AND manifest_content = 'DATA'", [p]).fetchone()[0]
        parts7 = c.execute("SELECT count(DISTINCT left(gid, 7)) FROM read_cell_dataset(?)", [p]).fetchone()[0]
        assert nfiles > parts7 and live == parts7, (nfiles, live, parts7)
        assert c.execute("SELECT count(*), sum(people) FROM iceberg_scan(?)", [p]).fetchone() == before[0][:2]
        # coarser partitions, then drop the old snapshots
        c.execute("SELECT * FROM compact_cell_dataset(?, partition_level := 5)", [p]).fetchall()
        assert data() == before[0] and info() == before[1] and cols() == before[2] and parts() != before[3]
        e = c.execute("SELECT * FROM expire_snapshots(?, dry_run := false)", [p]).fetchone()
        assert int(e[0]) == 1 and int(e[1]) == 7, e   # 6 appends + 2 compactions, keep the current one
        left = glob.glob(f"{p}/data/**/*.parquet", recursive=True)
        assert len(left) == c.execute("SELECT count(DISTINCT left(gid, 5)) FROM read_cell_dataset(?)", [p]).fetchone()[0], left
        assert data() == before[0]
# compaction keeps NULL numbers as NULL (it used to write them as 0)
for layout in ("partitioned", "iceberg"):
    p = f"{d}/nulls_{layout}"
    q = ("SELECT packed_id, CASE WHEN packed_id % 4 = 0 THEN NULL ELSE (packed_id % 7)::DOUBLE END AS hazard "
         "FROM polygon_to_cells('ID', 'POLYGON ((106.78 -6.24, 106.84 -6.24, 106.84 -6.20, 106.78 -6.20, 106.78 -6.24))', 12)")
    c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := ?, partition_level := 7, fill_nulls := false)", [q, p, layout]).fetchall()
    nulls = lambda: c.execute("SELECT count(*) FILTER (WHERE hazard IS NULL) FROM read_cell_dataset(?)", [p]).fetchone()[0]
    before = nulls()
    assert before > 0, (layout, before)
    c.execute("SELECT * FROM compact_cell_dataset(?)", [p]).fetchall()
    assert nulls() == before, (layout, before, nulls())
print("compact OK", d)
