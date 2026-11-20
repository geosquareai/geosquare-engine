"""Compact cell sets for the ``cell_compact_set`` dataset type (spec 2.0).

A compact set is a set of GIDs that may mix levels. Hierarchy comes from the
Rust core through ``decode``, ``parent`` and ``children``. This module adds no
grid rules of its own. The rules match ``geosquare_v2.compact`` and the vectors
in ``vectors/spec-2.0/compact-set.json``.
"""

from __future__ import annotations

from collections.abc import Iterable

from . import children as _children
from . import decode as _decode
from . import parent as _parent

MAX_LEVEL = 14


class CompactSetError(ValueError):
    """A compact set row or set breaks a rule in the ``cell_compact_set`` contract."""


def _level(gid: str) -> int:
    # decode raises ValueError for an invalid GID.
    return _decode(gid)[0]


def _ancestors(gid: str) -> Iterable[str]:
    current = _parent(gid)
    while current is not None:
        yield current
        current = _parent(current)


def child_gids(gid: str) -> tuple[str, ...]:
    """Return the direct children of a GID."""
    if _level(gid) == MAX_LEVEL:
        raise CompactSetError(f"level {MAX_LEVEL} has no children")
    return tuple(_children(gid))


def canonicalize(gids: Iterable[str]) -> tuple[str, ...]:
    """Return the canonical compact form of ``gids``, sorted.

    Descendants of a present ancestor are removed. Then every complete child
    group is replaced by its parent, from the deepest level up.
    """
    present = set(gids)
    for gid in present:
        _level(gid)
    kept = {gid for gid in present if not any(ancestor in present for ancestor in _ancestors(gid))}
    by_level: dict[int, set[str]] = {level: set() for level in range(MAX_LEVEL + 1)}
    for gid in kept:
        by_level[_level(gid)].add(gid)
    for level in range(MAX_LEVEL, 0, -1):
        for parent in {_parent(gid) for gid in by_level[level]}:
            children = child_gids(parent)
            if all(child in by_level[level] for child in children):
                by_level[level].difference_update(children)
                by_level[level - 1].add(parent)
    return tuple(sorted(gid for cells in by_level.values() for gid in cells))


def validate_compact_set(rows: Iterable[tuple[str, int]]) -> None:
    """Raise ``CompactSetError`` unless ``rows`` is a canonical compact set.

    Each row is ``(gid, level)``. The check rejects. It never canonicalizes.
    """
    gids: set[str] = set()
    for gid, level in rows:
        decoded_level = _level(gid)
        if type(level) is not int or level != decoded_level:
            raise CompactSetError(f"level {level!r} does not match GID {gid!r} at level {decoded_level}")
        if gid in gids:
            raise CompactSetError(f"duplicate row for GID {gid!r}")
        gids.add(gid)
    for gid in gids:
        covering = next((ancestor for ancestor in _ancestors(gid) if ancestor in gids), None)
        if covering is not None:
            raise CompactSetError(f"GID {gid!r} is inside row {covering!r}")
    if canonicalize(gids) != tuple(sorted(gids)):
        raise CompactSetError("set is not canonical: a complete child group must be replaced by its parent")


def expand(gids: Iterable[str], level: int) -> frozenset[str]:
    """Return every level-``level`` descendant of the rows, de-duplicated.

    A target level below any row's level is an error, not a roll-up.
    """
    if type(level) is not int or not 0 <= level <= MAX_LEVEL:
        raise CompactSetError(f"level must be an integer in [0, {MAX_LEVEL}]; received {level!r}")
    cells: set[str] = set()
    for gid in gids:
        row_level = _level(gid)
        if level < row_level:
            raise CompactSetError(f"target level {level} is below row level {row_level} for GID {gid!r}")
        frontier = [gid]
        for _ in range(row_level, level):
            frontier = [child for cell in frontier for child in child_gids(cell)]
        cells.update(frontier)
    return frozenset(cells)


def contains(gids: Iterable[str], gid: str) -> bool:
    """Return whether ``gid`` is covered: stored exactly, or under a stored ancestor."""
    _level(gid)
    members = set(gids)
    if gid in members:
        return True
    return any(ancestor in members for ancestor in _ancestors(gid))
