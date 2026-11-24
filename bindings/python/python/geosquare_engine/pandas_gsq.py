"""pandas accessor ``gsq`` for GID columns and Series.

Import this module once to register the accessor::

    import geosquare_engine.pandas_gsq  # noqa: F401
    df.gsq.parent(9)

Results are Series aligned with the input index. Attach them with ``assign``.
"""

from __future__ import annotations

from typing import Any

import pandas as pd
from pandas.api.extensions import register_dataframe_accessor, register_series_accessor

from . import _frame


def _values(series: pd.Series) -> list[Any]:
    # pd.NA becomes None. NaN is handled in _frame.
    return series.astype(object).where(series.notna(), None).tolist()


class _Gsq:
    def __init__(self, obj: pd.DataFrame | pd.Series) -> None:
        self._obj = obj

    def _gids(self, column: Any = None) -> tuple[pd.Series, str]:
        """Return the GID Series and its name. A Series is its own GID column."""
        if isinstance(self._obj, pd.Series):
            return self._obj, str(self._obj.name or "gid")
        frame = self._obj
        if column is None:
            names = [c for c in frame.columns if pd.api.types.is_string_dtype(frame[c].dtype)]
            columns = {c: _values(frame[c]) for c in names}
        else:
            columns = {column: _values(frame[column])}
        name = _frame.gid_column(columns, column)
        return frame[name], name

    def _result(self, series: pd.Series, values: list[Any], name: str, dtype: Any) -> pd.Series:
        return pd.Series(values, index=series.index, name=name, dtype=dtype)

    def parent(self, level: int, column: Any = None) -> pd.Series:
        """Parent GID at ``level``. Raises if a GID is below ``level``."""
        series, name = self._gids(column)
        values = _frame.parents(_values(series), level)
        return self._result(series, values, f"{name}_parent{level}", object)

    def ring(self, k: int, column: Any = None) -> pd.Series:
        """List of GIDs at Chebyshev distance ``k`` (the ring, not the disk)."""
        series, name = self._gids(column)
        values = _frame.rings(_values(series), k)
        return self._result(series, values, f"{name}_ring{k}", object)

    def level(self, column: Any = None) -> pd.Series:
        """GID level (length of the GID), as Int64."""
        series, name = self._gids(column)
        values = _frame.levels(_values(series))
        return self._result(series, values, f"{name}_level", "Int64")

    def to_geo(self, domain_id: int, column: Any = None) -> pd.DataFrame:
        """Cell polygons in WGS84.

        Returns a GeoDataFrame when geopandas is installed. Otherwise it returns a
        DataFrame with a ``geometry`` column of GeoJSON text.
        """
        series, name = self._gids(column)
        rings = _frame.polygons(_values(series), domain_id)
        base = self._obj.copy() if isinstance(self._obj, pd.DataFrame) else series.to_frame()
        if _frame.has_geopandas():
            import geopandas as gpd
            from shapely.geometry import Polygon

            geoms = [None if r is None else Polygon(r) for r in rings]
            return gpd.GeoDataFrame(base, geometry=geoms, crs="EPSG:4326")
        base["geometry"] = [_frame.geojson(r) for r in rings]
        return base


@register_dataframe_accessor("gsq")
class DataFrameGsq(_Gsq):
    pass


@register_series_accessor("gsq")
class SeriesGsq(_Gsq):
    pass
