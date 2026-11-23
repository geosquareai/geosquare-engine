"""Write the synthetic profile 2.1.0 fixtures in this folder, using geosquare-v2 1.0.0.

Needs geosquare-v2[geo,filesystem]==1.0.0 and pyarrow. Run it from any folder:

    python make_fixture.py <registry-trust.json at tag v1.0.0> <ID manifest.json template> <out-dir>

The trust file comes from `git show v1.0.0:release/registry-trust.json` in geosquare-v2. The
manifest template is any profile 2.1.0 manifest, such as geosquare-v2's
tests/fixtures/legacy_v1_0_0/ID/manifest.json. The points are seeded random samples inside each
domain's area of use. Each point keeps the GIDs that 1.0.0 gave it, at levels 12 and 14.
"""

import base64
import collections
import json
import os
import random
import sys

import geosquare_v2 as g
import pyarrow as pa
from geosquare_v2 import CellDataset, DbRegistryLoader, GeosquareService, write_cell_dataset_filesystem

TRUST, TEMPLATE, OUT = sys.argv[1], sys.argv[2], sys.argv[3]
pkg = os.path.dirname(g.__file__)
trust = {k: base64.b64decode(v) for k, v in json.load(open(TRUST)).items()}
reg = DbRegistryLoader(
    trust,
    os.path.join(pkg, "db"),
    boundary_root=os.path.join(pkg, "data", "registry"),
    verify_boundaries=True,
).load()
svc = GeosquareService(reg, boundary_root=os.path.join(pkg, "data", "registry"))
rng = random.Random(2026)

# Sample boxes [west, east, south, north]. VN has a box inside VN-2000 (1)'s area, where 2.1.0 and 2.2.0 differ.
BOXES = {
    "ID": [(106.0, 107.0, -6.6, -6.0)],
    "LA": [(101.0, 106.0, 15.0, 21.0), (100.1, 100.6, 20.5, 22.0)],
    "VN": [(104.3, 107.0, 9.4, 11.0), (104.0, 108.5, 12.0, 19.0)],
}
DOMAIN_ID = {"ID": 1, "VN": 2, "LA": 5}
template = json.load(open(TEMPLATE))
index = {}
for code, parts in BOXES.items():
    samples = []
    for w, e, s, n in parts:
        for _ in range(60):
            lon, lat = round(rng.uniform(w, e), 9), round(rng.uniform(s, n), 9)
            try:
                gids = {
                    12: str(svc.point_to_cell(code, lon, lat, 12).gid),
                    14: str(svc.point_to_cell(code, lon, lat, 14).gid),
                }
            except Exception:
                continue
            samples.append({"lon": lon, "lat": lat, "gid_l12": gids[12], "gid_l14": gids[14]})
    index[code] = samples
    for level in (12, 14):
        counts = collections.Counter(p[f"gid_l{level}"] for p in samples)
        gids = sorted(counts)
        table = pa.table(
            {
                "gid": pa.array(gids, pa.string()),
                "points": pa.array([float(counts[x]) for x in gids], pa.float64()),
            }
        )
        m = json.loads(json.dumps(template))
        m["domain_code"], m["domain_id"], m["level"] = code, DOMAIN_ID[code], level
        m["identity"]["uri_prefix"] = f"geosquare:v2:{code}:"
        m["value_fields"] = [
            {
                "aggregation_rule": None,
                "logical_type": "numeric",
                "name": "points",
                "nullable": False,
                "parquet_type": "double",
                "semantics": "COUNT",
                "unit": None,
            }
        ]
        m["storage"]["row_count"] = len(gids)
        m["storage"]["unique_cell_count"] = len(gids)
        m["source"] = {
            "format": "synthetic_points",
            "geometry_type": "Point",
            "source_crs": "EPSG:4326",
            "source_dataset": None,
            "source_id_column": None,
            "source_row_count": len(samples),
        }
        m["provenance"] = {
            "aggregation_rule": None,
            "created_at": "2026-11-23T00:00:00Z",
            "created_by": "geosquare-v2 1.0.0",
            "generator": "make_fixture.py",
            "notes": "synthetic seeded points (seed 2026), written with geosquare-v2 1.0.0 under PROJ 9.5.1",
            "value_semantics": "COUNT",
        }
        ds = CellDataset.from_table(table, m, registry=reg)
        write_cell_dataset_filesystem(ds, os.path.join(OUT, f"{code}-L{level}"))
    print(code, "points", len(samples), "L12 cells", len({p["gid_l12"] for p in samples}))
with open(os.path.join(OUT, "points.json"), "w") as f:
    json.dump(index, f, indent=1)
