"""Time cubes (cell_time_values) written and read in the three layouts: write_cell_dataset with
period_type, read_cell_dataset / query_cell_dataset(s) with period, period_from / period_to,
time_rollup and time_rule, file pruning by period, append / upsert keyed on (cell, period),
compaction, validate_cell_dataset, and the manifest / rows / rollup vectors of
geosquare-v2 conformance/spec-2.0/time-values.json (copied to tests/data).

    python tests/test_timecube_data.py build/linux_amd64/geosquare_v2.duckdb_extension

With geosquare_v2 importable, the flat output is also read back by
geosquare_v2.time_store.read_time_values, and a dataset it writes is read here.
"""
import glob, json, os, shutil, sys, tempfile
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_timecube_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
W = "POLYGON ((106.78 -6.24, 106.84 -6.24, 106.84 -6.20, 106.78 -6.20, 106.78 -6.24))"
Q = "POLYGON ((106.79 -6.23, 106.80 -6.23, 106.80 -6.22, 106.79 -6.22, 106.79 -6.23))"
LAYOUTS = ("flat", "partitioned", "iceberg")
SEM = json.dumps({"rain": "MEASUREMENT", "events": "COUNT"})


def src(lo, hi, add=0):
    return f"""SELECT packed_id, p.period, (packed_id % 97)::DOUBLE + p.i + {add} AS rain, (packed_id % 5)::BIGINT AS events
FROM polygon_to_cells('ID', '{W}', 12),
     (SELECT strftime(DATE '2026-01-01' + INTERVAL (i) DAY, '%Y-%m-%d') AS period, i FROM range({lo}, {hi}) t(i)) p"""


def fails(sql, params=(), match=""):
    try:
        c.execute(sql, list(params)).fetchall()
    except duckdb.Error as e:
        assert match in str(e), (match, str(e))
        return
    raise AssertionError(f"accepted: {sql}")


c.execute(f"CREATE TABLE truth AS {src(0, 90)}")
for L in LAYOUTS:
    r = c.execute(
        "SELECT * FROM write_cell_dataset(?, ?, layout := ?, partition_level := 7, period_type := 'day', "
        "value_semantics := ?, time_aggregation := 'mean')", [src(0, 90), f"{d}/{L}", L, SEM]).fetchone()
    assert int(r[3]) == 1072260, r
    info = c.execute("SELECT dataset_type, period_type, period_start, period_end, manifest FROM cell_dataset_info(?)", [f"{d}/{L}"]).fetchone()
    assert info[:4] == ("cell_time_values", "day", "2026-01-01", "2026-04-01"), info
    m = json.loads(info[4])
    assert m["identity"]["unique_key"] == ["gid", "period"] and m["identity"]["unique_gid"] is False
    assert m["storage"]["sorted_by"] == ["gid", "period"] and m["timezone"] == "UTC"
    assert {f["name"]: f["time_aggregation"] for f in m["value_fields"]} == {"rain": "mean", "events": None}
assert os.listdir(f"{d}/flat/data") == ["time_values.parquet"]
assert [r[0] for r in c.execute(f"DESCRIBE SELECT * FROM '{d}/flat/data/time_values.parquet'").fetchall()] == ["gid", "period", "rain", "events"]
assert sorted(os.listdir(glob.glob(f"{d}/partitioned/data/gid_prefix=*")[0])) == ["period=2026-01.parquet", "period=2026-02.parquet", "period=2026-03.parquet"]
assert os.path.exists(f"{d}/partitioned/period_chunks.json")


def same(name, sql, truth):
    want = c.execute(truth).fetchall()
    for L in LAYOUTS:
        got = c.execute(sql.replace("{P}", f"'{d}/{L}'")).fetchall()
        assert got == want, (name, L, got[:3], want[:3])


same("all", "SELECT count(*), round(sum(rain), 3), min(period), max(period) FROM read_cell_dataset({P})",
     "SELECT count(*), round(sum(rain), 3), min(period), max(period) FROM truth")
