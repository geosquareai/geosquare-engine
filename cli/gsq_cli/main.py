"""The ``gsq`` command-line interface.

All grid operations are delegated to the geosquare_v2 DuckDB extension.  This
module only handles input/output formats, argument validation, and atomic file
writes.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import pathlib
import shutil
import struct
import sys
import tempfile
from datetime import datetime, timezone
from typing import Any, Iterable

import duckdb

from . import __version__


CANDIDATE_DOMAINS = {"KH", "MM", "TH", "TL", "VN"}


class CLIError(Exception):
    """A user-facing data or validation error."""


class UsageError(Exception):
    """A user-facing usage error."""


def sql_string(value: str) -> str:
    """Return a safely quoted SQL string literal."""

    return "'" + value.replace("'", "''") + "'"


def sql_identifier(value: str) -> str:
    """Return a safely quoted SQL identifier."""

    return '"' + value.replace('"', '""') + '"'


def input_relation(path: str, *, allow_dataset: bool = True) -> str:
    """Build a DuckDB relation expression for a local input path."""

    p = pathlib.Path(path)
    if p.is_dir():
        if not allow_dataset:
            raise UsageError(f"input must be a CSV or Parquet file: {path}")
        return f"read_cell_dataset({sql_string(path)})"
    suffix = p.suffix.lower()
    if suffix == ".parquet":
        return f"read_parquet({sql_string(path)})"
    if suffix in {".csv", ".tsv", ".txt"}:
        return f"read_csv_auto({sql_string(path)})"
    raise UsageError(f"unsupported input format: {path} (expected CSV, Parquet, or a cell dataset folder)")


def output_format(path: str) -> str:
    suffix = pathlib.Path(path).suffix.lower()
    if suffix == ".csv":
        return "csv"
    if suffix == ".parquet":
        return "parquet"
    raise UsageError(f"unsupported output format: {path} (expected .csv or .parquet)")


def connect(extension: str | None) -> duckdb.DuckDBPyConnection:
    extension = extension or os.environ.get("GSQ_EXTENSION")
    if not extension:
        raise UsageError("no DuckDB extension path; pass --extension PATH or set GSQ_EXTENSION")
    if not pathlib.Path(extension).exists():
        raise CLIError(f"DuckDB extension not found: {extension}")
    try:
        con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
        con.execute(f"LOAD {sql_string(extension)}")
        return con
    except Exception as exc:
        raise CLIError(f"could not load DuckDB extension {extension}: {exc}") from exc


def create_input_table(con: duckdb.DuckDBPyConnection, path: str, name: str = "gsq_input") -> list[str]:
    relation = input_relation(path)
    try:
        con.execute(f"CREATE OR REPLACE TEMP TABLE {sql_identifier(name)} AS SELECT * FROM {relation}")
        return [row[0] for row in con.execute(f"DESCRIBE {sql_identifier(name)}").fetchall()]
    except Exception as exc:
        raise CLIError(f"could not read input {path}: {exc}") from exc


def detector_options(args: argparse.Namespace) -> str:
    parts: list[str] = []
    if args.domain:
        parts.append(f"domain_code := {sql_string(args.domain.upper())}")
    if args.level is not None:
        parts.append(f"level := {args.level}")
    if args.max_rows is not None:
        parts.append(f"max_rows := {args.max_rows}")
    return (", " + ", ".join(parts)) if parts else ""


def detect_rows(con: duckdb.DuckDBPyConnection, query: str, args: argparse.Namespace) -> list[tuple[Any, ...]]:
    options = detector_options(args)
    # Inline the query: DuckDB 2.x rejects a bound parameter as a table function argument.
    sql = f"SELECT * FROM detect_table({sql_string(query)}{options})"
    try:
        return con.execute(sql).fetchall()
    except Exception as exc:
        raise CLIError(f"detection failed: {exc}") from exc


def json_value(value: Any) -> Any:
    if isinstance(value, tuple):
        return [json_value(v) for v in value]
    if isinstance(value, list):
        return [json_value(v) for v in value]
    if isinstance(value, dict):
        return {str(k): json_value(v) for k, v in value.items()}
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    return str(value)


def result_records(cursor: Any) -> list[dict[str, Any]]:
    columns = [item[0] for item in cursor.description]
    return [{name: json_value(value) for name, value in zip(columns, row)} for row in cursor.fetchall()]


def print_table(cursor: Any) -> None:
    columns = [item[0] for item in cursor.description]
    rows = [[format_value(value) for value in row] for row in cursor.fetchall()]
    widths = [len(str(name)) for name in columns]
    for row in rows:
        widths = [max(width, len(value)) for width, value in zip(widths, row)]
    print("  ".join(str(name).ljust(width) for name, width in zip(columns, widths)))
    print("  ".join("-" * width for width in widths))
    for row in rows:
        print("  ".join(value.ljust(width) for value, width in zip(row, widths)))


def format_value(value: Any) -> str:
    if value is None:
        return "NULL"
    if isinstance(value, (list, tuple, dict)):
        return json.dumps(json_value(value), separators=(",", ":"))
    return str(value)


def temp_output(path: str) -> tuple[str, pathlib.Path]:
    destination = pathlib.Path(path)
    parent = destination.parent
    if not parent.exists():
        raise CLIError(f"output directory does not exist: {parent}")
    fd, temporary = tempfile.mkstemp(prefix=f".{destination.name}.", suffix=".tmp", dir=parent)
    os.close(fd)
    return temporary, destination


def copy_atomic(
    con: duckdb.DuckDBPyConnection, query: str, out: str, metadata: dict[str, str] | None = None
) -> None:
    fmt = output_format(out)
    temporary, destination = temp_output(out)
    try:
        if fmt == "csv":
            options = "(FORMAT CSV, HEADER TRUE)"
        elif metadata:
            pairs = ", ".join(f"{sql_string(k)}: {sql_string(v)}" for k, v in metadata.items())
            options = f"(FORMAT PARQUET, KV_METADATA {{{pairs}}})"
        else:
            options = "(FORMAT PARQUET)"
        con.execute(f"COPY ({query}) TO {sql_string(temporary)} {options}")
        os.replace(temporary, destination)
    except Exception as exc:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise CLIError(f"could not write {out}: {exc}") from exc


def require_columns(columns: Iterable[str], requested: Iterable[str]) -> None:
    available = set(columns)
    missing = [name for name in requested if name not in available]
    if missing:
        raise CLIError(f"unknown column(s): {', '.join(missing)}")


def detector_for_query(con: duckdb.DuckDBPyConnection, relation: str, column: str) -> tuple[str, float, list[Any]]:
    detection_query = f"SELECT {sql_identifier(column)} FROM {relation}"
    try:
        row = con.execute(f"SELECT * FROM detect_table({sql_string(detection_query)})").fetchone()
    except Exception as exc:
        raise CLIError(f"could not validate {column}: {exc}") from exc
    if row is None:
        raise CLIError(f"could not validate {column}")
    # column, kind, status, reason, valid_ratio, domain_codes, domain_ids, levels, invalid_examples
    valid_ratio = float(row[4] or 0.0)
    levels = list(row[7] or [])
    status = str(row[2])
    if valid_ratio < 1.0 or status == "rejected":
        raise CLIError(f"invalid {column} values: {row[3]}")
    if len(levels) != 1:
        raise CLIError(f"{column} must contain exactly one grid level; detected levels: {levels}")
    return status, valid_ratio, levels


def run_detect(args: argparse.Namespace) -> int:
    con = connect(args.extension)
    try:
        relation = input_relation(args.input, allow_dataset=False)
        query = f"SELECT * FROM {relation}"
        rows = detect_rows(con, query, args)
        columns = [
            "column", "kind", "status", "reason", "valid_ratio",
            "domain_codes", "domain_ids", "levels", "invalid_examples",
        ]
        if args.format == "json":
            print(json.dumps([dict(zip(columns, (json_value(v) for v in row))) for row in rows], separators=(",", ":")))
        else:
            class Cursor:
                description = [(name,) for name in columns]
                def fetchall(self) -> list[tuple[Any, ...]]:
                    return rows
            print_table(Cursor())
        return 0
    finally:
        con.close()


def run_rollup(args: argparse.Namespace) -> int:
    con = connect(args.extension)
    try:
        columns = create_input_table(con, args.input)
        require_columns(columns, ["gid", *args.sum])
        _, _, levels = detector_for_query(con, input_relation(args.input), "gid")
        input_level = int(levels[0])
        if args.level > input_level:
            raise CLIError(f"rollup level {args.level} is greater than input level {input_level}")
        sums = ", ".join(
            f"sum({sql_identifier(name)}) AS {sql_identifier(name)}" for name in args.sum
        )
        select = f"left({sql_identifier('gid')}, {args.level}) AS parent_gid, count(*) AS cells"
        if sums:
            select += ", " + sums
        query = f"SELECT {select} FROM gsq_input GROUP BY 1 ORDER BY 1"
        copy_atomic(con, query, args.out)
        return 0
    finally:
        con.close()


def run_join(args: argparse.Namespace) -> int:
    con = connect(args.extension)
    try:
        left_columns = create_input_table(con, args.left, "gsq_left")
        right_columns = create_input_table(con, args.right, "gsq_right")
        require_columns(left_columns, [args.on])
        require_columns(right_columns, [args.on])
        _, _, left_levels = detector_for_query(con, input_relation(args.left), args.on)
        _, _, right_levels = detector_for_query(con, input_relation(args.right), args.on)
        if left_levels != right_levels:
            raise CLIError(f"input levels differ: left={left_levels}, right={right_levels}")
        join_type = args.how.upper()
        right_select = f"r.* EXCLUDE ({sql_identifier(args.on)})"
        query = (
            f"SELECT l.*, {right_select} FROM gsq_left l {join_type} JOIN gsq_right r "
            f"ON l.{sql_identifier(args.on)} = r.{sql_identifier(args.on)}"
        )
        copy_atomic(con, query, args.out)
        return 0
    finally:
        con.close()


def rows_for_geojson(
    con: duckdb.DuckDBPyConnection,
    domain: str,
    gid_column: str,
    crs: str,
    allow_candidate: bool,
) -> tuple[list[str], list[tuple[Any, ...]]]:
    columns = [row[0] for row in con.execute("DESCRIBE gsq_input").fetchall()]
    require_columns(columns, [gid_column])
    args = "true" if allow_candidate else "false"
    selected = ", ".join(sql_identifier(name) for name in columns)
    query = (
        f"SELECT {selected}, cell_to_geometry({sql_string(domain)}, {sql_identifier(gid_column)}, "
        f"{sql_string(crs)}, 'geojson', {args}) AS __gsq_geometry FROM gsq_input"
    )
    try:
        cursor = con.execute(query)
        return [item[0] for item in cursor.description], cursor.fetchall()
    except Exception as exc:
        raise CLIError(f"geometry conversion failed: {exc}") from exc


def run_to_geo(args: argparse.Namespace) -> int:
    domain = args.domain.upper()
    con = connect(args.extension)
    if domain in CANDIDATE_DOMAINS and not args.allow_candidate_crs:
        con.close()
        raise CLIError(f"domain {domain} is a candidate CRS; pass --allow-candidate-crs for technical use")
    try:
        create_input_table(con, args.input)
        columns = [row[0] for row in con.execute("DESCRIBE gsq_input").fetchall()]
        require_columns(columns, [args.gid_column])
        if pathlib.Path(args.out).suffix.lower() == ".geojson":
            temporary, destination = temp_output(args.out)
            try:
                names, rows = rows_for_geojson(
                    con, domain, args.gid_column, args.crs, args.allow_candidate_crs
                )
                geometry_index = names.index("__gsq_geometry")
                features = []
                for row in rows:
                    geometry = json.loads(row[geometry_index]) if row[geometry_index] else None
                    properties = {name: json_value(row[i]) for i, name in enumerate(names) if i != geometry_index}
                    features.append({"type": "Feature", "geometry": geometry, "properties": properties})
                with open(temporary, "w", encoding="utf-8") as handle:
                    json.dump({"type": "FeatureCollection", "features": features}, handle, separators=(",", ":"))
                os.replace(temporary, destination)
            except Exception as exc:
                try:
                    os.unlink(temporary)
                except FileNotFoundError:
                    pass
                if isinstance(exc, CLIError):
                    raise
                raise CLIError(f"could not write {args.out}: {exc}") from exc
        else:
            selected = ", ".join(sql_identifier(name) for name in columns)
            query = (
                f"SELECT {selected}, cell_to_geometry({sql_string(domain)}, {sql_identifier(args.gid_column)}, "
                f"{sql_string(args.crs)}, 'wkt', {'true' if args.allow_candidate_crs else 'false'}) AS wkt FROM gsq_input"
            )
            copy_atomic(con, query, args.out)
        return 0
    finally:
        con.close()


def check_problems(report: dict[str, Any]) -> list[str]:
    problems: list[str] = []
    if report["rows"] == 0:
        problems.append("no rows with a GID value")
    for row_no, gid in report["invalid_gids"]:
        problems.append(f"invalid GID at row {row_no}: {gid if gid is not None else 'NULL'}")
    for gid, count in report["duplicate_gids"]:
        problems.append(f"duplicate GID {gid}: {count} rows")
    if len(report["levels"]) > 1:
        problems.append(f"mixed levels: {report['levels']}")
    totals = report["totals"]
    if totals and totals["difference"] != 0 and abs(totals["difference"]) > 1e-9 * max(1.0, abs(totals["source_total"])):
        problems.append(
            f"{totals['column']} total differs from source by {totals['difference']}: "
            f"input {totals['input_total']}, source {totals['source_total']}"
        )
    return problems


def column_total(con: duckdb.DuckDBPyConnection, table: str, column: str) -> float:
    try:
        row = con.execute(f"SELECT coalesce(sum(CAST({sql_identifier(column)} AS DOUBLE)), 0) FROM {table}").fetchone()
    except Exception as exc:
        raise CLIError(f"could not sum {column} in {table}: {exc}") from exc
    return float(row[0])


def gid_report(con: duckdb.DuckDBPyConnection, domain: str, gid_column: str) -> dict[str, Any]:
    """Check the GIDs of table gsq_input. Shared by `gsq check` and `gsq export`."""

    try:
        # Probe the domain once: the engine raises for an unknown domain, and per-row TRY would hide it.
        con.execute(f"SELECT gid_to_canonical({sql_string(domain)}, 'H2X545V39')").fetchall()
    except Exception as exc:
        raise UsageError(f"unknown domain {domain}: {exc}") from exc
    columns = [row[0] for row in con.execute("DESCRIBE gsq_input").fetchall()]
    require_columns(columns, [gid_column])
    gid = sql_identifier(gid_column)
    con.execute(
        "CREATE OR REPLACE TEMP TABLE gsq_check AS SELECT row_number() OVER () AS row_no, "
        f"CAST({gid} AS VARCHAR) AS gid, "
        f"TRY(gid_to_canonical({sql_string(domain)}, CAST({gid} AS VARCHAR))) AS cell FROM gsq_input"
    )
    return {
        "rows": con.execute("SELECT count(*) FROM gsq_check").fetchone()[0],
        "levels": [r[0] for r in con.execute(
            "SELECT DISTINCT cell.level FROM gsq_check WHERE cell IS NOT NULL ORDER BY 1").fetchall()],
        "invalid_gids": [list(r) for r in con.execute(
            "SELECT row_no, gid FROM gsq_check WHERE cell IS NULL ORDER BY row_no").fetchall()],
        "duplicate_gids": [list(r) for r in con.execute(
            "SELECT gid, count(*) FROM gsq_check WHERE gid IS NOT NULL "
            "GROUP BY gid HAVING count(*) > 1 ORDER BY gid").fetchall()],
        "totals": None,
    }


def run_check(args: argparse.Namespace) -> int:
    domain = args.domain.upper()
    if args.value and not args.source:
        raise UsageError("--value needs --source")
    if args.source and not args.value:
        raise UsageError("--source needs --value")
    con = connect(args.extension)
    try:
        columns = create_input_table(con, args.input)
        require_columns(columns, [args.gid_column])
        report: dict[str, Any] = {
            "input": args.input,
            "gid_column": args.gid_column,
            "domain": domain,
            **gid_report(con, domain, args.gid_column),
        }
        if args.source:
            source_columns = create_input_table(con, args.source, "gsq_source")
            require_columns(columns, [args.value])
            require_columns(source_columns, [args.value])
            input_total = column_total(con, "gsq_input", args.value)
            source_total = column_total(con, "gsq_source", args.value)
            report["totals"] = {
                "column": args.value,
                "input_total": input_total,
                "source": args.source,
                "source_total": source_total,
                "difference": input_total - source_total,
            }
        problems = check_problems(report)
        report["problems"] = problems
        report["ok"] = not problems
        if args.json:
            print(json.dumps(report, separators=(",", ":")))
        else:
            print(f"{args.input}: {report['rows']} rows, domain {domain}, levels {report['levels']}")
            for problem in problems:
                print(f"  {problem}")
            print("OK" if report["ok"] else f"{len(problems)} problem(s) found")
        return 0 if report["ok"] else 1
    finally:
        con.close()


GEOMETRY_COLUMN = "geometry"
WGS84_PROJJSON = {
    "$schema": "https://proj.org/schemas/v0.7/projjson.schema.json",
    "type": "GeographicCRS",
    "name": "WGS 84",
    "datum": {
        "type": "GeodeticReferenceFrame",
        "name": "World Geodetic System 1984",
        "ellipsoid": {"name": "WGS 84", "semi_major_axis": 6378137, "inverse_flattening": 298.257223563},
    },
    "coordinate_system": {
        "subtype": "ellipsoidal",
        "axis": [
            {"name": "Geodetic longitude", "abbreviation": "Lon", "direction": "east", "unit": "degree"},
            {"name": "Geodetic latitude", "abbreviation": "Lat", "direction": "north", "unit": "degree"},
        ],
    },
    "id": {"authority": "EPSG", "code": 4326},
}


def polygon_wkb(geometry: dict[str, Any]) -> bytes:
    """Encode a GeoJSON Polygon as little-endian WKB (ISO type 3)."""

    rings = geometry["coordinates"]
    parts = [struct.pack("<BII", 1, 3, len(rings))]
    for ring in rings:
        parts.append(struct.pack("<I", len(ring)))
        parts.extend(struct.pack("<dd", x, y) for x, y in ring)
    return b"".join(parts)


def kepler_type(duck_type: str) -> str:
    name = duck_type.upper()
    if "INTERVAL" in name:
        return "string"
    if "INT" in name:
        return "integer"
    if name.startswith(("DOUBLE", "FLOAT", "REAL", "DECIMAL")):
        return "real"
    return "string"


def kepler_config(dataset: str, bounds: list[float], color: str | None, color_type: str | None) -> dict[str, Any]:
    """A Kepler.gl map config with one GeoJSON layer on the geometry column."""

    layer: dict[str, Any] = {
        "dataId": dataset,
        "label": dataset,
        "isVisible": True,
        "columns": {"geojson": GEOMETRY_COLUMN},
        "visConfig": {"opacity": 0.8, "filled": True, "stroked": False, "thickness": 0.5},
    }
    if color:
        layer["colorField"] = {"name": color, "type": color_type}
        layer["colorScale"] = "quantile" if color_type in {"integer", "real"} else "ordinal"
        layer["colorRange"] = {
            "name": "GeoSquare",
            "type": "sequential",
            "category": "Custom",
            "colors": ["#5A1846", "#900C3F", "#C70039", "#E3611C", "#F1920E", "#FFC300"],
        }
    west, south, east, north = bounds
    return {
        "version": "v1",
        "config": {
            "visState": {"layers": [{"id": f"{dataset}-layer", "type": "geojson", "config": layer}]},
            "mapState": {"bounds": {"west": west, "south": south, "east": east, "north": north}},
        },
    }


def run_export(args: argparse.Namespace) -> int:
    if args.color and not args.kepler:
        raise UsageError("--color needs --kepler")
    if output_format(args.out) != "parquet":
        raise UsageError(f"export writes GeoParquet; --out must end in .parquet: {args.out}")
    domain = args.domain.upper()
    if domain in CANDIDATE_DOMAINS and not args.allow_candidate_crs:
        raise CLIError(f"domain {domain} is a candidate CRS; pass --allow-candidate-crs for technical use")
    con = connect(args.extension)
    try:
        create_input_table(con, args.input)
        types = {row[0]: row[1] for row in con.execute("DESCRIBE gsq_input").fetchall()}
        if GEOMETRY_COLUMN in types:
            raise CLIError(f"input already has a column named {GEOMETRY_COLUMN}")
        if args.color:
            require_columns(types, [args.color])
        report = gid_report(con, domain, args.gid_column)
        problems = check_problems(report)
        if problems:
            raise CLIError("input failed gsq check:\n  " + "\n  ".join(problems))

        names, rows = rows_for_geojson(con, domain, args.gid_column, "EPSG:4326", args.allow_candidate_crs)
        west = south = float("inf")
        east = north = float("-inf")
        values = []
        for row in rows:
            geometry = json.loads(row[-1])
            for lon, lat in geometry["coordinates"][0]:
                west, east = min(west, lon), max(east, lon)
                south, north = min(south, lat), max(north, lat)
            values.append((*row[:-1], polygon_wkb(geometry)))

        con.execute("CREATE OR REPLACE TEMP TABLE gsq_export AS SELECT * FROM gsq_input LIMIT 0")
        con.execute(f"ALTER TABLE gsq_export ADD COLUMN {sql_identifier(GEOMETRY_COLUMN)} BLOB")
        placeholders = ", ".join("?" for _ in range(len(names)))
        con.executemany(f"INSERT INTO gsq_export VALUES ({placeholders})", values)

        bbox = [west, south, east, north]
        geo = {
            "version": "1.1.0",
            "primary_column": GEOMETRY_COLUMN,
            "columns": {
                GEOMETRY_COLUMN: {
                    "encoding": "WKB",
                    "geometry_types": ["Polygon"],
                    "crs": WGS84_PROJJSON,
                    "bbox": bbox,
                }
            },
        }
        copy_atomic(con, "SELECT * FROM gsq_export", args.out, {"geo": json.dumps(geo, separators=(",", ":"))})

        if args.kepler:
            dataset = pathlib.Path(args.out).stem
            color_type = kepler_type(types[args.color]) if args.color else None
            config = kepler_config(dataset, bbox, args.color, color_type)
            temporary, destination = temp_output(args.kepler)
            try:
                with open(temporary, "w", encoding="utf-8") as handle:
                    json.dump(config, handle, indent=2)
                os.replace(temporary, destination)
            except OSError as exc:
                try:
                    os.unlink(temporary)
                except FileNotFoundError:
                    pass
                raise CLIError(f"could not write {args.kepler}: {exc}") from exc
        return 0
    finally:
        con.close()


PYRAMID_BASE = 12
# Coarser pyramid levels. Partition level is level - 5 (pyramid spec section 4). Fan-in is the
# number of base cells under one parent cell at that level (spec section 6.2).
PYRAMID_FAN_IN = {10: 100, 8: 10000}
PYRAMID_RULES = {"sum", "weighted_mean", "median", "max", "min"}


def cell_dataset_manifest(path: str) -> dict[str, Any]:
    """Read the manifest of the base cell dataset. Pyramid input must be one (pyramid spec section 5)."""

    folder = pathlib.Path(path)
    if not folder.is_dir():
        raise CLIError(
            f"pyramid input must be a cell dataset folder with manifest 1.1, not a file: {path}. "
            "Spec section 5 copies the grid fields and source from the base manifest, and a CSV or "
            "Parquet file has none. Write it as a dataset first: write_cell_dataset(..., manifest_version := '1.1')."
        )
    manifest_path = folder / "manifest.json"
    if not manifest_path.is_file():
        raise CLIError(f"{path} has no manifest.json; pyramid needs a cell dataset folder")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise CLIError(f"could not read {manifest_path}: {exc}") from exc
    if manifest.get("manifest_version") != "1.1":
        raise CLIError(
            f"pyramid needs manifest 1.1 (spec section 5), {path} has {manifest.get('manifest_version')!r}"
        )
    return manifest


def parse_value(text: str) -> tuple[str, str, str | None]:
    """Parse NAME=RULE or NAME=weighted_mean:WEIGHT into (name, rule, weight)."""

    name, sep, spec = text.partition("=")
    if not sep or not name or not spec:
        raise UsageError(f"--value needs NAME=RULE, got {text!r}")
    rule, _, weight = spec.partition(":")
    if rule not in PYRAMID_RULES:
        raise UsageError(f"{name}: unknown space_aggregation {rule!r}; use sum, weighted_mean, median, max or min")
    if rule == "weighted_mean" and not weight:
        raise UsageError(f"{name}: weighted_mean needs a weight, use {name}=weighted_mean:WEIGHT")
    if rule != "weighted_mean" and weight:
        raise UsageError(f"{name}: only weighted_mean takes a weight")
    return name, rule, weight or None


def rollup_sql(name: str, rule: str, weight: str | None) -> str:
    """One parent value over the base rows of a group (spec pyramid section 6.1)."""

    col = sql_identifier(name)
    if rule == "sum" or rule == "max" or rule == "min":
        return f"{rule}({col})"
    if rule == "median":
        # Null when fewer than 3 children have a value.
        return f"CASE WHEN count({col}) >= 3 THEN median({col}) END"
    w = "1.0" if weight == "area" else sql_identifier(weight)
    usable = f"{col} IS NOT NULL AND {w} IS NOT NULL"
    return (
        f"sum({col} * {w}) FILTER (WHERE {usable}) / "
        f"NULLIF(sum({w}) FILTER (WHERE {usable}), 0)"
    )


def encode_element(value: Any) -> str:
    """One element of a content-hash row (dataset-metadata 3.1). Shortcut: gid and doubles only."""

    if value is None:
        return "null"
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    if isinstance(value, float):
        if not math.isfinite(value):
            raise CLIError(f"content hash rejects {value}")
        return repr(value)
    raise CLIError(f"content hash has no encoding for {type(value).__name__}")


def content_sha256(rows: list[tuple[Any, ...]]) -> str:
    """SHA-256 over the sorted encoded rows, one line feed after each row (dataset-metadata 3.1)."""

    encoded = sorted("[" + ",".join(encode_element(v) for v in row) + "]" for row in rows)
    digest = hashlib.sha256()
    for line in encoded:
        digest.update(line.encode("utf-8") + b"\n")
    return digest.hexdigest()


def digest_of_hashes(hashes: list[tuple[str, str]]) -> str:
    """Digest of file hashes, ordered by path (pyramid spec section 6.3)."""

    digest = hashlib.sha256()
    for _, value in sorted(hashes, key=lambda item: item[0].encode("utf-8")):
        digest.update(value.encode("ascii") + b"\n")
    return digest.hexdigest()


def pyramid_check_inputs(con: duckdb.DuckDBPyConnection, values, share: str | None) -> None:
    """Refuse values that would not roll up or hash correctly. Runs on gsq_input."""

    problems: list[str] = []
    for name, _, weight in values:
        col = sql_identifier(name)
        bad = con.execute(
            f"SELECT count(*) FILTER (WHERE {col} IS NOT NULL AND TRY_CAST({col} AS DOUBLE) IS NULL), "
            f"count(*) FILTER (WHERE NOT isfinite(TRY_CAST({col} AS DOUBLE))) FROM gsq_input"
        ).fetchone()
        if bad[0]:
            problems.append(f"{name}: {bad[0]} value(s) are not numbers")
        if bad[1]:
            problems.append(f"{name}: {bad[1]} value(s) are NaN or infinite")
        if weight and weight != "area":
            negative = con.execute(
                f"SELECT count(*) FROM gsq_input WHERE TRY_CAST({sql_identifier(weight)} AS DOUBLE) < 0"
            ).fetchone()[0]
            if negative:
                problems.append(f"{weight}: {negative} negative weight(s)")
    if share:
        col = sql_identifier(share)
        bad = con.execute(
            f"SELECT count(*) FROM gsq_input WHERE {col} IS NULL OR TRY_CAST({col} AS DOUBLE) IS NULL "
            f"OR TRY_CAST({col} AS DOUBLE) < 0 OR TRY_CAST({col} AS DOUBLE) > 1"
        ).fetchone()[0]
        if bad:
            problems.append(f"{share}: {bad} share value(s) are null or outside [0, 1]")
    if problems:
        raise CLIError("input failed gsq pyramid checks:\n  " + "\n  ".join(problems))


def write_level(
    con: duckdb.DuckDBPyConnection, table: str, level: int, names: list[str], share: str | None, staging: pathlib.Path
) -> dict[str, Any]:
    """Write one level as partition files under level=<L>/gid_prefix=<p>/part-00000.parquet."""

    partition = level - 5
    columns = ["gid", *names, *([share] if share else [])]
    select = ", ".join(sql_identifier(c) for c in columns)
    prefixes = [r[0] for r in con.execute(
        f"SELECT DISTINCT left(gid, {partition}) FROM {table} ORDER BY 1").fetchall()]
    files: list[dict[str, Any]] = []
    for prefix in prefixes:
        relative = f"level={level}/gid_prefix={prefix}/part-00000.parquet"
        target = staging / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        where = f"WHERE left(gid, {partition}) = {sql_string(prefix)} ORDER BY gid"
        con.execute(f"COPY (SELECT {select} FROM {table} {where}) TO {sql_string(str(target))} (FORMAT parquet)")
        # Hash what is on disk, not the table, so the manifest describes the written file.
        rows = con.execute(f"SELECT {select} FROM read_parquet({sql_string(str(target))}) ORDER BY gid").fetchall()
        entry = {
            "gid_prefix": prefix,
            "path": relative,
            "row_count": len(rows),
            "gid_min": rows[0][0],
            "gid_max": rows[-1][0],
            # Value hash covers gid and the value columns; the share column has its own hash.
            "content_sha256": content_sha256([r[: 1 + len(names)] for r in rows]),
            "share_sha256": content_sha256([(r[0], r[-1]) for r in rows]) if share else None,
        }
        files.append(entry)
    return {
        "level": level,
        "partition_level": partition,
        "row_count": sum(f["row_count"] for f in files),
        "content_sha256": digest_of_hashes([(f["path"], f["content_sha256"]) for f in files]),
        "files": files,
    }


def run_pyramid(args: argparse.Namespace) -> int:
    base = cell_dataset_manifest(args.input)
    domain = args.domain.upper()
    if base.get("domain_code") != domain:
        raise UsageError(f"--domain {domain} does not match the dataset domain {base.get('domain_code')}")
    values = [parse_value(text) for text in args.value]
    if not values:
        raise UsageError("pyramid needs at least one --value NAME=RULE")
    names = [name for name, _, _ in values]
    if len(set(names)) != len(names):
        raise UsageError("each --value column may be given once")
    summed = {name for name, rule, _ in values if rule == "sum"}
    semantics = {field["name"]: field.get("semantics") for field in base.get("value_fields", [])}
    for name, _, weight in values:
        if weight and weight != "area" and weight not in summed:
            raise UsageError(f"{name}: weight {weight} must be a column declared with sum")
        if weight and weight != "area" and semantics.get(weight) not in ("COUNT", "TOTAL"):
            raise UsageError(f"{name}: weight {weight} must be a COUNT or TOTAL column (spec section 8, item 11)")
    out = pathlib.Path(args.out)
    if out.exists():
        raise CLIError(f"output already exists: {out}")
    if not out.parent.is_dir():
        raise CLIError(f"output directory does not exist: {out.parent}")
    if args.share_column and args.share_column in names:
        raise UsageError("the share column cannot also be a value column")

    con = connect(args.extension)
    try:
        columns = create_input_table(con, args.input)
        requested = [args.gid_column, *names, *([args.share_column] if args.share_column else [])]
        require_columns(columns, requested)
        report = gid_report(con, domain, args.gid_column)
        problems = check_problems(report)
        if report["levels"] != [PYRAMID_BASE]:
            problems.append(f"pyramid base must be level {PYRAMID_BASE}, found {report['levels']}")
        if problems:
            raise CLIError("input failed gsq check:\n  " + "\n  ".join(problems))
        pyramid_check_inputs(con, values, args.share_column)

        gid = sql_identifier(args.gid_column)
        cast = ", ".join(f"CAST({sql_identifier(n)} AS DOUBLE) AS {sql_identifier(n)}" for n in names)
        share_select = f", CAST({sql_identifier(args.share_column)} AS DOUBLE) AS {sql_identifier(args.share_column)}" if args.share_column else ""
        con.execute(
            f"CREATE OR REPLACE TEMP TABLE gsq_base AS SELECT CAST({gid} AS VARCHAR) AS gid, {cast}{share_select} FROM gsq_input"
        )
        base_rows = con.execute(f"SELECT count(*) FROM gsq_base").fetchone()[0]

        staging = pathlib.Path(tempfile.mkdtemp(prefix=".pyramid-", dir=out.parent))
        try:
            levels = [write_level(con, "gsq_base", PYRAMID_BASE, names, args.share_column, staging)]
            for level, fan_in in PYRAMID_FAN_IN.items():
                parent_sql = f"left(gid, {level}) AS gid"
                aggregates = ", ".join(
                    f"{rollup_sql(name, rule, weight)} AS {sql_identifier(name)}" for name, rule, weight in values
                )
                share_agg = (
                    f", sum({sql_identifier(args.share_column)}) / {fan_in} AS {sql_identifier(args.share_column)}"
                    if args.share_column else ""
                )
                con.execute(
                    f"CREATE OR REPLACE TEMP TABLE gsq_level_{level} AS SELECT {parent_sql}, {aggregates}{share_agg} "
                    f"FROM gsq_base GROUP BY 1"
                )
                levels.append(write_level(con, f"gsq_level_{level}", level, names, args.share_column, staging))

            base_level = levels[0]
            columns_meta = [
                {
                    "name": name,
                    "parquet_type": "double",
                    "space_aggregation": rule,
                    "space_weight": weight,
                }
                for name, rule, weight in values
            ]
            manifest = {
                "pyramid_version": "1.0",
                "grid_system": base["grid_system"],
                "grid_version": base["grid_version"],
                "domain_code": base["domain_code"],
                "profile_version": base["profile_version"],
                "registry_version": base["registry_version"],
                "source": {
                    "dataset_id": pathlib.Path(args.input).resolve().name,
                    "manifest_version": base["manifest_version"],
                    "base_level": PYRAMID_BASE,
                    "row_count": base_rows,
                    "content_sha256": base_level["content_sha256"],
                },
                "share_column": args.share_column,
                "columns": columns_meta,
                "levels": levels,
                "created_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
                "created_by": f"gsq pyramid {__version__}",
            }
            with open(staging / "pyramid.json", "w", encoding="utf-8") as handle:
                json.dump(manifest, handle, indent=2)
            os.replace(staging, out)
        except BaseException:
            shutil.rmtree(staging, ignore_errors=True)
            raise
        files = sum(len(level["files"]) for level in levels)
        print(f"wrote {out}: {len(levels)} levels, {files} files, base rows {base_rows}")
        return 0
    finally:
        con.close()


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="gsq", description="GeoSquare engine command-line tool")
    parser.add_argument("--version", action="version", version=__version__)
    parser.add_argument("--extension", help="path to the geosquare_v2 DuckDB extension")
    sub = parser.add_subparsers(dest="command", required=True)

    detect = sub.add_parser("detect", help="detect GeoSquare columns")
    detect.add_argument("input")
    detect.add_argument("--extension", dest="command_extension")
    detect.add_argument("--domain")
    detect.add_argument("--level", type=int)
    detect.add_argument("--max-rows", type=int)
    detect.add_argument("--format", choices=("table", "json"), default="table")
    detect.set_defaults(func=run_detect)

    rollup = sub.add_parser("rollup", help="roll cells up to an ancestor level")
    rollup.add_argument("input")
    rollup.add_argument("--extension", dest="command_extension")
    rollup.add_argument("--level", type=int, required=True)
    rollup.add_argument("--out", required=True)
    rollup.add_argument("--sum", action="append", default=[])
    rollup.set_defaults(func=run_rollup)

    join = sub.add_parser("join", help="join two cell tables")
    join.add_argument("left")
    join.add_argument("right")
    join.add_argument("--extension", dest="command_extension")
    join.add_argument("--on", choices=("gid", "packed_id"), required=True)
    join.add_argument("--how", choices=("inner", "left"), default="inner")
    join.add_argument("--out", required=True)
    join.set_defaults(func=run_join)

    geo = sub.add_parser("to-geo", help="convert cells to geometry")
    geo.add_argument("input")
    geo.add_argument("--extension", dest="command_extension")
    geo.add_argument("--domain", required=True)
    geo.add_argument("--out", required=True)
    geo.add_argument("--gid-column", default="gid")
    geo.add_argument("--crs", choices=("EPSG:4326", "grid"), default="EPSG:4326")
    geo.add_argument("--allow-candidate-crs", action="store_true")
    geo.set_defaults(func=run_to_geo)

    export = sub.add_parser("export", help="write cells as GeoParquet, with an optional Kepler.gl config")
    export.add_argument("input")
    export.add_argument("--extension", dest="command_extension")
    export.add_argument("--domain", required=True, help="domain of the GIDs, e.g. ID")
    export.add_argument("-o", "--out", required=True, help="output .parquet file")
    export.add_argument("--gid-column", default="gid")
    export.add_argument("--allow-candidate-crs", action="store_true")
    export.add_argument("--kepler", help="also write a Kepler.gl map config (JSON) to this path")
    export.add_argument("--color", help="column to color the Kepler layer by (needs --kepler)")
    export.set_defaults(func=run_export)

    check = sub.add_parser("check", help="check GID quality before upload")
    check.add_argument("input")
    check.add_argument("--extension", dest="command_extension")
    check.add_argument("--domain", required=True, help="domain of the GIDs, e.g. ID")
    check.add_argument("--gid-column", default="gid")
    check.add_argument("--source", help="source CSV or Parquet to compare column totals with")
    check.add_argument("--value", help="column to total in both files (with --source)")
    check.add_argument("--json", action="store_true", help="print a JSON report")
    check.set_defaults(func=run_check)

    pyramid = sub.add_parser("pyramid", help="write a cell pyramid: levels 12, 10 and 8 for a map")
    pyramid.add_argument("input", help="base level 12 cell dataset folder with manifest 1.1")
    pyramid.add_argument("--extension", dest="command_extension")
    pyramid.add_argument("--domain", required=True, help="domain of the GIDs, e.g. ID")
    pyramid.add_argument("-o", "--out", required=True, help="new folder to write, e.g. out/pyramid")
    pyramid.add_argument("--gid-column", default="gid")
    pyramid.add_argument("--value", action="append", default=[], metavar="NAME=RULE",
                         help="value column and its rule: sum, max, min, median, or weighted_mean:WEIGHT (repeatable)")
    pyramid.add_argument("--share-column", help="edge share column, a number in [0, 1]")
    pyramid.set_defaults(func=run_pyramid)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if getattr(args, "command_extension", None):
        args.extension = args.command_extension
    try:
        return args.func(args)
    except UsageError as exc:
        print(f"gsq: {exc}", file=sys.stderr)
        return 2
    except CLIError as exc:
        print(f"gsq: {exc}", file=sys.stderr)
        return 1
    except (duckdb.Error, OSError, ValueError) as exc:
        print(f"gsq: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
