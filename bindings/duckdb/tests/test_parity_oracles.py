"""The spec-parity functions against the geosquare-v2 Python reference, cell by cell:
aggregate_geometries_to_cells vs aggregate_geometry_table_to_cells (with and without an
operational boundary), boundary_coverage / boundary_cell_matches / boundary_covers_point vs
OperationalBoundary on the registry's country boundaries, polygon_crosswalk and
validate_crosswalk vs geosquare_v2.crosswalk, and compact sets read and written both ways.
Needs geosquare_v2 with its geo extras (shapely, pyproj, pandas).

    python tests/test_parity_oracles.py build/linux_amd64/geosquare_v2.duckdb_extension ../geosquare-v2
"""
import base64, json, random, sys, tempfile
from pathlib import Path
import duckdb, pandas as pd
from shapely import wkt as W
from shapely.geometry import shape
from shapely.ops import transform, unary_union
from geosquare_v2 import DbRegistryLoader, GeosquareService, aggregate_geometry_table_to_cells
from geosquare_v2.boundary import OperationalBoundary
from geosquare_v2.compact_store import read_compact_set, write_compact_set
from geosquare_v2.crosswalk import polygon_crosswalk_rows, validate_crosswalk
from geosquare_v2.geometry import profile_grid_crs

EXT, V2 = sys.argv[1], Path(sys.argv[2])
trust = {k: base64.b64decode(v, validate=True) for k, v in json.loads((V2 / "release/registry-trust.json").read_text()).items()}
REG = V2 / "src/geosquare_v2/data/registry"
svc = GeosquareService(DbRegistryLoader(trust, V2 / "src/geosquare_v2/db", boundary_root=REG, verify_boundaries=True).load(), boundary_root=REG, allow_candidate_crs=True)
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{EXT}'")
d = tempfile.mkdtemp(prefix="gs_parity_oracle_")
TOL = 1e-7  # polygon areas: Rust clipping vs shapely differ in the last digits


def boundary(code, fname):
    ctx = svc._context(code)
    doc = json.loads((REG / "boundaries" / fname).read_text())
    geom = unary_union([shape(f["geometry"]) for f in doc["features"]]) if doc.get("type") == "FeatureCollection" else shape(doc)
    return ctx, OperationalBoundary.from_geometry(geom, source_crs="EPSG:4326", grid_crs=profile_grid_crs(ctx.profile)), str(REG / "boundaries" / fname)


# --- geometry allocation ----------------------------------------------------------------
feats = [
    ("POLYGON ((106.800 -6.210, 106.812 -6.210, 106.812 -6.199, 106.800 -6.199, 106.800 -6.210))", 120.0, 3.5, "forest"),
    ("POLYGON ((106.806 -6.205, 106.818 -6.205, 106.818 -6.194, 106.806 -6.194, 106.806 -6.205))", 80.0, 7.0, "urban"),
    ("LINESTRING (106.801 -6.209, 106.817 -6.195)", 40.0, 1.0, "road"),
    ("MULTIPOINT ((106.803 -6.207), (106.803 -6.207), (106.815 -6.196))", 9.0, 2.0, "urban"),
]
src = pd.DataFrame({"fid": [f"f{i}" for i in range(len(feats))], "geometry": [W.loads(f[0]) for f in feats],
                    "pop": [f[1] for f in feats], "dens": [f[2] for f in feats], "lc": [f[3] for f in feats]})
c.execute("CREATE TABLE f (fid VARCHAR, geometry VARCHAR, pop DOUBLE, dens DOUBLE, lc VARCHAR)")
c.executemany("INSERT INTO f VALUES (?, ?, ?, ?, ?)", [[f"f{i}", *x] for i, x in enumerate(feats)])
ours = c.execute("""SELECT * FROM aggregate_geometries_to_cells('SELECT * FROM f', domain_code := 'ID', level := 12, id_column := 'fid',
  value_semantics := '{"pop": "TOTAL", "dens": "DENSITY", "lc": "CATEGORICAL"}')""").df().set_index("gid")
