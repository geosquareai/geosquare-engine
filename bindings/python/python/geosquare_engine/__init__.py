"""GeoSquare V2 grid engine for Python (spec 2.0).

The functions run the Rust core in `geosquare-core`. Their results must match the
conformance vectors in `vectors/spec-2.0`.
"""

from ._core import (
    __version__,
    cell_side_m,
    children,
    decode,
    descendant_range,
    detect_table,
    distance_m,
    encode,
    k_disk,
    k_ring,
    pack,
    parent,
    unpack,
)

__all__ = [
    "__version__",
    "cell_side_m",
    "children",
    "decode",
    "descendant_range",
    "detect_table",
    "distance_m",
    "encode",
    "k_disk",
    "k_ring",
    "pack",
    "parent",
    "unpack",
]
