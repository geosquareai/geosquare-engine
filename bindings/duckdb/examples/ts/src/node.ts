/**
 * Backend (Node.js): baca/tulis dataset sel GeoSquare V2 dengan @duckdb/node-api + ekstensi
 * `geosquare_v2`. Satu instance DuckDB dipakai ulang antar request (metadata dan manifest Iceberg
 * di-cache per proses, jadi request berikutnya lebih cepat).
 *
 *   const gs = await GeosquareCells.open({ extension: '/opt/geosquare_v2.duckdb_extension',
 *                                          s3: { provider: 'credential_chain', region: 'ap-southeast-3' } });
 *   const sum = await gs.summarize(PEOPLE, { geojson: polygon }, ['people_counts']);
 *   gs.close();
 */
import { type DuckDBConnection, DuckDBInstance } from '@duckdb/node-api';
import {
  type Area,
  type CellQueryOptions,
  GeosquareError,
  type S3Auth,
  type SummaryOptions,
  type WriteOptions,
  atPointsSql,
  cellsSql,
  columnsSql,
  plainRow,
  rollupSql,
  s3SecretSql,
  sqlString,
  summarySql,
  writeSql,
} from './sql';

export * from './sql';

export interface OpenOptions {
  /** Path file `geosquare_v2.duckdb_extension`, atau URL repository untuk INSTALL ... FROM. */
  extension: string;
  /** Kredensial S3 (opsional; dataset lokal/HTTP tidak perlu). */
  s3?: S3Auth;
  /** Batas memori DuckDB, mis. '4GB'. */
  memoryLimit?: string;
  threads?: number;
}

type Row = Record<string, unknown>;

export class GeosquareCells {
  private constructor(
    private readonly db: DuckDBInstance,
    private readonly conn: DuckDBConnection,
  ) {}

  static async open(o: OpenOptions): Promise<GeosquareCells> {
    const db = await DuckDBInstance.create(':memory:', {
      allow_unsigned_extensions: 'true',
      ...(o.memoryLimit ? { memory_limit: o.memoryLimit } : {}),
      ...(o.threads ? { threads: String(o.threads) } : {}),
    });
    const conn = await db.connect();
    try {
      await conn.run('INSTALL httpfs; LOAD httpfs');
      if (/^https?:\/\//.test(o.extension)) {
        await conn.run(`INSTALL geosquare_v2 FROM ${sqlString(o.extension)}`);
        await conn.run('LOAD geosquare_v2');
      } else {
        await conn.run(`LOAD ${sqlString(o.extension)}`);
      }
      if (o.s3) await conn.run(s3SecretSql(o.s3));
    } catch (e) {
      conn.closeSync();
      db.closeSync();
      throw new GeosquareError(`DuckDB/ekstensi geosquare_v2 tidak bisa dimuat: ${(e as Error).message}`);
    }
    return new GeosquareCells(db, conn);
  }

  /** Jalankan SELECT dan kembalikan baris sebagai objek JS biasa. */
  async query(sql: string): Promise<Row[]> {
    try {
      const reader = await this.conn.runAndReadAll(sql);
      return reader.getRowObjectsJS().map(plainRow);
    } catch (e) {
      throw new GeosquareError((e as Error).message);
    }
  }

  /** Kolom dataset (nama → tipe DuckDB). */
  async columns(path: string): Promise<Record<string, string>> {
    const rows = await this.query(columnsSql(path));
    return Object.fromEntries(rows.map((r) => [String(r.column_name), String(r.column_type)]));
  }

  /** Sel di dalam area: gid, packed_id (teks), coverage_ratio/length_ratio/point_count + kolom. */
  cells(path: string, area: Area, o: CellQueryOptions = {}): Promise<Row[]> {
    return this.query(cellsSql(path, area, o));
  }

  /** Total kolom di dalam area (default berbobot coverage_ratio) + jumlah sel. */
  async summarize(path: string, area: Area, columns: string[], o: SummaryOptions = {}): Promise<Row> {
    return (await this.query(summarySql(path, area, columns, o)))[0] ?? {};
  }

  /** Total per sel induk (level 9 = 1 km, 7 = 10 km) dengan geometri GeoJSON. */
  rollup(path: string, area: Area, columns: string[], level: number, o: SummaryOptions = {}): Promise<Row[]> {
    return this.query(rollupSql(path, area, columns, level, o));
  }

  /** Nilai di titik-titik [lon, lat] (satu baris per titik, urutan input). */
  atPoints(path: string, points: [number, number][], columns: string[]): Promise<Row[]> {
    return this.query(atPointsSql(path, points, columns));
  }

  /** FeatureCollection sel di dalam area (untuk peta / ekspor). */
  async featureCollection(path: string, area: Area, columns: string[]) {
    const rows = await this.cells(path, area, { columns, withGeometry: true });
    return {
      type: 'FeatureCollection' as const,
      features: rows.map(({ geometry, ...properties }) => ({ type: 'Feature' as const, properties, geometry })),
    };
  }

  /** Baca streaming per batch (ekspor besar): seluruh dataset, atau hanya area. */
  async stream(
    path: string,
    onBatch: (rows: Row[]) => Promise<void>,
    o: { area?: Area; columns?: string[]; batch?: number } = {},
  ): Promise<number> {
    const sql = o.area
      ? cellsSql(path, o.area, { columns: o.columns })
      : `SELECT ${o.columns ? ['gid', 'packed_id', ...o.columns].map((c) => `"${c}"`).join(', ') : '*'} FROM read_cell_dataset(${sqlString(path)})`;
    const size = o.batch ?? 1000;
    const result = await this.conn.stream(sql);
    let batch: Row[] = [];
    let n = 0;
    for await (const rows of result.yieldRowObjectJs()) {
      for (const row of rows) {
        batch.push(plainRow(row));
        n++;
        if (batch.length >= size) {
          await onBatch(batch);
          batch = [];
        }
      }
    }
    if (batch.length) await onBatch(batch);
    return n;
  }

  /**
   * Tulis hasil query (SELECT dengan packed_id atau gid + kolom nilai) sebagai dataset sel.
   * Mengembalikan { layout, path, files, rows }.
   */
  async write(query: string, path: string, o: WriteOptions): Promise<Row> {
    return (await this.query(writeSql(query, path, o)))[0] ?? {};
  }

  close(): void {
    this.conn.closeSync();
    this.db.closeSync();
  }
}
