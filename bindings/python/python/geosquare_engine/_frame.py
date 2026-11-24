"""Shared helpers for the pandas and Polars accessors.

This module imports no DataFrame library. Every grid rule comes from the Rust
binding, which is called once per value. Invalid GIDs raise ``ValueError``.
Missing values (``None`` or float NaN) give a missing result.
"""

from __future__ import annotations

import importlib.util
import json
import math
from collections.abc import Mapping, Sequence
from typing import Any

from . import cell_polygon_wgs84, decode, detect_table, k_ring, parent

MAX_LEVEL = 14


def is_missing(value: Any) -> bool:
    return value is None or (isinstance(value, float) and math.isnan(value))


def gid_column(columns: Mapping[str, Sequence[Any]], column: str | None) -> str:
    """Return the GID column name.

    With ``column`` set, check that it exists. Otherwise run the spec 2.0 detector
    over ``columns`` and take the one column it calls a GID. A column the detector
    rejects does not count.
    """
    if column is not None:
        if column not in columns:
            raise KeyError(f"column {column!r} is not in the table")
        return column
    tables = {
        name: ("string" if all(v is None or isinstance(v, str) for v in values) else "other", values)
        for name, values in columns.items()
    }
    found = [
        result["column"]
        for result in detect_table(tables)
        if result["kind"] == "gid" and result["status"] != "rejected"
    ]
    if len(found) != 1:
        names = ", ".join(found) if found else "none"
        raise ValueError(f"expected one GID column, found {names}; pass column=")
    return found[0]


def levels(gids: Sequence[Any]) -> list[int | None]:
    return [None if is_missing(g) else decode(g)[0] for g in gids]


def parents(gids: Sequence[Any], level: int) -> list[str | None]:
    """Walk each GID up to ``level`` with the Rust ``parent`` call."""
    if isinstance(level, bool) or not isinstance(level, int) or not 0 <= level <= MAX_LEVEL:
        raise ValueError(f"level must be an int from 0 to {MAX_LEVEL}, got {level!r}")
    out: list[str | None] = []
    for gid in gids:
        if is_missing(gid):
            out.append(None)
            continue
        current_level = decode(gid)[0]
        if level > current_level:
            raise ValueError(
                f"GID {gid!r} is level {current_level}, below the requested parent level {level}"
            )
        current = gid
        for _ in range(current_level - level):
            current = parent(current)
        out.append(current)
    return out


def rings(gids: Sequence[Any], k: int) -> list[list[str] | None]:
    return [None if is_missing(g) else k_ring(g, k) for g in gids]


def polygons(gids: Sequence[Any], domain_id: int) -> list[list[tuple[float, float]] | None]:
    return [None if is_missing(g) else cell_polygon_wgs84(domain_id, g) for g in gids]


def geojson(ring: Sequence[tuple[float, float]] | None) -> str | None:
    if ring is None:
        return None
    return json.dumps({"type": "Polygon", "coordinates": [[list(p) for p in ring]]})


def has_geopandas() -> bool:
    return importlib.util.find_spec("geopandas") is not None
