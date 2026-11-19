/**
 * SQL bersama untuk membaca/menulis dataset sel GeoSquare V2 (flat, partitioned, Iceberg) lewat
 * ekstensi DuckDB `geosquare_v2`. Dipakai oleh `node.ts` (backend, @duckdb/node-api) dan `web.ts`
 * (frontend, @duckdb/duckdb-wasm); isinya fungsi murni tanpa koneksi.
 */
import type { Geometry, Position } from 'geojson';

/** Area query: WKT (lon/lat), GeoJSON geometry, atau bbox [xmin, ymin, xmax, ymax]. */
export type Area =
  | { wkt: string }
  | { geojson: Geometry }
  | { bbox: [number, number, number, number] };

export interface CellQueryOptions {
  /** Kolom nilai yang diambil; default semua. Sedikit kolom = sedikit byte yang dibaca. */
  columns?: string[];
  /** Sel dengan coverage_ratio < nilai ini dibuang (0 = semua sel yang bersinggungan). */
  minCoverage?: number;
  /** GRID_PLANAR (default) atau EQUAL_AREA. */
  coverageMode?: 'GRID_PLANAR' | 'EQUAL_AREA';
  /** Sertakan batas sel sebagai GeoJSON (lon/lat) di kolom `geometry`. */
  withGeometry?: boolean;
  limit?: number;
}

export interface SummaryOptions {
  /** true (default): sum(nilai × coverage_ratio), perkiraan terbaik untuk COUNT di dalam area. */
  weighted?: boolean;
  minCoverage?: number;
  coverageMode?: 'GRID_PLANAR' | 'EQUAL_AREA';
}

export interface WriteOptions {
  layout: 'flat' | 'partitioned' | 'iceberg';
  mode?: 'create' | 'overwrite' | 'append';
  /** Level partisi (5 = 100 km, 6 = 50 km, 7 = 10 km); tabel lebar → 7. */
  partitionLevel?: number;
  rowGroupSize?: number;
  valueSemantics?: string | Record<string, string>;
  aggregationRule?: string | Record<string, string>;
  /** Default true: NULL numerik ditulis 0. */
  fillNulls?: boolean;
  /** 'key' untuk tabel lebar (> ~100 kolom). */
  columnMetrics?: 'auto' | 'key' | 'all';
  domainCode?: string;
  notes?: string;
}

/** Literal string SQL. */
export const sqlString = (s: string) => `'${s.split("'").join("''")}'`;

/** Nama kolom SQL. */
export const ident = (s: string) => `"${s.split('"').join('""')}"`;

const num = (v: number) => {
  if (!Number.isFinite(v)) throw new GeosquareError(`Angka tidak valid: ${v}`);
  return String(v);
};

/** Nilai DuckDB → nilai JS biasa (bigint → number/teks, tanggal → ISO), seperti parquet.ts. */
export function plain(v: unknown): unknown {
  if (typeof v === 'bigint')
    return v <= BigInt(Number.MAX_SAFE_INTEGER) && v >= BigInt(Number.MIN_SAFE_INTEGER)
      ? Number(v)
      : v.toString();
  if (v instanceof Date) {
    const iso = v.toISOString();
    return iso.endsWith('T00:00:00.000Z') ? iso.slice(0, 10) : iso;
  }
  if (v instanceof Uint8Array) return undefined;
  if (Array.isArray(v)) return v.map(plain);
  if (v && typeof v === 'object')
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, plain(x)]));
  return v;
}

/** packed_id selalu teks (int64 bisa melebihi Number.MAX_SAFE_INTEGER). */
export function plainRow(row: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(row)) {
    const value = k === 'packed_id' && v !== null && v !== undefined ? String(v) : plain(v);
    if (value !== undefined) out[k] = k === 'geometry' && typeof value === 'string' ? JSON.parse(value) : value;
  }
  return out;
}

const ring = (r: Position[]) => `(${r.map((p) => `${num(p[0])} ${num(p[1])}`).join(', ')})`;

