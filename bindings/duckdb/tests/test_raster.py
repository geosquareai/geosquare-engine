"""read_raster against rasterio on generated GeoTIFFs: compression, predictors, sample types,
BigTIFF, nodata, and the bbox window; plus refusals. Skipped when rasterio is missing.

    python tests/test_raster.py build/linux_amd64/geosquare_v2.duckdb_extension   # needs rasterio, numpy
"""
import sys, tempfile
import duckdb

try:
    import numpy as np
    import rasterio
    from rasterio.transform import from_origin
    from rasterio.windows import from_bounds
except ImportError:
    print("read_raster test skipped (rasterio not installed)")
    sys.exit(0)

ext = sys.argv[1]
d = tempfile.mkdtemp(prefix="gs_raster_")
c = duckdb.connect(config={"allow_unsigned_extensions": "true"})
c.execute(f"LOAD '{ext}'")
rng = np.random.default_rng(7)
W, H, res = 900, 700, 0.00027
transform = from_origin(106.70, -6.10, res, res)
cases = [
    ("float32", dict(compress="deflate", predictor=3), -9999.0),
    ("float32", dict(compress="deflate", predictor=1), None),
    ("float64", dict(compress="deflate", predictor=3, BIGTIFF="YES"), -1.0),
    ("int16", dict(compress="deflate", predictor=2), -32768),
    ("uint8", dict(compress=None), 255),
    ("int32", dict(compress="deflate", predictor=2, BIGTIFF="YES"), None),
]
bbox = [106.73, -6.25, 106.83, -6.16]
for i, (dt, opts, nodata) in enumerate(cases):
    if dt.startswith("float"):
        a = rng.random((H, W)).astype(dt)
    else:
        info = np.iinfo(dt)
        a = rng.integers(max(info.min, -1000), min(info.max, 1000), (H, W)).astype(dt)
    if nodata is not None:
        a[rng.random((H, W)) < 0.1] = nodata
    p = f"{d}/r{i}.tif"
    with rasterio.open(p, "w", driver="GTiff", width=W, height=H, count=1, dtype=dt, crs="EPSG:4326", transform=transform,
                       nodata=nodata, tiled=True, blockxsize=256, blockysize=256, **{k: v for k, v in opts.items() if v is not None}) as f:
        f.write(a, 1)
    with rasterio.open(p) as f:
        win = from_bounds(*bbox, transform=f.transform).round_offsets(op="floor").round_lengths(op="ceil")
        r0, c0 = max(int(win.row_off), 0), max(int(win.col_off), 0)
        r1, c1 = min(r0 + int(win.height) + 1, H), min(c0 + int(win.width) + 1, W)
        want = {}
        for r in range(r0, r1):
            for col in range(c0, c1):
                x, y = f.xy(r, col)  # pixel centre
                if not (bbox[0] <= x <= bbox[2] and bbox[1] <= y <= bbox[3]):
                    continue
                v = a[r, col]
                if nodata is not None and v == nodata:
                    continue
                want[(round(x, 9), round(y, 9))] = float(v)
    got = {(round(x, 9), round(y, 9)): v for x, y, v in c.execute("SELECT lon, lat, value FROM read_raster(?, bbox := ?)", [p, bbox]).fetchall()}
    inside = {k: v for k, v in want.items() if bbox[0] <= k[0] <= bbox[2] and bbox[1] <= k[1] <= bbox[3]}
    missing = [k for k in inside if k not in got]
    wrong = [k for k in got if k in want and want[k] != got[k]]
    extra = [k for k in got if k not in want]
    assert not missing and not wrong and not extra and len(got) > 1000, (dt, opts, len(missing), len(wrong), len(extra), len(got))
# cells: mean of the pixels whose centre lies in the cell
r = c.execute(f"SELECT count(*), sum(px) FROM (SELECT point_to_cell('ID', lon, lat, 12).packed_id, avg(value), count(*) px FROM read_raster(?, bbox := ?) GROUP BY 1)", [f"{d}/r0.tif", bbox]).fetchone()
assert r[0] > 1000 and r[1] > r[0], r
# refusals
with rasterio.open(f"{d}/strips.tif", "w", driver="GTiff", width=50, height=50, count=1, dtype="float32", crs="EPSG:4326", transform=transform) as f:
    f.write(np.zeros((50, 50), "float32"), 1)
with rasterio.open(f"{d}/utm.tif", "w", driver="GTiff", width=64, height=64, count=1, dtype="float32", crs="EPSG:32748",
                   transform=from_origin(700000, 9300000, 30, 30), tiled=True, blockxsize=32, blockysize=32) as f:
    f.write(np.zeros((64, 64), "float32"), 1)
for path, args, msg in ((f"{d}/strips.tif", f"bbox := {bbox}", "tiled GeoTIFF"), (f"{d}/utm.tif", f"bbox := {bbox}", "EPSG:4326"),
                        (f"{d}/r0.tif", "", "needs bbox"), (f"{d}/r0.tif", f"bbox := {bbox}, max_pixels := 10", "max_pixels")):
    try:
        c.execute(f"SELECT count(*) FROM read_raster(?{', ' + args if args else ''})", [path]).fetchall(); raise AssertionError(msg)
    except duckdb.Error as e:
        assert msg in str(e), (msg, e)
print("read_raster OK", d)