same("month", "SELECT count(*), round(sum(rain), 3) FROM read_cell_dataset({P}, period := '2026-02')",
     "SELECT count(*), round(sum(rain), 3) FROM truth WHERE period LIKE '2026-02-%'")
same("day", "SELECT count(*), round(sum(rain), 3) FROM read_cell_dataset({P}, period := '2026-02-03')",
     "SELECT count(*), round(sum(rain), 3) FROM truth WHERE period = '2026-02-03'")
same("range", "SELECT count(*), round(sum(rain), 3) FROM read_cell_dataset({P}, period_from := '2026-01-15', period_to := '2026-02-10')",
     "SELECT count(*), round(sum(rain), 3) FROM truth WHERE period >= '2026-01-15' AND period < '2026-02-10'")
same("open end", "SELECT count(*) FROM read_cell_dataset({P}, period_from := '2026-03-30')",
     "SELECT count(*) FROM truth WHERE period >= '2026-03-30'")
same("time_rollup month",
     "SELECT period, count(*), round(sum(rain), 3), sum(events), sum(period_count) FROM read_cell_dataset({P}, time_rollup := 'month') GROUP BY 1 ORDER BY 1",
     "SELECT period, count(*), round(sum(rain), 3), sum(events), sum(n) FROM (SELECT packed_id, left(period, 7) AS period, avg(rain) AS rain, "
     "sum(events) AS events, count(*) AS n FROM truth GROUP BY 1, 2) GROUP BY 1 ORDER BY 1")
same("time_rollup all, time_rule",
     "SELECT count(*), round(sum(rain), 3), sum(events) FROM read_cell_dataset({P}, time_rollup := 'all', time_rule := '{\"rain\": \"max\"}', period_from := '2026-02')",
     "SELECT count(*), round(sum(rain), 3), sum(events) FROM (SELECT packed_id, max(rain) AS rain, sum(events) AS events FROM truth WHERE period >= '2026-02' GROUP BY 1)")
same("area + period",
     f"SELECT count(*), round(sum(rain * coverage_ratio), 3) FROM query_cell_dataset({{P}}, geometry := '{Q}', period := '2026-03-05')",
     f"SELECT count(*), round(sum(rain * k.coverage_ratio), 3) FROM truth JOIN polygon_to_cells('ID', '{Q}', 12) k USING (packed_id) WHERE period = '2026-03-05'")
same("area, time then space roll-up",
     f"SELECT period, count(*), round(sum(events), 3) FROM query_cell_dataset({{P}}, geometry := '{Q}', time_rollup := 'month', rollup_level := 9, "
     "rollup_rule := '{\"rain\": \"max\"}') GROUP BY 1 ORDER BY 1",
     f"SELECT p, count(*), round(sum(e), 3) FROM (SELECT left(gid, 9) AS g, left(period, 7) AS p, sum(events * k.coverage_ratio) AS e FROM truth "
     f"JOIN polygon_to_cells('ID', '{Q}', 12) k USING (packed_id) GROUP BY 1, 2) GROUP BY 1 ORDER BY 1")
same("space roll-up per period",
     "SELECT count(*), round(sum(events), 3) FROM read_cell_dataset({P}, rollup_level := 7, period_from := '2026-01-03', period_to := '2026-01-05', rollup_rule := '{\"rain\": \"max\"}')",
     "SELECT count(*), round(sum(events), 3) FROM (SELECT left(gid, 7), period, sum(events) AS events FROM truth JOIN polygon_to_cells('ID', '" + W + "', 12) k "
     "USING (packed_id) WHERE period IN ('2026-01-03', '2026-01-04') GROUP BY 1, 2)")

P = f"{d}/iceberg"
fails(f"SELECT * FROM read_cell_dataset('{P}', period := '2026-02-01T03')", match="finer than")
fails(f"SELECT * FROM read_cell_dataset('{P}', period := '2026-13')", match="month must be 01 to 12")
fails(f"SELECT * FROM read_cell_dataset('{P}', time_rollup := 'hour:4')", match="does not hold whole")
fails(f"SELECT * FROM read_cell_dataset('{P}', time_rule := 'max')", match="time_rule needs time_rollup")
fails(f"SELECT * FROM read_cell_dataset('{P}', period := '2026-02', period_from := '2026-01')", match="not both")
fails(f"SELECT * FROM read_cell_dataset('{P}', period_from := '2026-03', period_to := '2026-02')", match="must be before")

