"""Manifest 1.1 (opt-in with manifest_version := '1.1'): what the writer records, and which
operations it refuses until they are implemented (docs/manifest-1.1.md).

    python tests/test_manifest_11.py build/linux_amd64/geosquare_v2.duckdb_extension

When geosquare-v2 can be imported (for example PYTHONPATH=<geosquare-v2>/src), its reader also
validates the flat and partitioned datasets written here.
"""

import glob
import json
import math
import re
import sys
import tempfile

import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_m11_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'; LOAD iceberg")

try:
    from geosquare_v2.dataset import read_cell_dataset as python_read
except ImportError:
    python_read = None

AREA_A = "POLYGON ((106.78 -6.24, 106.84 -6.24, 106.84 -6.20, 106.78 -6.20, 106.78 -6.24))"
AREA_B = "POLYGON ((106.86 -6.24, 106.92 -6.24, 106.92 -6.20, 106.86 -6.20, 106.86 -6.24))"
SEMANTICS = '{"people": "COUNT", "rate": "RATE", "zone": "CATEGORICAL"}'
RULES = '{"people": "sum", "rate": "weighted_mean"}'
WEIGHTS = '{"rate": "people"}'


def check(cond, msg):
    if not cond:
        raise AssertionError(msg)


def refused(sql, params, needle):
    try:
        c.execute(sql, params).fetchall()
    except duckdb.Error as e:
        check(needle in str(e), f"expected {needle!r} in the error, got: {e}")
        return
    raise AssertionError(f"expected a refusal containing {needle!r}: {sql[:100]}")


def cells(area, nulls=False):
    hazard = (
        "CASE WHEN packed_id % 5 = 0 THEN NULL ELSE (packed_id % 7)::DOUBLE / 10 END"
        if nulls
        else "(packed_id % 7)::DOUBLE / 10"
    )
    return (
        f"SELECT packed_id, (packed_id % 97)::BIGINT AS people, {hazard} AS hazard, "
        f"(packed_id % 11)::DOUBLE / 100 AS rate, CASE WHEN packed_id % 3 = 0 THEN 'low' ELSE 'high' END AS zone "
        f"FROM polygon_to_cells('ID', '{area}', 12)"
    )


def manifest(path):
    return json.loads(c.execute("SELECT manifest::VARCHAR FROM cell_dataset_info(?)", [path]).fetchone()[0])


def write_11(query, path, layout="flat", mode=None, partition_level=None):
    """Write with manifest 1.1: stats, rules and governance set; nulls kept."""
    extra = f", mode := '{mode}'" if mode else ""
    extra += f", partition_level := {partition_level}" if partition_level else ""
    c.execute(
        f"SELECT * FROM write_cell_dataset(?, ?, layout := '{layout}', manifest_version := '1.1', "
        "fill_nulls := false, title := 'Test grid', owner := 'data team', source_name := 'synthetic', "
        "license := 'internal-test', derived_from := 'src-a, src-b', value_semantics := ?, "
        f"space_aggregation := ?, space_weight := ?{extra})",
        [query, path, SEMANTICS, RULES, WEIGHTS],
    ).fetchall()


def engine_hashes(path):
    return {
        r[0]: (r[1], r[2])
        for r in c.execute(
            "SELECT file, content_sha256, manifest_content_sha256 FROM cell_dataset_content_hashes(?)", [path]
        ).fetchall()
    }


def bbox_of(path):
    wkts = c.execute("SELECT cell_to_geometry('ID', gid, 'EPSG:4326') FROM read_cell_dataset(?)", [path]).fetchall()
    numbers = [float(x) for w in wkts for x in re.findall(r"-?\d+(?:\.\d+)?(?:[eE][-+]?\d+)?", w[0])]
    lons, lats = numbers[0::2], numbers[1::2]
    return [min(lons), min(lats), max(lons), max(lats)]


