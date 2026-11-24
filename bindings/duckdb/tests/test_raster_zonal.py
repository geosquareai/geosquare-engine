"""Raster to cells (E16): cell means from read_raster against a numpy oracle on a synthetic GeoTIFF.

The raster has GEE-style near-zero values outside the area of interest. The bbox read must
drop them, so no cell outside the area appears and the means inside the area are unchanged.
Needs rasterio, numpy and a built extension (GSQ_EXTENSION or the path given as argument).
"""

import os
import sys
import tempfile
from pathlib import Path

import pytest

np = pytest.importorskip("numpy")
rasterio = pytest.importorskip("rasterio")
duckdb = pytest.importorskip("duckdb")
from rasterio.transform import from_origin  # noqa: E402

DEFAULT = Path(__file__).resolve().parents[1] / "build/osx_arm64/geosquare_v2.duckdb_extension"
AOI = [106.80, -6.22, 106.84, -6.18]  # lon/lat box
RES = 0.0003


def extension() -> str:
    path = os.environ.get("GSQ_EXTENSION") or (sys.argv[1] if len(sys.argv) > 1 and sys.argv[1].endswith(".duckdb_extension") else str(DEFAULT))
    if not Path(path).exists():
        pytest.skip(f"extension not built: {path}")
    return path


@pytest.fixture(scope="module")
def raster(tmp_path_factory) -> tuple[str, np.ndarray]:
    width, height = 400, 400
    west, north = 106.78, -6.16  # covers the AOI with a margin of about 0.02 degrees
    rng = np.random.default_rng(11)
    data = rng.random((height, width)).astype("float32") * 5 + 1
    # GEE-style: near-zero outside the AOI, not NoData.
    cols = (np.arange(width) + 0.5) * RES + west
    rows = north - (np.arange(height) + 0.5) * RES
    outside = (cols[None, :] < AOI[0]) | (cols[None, :] > AOI[2]) | (rows[:, None] < AOI[1]) | (rows[:, None] > AOI[3])
    data[np.broadcast_to(outside, data.shape)] = 1e-9
    path = tmp_path_factory.mktemp("raster") / "flood.tif"
    with rasterio.open(path, "w", driver="GTiff", width=width, height=height, count=1, dtype="float32",
                       crs="EPSG:4326", transform=from_origin(west, north, RES, RES), tiled=True,
                       blockxsize=256, blockysize=256) as f:
        f.write(data, 1)
    return str(path), data


def connect() -> "duckdb.DuckDBPyConnection":
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{extension()}'")
    return con


def test_cell_means_match_numpy_oracle(raster) -> None:
    path, _ = raster
    con = connect()
    got = con.execute(
        "SELECT point_to_cell('ID', lon, lat, 12).gid AS gid, avg(value) AS mean, count(*) AS n "
        "FROM read_raster(?, bbox := ?) GROUP BY 1",
        [path, AOI],
    ).fetchall()
    assert len(got) > 500

    # Oracle: read the same pixels with rasterio, keep centres inside the AOI, group by cell.
    with rasterio.open(path) as f:
        arr = f.read(1)
        xs, ys = np.meshgrid(np.arange(f.width), np.arange(f.height))
        lon, lat = f.xy(ys.ravel(), xs.ravel())
    lon, lat, values = np.asarray(lon), np.asarray(lat), arr.ravel().astype(float)
    inside = (lon >= AOI[0]) & (lon <= AOI[2]) & (lat >= AOI[1]) & (lat <= AOI[3])
    lon, lat, values = lon[inside], lat[inside], values[inside]
    cells = con.execute(
        "SELECT point_to_cell('ID', lon, lat, 12).gid FROM (SELECT unnest(?::DOUBLE[]) AS lon, unnest(?::DOUBLE[]) AS lat)",
        [lon.tolist(), lat.tolist()],
    ).fetchall()
    sums: dict[str, list[float]] = {}
    for (gid,), v in zip(cells, values):
        sums.setdefault(gid, []).append(v)

    assert {g for g, _, _ in got} == set(sums)
    for gid, mean, n in got:
        want = sums[gid]
        assert n == len(want), gid
        assert mean == pytest.approx(float(np.mean(want)), rel=1e-6), gid


def test_no_cell_from_near_zero_outside_the_area(raster) -> None:
    path, _ = raster
    con = connect()
    lows = con.execute(
        "SELECT min(avg_value) FROM (SELECT point_to_cell('ID', lon, lat, 12).gid AS gid, avg(value) AS avg_value "
        "FROM read_raster(?, bbox := ?) GROUP BY 1)",
        [path, AOI],
    ).fetchone()[0]
    assert lows > 0.5, "values near 0 from outside the AOI leaked into the cells"
