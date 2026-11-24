"""The ``gsq`` command-line interface.

All grid operations are delegated to the geosquare_v2 DuckDB extension.  This
module only handles input/output formats, argument validation, and atomic file
writes.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import struct
import sys
import tempfile
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