/** GeoJSON geometry (lon/lat) → WKT yang dipahami ekstensi. */
export function geojsonToWkt(g: Geometry): string {
  switch (g.type) {
    case 'Point':
      return `POINT (${num(g.coordinates[0])} ${num(g.coordinates[1])})`;
    case 'MultiPoint':
      return `MULTIPOINT (${g.coordinates.map((p) => `(${num(p[0])} ${num(p[1])})`).join(', ')})`;
    case 'LineString':
      return `LINESTRING ${ring(g.coordinates)}`;
    case 'MultiLineString':
      return `MULTILINESTRING (${g.coordinates.map(ring).join(', ')})`;
    case 'Polygon':
      return `POLYGON (${g.coordinates.map(ring).join(', ')})`;
    case 'MultiPolygon':
      return `MULTIPOLYGON (${g.coordinates.map((p) => `(${p.map(ring).join(', ')})`).join(', ')})`;
    default:
      throw new GeosquareError(`Geometri ${g.type} tidak didukung`);
  }
}

/** Argumen area untuk query_cell_dataset. */
export function areaArg(area: Area): string {
  if ('bbox' in area) return `bbox := [${area.bbox.map(num).join(', ')}]`;
  const wkt = 'wkt' in area ? area.wkt : geojsonToWkt(area.geojson);
  return `geometry := ${sqlString(wkt)}`;
}