def check_values(path, m):
    """Each value field's null_share, min and max equal what DuckDB reads back from the dataset."""
    n = c.execute("SELECT count(*) FROM read_cell_dataset(?)", [path]).fetchone()[0]
    for f in m["value_fields"]:
        name = f["name"]
        nulls, lo, hi = c.execute(
            f"SELECT count(*) FILTER (WHERE {name} IS NULL), min({name}), max({name}) FROM read_cell_dataset(?)",
            [path],
        ).fetchone()
        check(math.isclose(f["null_share"], nulls / n, rel_tol=1e-12, abs_tol=1e-15), (name, f["null_share"], nulls / n))
        if f["logical_type"] == "numeric":
            check(f["min_value"] == lo and f["max_value"] == hi, (name, f["min_value"], lo, f["max_value"], hi))
        else:
            check(f["min_value"] is None and f["max_value"] is None, name)


def check_bbox(path, m):
    box = m["extent"]["bbox_wgs84"]
    check(len(box) == 4, box)
    real = bbox_of(path)
    for got, want in zip(box, real):
        check(math.isclose(got, want, abs_tol=1e-9), (box, real))


def check_files(path, m, files):
    """Every storage.data_files entry matches the file's rows, and the engine's own hash of them."""
    listed = {f["path"]: f for f in m["storage"]["data_files"]}
    on_disk = {p.split(f"{path}/", 1)[1] for p in glob.glob(f"{path}/data/**/*.parquet", recursive=True)}
    check(set(listed) == on_disk == set(files), (sorted(listed), sorted(on_disk)))
    hashes = engine_hashes(path)
    for rel, f in listed.items():
        check(f["content_sha256"] == hashes[rel][0] == hashes[rel][1], (rel, f, hashes[rel]))
        rows = c.execute(f"SELECT count(*) FROM read_parquet('{path}/{rel}')").fetchone()[0]
        check(f["row_count"] == rows, (rel, f["row_count"], rows))
    check(m["storage"]["row_count"] == sum(f["row_count"] for f in listed.values()), "storage.row_count")


def check_python(path):
    if python_read is None:
        print("skip: geosquare_v2 not importable, so the Python reader check did not run")
        return
    python_read(path).validate()


def test_default_stays_1_0():
    p = f"{d}/flat10"
    c.execute(
        "SELECT * FROM write_cell_dataset(?, ?, layout := 'flat', value_semantics := ?)",
        [cells(AREA_A), p, '{"people": "COUNT"}'],
    ).fetchall()
    m = manifest(p)
    check(m["manifest_version"] == "1.0", m["manifest_version"])
    check("governance" not in m and "extent" not in m, "1.0 has no 1.1 blocks")
    check("space_aggregation" not in m["value_fields"][0], "1.0 has no space rules")


def test_flat_1_1():
    p = f"{d}/flat11"
    write_11(cells(AREA_A, nulls=True), p)
    m = manifest(p)
    check(m["manifest_version"] == "1.1", m["manifest_version"])
    check(
        m["governance"]
        == {
            "title": "Test grid",
            "owner": "data team",
            "source_name": "synthetic",
            "license": "internal-test",
            "share_policy": "internal",
            "refresh": "once",
        },
        m["governance"],
    )
    check(m["provenance"]["spec_version"] == "2.0", m["provenance"])
    check(m["provenance"]["derived_from"] == ["src-a", "src-b"], m["provenance"])
    check(m["extent"]["compact_set_path"] is None, m["extent"])
    rules = {f["name"]: (f["space_aggregation"], f["space_weight"]) for f in m["value_fields"]}
    check(
        rules == {"people": ("sum", None), "hazard": ("none", None), "rate": ("weighted_mean", "people"), "zone": ("none", None)},
        rules,
    )
    hazard = next(f for f in m["value_fields"] if f["name"] == "hazard")
    check(hazard["null_share"] > 0, "hazard has NULLs (fill_nulls := false keeps them)")
    check_values(p, m)
    check_bbox(p, m)
    check_files(p, m, ["data/part-00000.parquet"])
    check_python(p)


