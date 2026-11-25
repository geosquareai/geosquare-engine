"""Time cube (cell_time_values) against the geosquare-v2 conformance vectors
(conformance/spec-2.0/time-values.json, copied to tests/data): labels, intervals and
relations through the SQL functions period_valid, period_interval, period_relation, plus
period_rollup.

    python tests/test_timecube.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import json, os, sys
from datetime import datetime
import duckdb

ext = sys.argv[1]
vectors = json.load(open(os.path.join(os.path.dirname(__file__), "data", "time-values.json")))["cases"]
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
checked = {}


def args(label, period_type, bucket):
    return ("?, ?, ?", [label, period_type, bucket]) if bucket is not None else ("?, ?", [label, period_type])


def spec(period_type, bucket):
    """period type with its bucket written in it: 'hour:4'"""
    return f"{period_type}:{bucket}" if bucket is not None else period_type


for case in vectors:
    op, exp = case["operation"], case["expect"]
    if op == "parse":
        a, p = args(case["label"], case["period_type"], case["bucket_hours"])
        valid = c.execute(f"SELECT period_valid({a})", p).fetchone()[0]
        assert valid == exp["valid"], case["name"]
        if valid:
            start = c.execute(f"SELECT period_interval({a}).start", p).fetchone()[0]
            assert start == datetime.fromisoformat(exp["start"]), (case["name"], start)
    elif op == "interval":
        a, p = args(case["label"], case["period_type"], case["bucket_hours"])
        s, e = c.execute(f"SELECT period_interval({a}).start, period_interval({a}).end", p + p).fetchone()
        assert (s, e) == (datetime.fromisoformat(exp["start"]), datetime.fromisoformat(exp["end"])), case["name"]
    elif op == "relation":
        l, r = case["left"], case["right"]
        got = c.execute("SELECT period_relation(?, ?, ?, ?)",
                        [l["label"], spec(l["period_type"], l["bucket_hours"]), r["label"], spec(r["period_type"], r["bucket_hours"])]).fetchone()[0]
        got6 = c.execute("SELECT period_relation(?, ?, ?, ?, ?, ?)",
                         [l["label"], l["period_type"], l["bucket_hours"] or 0, r["label"], r["period_type"], r["bucket_hours"] or 0]).fetchone()[0]
        assert got == got6 == exp["relation"], (case["name"], got, got6)
    else:
        continue
    checked[op] = checked.get(op, 0) + 1

# symmetry: if A contains B then B is contained in A, over many label pairs
labels = [("2026", "year", None), ("2026-11", "month", None), ("2026-12", "month", None), ("2026-11-19", "day", None),
          ("2026-11-30", "day", None), ("2026-11-19T04", "hour", 4), ("2026-11-19T06", "hour", 6), ("2026-11-19T05", "hour", 1)]
for a in labels:
    for b in labels:
        sa, sb = (a[0], spec(a[1], a[2])), (b[0], spec(b[1], b[2]))
        ab, ba = c.execute("SELECT period_relation(?, ?, ?, ?), period_relation(?, ?, ?, ?)", [*sa, *sb, *sb, *sa]).fetchone()
        assert {"contains": "contained", "contained": "contains"}.get(ab, ab) == ba, (a, b, ab, ba)
# rollups
for src, to, want in ((("2026-11-19T05", "hour", 1), ("hour", 4), "2026-11-19T04"), (("2026-11-19", "day", None), ("month", None), "2026-11"),
                      (("2026-11-19T20", "hour", 4), ("day", None), "2026-11-19"), (("2026-11", "month", None), ("year", None), "2026")):
    got = c.execute("SELECT period_rollup(?, ?, ?)", [src[0], spec(src[1], src[2]), spec(*to)]).fetchone()[0]
    assert got == want, (src, to, got)
assert c.execute("SELECT period_rollup('2026-11-19', 'day', 'month')").fetchone()[0] == "2026-11"
for bad in ("SELECT period_rollup('2026-11', 'month', 'day')", "SELECT period_interval('9999', 'year')",
            "SELECT period_rollup('2026-11-19T04', 'hour', 4, 'hour', 6)"):
    try:
        c.execute(bad).fetchall(); raise AssertionError(bad)
    except duckdb.Error:
        pass
assert c.execute("SELECT period_valid(NULL, 'day'), period_relation('2026', 'year', NULL, 'day')").fetchone() == (None, None)
assert c.execute("SELECT period_valid('2026-11-19T04', 'hour:4'), period_valid('2026-11-19T02', 'hour:4'), period_interval('2026-11-19T04', 'hour:4').end::VARCHAR").fetchone() == (True, False, "2026-11-19 08:00:00")
print("time cube labels OK:", checked)