opts = {"geometry_column": "geometry", "source_id_column": "fid", "source_crs": "EPSG:4326", "keep_columns": ["pop", "dens", "lc"]}
for col, sem in [("pop", "TOTAL"), ("dens", "DENSITY"), ("lc", "CATEGORICAL")]:
    py = aggregate_geometry_table_to_cells(src, svc, "ID", 12, value_semantics=sem, value_column=col, geometry_options=opts).set_index("gid")
    assert set(py.index) == set(ours.index), col
    if sem == "CATEGORICAL":
        assert (ours[col] == py[col].reindex(ours.index)).all()
        assert (ours["lc_share"] - py["category_share"].reindex(ours.index)).abs().max() < TOL
    else:
        assert ((ours[col] - py[col].reindex(ours.index)).abs() <= TOL * py[col].reindex(ours.index).abs().clip(lower=1)).all(), col
    assert (ours["feature_count"] == py["feature_count"].reindex(ours.index)).all()
cnt = aggregate_geometry_table_to_cells(src, svc, "ID", 12, value_semantics="COUNT", geometry_options=opts).set_index("gid")
ours_c = c.execute("SELECT gid, pop FROM aggregate_geometries_to_cells('SELECT geometry, pop FROM f', domain_code := 'ID', level := 12, value_semantics := 'COUNT')").df().set_index("gid")
assert (ours_c["pop"] == cnt["count"].reindex(ours_c.index)).all()
print("geometry allocation: identical on", len(ours), "cells")

# --- boundary predicates ----------------------------------------------------------------
for code, fname, level in [("SG", "SG_simplified.geojson", 9), ("SG", "SG_full.geojson", 10), ("ID", "ID_simplified.geojson", 6)]:
    ctx, ob, path = boundary(code, fname)
    doc = json.loads(Path(path).read_text())
    coords = [p for f in doc["features"] for poly in (f["geometry"]["coordinates"] if f["geometry"]["type"] == "MultiPolygon" else [f["geometry"]["coordinates"]]) for ring in poly for p in ring]
    lo = (min(p[0] for p in coords), min(p[1] for p in coords), max(p[0] for p in coords), max(p[1] for p in coords))
    random.seed(7)
    pts = [(random.uniform(lo[0], lo[2]), random.uniform(lo[1], lo[3])) for _ in range(300)]
    gids = sorted({c.execute("SELECT point_to_cell(?, ?, ?, ?).gid", [code, x, y, level]).fetchone()[0] for x, y in pts})
    edge = 0
    for g in gids:
        o = c.execute("SELECT boundary_coverage(?, ?, ?), boundary_cell_matches(?, ?, ?, 'CENTROID_COVERED'), boundary_cell_matches(?, ?, ?, 'INTERSECTS'), "
                      "boundary_cell_matches(?, ?, ?, 'MIN_COVERAGE', 0.5)", [code, g, path] * 4).fetchone()
        cov = ob.coverage_ratio(ctx.grid, g)
        assert abs(o[0] - cov) < TOL, (fname, g, o, cov)
        assert o[1:] == (ob.cell_matches(ctx.grid, g, "CENTROID_COVERED"), ob.cell_matches(ctx.grid, g, "INTERSECTS"),
                         ob.cell_matches(ctx.grid, g, "MIN_COVERAGE", min_coverage=0.5)), (fname, g)
        edge += 0 < cov < 1
    for x, y in pts:
        assert c.execute("SELECT boundary_covers_point(?, ?, ?, ?)", [code, x, y, path]).fetchone()[0] == ob.covers_lonlat(x, y, ctx.transformer)
    print(f"boundary {fname}: identical on {len(gids)} cells ({edge} on the line) and {len(pts)} points")

