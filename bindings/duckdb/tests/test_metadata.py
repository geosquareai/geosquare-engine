"""Descriptive metadata: write with units/descriptions/source/coverage/period, update it later
without touching data, read it back; the cell manifest stays valid for geosquare-v2.

    python tests/test_metadata.py build/linux_amd64/geosquare_v2.duckdb_extension   # needs pyiceberg, geosquare_v2
"""
import glob, json, sys, tempfile
import duckdb
from pyiceberg.table import StaticTable
from geosquare_v2.cell_manifest import CellDatasetManifest

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_meta_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
src = ("SELECT packed_id, (packed_id % 97)::DOUBLE AS people_a, (packed_id % 13)::DOUBLE AS people_a_night, "
       "(packed_id % 7)::DOUBLE / 10 AS hazard_x FROM polygon_to_cells('ID', "
       "'POLYGON ((106.80 -6.21, 106.82 -6.21, 106.82 -6.19, 106.80 -6.19, 106.80 -6.21))', 12)")
units = json.dumps({"people_*": "people", "hazard_x": "index 0-1"})
desc = json.dumps({"people_a": "people of segment A, all day", "people_*_night": "people of segment A, night", "hazard_x": "hazard X"})
source = json.dumps({"format": "parquet", "source_dataset": "s3://example/a", "source_crs": "EPSG:4326", "source_row_count": 123})
coverage = json.dumps({"assignment_method": "point_in_cell", "boundary_policy": "CENTROID_COVERED"})
for layout in ("flat", "partitioned", "iceberg"):
    p = f"{d}/{layout}"
    c.execute("SELECT * FROM write_cell_dataset(?, ?, layout := ?, partition_level := 7, units := ?, column_descriptions := ?, "
              "title := 'Test', description := 'A test dataset', data_period := '2023', data_version := 'v2023', source := ?, coverage := ?)",
              [src, p, layout, units, desc, source, coverage]).fetchall()
    info = c.execute("SELECT title, description, data_period, data_version, source->>'source_dataset', coverage->>'boundary_policy', "
                     "manifest FROM cell_dataset_info(?)", [p]).fetchone()
    assert info[:6] == ("Test", "A test dataset", "2023", "v2023", "s3://example/a", "CENTROID_COVERED"), info
    CellDatasetManifest.from_dict(json.loads(info[6]))          # still a valid v2 cell manifest
    cols = {r[0]: r for r in c.execute("SELECT * FROM cell_dataset_columns(?)", [p]).fetchall()}
    assert cols["people_a"][4] == "people" and cols["hazard_x"][4] == "index 0-1", cols
    assert cols["people_a_night"][5] == "people of segment A, night" and cols["people_a"][5] == "people of segment A, all day", cols
    # update later: data untouched, metadata changed
    before = c.execute("SELECT count(*), sum(people_a) FROM read_cell_dataset(?)", [p]).fetchone()
    r = c.execute("SELECT * FROM update_cell_metadata(?, units := ?, data_version := 'v2023r2', column_descriptions := ?, "
                  "coverage := '{\"min_coverage\": 0.5}')", [p, json.dumps({"hazard_x": "score"}), json.dumps({"hazard_x": "hazard X (0-1)"})]).fetchall()
    after = c.execute("SELECT count(*), sum(people_a) FROM read_cell_dataset(?)", [p]).fetchone()
    assert before == after, (before, after)
    info = c.execute("SELECT title, data_version, coverage->>'min_coverage', coverage->>'boundary_policy', manifest FROM cell_dataset_info(?)", [p]).fetchone()
    assert info[:4] == ("Test", "v2023r2", "0.5", "CENTROID_COVERED"), info
    CellDatasetManifest.from_dict(json.loads(info[4]))
    cols = {r[0]: r for r in c.execute("SELECT * FROM cell_dataset_columns(?)", [p]).fetchall()}
    assert cols["hazard_x"][4] == "score" and cols["hazard_x"][5] == "hazard X (0-1)" and cols["people_a"][4] == "people", cols
    # a key outside the contract is refused
    try:
        c.execute("SELECT * FROM update_cell_metadata(?, source := '{\"provider\": \"x\"}')", [p]).fetchall()
        raise SystemExit("unknown source key accepted")
    except duckdb.Error as e:
        assert "not a field of the cell manifest" in str(e), e
# Iceberg: column docs in the schema, readable by pyiceberg; query_cell_dataset still works
meta = sorted(glob.glob(f"{d}/iceberg/metadata/*.metadata.json"))[-1]
t = StaticTable.from_metadata(meta)
docs = {f.name: f.doc for f in t.schema().fields}
assert docs["hazard_x"] == "hazard X (0-1)" and docs["people_a"] == "people of segment A, all day", docs
assert t.properties["comment"] == "A test dataset"
assert t.scan().to_arrow().num_rows == c.execute(f"SELECT count(*) FROM read_cell_dataset('{d}/iceberg')").fetchone()[0]
n = c.execute(f"SELECT count(*) FROM query_cell_dataset('{d}/iceberg', bbox := [106.80, -6.21, 106.82, -6.19])").fetchone()[0]
assert n > 0
print("metadata OK (flat, partitioned, iceberg)", d)
