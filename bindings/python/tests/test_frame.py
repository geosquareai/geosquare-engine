"""pandas and Polars accessor tests.

Each accessor result must equal the plain engine function on the same GIDs. Invalid
GIDs must raise. The import check runs with pandas and polars blocked.
"""

import json
import subprocess
import sys

import pytest

import geosquare_engine as gs
from geosquare_engine import _frame

# Level 5 and up: the detector only accepts strict GIDs (length 5 to 14).
GIDS = [gs.encode(12, 475168, 491792), gs.encode(9, 1234, 5678), gs.encode(5, 3, 7)]
INVALID = "ZZZ"


def _pandas():
    return pytest.importorskip("pandas")


def _polars():
    return pytest.importorskip("polars")


@pytest.fixture
def pd_mod():
    pd = _pandas()
    import geosquare_engine.pandas_gsq  # noqa: F401

    return pd


@pytest.fixture
def pl_mod():
    pl = _polars()
    import geosquare_engine.polars_gsq  # noqa: F401

    return pl


def test_pandas_parent_ring_level_match_engine(pd_mod):
    df = pd_mod.DataFrame({"gid": GIDS})
    assert list(df.gsq.parent(2)) == [_parent_to(g, 2) for g in GIDS]
    assert list(df.gsq.ring(1)) == [gs.k_ring(g, 1) for g in GIDS]
    assert list(df.gsq.level()) == [gs.decode(g)[0] for g in GIDS]


def test_polars_parent_ring_level_match_engine(pl_mod):
    df = pl_mod.DataFrame({"gid": GIDS})
    assert df.gsq.parent(2).to_list() == [_parent_to(g, 2) for g in GIDS]
    assert df.gsq.ring(1).to_list() == [gs.k_ring(g, 1) for g in GIDS]
    assert df.gsq.level().to_list() == [gs.decode(g)[0] for g in GIDS]


def test_series_accessor_matches_engine(pd_mod, pl_mod):
    assert list(pd_mod.Series(GIDS, name="g").gsq.parent(2)) == [_parent_to(g, 2) for g in GIDS]
    assert pl_mod.Series("g", GIDS).gsq.parent(2).to_list() == [_parent_to(g, 2) for g in GIDS]


def test_parent_at_own_level_is_same_gid(pl_mod):
    df = pl_mod.DataFrame({"gid": GIDS})
    assert df.gsq.parent(5).to_list()[2] == GIDS[2]
    assert pl_mod.Series([GIDS[0]]).gsq.parent(12).to_list() == [GIDS[0]]


def test_missing_values_stay_missing(pd_mod, pl_mod):
    assert list(pd_mod.Series([GIDS[0], None]).gsq.parent(3))[1] is None
    assert pl_mod.Series([GIDS[0], None]).gsq.parent(3).to_list()[1] is None
    assert pl_mod.Series([GIDS[0], None]).gsq.level().to_list()[1] is None


def test_parent_below_gid_level_raises(pl_mod):
    with pytest.raises(ValueError, match="below the requested parent level"):
        pl_mod.Series([GIDS[2]]).gsq.parent(7)


def test_bad_level_raises(pl_mod):
    with pytest.raises(ValueError, match="level must be"):
        pl_mod.Series(GIDS).gsq.parent(15)


@pytest.mark.parametrize("lib", ["pandas", "polars"])
def test_invalid_gid_raises_and_nothing_is_repaired(lib, request):
    mod = request.getfixturevalue("pd_mod" if lib == "pandas" else "pl_mod")
    bad = mod.Series([GIDS[0], INVALID])
    with pytest.raises(ValueError):
        bad.gsq.parent(3)
    with pytest.raises(ValueError):
        bad.gsq.level()


def test_invalid_gid_in_named_column_raises(pl_mod):
    df = pl_mod.DataFrame({"a": [GIDS[0]], "b": [INVALID]})
    with pytest.raises(ValueError):
        df.gsq.parent(3, column="b")


def test_detector_finds_the_only_gid_column(pl_mod):
    df = pl_mod.DataFrame({"name": ["x", "y"], "cell": GIDS[:2]})
    assert df.gsq.level().name == "cell_level"


def test_detector_rejects_ambiguous_or_missing_gid_column(pl_mod):
    df = pl_mod.DataFrame({"a": GIDS[:2], "b": GIDS[:2]})
    with pytest.raises(ValueError, match="expected one GID column"):
        df.gsq.level()
    with pytest.raises(ValueError, match="expected one GID column"):
        pl_mod.DataFrame({"name": ["x"]}).gsq.level()


def test_short_gid_needs_explicit_column(pl_mod):
    # The detector rejects GIDs shorter than 5 characters, so a short GID column
    # must be named.
    df = pl_mod.DataFrame({"cell": [gs.encode(3, 2, 1)]})
    with pytest.raises(ValueError, match="expected one GID column"):
        df.gsq.level()
    assert df.gsq.level(column="cell").to_list() == [3]


def test_unknown_column_name_raises(pl_mod):
    with pytest.raises(Exception, match="nope"):
        pl_mod.DataFrame({"gid": GIDS}).gsq.level(column="nope")


def test_to_geo_without_geopandas_gives_geojson(pl_mod, pd_mod, monkeypatch):
    monkeypatch.setattr(_frame, "has_geopandas", lambda: False)
    out = pl_mod.DataFrame({"gid": GIDS[:1]}).gsq.to_geo(1)
    ring = gs.cell_polygon_wgs84(1, GIDS[0])
    assert json.loads(out["geometry"][0]) == {
        "type": "Polygon",
        "coordinates": [[list(p) for p in ring]],
    }
    out = pd_mod.DataFrame({"gid": GIDS[:1]}).gsq.to_geo(1)
    assert json.loads(out["geometry"][0])["type"] == "Polygon"


def test_to_geo_with_geopandas_gives_geodataframe(pd_mod):
    gpd = pytest.importorskip("geopandas")
    out = pd_mod.DataFrame({"gid": GIDS[:1]}).gsq.to_geo(1)
    assert isinstance(out, gpd.GeoDataFrame)
    expected = gs.cell_polygon_wgs84(1, GIDS[0])
    assert list(out.geometry[0].exterior.coords) == [tuple(p) for p in expected]


def test_import_works_without_pandas_or_polars():
    code = (
        "import sys\n"
        "sys.modules['pandas'] = None\n"
        "sys.modules['polars'] = None\n"
        "import geosquare_engine as gs\n"
        "import geosquare_engine._frame\n"
        "assert gs.encode(3, 2, 1)\n"
    )
    subprocess.run([sys.executable, "-c", code], check=True)


def _parent_to(gid, level):
    current = gid
    while gs.decode(current)[0] > level:
        current = gs.parent(current)
    return current
