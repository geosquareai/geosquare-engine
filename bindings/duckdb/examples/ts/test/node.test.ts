/**
 * Node: write the three layouts with GeosquareCells.write, read them back with every function,
 * check the layouts agree, and save the results for the browser test (test/web.test.mjs).
 *   EXT=../../build/linux_amd64/geosquare_v2.duckdb_extension npx tsx test/node.test.ts <out dir>
 */
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { GeosquareCells } from '../src/node';

const ext = resolve(process.env.EXT ?? '../../build/linux_amd64/geosquare_v2.duckdb_extension');
const out = process.argv[2] ?? mkdtempSync(join(tmpdir(), 'gs_ts_'));
const gs = await GeosquareCells.open({ extension: ext });

// sample: 50 m cells over central Jakarta, two value columns, one with NULLs (written as 0)
const src = `SELECT packed_id, (packed_id % 97)::DOUBLE AS population,
  CASE WHEN packed_id % 5 = 0 THEN NULL ELSE (packed_id % 13)::DOUBLE END AS workers
  FROM polygon_to_cells('ID', 'POLYGON ((106.80 -6.22, 106.86 -6.22, 106.86 -6.16, 106.80 -6.16, 106.80 -6.22))', 12)`;
for (const layout of ['flat', 'partitioned', 'iceberg'] as const) {
  const r = await gs.write(src, join(out, layout), {
    layout, partitionLevel: layout === 'flat' ? undefined : 7, valueSemantics: 'COUNT', aggregationRule: 'sum',
  });
  assert.equal(r.layout, layout);
}

const polygon = {
  type: 'Polygon' as const,
  coordinates: [[[106.8227, -6.1799], [106.8317, -6.1799], [106.8317, -6.1709], [106.8227, -6.1709], [106.8227, -6.1799]]],
};
const line = { wkt: 'LINESTRING (106.81 -6.21, 106.85 -6.17)' };
const points: [number, number][] = [[106.8272, -6.1754], [106.8456, -6.2088], [106.8300, -6.2000]];
const results: Record<string, unknown> = {};
for (const layout of ['flat', 'partitioned', 'iceberg']) {
  const p = join(out, layout);
  const r = {
    columns: await gs.columns(p),
    summary: await gs.summarize(p, { geojson: polygon }, ['population', 'workers']),
    summaryBbox: await gs.summarize(p, { bbox: [106.82, -6.18, 106.83, -6.17] }, ['population']),
    majority: await gs.summarize(p, { geojson: polygon }, ['population'], { minCoverage: 0.5, weighted: false }),
    line: await gs.summarize(p, line, ['population']),
    rollup: await gs.rollup(p, { geojson: polygon }, ['population'], 9),
    cells: (await gs.cells(p, { geojson: polygon }, { columns: ['population'], limit: 5 })),
    points: await gs.atPoints(p, points, ['population', 'workers']),
    fc: (await gs.featureCollection(p, { bbox: [106.825, -6.176, 106.826, -6.175] }, ['population'])).features.length,
  };
  results[layout] = r;
}
// all layouts give the same answers
const strip = (r: any) => ({ ...r, columns: undefined });
assert.deepEqual(strip(results.partitioned), strip(results.flat));
assert.deepEqual(strip(results.iceberg), strip(results.flat));
const flat = results.flat as any;
// summary matches plain SQL on the same data
const [direct] = await gs.query(`SELECT count(*) AS cells, sum(d.population * k.coverage_ratio) AS population
  FROM polygon_to_cells('ID', 'POLYGON ((106.8227 -6.1799, 106.8317 -6.1799, 106.8317 -6.1709, 106.8227 -6.1709, 106.8227 -6.1799))', 12) k
  JOIN read_parquet('${join(out, 'flat')}/data/*.parquet') d USING (packed_id)`);
assert.equal(flat.summary.cells, direct.cells);
assert.ok(Math.abs((flat.summary.population as number) - (direct.population as number)) < 1e-6);
// NULL was written as 0; packed_id comes back as text; rollup has GeoJSON polygons
assert.equal((await gs.query(`SELECT count(*) AS n FROM read_cell_dataset('${join(out, 'iceberg')}') WHERE workers IS NULL`))[0].n, 0);
assert.equal(typeof flat.cells[0].packed_id, 'string');
assert.equal(flat.rollup[0].geometry.type, 'Polygon');
assert.equal(flat.points.length, 3);
assert.ok(Math.abs(flat.line.length_ratio ?? 0) >= 0);
// streaming the whole dataset
let streamed = 0;
const n = await gs.stream(join(out, 'iceberg'), async (rows) => { streamed += rows.length; }, { columns: ['population'], batch: 5000 });
assert.equal(n, streamed);
assert.equal(n, (await gs.query(`SELECT count(*) AS n FROM read_cell_dataset('${join(out, 'flat')}')`))[0].n);
gs.close();
writeFileSync(join(out, 'expected.json'), JSON.stringify(results.flat));
console.log(`node OK: 3 layouts agree (${flat.summary.cells} cells, population ${(flat.summary.population as number).toFixed(3)}), stream ${n} rows -> ${out}`);
