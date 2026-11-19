"""Baca/tulis dataset sel GeoSquare V2 (flat, partitioned, Iceberg) dengan DuckDB + ekstensi geosquare_v2.

Sama dengan examples/ts/src/node.ts, untuk pipeline Python, notebook dan backend Python.

    from geosquare_cells import GeosquareCells
    gs = GeosquareCells("build/linux_amd64/geosquare_v2.duckdb_extension", s3_region="ap-southeast-3")
    gs.summarize(PEOPLE, {"wkt": "POLYGON ((...))"}, ["people_counts"])
    gs.rollup(PEOPLE, {"bbox": [106.80, -6.20, 106.84, -6.16]}, ["people_counts"], level=9)
    gs.write("SELECT packed_id, population FROM read_parquet('in.parquet')", "s3://bucket/pop", layout="iceberg")

Area: {"wkt": "..."} | {"geojson": {...}} (dict GeoJSON geometry) | {"bbox": [xmin, ymin, xmax, ymax]}.
"""
from __future__ import annotations

import json
import math
import re
from typing import Any, Callable, Iterable, Sequence

import duckdb

Area = dict


class GeosquareError(Exception):
    """Kesalahan input atau dataset (pesan untuk user)."""


def sql_string(s: str) -> str:
    return "'" + s.replace("'", "''") + "'"


def ident(s: str) -> str:
    return '"' + s.replace('"', '""') + '"'


def _num(v: float) -> str:
    if not math.isfinite(v):
        raise GeosquareError(f"Angka tidak valid: {v}")
    return repr(float(v))


def _ring(r: Sequence[Sequence[float]]) -> str:
    return "(" + ", ".join(f"{_num(p[0])} {_num(p[1])}" for p in r) + ")"


def geojson_to_wkt(g: dict) -> str:
    """GeoJSON geometry (lon/lat) -> WKT yang dipahami ekstensi."""
    t, c = g["type"], g.get("coordinates")
    if t == "Point":
        return f"POINT ({_num(c[0])} {_num(c[1])})"
    if t == "MultiPoint":
        return "MULTIPOINT (" + ", ".join(f"({_num(p[0])} {_num(p[1])})" for p in c) + ")"
    if t == "LineString":
        return "LINESTRING " + _ring(c)
    if t == "MultiLineString":
        return "MULTILINESTRING (" + ", ".join(_ring(r) for r in c) + ")"
    if t == "Polygon":
        return "POLYGON (" + ", ".join(_ring(r) for r in c) + ")"
    if t == "MultiPolygon":
        return "MULTIPOLYGON (" + ", ".join("(" + ", ".join(_ring(r) for r in p) + ")" for p in c) + ")"
    raise GeosquareError(f"Geometri {t} tidak didukung")


def area_arg(area: Area) -> str:
    if "bbox" in area:
        return "bbox := [" + ", ".join(_num(v) for v in area["bbox"]) + "]"
    wkt = area["wkt"] if "wkt" in area else geojson_to_wkt(area["geojson"])
    return f"geometry := {sql_string(wkt)}"


def overlap_column(area: Area) -> str:
    if "bbox" in area:
        return "coverage_ratio"
    kind = (re.split(r"[\s(]", area["wkt"].strip())[0] if "wkt" in area else area["geojson"]["type"]).upper()
    if kind.endswith("POINT"):
        return "point_count"
    if kind.endswith("LINESTRING"):
        return "length_ratio"
    return "coverage_ratio"


def _source(path: str, area: Area, min_coverage: float | None, coverage_mode: str | None) -> str:
    extra = ""
    if min_coverage is not None:
        extra += f", min_coverage := {_num(min_coverage)}"
    if coverage_mode:
        extra += f", coverage_mode := {sql_string(coverage_mode)}"
    return f"query_cell_dataset({sql_string(path)}, {area_arg(area)}{extra})"


def _plain(row: dict) -> dict:
    out = {}
    for k, v in row.items():
        if k == "packed_id" and v is not None:
            v = str(v)
        elif k == "geometry" and isinstance(v, str):
            v = json.loads(v)
        out[k] = v
    return out


