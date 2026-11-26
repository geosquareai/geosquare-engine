"""Run every SQL block of docs/query-examples.md (except blocks starting with
`-- not run in tests`) against a temporary folder that stands in for /data.

    python tests/test_docs_examples.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import re, sys, tempfile
from pathlib import Path
import duckdb

ext = sys.argv[1]
docs = sorted((Path(__file__).resolve().parent.parent / "docs").glob("*.md"))
ran = skipped = 0
dirs, conns = {}, {}
for doc in docs:   # each document is self-contained: its own database and folder
    d = dirs[doc.name] = tempfile.mkdtemp(prefix="gs_docs_")
    c = conns[doc.name] = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    c.execute(f"LOAD '{ext}'")
    for i, b in enumerate(re.findall(r"```sql\n(.*?)```", doc.read_text(), flags=re.S)):
        if b.lstrip().startswith("-- not run in tests"):
            skipped += 1
            continue
        sql = b.replace("'/data/", f"'{d}/").replace("''/data/", f"''{d}/")
        # one statement at a time, so a failure names the statement
        for stmt in re.split(r";\s*\n", sql):
            body = "\n".join(l for l in stmt.splitlines() if not l.strip().startswith("--")).strip().rstrip(";")
            if not body:
                continue
            try:
                c.execute(body).fetchall()
            except Exception as e:
                raise SystemExit(f"{doc.name} block {i} failed:\n{body}\n-> {e}")
            ran += 1
d, c = dirs["query-examples.md"], conns["query-examples.md"]
# The claims the doc makes about results.
poly = "POLYGON ((106.8227 -6.1799, 106.8317 -6.1799, 106.8317 -6.1709, 106.8227 -6.1709, 106.8227 -6.1799))"
tot = [c.execute(f"SELECT count(*), sum(population) FROM read_cell_dataset('{d}/{n}')").fetchone() for n in ("pop_flat", "pop_part", "pop_ice")]
assert tot[0] == tot[1] == tot[2], tot
q = [c.execute(f"SELECT count(*), sum(population * coverage_ratio) FROM query_cell_dataset('{d}/{n}', geometry := ?)", [poly]).fetchone()
     for n in ("pop_flat", "pop_part", "pop_ice")]
assert q[0][0] == q[1][0] == q[2][0] and max(abs(q[0][1] - x[1]) for x in q) < 1e-6, q
manual = c.execute(f"SELECT count(*), sum(d.population * k.coverage_ratio) FROM polygon_to_cells('ID', ?, 12) k "
                   f"JOIN read_parquet('{d}/pop_flat/data/*.parquet') d USING (packed_id)", [poly]).fetchone()
assert manual[0] == q[0][0] and abs(manual[1] - q[0][1]) < 1e-6, (manual, q[0])
line = c.execute(f"SELECT sum(length_ratio) FROM query_cell_dataset('{d}/pop_ice', geometry := 'LINESTRING (106.80 -6.20, 106.82 -6.18, 106.85 -6.17)')").fetchone()[0]
assert abs(line - 1) < 1e-9, line
# write-guide.md: the end state of the insert / upsert / delete steps
w, wc = dirs["write-guide.md"], conns["write-guide.md"]
n_src, n_more, n_more2, n_changes = (wc.execute(f"SELECT count(*) FROM {t}").fetchone()[0] for t in ("src", "more", "more2", "changes"))
part = wc.execute(f"SELECT count(*), count(DISTINCT packed_id) FROM read_cell_dataset('{w}/w_part')").fetchone()
assert part[0] == part[1] == wc.execute("SELECT count(*) FROM (SELECT packed_id FROM src UNION SELECT packed_id FROM more)").fetchone()[0], part
ice = wc.execute(f"SELECT count(*), count(DISTINCT packed_id), count(*) FILTER (WHERE population = 1000), "
                 f"count(*) FILTER (WHERE non_workers IS NULL) FROM read_cell_dataset('{w}/w_ice')").fetchone()
expected = wc.execute("""SELECT count(*) FROM (SELECT packed_id FROM src UNION SELECT packed_id FROM more
    UNION SELECT packed_id FROM more2 UNION SELECT packed_id FROM changes) u
    ANTI JOIN polygon_to_cells('ID', 'POLYGON ((106.80 -6.22, 106.81 -6.22, 106.81 -6.21, 106.80 -6.21, 106.80 -6.22))', 12) d
    USING (packed_id)""").fetchone()[0]
assert ice[0] == ice[1] == expected and ice[3] == 0, (ice, expected)
assert ice[2] > 0 and ice[2] <= n_changes, (ice, n_changes)
# time-cube.md: the values its comments state
t, tc = dirs["time-cube.md"], conns["time-cube.md"]
assert tc.execute("SELECT period_valid('2026-02-29', 'day'), period_valid('2026-11-19T04', 'hour:4'), "
                  "period_relation('2026-11-19', 'day', '2026-11', 'month'), period_rollup('2026-11-19T05', 'hour:1', 'day')").fetchone() == (
    False, True, "contained", "2026-11-19")
assert tc.execute(f"SELECT period_start, period_end FROM cell_dataset_info('{t}/rain_ice')").fetchone() == ("2026-01-01", "2026-04-04")
assert tc.execute(f"SELECT count(*), count(DISTINCT period) FROM read_cell_dataset('{t}/rain_ice')").fetchone() == (11914 * 93, 93)
print(f"docs examples OK: {ran} statements in {len(docs)} documents ({skipped} blocks need S3/network)", dirs)
