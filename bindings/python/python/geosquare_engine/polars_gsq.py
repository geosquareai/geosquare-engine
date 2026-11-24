"""Polars accessor ``gsq`` for GID columns and Series.

Import this module once to register the accessor::

    import geosquare_engine.polars_gsq  # noqa: F401
    df.gsq.parent(9)

Results are Series of the same length. Attach them with ``with_columns``.
"""

from __future__ import annotations

from typing import Any

import polars as pl

from . import _frame


class _Gsq:
    def __init__(self, obj: pl.DataFrame | pl.Series) -> None:
        self._obj = obj

    def _gids(self, column: Any = None) -> tuple[pl.Series, str]:
        """Return the GID Series and its name. A Series is its own GID column."""
        if isinstance(self._obj, pl.Series):
            return self._obj, self._obj.name or "gid"
        frame = self._obj
        if column is None:
            names = [c for c in frame.columns if frame.schema[c] == pl.String]
        else:
            names = [column]
        columns = {c: frame.get_column(c).to_list() for c in names}
        name = _frame.gid_column(columns, column)
        return frame.get_column(name), name

    def parent(self, level: int, column: Any = None) -> pl.Series:
        """Parent GID at ``level``. Raises if a GID is below ``level``."""
        series, name = self._gids(column)
        values = _frame.parents(series.to_list(), level)
        return pl.Series(f"{name}_parent{level}", values, dtype=pl.String)

    def ring(self, k: int, column: Any = None) -> pl.Series:
        """List of GIDs at Chebyshev distance ``k`` (the ring, not the disk)."""
        series, name = self._gids(column)
        values = _frame.rings(series.to_list(), k)
        return pl.Series(f"{name}_ring{k}", values, dtype=pl.List(pl.String))

    def level(self, column: Any = None) -> pl.Series:
        """GID level (length of the GID), as Int64."""
        series, name = self._gids(column)
        values = _frame.levels(series.to_list())
        return pl.Series(f"{name}_level", values, dtype=pl.Int64)

    def to_geo(self, domain_id: int, column: Any = None) -> Any:
        """Cell polygons in WGS84.

        Returns a GeoDataFrame when geopandas is installed. Otherwise it returns a
        Polars DataFrame with a ``geometry`` column of GeoJSON text.
        """
        series, name = self._gids(column)
        rings = _frame.polygons(series.to_list(), domain_id)
        if _frame.has_geopandas():
            import geopandas as gpd
            from shapely.geometry import Polygon

            base = self._obj.to_pandas() if isinstance(self._obj, pl.DataFrame) else series.to_pandas().to_frame()
            geoms = [None if r is None else Polygon(r) for r in rings]
            return gpd.GeoDataFrame(base, geometry=geoms, crs="EPSG:4326")
        base = self._obj if isinstance(self._obj, pl.DataFrame) else series.to_frame()
        return base.with_columns(pl.Series("geometry", [_frame.geojson(r) for r in rings], dtype=pl.String))


@pl.api.register_dataframe_namespace("gsq")
class DataFrameGsq(_Gsq):
    pass


@pl.api.register_series_namespace("gsq")
class SeriesGsq(_Gsq):
    pass
