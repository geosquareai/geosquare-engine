"""append and overwrite keep the metadata of the dataset they write to (units, rules,
semantics, source, coverage, notes, title, descriptions); what a write passes wins.

    python tests/test_keep_metadata.py build/linux_amd64/geosquare_v2.duckdb_extension
"""
import json, sys, tempfile
import duckdb

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_keep_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
cells = lambda x: f"polygon_to_cells('ID', 'POLYGON (({x} -6.22, {x + 0.01} -6.22, {x + 0.01} -6.21, {x} -6.21, {x} -6.22))', 12, min_coverage := 0.99)"
first = dict(units=json.dumps({"people": "people"}), column_descriptions=json.dumps({"people": "People"}), title="T",
             aggregation_rule="sum", value_semantics="COUNT", source=json.dumps({"source_dataset": "s3://a"}),
             coverage=json.dumps({"boundary_policy": "CENTROID_COVERED"}), notes="first")
args = ", ".join(f"{k} := ?" for k in first)
state = lambda p: (c.execute("SELECT title, source->>'source_dataset', coverage->>'boundary_policy', provenance->>'notes', "
                             "provenance->>'aggregation_rule' FROM cell_dataset_info(?)", [p]).fetchone(),
                   c.execute("SELECT \"column\", semantics, aggregation_rule, unit, description FROM cell_dataset_columns(?) ORDER BY 1", [p]).fetchall())
want = (("T", "s3://a", "CENTROID_COVERED", "first", "sum"), [("people", "COUNT", "sum", "people", "People")])
for layout in ("iceberg", "partitioned", "flat"):
    p = f"{d}/{layout}"
    c.execute(f"SELECT * FROM write_cell_dataset('SELECT packed_id, 1.0 AS people FROM {cells(106.80).replace(chr(39), chr(39)*2)}', ?, layout := ?, partition_level := 7, {args})",
              [p, layout, *first.values()]).fetchall()
    assert state(p) == want, (layout, state(p))
    if layout == "iceberg":
        c.execute(f"SELECT * FROM write_cell_dataset('SELECT packed_id, 2.0 AS people FROM {cells(106.82).replace(chr(39), chr(39)*2)}', ?, layout := 'iceberg', mode := 'append')", [p]).fetchall()
        assert state(p) == want, ("append", state(p))
    # update by overwrite, no metadata passed: kept
    c.execute(f"SELECT * FROM write_cell_dataset('SELECT packed_id, people * 2 AS people FROM read_cell_dataset(''{p}'')', ?, layout := ?, mode := 'overwrite', partition_level := 7)", [p, layout]).fetchall()
    assert state(p) == want, ("overwrite", layout, state(p))
    # a new column: no inherited metadata for it; passed values win
    c.execute(f"SELECT * FROM write_cell_dataset('SELECT packed_id, people, people / 2 AS hazard FROM read_cell_dataset(''{p}'')', ?, layout := ?, mode := 'overwrite', "
              "partition_level := 7, notes := 'second', aggregation_rule := '{{\"people\": \"sum\", \"hazard\": \"mean\"}}', units := '{{\"hazard\": \"index\"}}')".replace("{{", "{").replace("}}", "}"), [p, layout]).fetchall()
    info, cols = state(p)
    assert info[:4] == ("T", "s3://a", "CENTROID_COVERED", "second"), info
    assert cols == [("hazard", "MEASUREMENT", "mean", "index", None), ("people", "COUNT", "sum", "people", "People")], cols
# create in a new folder inherits nothing
c.execute(f"SELECT * FROM write_cell_dataset('SELECT packed_id, 1.0 AS people FROM {cells(106.80).replace(chr(39), chr(39)*2)}', ?)", [f"{d}/fresh"]).fetchall()
assert state(f"{d}/fresh")[0][0] is None and state(f"{d}/fresh")[1][0][3] is None
print("metadata kept OK")
