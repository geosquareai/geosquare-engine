/**
 * Frontend (browser): baca dataset sel GeoSquare V2 dengan DuckDB-Wasm + ekstensi `geosquare_v2`
 * (build wasm_eh). Hanya membaca; menulis dilakukan di backend.
 *
 * Data dibaca langsung dari:
 *   - HTTP(S)/CDN: dataset yang ditulis writer punya `gid_prefixes.json` / `metadata/version-hint.text`,
 *     jadi tidak butuh listing. Host harus mengirim header CORS.
 *   - S3: kunci sementara (STS) dari backend + CORS di bucket.
 *
 *   const gs = await GeosquareCellsWasm.open({ extension: 'https://cdn.example.com/geosquare_v2.duckdb_extension.wasm' });
 *   const sum = await gs.summarize(url, { geojson: drawnPolygon }, ['people_counts']);
 */
import * as duckdb from '@duckdb/duckdb-wasm';
import { Type } from 'apache-arrow';
import {
  type Area,
  type CellQueryOptions,
  GeosquareError,
  type S3Auth,
  type SummaryOptions,
  atPointsSql,
  cellsSql,
  columnsSql,
  plainRow,
  rollupSql,
  s3SecretSql,
  sqlString,
  summarySql,
} from './sql';

export * from './sql';

export interface WasmOpenOptions {
  /** URL `geosquare_v2.duckdb_extension.wasm`, atau URL repository untuk INSTALL ... FROM. */
  extension: string;
  /** Bundle DuckDB-Wasm; default jsDelivr (versi paket yang terpasang). */
  bundles?: duckdb.DuckDBBundles;
  /** Kunci S3 sementara (opsional). */
  s3?: Extract<S3Auth, { keyId: string }>;
}

type Row = Record<string, unknown>;

export class GeosquareCellsWasm {
  private constructor(
    private readonly db: duckdb.AsyncDuckDB,
    private readonly conn: duckdb.AsyncDuckDBConnection,
  ) {}

  static async open(o: WasmOpenOptions): Promise<GeosquareCellsWasm> {
    const bundle = await duckdb.selectBundle(o.bundles ?? duckdb.getJsDelivrBundles());
    // Worker dari URL lintas origin (CDN) dibungkus blob agar bisa dibuat dari halaman ini.
    const workerUrl = URL.createObjectURL(
      new Blob([`importScripts(${JSON.stringify(bundle.mainWorker)});`], { type: 'text/javascript' }),
    );
    const db = new duckdb.AsyncDuckDB(new duckdb.VoidLogger(), new Worker(workerUrl));
    await db.instantiate(bundle.mainModule, bundle.pthreadWorker);
    URL.revokeObjectURL(workerUrl);
    await db.open({ allowUnsignedExtensions: true });
    const conn = await db.connect();
    try {
      // Threads bundle (coi/threads): a thread of DuckDB's pool can run extension code before
      // it has picked up the newly loaded module (Emscripten dlopen with pthreads) and hang.
      // Keep queries on one thread there; the eh/mvp bundles are single-threaded anyway.
      if (bundle.pthreadWorker) await conn.query('SET threads = 1');
      if (o.extension.endsWith('.wasm')) {
        await conn.query(`LOAD ${sqlString(o.extension)}`);
      } else {
        await conn.query(`INSTALL geosquare_v2 FROM ${sqlString(o.extension)}`);
        await conn.query('LOAD geosquare_v2');
      }
      if (o.s3) await conn.query(s3SecretSql(o.s3));
    } catch (e) {
      await conn.close();
      await db.terminate();
      throw new GeosquareError(`Ekstensi geosquare_v2 tidak bisa dimuat: ${(e as Error).message}`);
    }
    return new GeosquareCellsWasm(db, conn);
  }

  /** Jalankan SELECT dan kembalikan baris sebagai objek JS biasa. */
  async query(sql: string): Promise<Row[]> {
    let table;
    try {
      table = await this.conn.query(sql);
    } catch (e) {
      throw new GeosquareError((e as Error).message);
    }
    // Arrow mengembalikan DECIMAL sebagai 4 kata 32-bit; ubah ke angka dengan skalanya.
    const decimals = table.schema.fields
      .filter((f) => f.type.typeId === Type.Decimal)
      .map((f) => [f.name, (f.type as unknown as { scale: number }).scale] as const);
    return table.toArray().map((r) => {
      const row = r.toJSON() as Row;
      for (const [name, scale] of decimals) row[name] = decimalToNumber(row[name], scale);
      return plainRow(row);
    });
  }

  async columns(path: string): Promise<Record<string, string>> {
    const rows = await this.query(columnsSql(path));
    return Object.fromEntries(rows.map((r) => [String(r.column_name), String(r.column_type)]));
  }

  cells(path: string, area: Area, o: CellQueryOptions = {}): Promise<Row[]> {
    return this.query(cellsSql(path, area, o));
  }

  async summarize(path: string, area: Area, columns: string[], o: SummaryOptions = {}): Promise<Row> {
    return (await this.query(summarySql(path, area, columns, o)))[0] ?? {};
  }

  rollup(path: string, area: Area, columns: string[], level: number, o: SummaryOptions = {}): Promise<Row[]> {
    return this.query(rollupSql(path, area, columns, level, o));
  }

  atPoints(path: string, points: [number, number][], columns: string[]): Promise<Row[]> {
    return this.query(atPointsSql(path, points, columns));
  }

  /** FeatureCollection sel di dalam area, siap untuk MapLibre/Leaflet/deck.gl. */
  async featureCollection(path: string, area: Area, columns: string[]) {
    const rows = await this.cells(path, area, { columns, withGeometry: true });
    return {
      type: 'FeatureCollection' as const,
      features: rows.map(({ geometry, ...properties }) => ({ type: 'Feature' as const, properties, geometry })),
    };
  }

  async close(): Promise<void> {
    await this.conn.close();
    await this.db.terminate();
  }
}

/** DECIMAL Arrow (Uint32Array little-endian, two's complement) → number. */
function decimalToNumber(v: unknown, scale: number): unknown {
  if (v === null || v === undefined || typeof v === 'number') return v;
  const words = v instanceof Uint32Array ? v : Uint32Array.from(Object.values(v as Record<string, number>));
  let big = 0n;
  for (let i = words.length - 1; i >= 0; i--) big = (big << 32n) | BigInt(words[i]);
  const bits = BigInt(words.length * 32);
  if (big >> (bits - 1n)) big -= 1n << bits;
  return Number(big) / 10 ** scale;
}