def test_refusals():
    p = f"{d}/refused"
    q = cells(AREA_A)
    base = "SELECT * FROM write_cell_dataset(?, ?, layout := 'flat', manifest_version := '1.1', value_semantics := ?"
    refused(base + ", space_aggregation := ?, space_weight := ?)", [q, p, SEMANTICS, '{"rate": "weighted_mean"}', '{"rate": "hazard"}'], "must name a COUNT or TOTAL")
    refused(base + ", space_aggregation := ?)", [q, p, SEMANTICS, '{"rate": "weighted_mean"}'], "weighted_mean needs a space_weight")
    refused(base + ", share_policy := ?)", [q, p, SEMANTICS, "secret"], "share_policy must be one of")
    refused(base + ", refresh := ?)", [q, p, SEMANTICS, "hourly"], "refresh must be one of")
    refused(
        "SELECT * FROM write_cell_dataset(?, ?, layout := 'flat', manifest_version := '1.1', value_semantics := ?)",
        [f"SELECT packed_id, packed_id % 2 = 0 AS flag FROM polygon_to_cells('ID', '{AREA_A}', 12)", p, '{"flag": "CATEGORICAL"}'],
        "cannot be a manifest 1.1 value",
    )
    refused(
        "SELECT * FROM write_cell_dataset(?, ?, layout := 'flat', manifest_version := '1.1')",
        [f"SELECT packed_id, 1::BIGINT AS n FROM polygon_to_cells('VN', '{AREA_A}', 12, allow_candidate_crs := 1)", p],
        "not released for lon/lat",
    )
    refused(
        "SELECT * FROM write_cell_dataset(?, ?, layout := 'flat', manifest_version := '2.0')", [q, p], "manifest_version must be"
    )
    refused(
        "SELECT * FROM write_cell_dataset(?, ?, layout := 'flat', space_aggregation := ?)", [q, p, RULES], "need rollup_level"
    )
    check(not glob.glob(f"{p}/data/**/*.parquet", recursive=True), "a refused write wrote data")


def test_refused_overwrite_keeps_the_old_folder():
    p = f"{d}/keep"
    write_11(cells(AREA_A), p)
    before = c.execute("SELECT count(*) FROM read_cell_dataset(?)", [p]).fetchone()[0]
    nan = f"SELECT packed_id, 'NaN'::DOUBLE AS n FROM polygon_to_cells('ID', '{AREA_A}', 12)"
    refused(
        "SELECT * FROM write_cell_dataset(?, ?, layout := 'flat', mode := 'overwrite', manifest_version := '1.1', value_semantics := ?)",
        [nan, p, '{"n": "MEASUREMENT"}'],
        "cannot be recorded",
    )
    vn = f"SELECT packed_id, 1::BIGINT AS n FROM polygon_to_cells('VN', '{AREA_A}', 12, allow_candidate_crs := 1)"
    refused(
        "SELECT * FROM write_cell_dataset(?, ?, layout := 'flat', mode := 'overwrite', manifest_version := '1.1', value_semantics := ?)",
        [vn, p, '{"n": "COUNT"}'],
        "not released for lon/lat",
    )
    check(c.execute("SELECT count(*) FROM read_cell_dataset(?)", [p]).fetchone()[0] == before, "refused overwrite changed the folder")
    check(manifest(p)["manifest_version"] == "1.1", "refused overwrite changed the manifest")


def test_partitioned_compact_keeps_1_1():
    p = f"{d}/part11"
    union = f"SELECT * FROM ({cells(AREA_A)}) UNION ALL SELECT * FROM ({cells(AREA_B)})"
    write_11(union, p, layout="partitioned", partition_level=7)
    before = manifest(p)
    rows_before = c.execute("SELECT count(*), sum(people) FROM read_cell_dataset(?)", [p]).fetchone()
    c.execute("SELECT * FROM compact_cell_dataset(?, partition_level := 6)", [p]).fetchall()
    m = manifest(p)
    check(m["manifest_version"] == "1.1", "compact keeps 1.1")
    check(m["governance"] == before["governance"] and m["provenance"]["derived_from"] == ["src-a", "src-b"], "governance kept")
    check(m["value_fields"] and [f["space_aggregation"] for f in m["value_fields"]] == [f["space_aggregation"] for f in before["value_fields"]], "rules kept")
    check(c.execute("SELECT count(*), sum(people) FROM read_cell_dataset(?)", [p]).fetchone() == rows_before, "data kept")
    check(len(m["storage"]["data_files"]) != len(before["storage"]["data_files"]), "compact changed the files")
    check_values(p, m)
    check_bbox(p, m)
    check_files(p, m, [f["path"] for f in m["storage"]["data_files"]])
    # The Python reader (1.3.0) reads flat folders only, so partitioned folders are not checked there.


