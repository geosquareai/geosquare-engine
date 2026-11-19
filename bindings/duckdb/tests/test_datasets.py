"""End-to-end tests for write_cell_dataset / read_cell_dataset / query_cell_dataset.

Writes flat, gid_prefix-partitioned and Iceberg datasets into a temporary folder,
reads them back, queries them by polygon and bbox, and checks the error cases.
Only the `duckdb` package is needed (DuckDB 1.4 to 2.0):

    python tests/test_datasets.py build/linux_amd64/geosquare_v2.duckdb_extension
"""

import functools
import http.server
import json
import math
import sys
import tempfile
import threading
from pathlib import Path

import duckdb

EXT = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux_amd64/geosquare_v2.duckdb_extension").resolve()


def circle(lon, lat, r_deg, n=48):
    pts = [(lon + r_deg * math.cos(2 * math.pi * i / n), lat + r_deg * math.sin(2 * math.pi * i / n)) for i in range(n)]
    pts.append(pts[0])
    return "POLYGON ((" + ", ".join(f"{x} {y}" for x, y in pts) + "))"


AREA_A = circle(106.85, -6.20, 0.03)  # about 3 km, level 12
AREA_B = circle(106.95, -6.20, 0.03)  # disjoint from A
QUERY = circle(106.86, -6.20, 0.01)


def check(cond, msg):
    if not cond:
        raise AssertionError(msg)


def raises(con, sql, params=None, contains=""):
    try:
        con.execute(sql, params or []).fetchall()
    except duckdb.Error as e:
        check(contains.lower() in str(e).lower(), f"error should mention {contains!r}: {e}")
        return
    raise AssertionError(f"expected an error: {sql[:80]}")


def source(area):
    # packed_id + three value columns of different types; deterministic values.
    return (
        f"SELECT packed_id, (packed_id % 97)::INTEGER AS population, "
        f"((packed_id % 1000) / 10.0)::DOUBLE AS score, "
        f"CASE WHEN packed_id % 3 = 0 THEN 'urban' ELSE 'rural' END AS landuse "
        f"FROM polygon_to_cells('ID', '{area}', 12)"
    )


def sorted_files(con, glob):
    r = con.execute(
        f"SELECT bool_and(ok) FROM (SELECT packed_id > lag(packed_id) OVER (PARTITION BY filename ORDER BY file_row_number) "
        f"OR lag(packed_id) OVER (PARTITION BY filename ORDER BY file_row_number) IS NULL AS ok "
        f"FROM read_parquet('{glob}', filename = true, file_row_number = true, hive_partitioning = false))"
    ).fetchone()[0]
    return bool(r)


def expected(con, area_sql_list, query_wkt, min_cov=0.0):
    union = " UNION ALL ".join(f"({s})" for s in area_sql_list)
    return con.execute(
        f"SELECT count(*), sum(population), round(sum(score), 6) FROM ({union}) d "
        f"JOIN polygon_to_cells('ID', ?, 12, min_coverage := ?) k USING (packed_id)",
        [query_wkt, min_cov],
    ).fetchone()