# Pruning: with the March files gone, the other months still read; March does not.
for L in ("partitioned", "iceberg"):
    q = f"{d}/{L}_pruned"
    shutil.copytree(f"{d}/{L}", q)
    gone = [f for f in glob.glob(f"{q}/data/*/*.parquet") if c.execute("SELECT min(period) FROM read_parquet(?)", [f]).fetchone()[0] >= "2026-03"]
    assert gone
    for f in gone:
        os.remove(f)
    assert c.execute("SELECT count(*) FROM read_cell_dataset(?, period := '2026-02')", [q]).fetchone()[0] == 333592
    assert c.execute(f"SELECT count(*) FROM query_cell_dataset(?, geometry := '{Q}', period_to := '2026-03')", [q]).fetchone()[0] == 31506
    if L == "iceberg":
        fails("SELECT count(*) FROM read_cell_dataset(?, period := '2026-03')", [q], "No files found")

# Several datasets: time cubes join on (cell, period), a static dataset on the cell.
st = f"{d}/static"
c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'flat', value_semantics := 'COUNT')",
          [f"SELECT packed_id, 1::DOUBLE AS people FROM polygon_to_cells('ID', '{W}', 12)", st]).fetchall()
fails(f"SELECT * FROM read_cell_dataset('{st}', period := '2026-02')", match="this dataset has no periods")
r = c.execute(f"SELECT count(*), count(DISTINCT period), sum(people) FROM query_cell_dataset(['{d}/iceberg', '{st}'], geometry := '{Q}', period := '2026-02')").fetchone()
assert r == (14952, 28, 14952.0), r
r = c.execute(f"SELECT count(*), count(DISTINCT period), sum(iceberg__rain - flat__rain) FROM query_cell_dataset(['{d}/iceberg', '{d}/flat'], geometry := '{Q}', period := '2026-02')").fetchone()
assert r == (14952, 28, 0.0), r
r = c.execute(f"SELECT count(*), count(DISTINCT period) FROM query_cell_datasets(['{d}/partitioned', '{d}/flat', '{st}'], geometry := '{Q}', time_rollup := 'month', "
              "rollup_level := 9, rollup_rule := '{\"rain\": \"max\"}')").fetchone()
assert r == (12, 3), r

# validate_cell_dataset, columns, compaction
for L in LAYOUTS:
    bad = [x for x in c.execute("SELECT * FROM validate_cell_dataset(?)", [f"{d}/{L}"]).fetchall() if not x[1]]
    assert not bad, (L, bad)
    checks = {x[0] for x in c.execute("SELECT * FROM validate_cell_dataset(?)", [f"{d}/{L}"]).fetchall()}
    assert {"time_manifest", "unique_cell_periods", "periods"} <= checks, checks
    cols = c.execute("SELECT \"column\", time_aggregation FROM cell_dataset_columns(?) ORDER BY 1", [f"{d}/{L}"]).fetchall()
    assert cols == [("events", None), ("rain", "mean")], cols
for L in ("partitioned", "iceberg"):
    q = f"{d}/{L}_compact"
    shutil.copytree(f"{d}/{L}", q)
    before = c.execute("SELECT count(*), sum(rain), min(period), max(period) FROM read_cell_dataset(?)", [q]).fetchone()
    c.execute("SELECT * FROM compact_cell_dataset(?)", [q]).fetchall()
    assert c.execute("SELECT count(*), sum(rain), min(period), max(period) FROM read_cell_dataset(?)", [q]).fetchone() == before
    assert c.execute("SELECT period_type, period_start, period_end FROM cell_dataset_info(?)", [q]).fetchone() == ("day", "2026-01-01", "2026-04-01")

