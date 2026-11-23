"""RATE rollups are weighted by their denominator, in the writer and in the query path.

    python tests/test_rate_rollup.py build/linux_amd64/geosquare_v2.duckdb_extension

Two children of one parent: 1 person at 100 % and 999 people at 5 %. The parent rate is
(100 x 1 + 5 x 999) / 1000 = 5.095 %. A plain average would give 52.5 %, so a RATE without a
denominator is refused, and so is a RATE with a rule that is not a mean.
"""

import json
import math
import sys
import tempfile

import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_rate_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")

area = "POLYGON ((106.80 -6.21, 106.83 -6.21, 106.83 -6.18, 106.80 -6.18, 106.80 -6.21))"
groups = {}
for (g,) in c.execute("SELECT gid FROM polygon_to_cells('ID', ?, 12)", [area]).fetchall():
    groups.setdefault(g[:9], []).append(g)
parent = next(k for k, v in groups.items() if len(v) >= 2)
a, b = groups[parent][:2]
src = (
    "SELECT pack('ID', gid) AS packed_id, rate, people FROM (VALUES "
    f"('{a}', 100.0, 1.0), ('{b}', 5.0, 999.0)) AS t(gid, rate, people)"
)
SEM = '{"rate": "RATE", "people": "COUNT"}'
WEIGHT = json.dumps({"rate": "people"})
EXPECT = (100.0 * 1 + 5.0 * 999) / 1000


def check(cond, msg):
    if not cond:
        raise AssertionError(msg)


def refused(sql, args, msg):
    try:
        c.execute(sql, args).fetchall()
    except duckdb.Error as e:
        check(msg in str(e), f"wrong error for {msg!r}: {e}")
        return
    raise AssertionError(f"no error for {msg!r}")


# Writer: the rollup is weighted by people.
out = f"{d}/w_rate"
c.execute(
    "SELECT * FROM write_cell_dataset(?, ?, rollup_level := 9, value_semantics := ?, rollup_weight := ?)",
    [src, out, SEM, WEIGHT],
).fetchall()
got = c.execute("SELECT gid, rate, people FROM read_cell_dataset(?)", [out]).fetchall()
check(len(got) == 1 and got[0][0] == parent, f"writer rows: {got}")
check(math.isclose(got[0][1], EXPECT, rel_tol=1e-9), f"writer rate {got[0][1]} != {EXPECT}")
check(got[0][2] == 1000.0, f"writer people {got[0][2]}")

# Writer refusals: no denominator, a rule that is not a mean, and no rollup level.
refused(
    "SELECT * FROM write_cell_dataset(?, ?, rollup_level := 9, value_semantics := ?)",
    [src, f"{d}/no_weight", SEM],
    "needs its denominator",
)
refused(
    "SELECT * FROM write_cell_dataset(?, ?, rollup_level := 9, value_semantics := ?, "
    "aggregation_rule := ?, rollup_weight := ?)",
    [src, f"{d}/sum_rule", SEM, json.dumps({"rate": "sum"}), WEIGHT],
    "roll it up as a weighted mean",
)
refused(
    "SELECT * FROM write_cell_dataset(?, ?, value_semantics := ?, rollup_weight := ?)",
    [src, f"{d}/no_level", SEM, WEIGHT],
    "rollup_weight needs rollup_level",
)

# Query path: the same rule on a dataset read with rollup_level.
fine = f"{d}/fine"
c.execute("SELECT * FROM write_cell_dataset(?, ?, value_semantics := ?)", [src, fine, SEM]).fetchall()
got_q = c.execute(
    "SELECT gid, rate FROM read_cell_dataset(?, rollup_level := 9, rollup_weight := ?)", [fine, WEIGHT]
).fetchall()
check(len(got_q) == 1 and got_q[0][0] == parent, f"query rows: {got_q}")
check(math.isclose(got_q[0][1], EXPECT, rel_tol=1e-9), f"query rate {got_q[0][1]} != {EXPECT}")
refused(
    "SELECT * FROM read_cell_dataset(?, rollup_level := 9)",
    [fine],
    "needs its denominator",
)
print("rate rollup OK")
