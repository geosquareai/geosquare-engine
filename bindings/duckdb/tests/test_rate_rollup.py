"""A RATE rolls up as a weighted mean, weighted by its denominator (spec 2.0, dataset metadata
section 4). Writer and query paths.

    python tests/test_rate_rollup.py build/linux_amd64/geosquare_v2.duckdb_extension

Two children of one parent: 1 person at 100 % and 999 people at 5 %. The parent rate is
(100 x 1 + 5 x 999) / 1000 = 5.095 %. A plain average would give 52.5 %, so a RATE needs
space_weight, and its rule must be weighted_mean.
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
SEM = json.dumps({"rate": "RATE", "people": "COUNT"})
RULE = json.dumps({"rate": "weighted_mean"})
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


# Writer: the parent rate is weighted by people.
out = f"{d}/w_rate"
c.execute(
    "SELECT * FROM write_cell_dataset(?, ?, rollup_level := 9, value_semantics := ?, "
    "space_aggregation := ?, space_weight := ?)",
    [src, out, SEM, RULE, WEIGHT],
).fetchall()
got = c.execute("SELECT gid, rate, people FROM read_cell_dataset(?)", [out]).fetchall()
check(len(got) == 1 and got[0][0] == parent, f"writer rows: {got}")
check(math.isclose(got[0][1], EXPECT, rel_tol=1e-9), f"writer rate {got[0][1]} != {EXPECT}")
check(got[0][2] == 1000.0, f"writer people {got[0][2]}")

# Writer refusals.
refused(
    "SELECT * FROM write_cell_dataset(?, ?, rollup_level := 9, value_semantics := ?)",
    [src, f"{d}/no_rule", SEM],
    "has no space_aggregation",
)
refused(
    "SELECT * FROM write_cell_dataset(?, ?, rollup_level := 9, value_semantics := ?, space_aggregation := ?, space_weight := ?)",
    [src, f"{d}/sum_rule", SEM, json.dumps({"rate": "sum"}), WEIGHT],
    "is not allowed for RATE",
)
refused(
    "SELECT * FROM write_cell_dataset(?, ?, rollup_level := 9, value_semantics := ?, space_aggregation := ?)",
    [src, f"{d}/no_weight", SEM, RULE],
    "weighted_mean needs a space_weight",
)
refused(
    "SELECT * FROM write_cell_dataset(?, ?, value_semantics := ?, space_aggregation := ?)",
    [src, f"{d}/no_level", SEM, RULE],
    "need rollup_level",
)

# Query path: the rule and weight are given to the read; the stored dataset has none (1.0).
fine = f"{d}/fine"
c.execute("SELECT * FROM write_cell_dataset(?, ?, value_semantics := ?)", [src, fine, SEM]).fetchall()
got_q = c.execute(
    "SELECT gid, rate FROM read_cell_dataset(?, rollup_level := 9, rollup_rule := ?, space_weight := ?)",
    [fine, RULE, WEIGHT],
).fetchall()
check(len(got_q) == 1 and got_q[0][0] == parent, f"query rows: {got_q}")
check(math.isclose(got_q[0][1], EXPECT, rel_tol=1e-9), f"query rate {got_q[0][1]} != {EXPECT}")
refused(
    "SELECT * FROM read_cell_dataset(?, rollup_level := 9)",
    [fine],
    "has no space_aggregation",
)
refused(
    "SELECT * FROM read_cell_dataset(?, rollup_level := 9, rollup_rule := ?)",
    [fine, RULE],
    "weighted_mean needs a space_weight",
)
print("rate rollup OK")