# Iceberg append / upsert keyed on (cell, period); the range grows.
P = f"{d}/ice_rw"
c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', period_type := 'day', value_semantics := ?, time_aggregation := 'max')", [src(0, 5), P, SEM]).fetchall()
fails("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'append')", [src(3, 7), P], "(cell, period) rows are already in the table")
c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'append')", [src(5, 9), P]).fetchall()
c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'upsert')", [src(8, 10, 1000), P]).fetchall()
r = c.execute("SELECT count(*), count(DISTINCT period), max(period), sum(rain) FILTER (WHERE period >= '2026-01-09') FROM read_cell_dataset(?)", [P]).fetchone()
want = c.execute(f"SELECT 11914 * 10, 10, '2026-01-10', sum(rain) FROM ({src(8, 10, 1000)})").fetchone()
assert r == want, (r, want)
assert c.execute("SELECT period_start, period_end FROM cell_dataset_info(?)", [P]).fetchone() == ("2026-01-01", "2026-01-11")
fails("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'append', period_type := 'month')", [f"SELECT 1::BIGINT AS packed_id, '2026-05' AS period", P], "use mode := 'overwrite'")

# Writes the writer refuses
one = f"SELECT packed_id, {{}} AS period, 1.0 AS v FROM polygon_to_cells('ID', '{W}', 12)"
fails("SELECT * FROM write_cell_dataset(?, ?, period_type := 'day')", [one.format("'2026-02-30'"), f"{d}/x1"], "not a valid day label")
fails("SELECT * FROM write_cell_dataset(?, ?, period_type := 'day')", [one.format("'2026-02-01'") + ", range(2)", f"{d}/x2"], "duplicate (cell, period) pairs")
fails("SELECT * FROM write_cell_dataset(?, ?, period_type := 'day', period_start := '2026-01-01', period_end := '2026-02-01')", [one.format("'2026-02-01'"), f"{d}/x3"], "not all inside")
fails("SELECT * FROM write_cell_dataset(?, ?, period_type := 'hour', bucket_hours := 4)", [one.format("'2026-02-01T02'"), f"{d}/x4"], "not a valid hour")
fails("SELECT * FROM write_cell_dataset(?, ?, period_type := 'day')", [f"SELECT packed_id, 1.0 AS v FROM polygon_to_cells('ID', '{W}', 12)", f"{d}/x5"], "needs a period column")
fails("SELECT * FROM write_cell_dataset(?, ?, time_chunk := 'year')", [one.format("'2026'"), f"{d}/x6"], "is for time cubes")
fails("SELECT * FROM write_cell_dataset(?, ?, period_type := 'day', manifest_version := '1.1')", [one.format("'2026-02-01'"), f"{d}/x7"], "manifest 1.0")
r = c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'partitioned', period_type := 'hour:4', time_chunk := 'day', value_semantics := 'COUNT')",
              [one.format("'2026-02-01T' || lpad(((packed_id % 6) * 4)::VARCHAR, 2, '0')"), f"{d}/h4"]).fetchone()
assert c.execute("SELECT period_type, period_start, period_end FROM cell_dataset_info(?)", [f"{d}/h4"]).fetchone() == ("hour:4", "2026-02-01T00", "2026-02-01T20")
assert c.execute("SELECT count(*), sum(v) FROM read_cell_dataset(?, time_rollup := 'hour:12')", [f"{d}/h4"]).fetchone()[1] == 11914