def main():
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{EXT}'")
    version = con.execute("SELECT version()").fetchone()[0]
    n_a = con.execute(f"SELECT count(*) FROM ({source(AREA_A)})").fetchone()[0]
    n_b = con.execute(f"SELECT count(*) FROM ({source(AREA_B)})").fetchone()[0]
    want_q = expected(con, [source(AREA_A)], QUERY, 0.5)
    tmp = Path(tempfile.mkdtemp(prefix="gs_duckdb_"))
    out = {}

    # ---- flat ----
    flat = tmp / "flat"
    r = con.execute("SELECT * FROM write_cell_dataset(?, ?)", [source(AREA_A), str(flat)]).fetchall()
    check(r == [("flat", str(flat), "1", str(n_a))], f"flat write result {r}")
    m = json.loads((flat / "manifest.json").read_text())
    check(m["storage"]["row_count"] == n_a and m["level"] == 12 and m["domain_code"] == "ID", f"manifest {m['storage']}")
    check({f["name"] for f in m["value_fields"]} == {"population", "score", "landuse"}, "value fields")
    kv = con.execute(f"SELECT value FROM parquet_kv_metadata('{flat}/data/*.parquet') WHERE key = 'geosquare.manifest'").fetchone()[0]
    check(json.loads(kv) == m, "embedded manifest equals manifest.json")
    check(sorted_files(con, f"{flat}/data/*.parquet"), "flat file sorted by packed_id")
    got = con.execute(f"SELECT count(*), count(DISTINCT gid), bool_and(pack('ID', gid) = packed_id) FROM read_cell_dataset('{flat}')").fetchone()
    check(got == (n_a, n_a, True), f"flat read {got}")
    q = con.execute(f"SELECT count(*), sum(population), round(sum(score), 6) FROM query_cell_dataset('{flat}', geometry := ?, min_coverage := 0.5)", [QUERY]).fetchone()
    check(q == want_q, f"flat query {q} != {want_q}")
    out["flat"] = n_a

    # ---- partitioned (level 9 = 1 km prefixes, so many partitions) ----
    part = tmp / "part"
    r = con.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'partitioned', partition_level := 9)", [source(AREA_A), str(part)]).fetchall()
    nparts = int(r[0][2])
    check(r[0][0] == "partitioned" and nparts > 10 and r[0][3] == str(n_a), f"partitioned write {r}")
    check(len(list((part / "data").iterdir())) == nparts, "one folder per gid_prefix")
    check(all(len(list(d.glob("*.parquet"))) == 1 for d in (part / "data").iterdir()), "one file per partition")
    check(sorted_files(con, f"{part}/data/*/*.parquet"), "partition files sorted by packed_id")
    bad = con.execute(f"SELECT count(*) FROM read_parquet('{part}/data/*/*.parquet', hive_partitioning = true) WHERE left(gid, 9) <> gid_prefix").fetchone()[0]
    check(bad == 0, "rows are in their gid_prefix folder")
    m = json.loads((part / "manifest.json").read_text())
    check(m["storage"]["partitioning"] == ["domain_code", "level", "gid_prefix:9"], "partitioning in manifest")
    got = con.execute(f"SELECT count(*), sum(population) FROM read_cell_dataset('{part}')").fetchone()
    check(got[0] == n_a, f"partitioned read {got}")
    q = con.execute(f"SELECT count(*), sum(population), round(sum(score), 6) FROM query_cell_dataset('{part}', geometry := ?, min_coverage := 0.5)", [QUERY]).fetchone()
    check(q == want_q, f"partitioned query {q} != {want_q}")
    qb = con.execute(f"SELECT count(*) FROM query_cell_dataset('{part}', bbox := [106.85, -6.21, 106.87, -6.19])").fetchone()[0]
    qf = con.execute(f"SELECT count(*) FROM query_cell_dataset('{flat}', bbox := [106.85, -6.21, 106.87, -6.19])").fetchone()[0]
    for path in (flat, part):   # with_geometry on flat and partitioned
        ng = con.execute(f"SELECT count(*) FROM query_cell_dataset('{path}', bbox := [106.85, -6.21, 106.87, -6.19], with_geometry := 'geojson') "
                         f"WHERE cell_geometry = cell_to_geometry(packed_id, 'EPSG:4326', 'geojson')").fetchone()[0]
        assert ng == qf, (path, ng, qf)
    check(qb == qf and qb > 0, f"bbox query partitioned {qb} vs flat {qf}")
    out["partitioned"] = nparts

    # ---- line and point inputs ----
    line = "LINESTRING (106.84 -6.21, 106.85 -6.20, 106.87 -6.205)"
    want_l = con.execute(f"SELECT count(*), sum(population), round(sum(length_ratio), 9) FROM ({source(AREA_A)}) d "
                         f"JOIN line_to_cells('ID', '{line}', 12) k USING (packed_id)").fetchone()
    for ds in (flat, part):
        got_l = con.execute(f"SELECT count(*), sum(population), round(sum(length_ratio), 9) FROM query_cell_dataset('{ds}', geometry := ?)", [line]).fetchone()
        check(got_l == want_l and got_l[0] > 10, f"line query {ds.name}: {got_l} != {want_l}")
    pts = "MULTIPOINT ((106.85 -6.20), (106.85 -6.20), (106.86 -6.21), (110.0 -7.0))"
    got_p = con.execute(f"SELECT gid, population, point_count FROM query_cell_dataset('{part}', geometry := ?) ORDER BY gid", [pts]).fetchall()
    want_p = con.execute(f"SELECT d.gid, d.population FROM read_cell_dataset('{flat}') d WHERE d.packed_id IN "
                         f"(point_to_cell('ID', 106.85, -6.20, 12).packed_id, point_to_cell('ID', 106.86, -6.21, 12).packed_id) ORDER BY d.gid").fetchall()
    check([(g, v) for g, v, _ in got_p] == want_p and sorted(c for *_, c in got_p) == [1, 2], f"point query {got_p} vs {want_p}")
    check(con.execute("SELECT count(*) FROM line_to_cells('ID', 'LINESTRING (106.80 -6.30, 106.82 -6.28)', 12)").fetchone()[0] > 50, "line_to_cells")
    out["line_point"] = (want_l[0], len(got_p))

    # ---- iceberg: create, append, read newest, reject duplicates ----
    ice = tmp / "ice"
    r = con.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', partition_level := 7)", [source(AREA_A), str(ice)]).fetchall()
    check(r[0][0] == "iceberg" and r[0][3] == str(n_a), f"iceberg create {r}")
    r2 = con.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'append')", [source(AREA_B), str(ice)]).fetchall()
    check(r2[0][3] == str(n_b), f"iceberg append {r2}")
    got = con.execute(f"SELECT count(*), count(DISTINCT packed_id) FROM read_cell_dataset('{ice}')").fetchone()
    check(got == (n_a + n_b, n_a + n_b), f"iceberg read after append {got} (want {n_a + n_b})")
    con.execute("LOAD iceberg")
    direct = con.execute(f"SELECT count(*) FROM iceberg_scan('{ice}')").fetchone()[0]
    check(direct == n_a + n_b, f"iceberg_scan via version-hint {direct}")
    first = r[0][1]
    old = con.execute(f"SELECT count(*) FROM read_cell_dataset('{first}')").fetchone()[0]
    check(old == n_a, "the first metadata file still reads the first snapshot")
    q = con.execute(f"SELECT count(*), sum(population), round(sum(score), 6) FROM query_cell_dataset('{ice}', geometry := ?, min_coverage := 0.5)", [QUERY]).fetchone()
    check(q == want_q, f"iceberg query {q} != {want_q}")
    meta = json.loads(sorted((ice / "metadata").glob("*.metadata.json"))[-1].read_text())
    cm = json.loads(meta["properties"]["geosquare.manifest"])
    check(cm["storage"]["row_count"] == n_a + n_b and len(meta["snapshots"]) == 2, "iceberg manifest row_count and snapshots")
    raises(con, "SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'append')", [source(AREA_A), str(ice)], "already in the table")
    raises(con, "SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg')", [source(AREA_B), str(ice)], "append")
    out["iceberg"] = len(meta["snapshots"])

    # ---- the same datasets over plain HTTP (no directory listing) ----
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(tmp))
    handler.log_message = lambda *a: None
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_address[1]}"
    try:
        con.execute("INSTALL httpfs; LOAD httpfs")
        for name in ("flat", "part", "ice"):
            local = con.execute(f"SELECT count(*), sum(population) FROM read_cell_dataset('{tmp / name}')").fetchone()
            remote = con.execute(f"SELECT count(*), sum(population) FROM read_cell_dataset('{base}/{name}')").fetchone()
            check(local == remote, f"{name} over HTTP {remote} != local {local}")
            q_local = con.execute(f"SELECT count(*), sum(population) FROM query_cell_dataset('{tmp / name}', geometry := ?)", [QUERY]).fetchone()
            q_remote = con.execute(f"SELECT count(*), sum(population) FROM query_cell_dataset('{base}/{name}', geometry := ?)", [QUERY]).fetchone()
            check(q_local == q_remote, f"{name} query over HTTP {q_remote} != local {q_local}")
        check((part / "gid_prefixes.json").exists(), "partition index written")
        out["http"] = "ok"
    finally:
        server.shutdown()

    # ---- validation errors ----
    raises(con, "SELECT * FROM line_to_cells('ID', 'POLYGON ((0 0, 1 0, 1 1, 0 0))', 12)", None, "LINESTRING")
    raises(con, f"SELECT * FROM write_cell_dataset(?, ?)", [source(AREA_A), str(flat)], "already exists")
    raises(con, "SELECT * FROM write_cell_dataset(?, ?)", [f"SELECT * FROM (SELECT * FROM ({source(AREA_A)}) ORDER BY packed_id LIMIT 3) UNION ALL SELECT * FROM (SELECT * FROM ({source(AREA_A)}) ORDER BY packed_id LIMIT 3)", str(tmp / "dup")], "duplicate")
    raises(con, "SELECT * FROM write_cell_dataset(?, ?)",
           [f"SELECT packed_id, 1 AS v FROM polygon_to_cells('ID', '{AREA_A}', 12) UNION ALL SELECT packed_id, 1 FROM polygon_to_cells('ID', '{AREA_A}', 11)", str(tmp / "mix")], "level")
    raises(con, "SELECT * FROM write_cell_dataset('SELECT 1 AS v', ?)", [str(tmp / "noid")], "gid or a packed_id")
    raises(con, "SELECT * FROM write_cell_dataset(?, ?)", [f"SELECT gid, packed_id + 2 AS packed_id, 1 AS v FROM polygon_to_cells('ID', '{AREA_A}', 12)", str(tmp / "bad")], "packed")
    raises(con, "SELECT * FROM write_cell_dataset(?, ?)", [f"SELECT * FROM (SELECT gid, lead(packed_id) OVER (ORDER BY packed_id) AS packed_id, 1 AS v FROM polygon_to_cells('ID', '{AREA_A}', 12)) WHERE packed_id IS NOT NULL", str(tmp / "bad2")], "does not match")
    raises(con, "SELECT * FROM write_cell_dataset(?, ?, mode := 'append')", [source(AREA_A), str(tmp / "x")], "only for layout")
    # gid-only source: packed_id is derived; overwrite replaces the folder
    r = con.execute("SELECT * FROM write_cell_dataset(?, ?, mode := 'overwrite', domain_code := 'ID')",
                    [f"SELECT gid, 1 AS v FROM polygon_to_cells('ID', '{AREA_B}', 12)", str(flat)]).fetchall()
    check(r[0][3] == str(n_b) and con.execute(f"SELECT count(*) FROM read_cell_dataset('{flat}')").fetchone()[0] == n_b, "overwrite")
    print(f"duckdb {version}: datasets OK {out} (A={n_a} B={n_b} cells, query={want_q[0]} cells) in {tmp}")


if __name__ == "__main__":
    main()
