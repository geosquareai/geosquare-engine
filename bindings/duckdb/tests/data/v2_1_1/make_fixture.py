"""Write a synthetic manifest 1.1 dataset with the Python library, for the engine's hash test.

Needs geosquare-v2 with manifest 1.1 support (1.3.0 or later) and pyarrow:

    python make_fixture.py <output folder>

The GIDs come from the registry for points in Jakarta, so the dataset is valid. The output is a
flat dataset: manifest.json and data/part-00000.parquet. Its storage.data_files entry records the
content hash that Python computes (docs/dataset-metadata.md, section 3.1). The engine test checks
that DuckDB reproduces that hash from the same file.
"""

import base64
import json
import os
import sys
from pathlib import Path

import pyarrow as pa

import geosquare_v2 as g
from geosquare_v2 import CellDataset, DbRegistryLoader, GeosquareService, read_cell_dataset
from geosquare_v2.cell_metadata import data_file_entry

OUT = Path(sys.argv[1])
POINTS = [(106.80 + 0.002 * (i % 25), -6.25 + 0.002 * (i // 25)) for i in range(60)]
FIELDS = [{"name": "population", "parquet_type": "int64"}]

pkg = Path(os.path.dirname(g.__file__))
if "GEOSQUARE_TRUST" not in os.environ:
    raise SystemExit("set GEOSQUARE_TRUST to the registry-trust.json of geosquare-v2")
keys = json.load(open(os.environ["GEOSQUARE_TRUST"]))
trust = {k: base64.b64decode(v, validate=True) for k, v in keys.items()}
registry = DbRegistryLoader(
    trust,
    pkg / "db",
    boundary_root=pkg / "data" / "registry",
    verify_boundaries=True,
).load()
service = GeosquareService(registry, boundary_root=pkg / "data" / "registry")

gids = []
for lon, lat in POINTS:
    gids.append(str(service.point_to_cell("ID", lon, lat, 12).gid))
counts = {}
for gid in gids:
    counts[gid] = counts.get(gid, 0) + 1
ordered = sorted(counts)
table = pa.table(
    {
        "gid": pa.array(ordered, type=pa.string()),
        "population": pa.array([counts[gid] for gid in ordered], type=pa.int64()),
    }
)
values = [counts[gid] for gid in ordered]
document = {
    "manifest_version": "1.1",
    "dataset_type": "cell_values",
    "grid_system": "geosquare",
    "grid_version": "v2",
    "domain_code": "ID",
    "domain_id": 1,
    "level": 12,
    "profile_version": "2.2.0-explicit-datum",
    "registry_version": "2.0.0",
    "value_fields": [
        {
            "name": "population",
            "parquet_type": "int64",
            "logical_type": "numeric",
            "semantics": "COUNT",
            "unit": "persons",
            "nullable": False,
            "aggregation_rule": None,
            "space_aggregation": "sum",
            "space_weight": None,
            "null_share": 0.0,
            "min_value": min(values),
            "max_value": max(values),
        }
    ],
    "identity": {
        "gid_column": "gid",
        "uri_prefix": "geosquare:v2:ID:",
        "packed_id_column": None,
        "x_idx_column": None,
        "y_idx_column": None,
        "unique_gid": True,
    },
    "coverage": {
        "assignment_method": "given_gid",
        "boundary_policy": None,
        "boundary_sha256": None,
        "coverage_mode": None,
        "min_coverage": None,
        "length_mode": None,
    },
    "source": {
        "format": "synthetic_points",
        "geometry_type": "Point",
        "source_crs": "EPSG:4326",
        "source_id_column": None,
        "source_dataset": None,
        "source_row_count": len(POINTS),
    },
    "extent": {"bbox_wgs84": None, "compact_set_path": None},
    "governance": {
        "title": "Engine hash fixture",
        "owner": "geosquare",
        "source_name": "synthetic",
        "license": "internal-test",
        "share_policy": "internal",
        "refresh": "once",
    },
    "storage": {
        "parquet_compression": "zstd",
        "geometry_column": None,
        "geometry_format": None,
        "geometry_crs": None,
        "partitioning": [],
        "sorted_by": ["gid"],
        "row_count": len(POINTS),
        "unique_cell_count": len(ordered),
        "data_files": [data_file_entry(table, FIELDS, path="data/part-00000.parquet")],
    },
    "provenance": {
        "created_at": "2026-11-23T00:00:00Z",
        "created_by": "geosquare-v2 synthetic fixture",
        "generator": "make_fixture.py",
        "aggregation_rule": None,
        "value_semantics": "COUNT",
        "notes": "synthetic points inside Jakarta, for the engine content hash test",
        "spec_version": "2.0",
        "derived_from": [],
    },
}
# The library writes the Parquet file (with the manifest embedded) and manifest.json, and
# reads them back with the same hash check.
dataset = CellDataset.from_table(table, document, registry=registry)
dataset.write(OUT)
read_cell_dataset(str(OUT), registry=registry)
print("wrote", OUT, "rows", len(POINTS), "cells", len(ordered))
