// Browser side of test/web.test.mjs: the same calls as test/node.test.ts, over HTTP.
import { GeosquareCellsWasm } from '../src/web';

const base = location.origin;
const dist = `${base}/node_modules/@duckdb/duckdb-wasm/dist/`;
(async () => {
  const results: Record<string, unknown> = {};
  try {
    const gs = await GeosquareCellsWasm.open({
      extension: `${base}/www/geosquare_v2.duckdb_extension.wasm`,
      bundles: { base: { mainModule: `${dist}duckdb-base.wasm`, mainWorker: `${dist}duckdb-browser-base.worker.js` } },
    });
    const polygon = {
      type: 'Polygon' as const,
      coordinates: [[[106.8227, -6.1799], [106.8317, -6.1799], [106.8317, -6.1709], [106.8227, -6.1709], [106.8227, -6.1799]]],
    };
    const points: [number, number][] = [[106.8272, -6.1754], [106.8456, -6.2088], [106.83, -6.2]];
    for (const layout of ['flat', 'partitioned', 'iceberg']) {
      const p = `${base}/www/data/${layout}`;
      const t = performance.now();
      results[layout] = {
        summary: await gs.summarize(p, { geojson: polygon }, ['population', 'workers']),
        summaryBbox: await gs.summarize(p, { bbox: [106.82, -6.18, 106.83, -6.17] }, ['population']),
        majority: await gs.summarize(p, { geojson: polygon }, ['population'], { minCoverage: 0.5, weighted: false }),
        line: await gs.summarize(p, { wkt: 'LINESTRING (106.81 -6.21, 106.85 -6.17)' }, ['population']),
        rollup: await gs.rollup(p, { geojson: polygon }, ['population'], 9),
        cells: await gs.cells(p, { geojson: polygon }, { columns: ['population'], limit: 5 }),
        points: await gs.atPoints(p, points, ['population', 'workers']),
        fc: (await gs.featureCollection(p, { bbox: [106.825, -6.176, 106.826, -6.175] }, ['population'])).features.length,
        ms: Math.round(performance.now() - t),
      };
    }
    await gs.close();
  } catch (e) {
    results.error = String((e as Error)?.message ?? e);
  }
  (window as any).__results = results;
})();