# boundary-filtered allocation (shares renormalised over the kept cells)
ctx, ob, path = boundary("SG", "SG_simplified.geojson")
for pred, t, geoms in [("MIN_COVERAGE", 0.5, ["POLYGON ((103.60 1.20, 103.70 1.20, 103.70 1.30, 103.60 1.30, 103.60 1.20))", "LINESTRING (103.62 1.22, 103.72 1.33)"]),
                       ("INTERSECTS", None, ["POLYGON ((103.95 1.30, 104.05 1.30, 104.05 1.40, 103.95 1.40, 103.95 1.30))"]),
                       ("COVERS_POINT", None, ["MULTIPOINT ((103.80 1.35), (103.65 1.20), (104.00 1.33), (103.82 1.29))"])]:
    s = pd.DataFrame({"geometry": [W.loads(g) for g in geoms], "v": [100.0 + i for i in range(len(geoms))]})
    o = {"geometry_column": "geometry", "source_crs": "EPSG:4326", "keep_columns": ["v"], "boundary": ob, "boundary_policy": pred}
    if t is not None:
        o["boundary_min_coverage"] = t
    py = aggregate_geometry_table_to_cells(s, svc, "SG", 11, value_semantics="TOTAL", value_column="v", geometry_options=o).set_index("gid")["v"]
    c.execute("CREATE OR REPLACE TABLE g (geometry VARCHAR, v DOUBLE)")
    c.executemany("INSERT INTO g VALUES (?, ?)", [[x, 100.0 + i] for i, x in enumerate(geoms)])
    extra = f", boundary_min_coverage := {t}" if t is not None else ""
    ou = c.execute(f"SELECT gid, v FROM aggregate_geometries_to_cells('SELECT * FROM g', domain_code := 'SG', level := 11, value_semantics := 'TOTAL', boundary := ?, boundary_predicate := ?{extra})",
                   [path, pred]).df().set_index("gid")["v"]
    assert set(py.index) == set(ou.index) and (ou - py.reindex(ou.index)).abs().max() < TOL, pred
print("boundary-filtered allocation: identical")

# --- crosswalk --------------------------------------------------------------------------
ctx = svc._context("ID")
polys = [("DESA-1", feats[0][0]),
         ("DESA-2", "POLYGON ((106.806 -6.205, 106.818 -6.205, 106.818 -6.194, 106.806 -6.194, 106.806 -6.205), (106.809 -6.202, 106.812 -6.202, 106.812 -6.199, 106.809 -6.199, 106.809 -6.202))")]
proj = [(s, transform(lambda x, y, z=None: ctx.transformer.transform(x, y), W.loads(w))) for s, w in polys]
py = {(r["sumber_id"], r["gid"]): r["bobot"] for r in polygon_crosswalk_rows(proj, ctx.grid, 11)}
c.execute("CREATE TABLE p (sumber_id VARCHAR, geometry VARCHAR)")
c.executemany("INSERT INTO p VALUES (?, ?)", [list(x) for x in polys])
ou = {(s, g): w for s, g, w in c.execute("SELECT * FROM polygon_crosswalk('SELECT * FROM p', domain_code := 'ID', level := 11)").fetchall()}
assert set(ou) == set(py) and max(abs(ou[k] - py[k]) for k in py) < TOL
M = {"format_version": "1.0", "spec_version": "2.0", "domain_code": "ID", "level": 11, "weight_kind": "luas",
     "source_description": "x", "created_at": "2026-11-19T10:00:00Z", "registry_version": "2.0.0"}
want = validate_crosswalk([{"sumber_id": s, "gid": g, "bobot": w} for (s, g), w in ou.items()], M)
got = c.execute("SELECT * FROM validate_crosswalk('SELECT * FROM polygon_crosswalk(''SELECT * FROM p'', domain_code := ''ID'', level := 11)', manifest := ?)", [json.dumps(M)]).fetchone()
assert got == (want["sources"], want["rows"], want["cells"]), (got, want)
print("crosswalk: identical on", len(ou), "rows")

# --- compact sets both ways -------------------------------------------------------------
c.execute(f"SELECT * FROM write_compact_set('SELECT gid FROM polygon_to_cells(''ID'', ''{feats[0][0]}'', 12)', '{d}/ours', domain_code := 'ID', canonicalize := true)").fetchall()
m, rows = read_compact_set(f"{d}/ours")
assert len(rows) == c.execute(f"SELECT count(*) FROM read_compact_set('{d}/ours')").fetchone()[0]
write_compact_set(f"{d}/py", [("2", 1), ("32", 2)], domain_code="ID", domain_id=1)
assert c.execute(f"SELECT * FROM read_compact_set('{d}/py')").fetchall() == [("2", 1), ("32", 2)]
print("compact sets: each reads the other's", d)