# Conformance vectors: manifest and rows (stored flat datasets checked by validate_cell_dataset),
# rollup (rules applied by time_rollup).
vectors = json.load(open(os.path.join(os.path.dirname(__file__), "data", "time-values.json")))["cases"]
checked = {"manifest": 0, "rows": 0, "rollup": 0}
for i, case in enumerate(vectors):
    op = case["operation"]
    if op in ("manifest", "rows"):
        root = f"{d}/vec{i}"
        os.makedirs(f"{root}/data")
        m = case["manifest"]
        rows = case.get("rows") or []
        names = [f["name"] for f in m["value_fields"]]
        cols = ", ".join(["gid VARCHAR", "period VARCHAR"] + [f'"{n}" DOUBLE' for n in names if n != "period"])
        c.execute(f"CREATE OR REPLACE TABLE vrows ({cols})")
        for r in rows:
            c.execute("INSERT INTO vrows (gid, period) VALUES (?, ?)", [r["gid"], r["period"]])
        if "period" not in names:
            for n in names:
                c.execute(f'UPDATE vrows SET "{n}" = 1.0')
        c.execute(f"COPY (SELECT * FROM vrows) TO '{root}/data/time_values.parquet' (FORMAT parquet)")
        json.dump(m, open(f"{root}/manifest.json", "w"))
        res = {x[0]: x[1] for x in c.execute("SELECT * FROM validate_cell_dataset(?)", [root]).fetchall()}
        keys = ["time_manifest"] + (["unique_cell_periods", "periods", "domain_and_level", "sorted_by_gid_period"] if op == "rows" else [])
        ok = all(res.get(k, False) for k in keys)
        assert ok == case["expect"]["valid"], (case["name"], res)
    elif op == "rollup":
        root = f"{d}/vec{i}"
        sem = case["semantics"]
        t = "VARCHAR" if sem == "CATEGORICAL" else "DOUBLE"
        q = f"SELECT packed_id, '2026-11-0' || (1 + packed_id % 2)::VARCHAR AS period, (packed_id % 3)::{t} AS v FROM polygon_to_cells('ID', '{Q}', 12)"
        extra = f", time_aggregation := '{case['declared']}'" if case["declared"] else ""
        c.execute(f"SELECT * FROM write_cell_dataset(?, ?, period_type := 'day', value_semantics := '{sem}'{extra})", [q, root]).fetchall()
        try:
            got = c.execute("SELECT sum(v) FROM read_cell_dataset(?, time_rollup := 'month')", [root]).fetchone()[0]
            valid = True
        except duckdb.Error:
            valid = False
        assert valid == case["expect"].get("valid", True), (case["name"], valid)
        if valid:
            want = c.execute(f"SELECT sum(x) FROM (SELECT {dict(sum='sum', mean='avg', min='min', max='max')[case['expect']['rule']]}(v) AS x FROM ({q}) GROUP BY packed_id)").fetchone()[0]
            assert abs(got - want) < 1e-9, (case["name"], got, want)
    else:
        continue
    checked[op] += 1
assert checked == {"manifest": 6, "rows": 5, "rollup": 4}, checked

# geosquare_v2.time_store: it reads our flat file, we read the files it writes.
try:
    from geosquare_v2.time_store import read_time_values, write_time_values
except ImportError:
    print("geosquare_v2 not importable: skipped the interop checks")
else:
    m, rows = read_time_values(f"{d}/flat")
    assert len(rows) == 1072260 and m["period_type"] == "day"
    py = f"{d}/py"
    write_time_values(py, [dict(gid=g, period=p, rain=float(i)) for i, (g, p) in enumerate(
        (g, p) for g in ["H2X545N4M2G4", "H2X545N4M2G5"] for p in ["2026-01-01T00", "2026-01-01T06", "2026-01-02T12"])],
        domain_code="ID", domain_id=1, level=12, period_type="hour", bucket_hours=6, period_start="2026-01-01T00",
        period_end="2026-01-03T00", value_fields=[dict(name="rain", semantics="MEASUREMENT", time_aggregation="max")])
    assert c.execute("SELECT gid, period, rain, period_count FROM read_cell_dataset(?, time_rollup := 'day') ORDER BY 1, 2", [py]).fetchall() == [
        ("H2X545N4M2G4", "2026-01-01", 1.0, 2), ("H2X545N4M2G4", "2026-01-02", 2.0, 1),
        ("H2X545N4M2G5", "2026-01-01", 4.0, 2), ("H2X545N4M2G5", "2026-01-02", 5.0, 1)]
    assert all(x[1] for x in c.execute("SELECT * FROM validate_cell_dataset(?)", [py]).fetchall())
print("time cube data OK", d)
