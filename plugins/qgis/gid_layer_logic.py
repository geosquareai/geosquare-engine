"""Pure GeoSquare cell-layer logic used by the QGIS adapter.

This module deliberately has no QGIS imports. It validates GIDs and levels before
building any output features, so callers can add a memory layer atomically.
"""

from __future__ import annotations

from dataclasses import dataclass
import math
from numbers import Real
from typing import Any, Iterable, Mapping, Sequence

import geosquare_engine as gs


@dataclass(frozen=True)
class CellFeature:
    """A validated cell and its numeric attributes, independent of QGIS."""

    gid: str
    level: int
    geometry: tuple[tuple[float, float], ...]
    attributes: dict[str, int | float | None]


class GidLayerError(ValueError):
    """An input row cannot be converted to a valid, single-level cell layer."""


def _numeric_value(value: Any, field: str, row_number: int) -> int | float | None:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, Real):
        raise GidLayerError(
            f"Numeric field {field!r} has a non-numeric value in row {row_number}."
        )
    if isinstance(value, float) and not math.isfinite(value):
        raise GidLayerError(
            f"Numeric field {field!r} has a non-finite value in row {row_number}."
        )
    return value


def _polygon(
    domain_id: int,
    gid: str,
    max_segment_length_m: float,
    allow_candidate_crs: bool,
    engine: Any,
    row_number: int | None = None,
) -> tuple[tuple[float, float], ...]:
    try:
        ring = engine.cell_polygon_wgs84(
            domain_id, gid, max_segment_length_m, allow_candidate_crs
        )
    except Exception as exc:  # Rust errors become ValueError in the binding.
        where = f" in row {row_number}" if row_number is not None else ""
        raise GidLayerError(f"Cannot build polygon for GID {gid!r}{where}: {exc}") from exc
    return tuple((float(lon), float(lat)) for lon, lat in ring)


def build_cell_features(
    rows: Iterable[Mapping[str, Any]],
    *,
    gid_field: str,
    numeric_fields: Sequence[str],
    domain_id: int,
    max_segment_length_m: float = 1000.0,
    allow_candidate_crs: bool = False,
    engine: Any = gs,
) -> list[CellFeature]:
    """Validate rows and turn them into WGS84 cell features.

    Every GID is decoded by the engine. All rows must have one level, and every
    requested numeric field must contain a number or null. No output is returned
    until the complete input has passed validation and polygon construction.
    """
    materialized = list(rows)
    if not materialized:
        raise GidLayerError("The selected layer has no features.")

    field_names = tuple(numeric_fields)
    decoded: list[tuple[int, str, dict[str, int | float | None]]] = []
    levels: set[int] = set()
    for row_number, row in enumerate(materialized, start=1):
        if gid_field not in row:
            raise GidLayerError(f"GID field {gid_field!r} is missing in row {row_number}.")
        gid = row[gid_field]
        if not isinstance(gid, str):
            raise GidLayerError(
                f"GID field {gid_field!r} must contain text in row {row_number}."
            )
        try:
            level, _, _ = engine.decode(gid)
        except Exception as exc:
            raise GidLayerError(
                f"Invalid GID {gid!r} in row {row_number}: {exc}"
            ) from exc
        attributes: dict[str, int | float | None] = {}
        for field_name in field_names:
            if field_name not in row:
                raise GidLayerError(
                    f"Numeric field {field_name!r} is missing in row {row_number}."
                )
            attributes[field_name] = _numeric_value(
                row[field_name], field_name, row_number
            )
        levels.add(level)
        decoded.append((level, gid, attributes))

    if len(levels) != 1:
        level_list = ", ".join(str(level) for level in sorted(levels))
        raise GidLayerError(f"GIDs must all have one level; found levels {level_list}.")

    output: list[CellFeature] = []
    for row_number, (level, gid, attributes) in enumerate(decoded, start=1):
        output.append(
            CellFeature(
                gid=gid,
                level=level,
                geometry=_polygon(
                    domain_id,
                    gid,
                    max_segment_length_m,
                    allow_candidate_crs,
                    engine,
                    row_number,
                ),
                attributes=attributes,
            )
        )
    return output


def _ancestor(engine: Any, gid: str, target_level: int) -> str:
    current = gid
    while len(current) > target_level:
        parent = engine.parent(current)
        if parent is None:
            raise GidLayerError(
                f"Cannot find level {target_level} ancestor for GID {gid!r}."
            )
        current = parent
    return current


def rollup_cell_features(
    features: Iterable[CellFeature],
    *,
    target_level: int,
    domain_id: int,
    max_segment_length_m: float = 1000.0,
    allow_candidate_crs: bool = False,
    engine: Any = gs,
) -> list[CellFeature]:
    """Group a cell layer by ancestors and sum its numeric attributes.

    The target level must be no deeper than the single source level. Missing
    numeric values stay null when every value in a group is null.
    """
    materialized = list(features)
    if not materialized:
        raise GidLayerError("The selected GID layer has no features.")
    source_levels: set[int] = set()
    field_names: tuple[str, ...] = tuple(materialized[0].attributes)
    grouped: dict[str, dict[str, int | float | None]] = {}

    for row_number, feature in enumerate(materialized, start=1):
        try:
            decoded_level, _, _ = engine.decode(feature.gid)
        except Exception as exc:
            raise GidLayerError(
                f"Invalid GID {feature.gid!r} in row {row_number}: {exc}"
            ) from exc
        if decoded_level != feature.level:
            raise GidLayerError(
                f"GID {feature.gid!r} has level {decoded_level}, "
                f"but the layer records level {feature.level}."
            )
        source_levels.add(decoded_level)
        if tuple(feature.attributes) != field_names:
            raise GidLayerError("All cell rows must have the same numeric fields.")

    if len(source_levels) != 1:
        level_list = ", ".join(str(level) for level in sorted(source_levels))
        raise GidLayerError(f"GIDs must all have one level; found levels {level_list}.")
    source_level = next(iter(source_levels))
    if not isinstance(target_level, int) or isinstance(target_level, bool):
        raise GidLayerError("Rollup level must be an integer.")
    if target_level < 0 or target_level > source_level:
        raise GidLayerError(
            f"Rollup level must be between 0 and {source_level}; got {target_level}."
        )

    for row_number, feature in enumerate(materialized, start=1):
        ancestor = _ancestor(engine, feature.gid, target_level)
        totals = grouped.setdefault(ancestor, {name: None for name in field_names})
        for field_name in field_names:
            value = _numeric_value(
                feature.attributes[field_name], field_name, row_number
            )
            if value is not None:
                previous = totals[field_name]
                totals[field_name] = value if previous is None else previous + value

    output: list[CellFeature] = []
    for gid in sorted(grouped):
        output.append(
            CellFeature(
                gid=gid,
                level=target_level,
                geometry=_polygon(
                    domain_id,
                    gid,
                    max_segment_length_m,
                    allow_candidate_crs,
                    engine,
                ),
                attributes=grouped[gid],
            )
        )
    return output