def test_iceberg_append_merges_1_1():
    p = f"{d}/ice11"
    write_11(cells(AREA_A, nulls=True), p, layout="iceberg", partition_level=7)
    c.execute(
        "SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'append', fill_nulls := false)",
        [cells(AREA_B, nulls=True), p],
    ).fetchall()
    m = manifest(p)
    check(m["manifest_version"] == "1.1", "append keeps 1.1 without asking")
    check(m["governance"]["owner"] == "data team", "append keeps governance")
    rules = {f["name"]: f["space_aggregation"] for f in m["value_fields"]}
    check(rules == {"people": "sum", "hazard": "none", "rate": "weighted_mean", "zone": "none"}, rules)
    check(next(f for f in m["value_fields"] if f["name"] == "rate")["space_weight"] == "people", "append keeps the weight")
    check_values(p, m)
    check_bbox(p, m)
    files = [p_.split(f"{p}/", 1)[1] for p_ in glob.glob(f"{p}/data/**/*.parquet", recursive=True)]
    check_files(p, m, files)
    check(len(files) > 1, "two areas make several partitions")
    c.execute("SELECT * FROM update_cell_metadata(?, title := 'Renamed grid')", [p]).fetchall()
    m = manifest(p)
    title = c.execute("SELECT title FROM cell_dataset_info(?)", [p]).fetchone()[0]
    check(m["governance"]["title"] == "Renamed grid" and title == "Renamed grid", (m["governance"], title))
    check(m["governance"]["owner"] == "data team", "a metadata update keeps the owner")


def test_refused_operations_on_1_1():
    p = f"{d}/ice11"  # built by the test above
    refused("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'upsert')", [cells(AREA_A), p], "not available for manifest 1.1")
    refused("SELECT * FROM alter_cell_dataset(?, add_columns := ?)", [p, '{"x": "DOUBLE"}'], "no column changes")
    refused("SELECT * FROM alter_cell_dataset(?, drop_columns := 'zone')", [p], "no column changes")
    refused("SELECT * FROM alter_cell_dataset(?, rename_columns := ?)", [p, '{"people": "persons"}'], "no column changes")
    refused("SELECT * FROM update_cell_metadata(?, title := 'x')", [f"{d}/flat11"], "cannot change a manifest 1.1 folder")
    # append with 1.0 onto a 1.1 table would drop the hashes
    refused(
        "SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'append', manifest_version := '1.0')",
        [cells(AREA_B), p],
        "is manifest 1.1",
    )
    # append with 1.1 onto a 1.0 table is refused until the table is overwritten
    p10 = f"{d}/ice10"
    c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', partition_level := 7)", [cells(AREA_A), p10]).fetchall()
    refused(
        "SELECT * FROM write_cell_dataset(?, ?, layout := 'iceberg', mode := 'append', manifest_version := '1.1')",
        [cells(AREA_B), p10],
        "is manifest 1.0",
    )
    check(manifest(p10)["manifest_version"] == "1.0", "the refused append left the 1.0 table alone")


test_default_stays_1_0()
test_flat_1_1()
test_refusals()
test_refused_overwrite_keeps_the_old_folder()
test_partitioned_compact_keeps_1_1()
test_iceberg_append_merges_1_1()
test_refused_operations_on_1_1()
print("manifest 1.1 tests passed")