/** Kolom ukuran tumpang-tindih untuk jenis area: coverage_ratio / length_ratio / point_count. */
export function overlapColumn(area: Area): 'coverage_ratio' | 'length_ratio' | 'point_count' {
  if ('bbox' in area) return 'coverage_ratio';
  const kind = ('wkt' in area ? area.wkt.trim().split(/[\s(]/)[0] : area.geojson.type).toUpperCase();
  if (kind.endsWith('POINT')) return 'point_count';
  if (kind.endsWith('LINESTRING')) return 'length_ratio';
  return 'coverage_ratio';
}

function coverageArgs(o: { minCoverage?: number; coverageMode?: string }): string {
  let s = '';
  if (o.minCoverage !== undefined) s += `, min_coverage := ${num(o.minCoverage)}`;
  if (o.coverageMode) s += `, coverage_mode := ${sqlString(o.coverageMode)}`;
  return s;
}

const source = (path: string, area: Area, o: { minCoverage?: number; coverageMode?: string }) =>
  `query_cell_dataset(${sqlString(path)}, ${areaArg(area)}${coverageArgs(o)})`;

/** Sel di dalam area beserta nilainya. */
export function cellsSql(path: string, area: Area, o: CellQueryOptions = {}): string {
  const extra = overlapColumn(area);
  const cols = ['gid', 'packed_id', extra, ...(o.columns ?? []).map(ident)];
  const select = o.columns ? cols.join(', ') : '*';
  const geometry = o.withGeometry ? `, cell_to_geometry(packed_id, 'EPSG:4326', 'geojson') AS geometry` : '';
  const limit = o.limit !== undefined ? ` LIMIT ${Math.trunc(o.limit)}` : '';
  return `SELECT ${select}${geometry} FROM ${source(path, area, o)} ORDER BY packed_id${limit}`;
}

/** Satu baris ringkasan: jumlah sel dan total tiap kolom di dalam area. */
export function summarySql(path: string, area: Area, columns: string[], o: SummaryOptions = {}): string {
  if (!columns.length) throw new GeosquareError('Pilih minimal satu kolom');
  const extra = overlapColumn(area);
  const weight = o.weighted === false || extra === 'point_count' ? '' : ` * ${extra}`;
  const sums = columns.map((c) => `sum(${ident(c)}${weight}) AS ${ident(c)}`);
  return `SELECT count(*) AS cells, ${sums.join(', ')} FROM ${source(path, area, o)}`;
}

/** Total per sel induk (level 9 = 1 km, 7 = 10 km, ...) beserta batasnya (GeoJSON). */
export function rollupSql(path: string, area: Area, columns: string[], level: number, o: SummaryOptions = {}): string {
  if (!Number.isInteger(level) || level < 1 || level > 12) throw new GeosquareError(`Level ${level} tidak valid`);
  const extra = overlapColumn(area);
  const weight = o.weighted === false || extra === 'point_count' ? '' : ` * ${extra}`;
  const sums = columns.map((c) => `sum(${ident(c)}${weight}) AS ${ident(c)}`);
  return (
    `SELECT left(gid, ${level}) AS gid, count(*) AS cells, ${sums.join(', ')}, ` +
    `cell_to_geometry(pack(any_value(unpack_int64(packed_id).domain_id), left(gid, ${level})), 'EPSG:4326', 'geojson') AS geometry ` +
    `FROM ${source(path, area, o)} GROUP BY 1 ORDER BY 1`
  );
}

/** Nilai di sel tempat tiap titik [lon, lat] berada (satu baris per titik, urut sesuai input). */
export function atPointsSql(path: string, points: [number, number][], columns: string[]): string {
  if (!points.length) throw new GeosquareError('Daftar titik kosong');
  const values = points.map((p, i) => `(${i}, ${num(p[0])}::DOUBLE, ${num(p[1])}::DOUBLE)`).join(', ');
  const mp = `MULTIPOINT (${points.map((p) => `(${num(p[0])} ${num(p[1])})`).join(', ')})`;
  const cols = columns.map((c) => `d.${ident(c)}`).join(', ');
  return (
    `WITH pts(i, lon, lat) AS (VALUES ${values}), ` +
    `c AS (SELECT i, lon, lat, point_to_cell('ID', lon, lat, 12) AS cell FROM pts), ` +
    `d AS (SELECT * FROM query_cell_dataset(${sqlString(path)}, geometry := ${sqlString(mp)})) ` +
    `SELECT c.i AS point, c.lon, c.lat, c.cell.gid AS gid${cols ? ', ' + cols : ''} ` +
    `FROM c LEFT JOIN d ON d.packed_id = c.cell.packed_id ORDER BY c.i`
  );
}

/** write_cell_dataset untuk sebuah query (string SELECT) ke `path` (lokal atau s3://). */
export function writeSql(query: string, path: string, o: WriteOptions): string {
  const opt = (k: string, v: string | number | boolean | Record<string, string> | undefined) => {
    if (v === undefined) return '';
    if (typeof v === 'number') return `, ${k} := ${num(v)}`;
    if (typeof v === 'boolean') return `, ${k} := ${v}`;
    return `, ${k} := ${sqlString(typeof v === 'string' ? v : JSON.stringify(v))}`;
  };
  return (
    `SELECT * FROM write_cell_dataset(${sqlString(query)}, ${sqlString(path)}` +
    opt('layout', o.layout) +
    opt('mode', o.mode) +
    opt('partition_level', o.partitionLevel) +
    opt('row_group_size', o.rowGroupSize) +
    opt('value_semantics', o.valueSemantics) +
    opt('aggregation_rule', o.aggregationRule) +
    opt('fill_nulls', o.fillNulls) +
    opt('column_metrics', o.columnMetrics) +
    opt('domain_code', o.domainCode) +
    opt('notes', o.notes) +
    ')'
  );
}

/** Kolom dataset (nama dan tipe) tanpa membaca data. */
export const columnsSql = (path: string) =>
  `SELECT column_name, column_type FROM (DESCRIBE SELECT * FROM read_cell_dataset(${sqlString(path)}))`;

/** Kredensial S3 untuk DuckDB: provider chain (backend) atau kunci sementara (frontend). */
export type S3Auth =
  | { provider: 'credential_chain'; region: string }
  | { keyId: string; secret: string; sessionToken?: string; region: string; endpoint?: string };

export function s3SecretSql(auth: S3Auth): string {
  if ('provider' in auth)
    return `CREATE OR REPLACE SECRET gs_s3 (TYPE s3, PROVIDER credential_chain, REGION ${sqlString(auth.region)})`;
  const parts = [
    `KEY_ID ${sqlString(auth.keyId)}`,
    `SECRET ${sqlString(auth.secret)}`,
    `REGION ${sqlString(auth.region)}`,
    ...(auth.sessionToken ? [`SESSION_TOKEN ${sqlString(auth.sessionToken)}`] : []),
    ...(auth.endpoint ? [`ENDPOINT ${sqlString(auth.endpoint)}`] : []),
  ];
  return `CREATE OR REPLACE SECRET gs_s3 (TYPE s3, ${parts.join(', ')})`;
}

/** Kesalahan input atau dataset (pesan untuk user). */
export class GeosquareError extends Error {}