class GeosquareCells:
    """Satu koneksi DuckDB dengan ekstensi geosquare_v2 (pakai ulang; cache metadata Iceberg per proses)."""

    def __init__(self, extension: str, *, s3_region: str | None = None, s3_keys: dict | None = None,
                 memory_limit: str | None = None, threads: int | None = None, database: str = ":memory:"):
        config = {"allow_unsigned_extensions": "true"}
        if memory_limit:
            config["memory_limit"] = memory_limit
        if threads:
            config["threads"] = str(threads)
        self.con = duckdb.connect(database, config=config)
        try:
            self.con.execute("INSTALL httpfs; LOAD httpfs")
            if re.match(r"^https?://", extension):
                self.con.execute(f"INSTALL geosquare_v2 FROM {sql_string(extension)}")
                self.con.execute("LOAD geosquare_v2")
            else:
                self.con.execute(f"LOAD {sql_string(extension)}")
            if s3_keys:   # {"key_id", "secret", "region", optional "session_token", "endpoint"}
                parts = [f"KEY_ID {sql_string(s3_keys['key_id'])}", f"SECRET {sql_string(s3_keys['secret'])}",
                         f"REGION {sql_string(s3_keys.get('region') or s3_region or 'ap-southeast-3')}"]
                if s3_keys.get("session_token"):
                    parts.append(f"SESSION_TOKEN {sql_string(s3_keys['session_token'])}")
                if s3_keys.get("endpoint"):
                    parts.append(f"ENDPOINT {sql_string(s3_keys['endpoint'])}")
                self.con.execute(f"CREATE OR REPLACE SECRET gs_s3 (TYPE s3, {', '.join(parts)})")
            elif s3_region:
                self.con.execute(f"CREATE OR REPLACE SECRET gs_s3 (TYPE s3, PROVIDER credential_chain, REGION {sql_string(s3_region)})")
        except duckdb.Error as e:
            self.con.close()
            raise GeosquareError(f"DuckDB/ekstensi geosquare_v2 tidak bisa dimuat: {e}") from e

    # -- dasar -----------------------------------------------------------------------------------
    def query(self, sql: str) -> list[dict]:
        """SELECT -> list of dict (packed_id sebagai teks, geometry GeoJSON sebagai dict)."""
        try:
            cur = self.con.execute(sql)
            names = [d[0] for d in cur.description]
            return [_plain(dict(zip(names, r))) for r in cur.fetchall()]
        except duckdb.Error as e:
            raise GeosquareError(str(e)) from e

    def df(self, sql: str):
        """SELECT -> pandas DataFrame."""
        return self.con.execute(sql).df()

    def columns(self, path: str) -> dict[str, str]:
        rows = self.query(f"SELECT column_name, column_type FROM (DESCRIBE SELECT * FROM read_cell_dataset({sql_string(path)}))")
        return {r["column_name"]: r["column_type"] for r in rows}

    # -- baca ------------------------------------------------------------------------------------
    def cells_sql(self, path: str, area: Area, columns: Sequence[str] | None = None, *, min_coverage: float | None = None,
                  coverage_mode: str | None = None, with_geometry: bool = False, limit: int | None = None) -> str:
        extra = overlap_column(area)
        select = ", ".join(["gid", "packed_id", extra, *map(ident, columns)]) if columns else "*"
        geom = ", cell_to_geometry(packed_id, 'EPSG:4326', 'geojson') AS geometry" if with_geometry else ""
        lim = f" LIMIT {int(limit)}" if limit is not None else ""
        return f"SELECT {select}{geom} FROM {_source(path, area, min_coverage, coverage_mode)} ORDER BY packed_id{lim}"

    def cells(self, path: str, area: Area, columns: Sequence[str] | None = None, **kw) -> list[dict]:
        """Sel di dalam area: gid, packed_id, coverage_ratio/length_ratio/point_count + kolom."""
        return self.query(self.cells_sql(path, area, columns, **kw))

    def summarize(self, path: str, area: Area, columns: Sequence[str], *, weighted: bool = True,
                  min_coverage: float | None = None, coverage_mode: str | None = None) -> dict:
        """Total kolom di dalam area (default berbobot coverage_ratio) + jumlah sel."""
        if not columns:
            raise GeosquareError("Pilih minimal satu kolom")
        extra = overlap_column(area)
        w = "" if not weighted or extra == "point_count" else f" * {extra}"
        sums = ", ".join(f"sum({ident(c)}{w}) AS {ident(c)}" for c in columns)
        return self.query(f"SELECT count(*) AS cells, {sums} FROM {_source(path, area, min_coverage, coverage_mode)}")[0]

    def rollup(self, path: str, area: Area, columns: Sequence[str], level: int = 9, *, weighted: bool = True) -> list[dict]:
        """Total per sel induk (9 = 1 km, 7 = 10 km) dengan geometri GeoJSON."""
        if not 1 <= int(level) <= 12:
            raise GeosquareError(f"Level {level} tidak valid")
        extra = overlap_column(area)
        w = "" if not weighted or extra == "point_count" else f" * {extra}"
        sums = ", ".join(f"sum({ident(c)}{w}) AS {ident(c)}" for c in columns)
        return self.query(
            f"SELECT left(gid, {level}) AS gid, count(*) AS cells, {sums}, "
            f"cell_to_geometry(pack(any_value(unpack_int64(packed_id).domain_id), left(gid, {level})), 'EPSG:4326', 'geojson') AS geometry "
            f"FROM {_source(path, area, None, None)} GROUP BY 1 ORDER BY 1")

    def at_points(self, path: str, points: Sequence[tuple[float, float]], columns: Sequence[str]) -> list[dict]:
        """Nilai di sel tiap titik (lon, lat); satu baris per titik, urutan input."""
        if not points:
            raise GeosquareError("Daftar titik kosong")
        values = ", ".join(f"({i}, {_num(x)}::DOUBLE, {_num(y)}::DOUBLE)" for i, (x, y) in enumerate(points))
        mp = "MULTIPOINT (" + ", ".join(f"({_num(x)} {_num(y)})" for x, y in points) + ")"
        cols = "".join(f", d.{ident(c)}" for c in columns)
        return self.query(
            f"WITH pts(i, lon, lat) AS (VALUES {values}), "
            f"c AS (SELECT i, lon, lat, point_to_cell('ID', lon, lat, 12) AS cell FROM pts), "
            f"d AS (SELECT * FROM query_cell_dataset({sql_string(path)}, geometry := {sql_string(mp)})) "
            f"SELECT c.i AS point, c.lon, c.lat, c.cell.gid AS gid{cols} "
            f"FROM c LEFT JOIN d ON d.packed_id = c.cell.packed_id ORDER BY c.i")

    def feature_collection(self, path: str, area: Area, columns: Sequence[str]) -> dict:
        rows = self.cells(path, area, columns, with_geometry=True)
        return {"type": "FeatureCollection",
                "features": [{"type": "Feature", "geometry": r.pop("geometry"), "properties": r} for r in rows]}

    def stream(self, path: str, on_batch: Callable[[list[dict]], Any], *, area: Area | None = None,
               columns: Sequence[str] | None = None, batch: int = 10000) -> int:
        """Baca per batch (ekspor besar). Mengembalikan jumlah baris."""
        if area:
            sql = self.cells_sql(path, area, columns)
        else:
            sel = ", ".join(map(ident, ["gid", "packed_id", *columns])) if columns else "*"
            sql = f"SELECT {sel} FROM read_cell_dataset({sql_string(path)})"
        cur = self.con.execute(sql)
        names = [d[0] for d in cur.description]
        n = 0
        while rows := cur.fetchmany(batch):
            on_batch([_plain(dict(zip(names, r))) for r in rows])
            n += len(rows)
        return n

    # -- tulis -----------------------------------------------------------------------------------
    def write(self, query: str, path: str, *, layout: str = "iceberg", mode: str | None = None,
              partition_level: int | None = 7, row_group_size: int | None = None,
              value_semantics: str | dict | None = None, aggregation_rule: str | dict | None = None,
              fill_nulls: bool | None = None, column_metrics: str | None = None, domain_code: str | None = None,
              notes: str | None = None) -> dict:
        """Tulis hasil query (packed_id atau gid + kolom nilai) sebagai dataset sel. -> {layout, path, files, rows}"""
        opts = {"layout": layout, "mode": mode, "partition_level": None if layout == "flat" else partition_level,
                "row_group_size": row_group_size, "value_semantics": value_semantics,
                "aggregation_rule": aggregation_rule, "fill_nulls": fill_nulls, "column_metrics": column_metrics,
                "domain_code": domain_code, "notes": notes}
        args = ""
        for k, v in opts.items():
            if v is None:
                continue
            if isinstance(v, bool):
                args += f", {k} := {str(v).lower()}"
            elif isinstance(v, (int, float)):
                args += f", {k} := {v}"
            else:
                args += f", {k} := {sql_string(v if isinstance(v, str) else json.dumps(v))}"
        r = self.query(f"SELECT * FROM write_cell_dataset({sql_string(query)}, {sql_string(path)}{args})")
        return r[0] if r else {}

    def close(self) -> None:
        self.con.close()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
