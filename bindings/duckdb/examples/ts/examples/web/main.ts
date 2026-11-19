// Demo page: read a cell dataset in the browser with GeosquareCellsWasm.
import { type Area, GeosquareCellsWasm } from '../../src/web';

const $ = (id: string) => document.getElementById(id) as HTMLInputElement;
const out = $('out') as unknown as HTMLPreElement;
const show = (label: string, v: unknown, t0: number) =>
  (out.textContent = `${label} (${Math.round(performance.now() - t0)} ms)\n` + JSON.stringify(v, null, 1));
const abs = (u: string) => (u.startsWith('/') ? location.origin + u : u);

function area(): Area {
  const a = $('area').value.trim();
  if (/^-?[\d.]+\s*(,\s*-?[\d.]+\s*){3}$/.test(a)) return { bbox: a.split(',').map(Number) as [number, number, number, number] };
  if (a.startsWith('{')) {
    const g = JSON.parse(a);
    return { geojson: g.type === 'Feature' ? g.geometry : g };
  }
  return { wkt: a };
}
const cols = () => $('cols').value.split(',').map((s) => s.trim()).filter(Boolean);

const dist = `${location.origin}/node_modules/@duckdb/duckdb-wasm/dist/`;
const gs = await GeosquareCellsWasm.open({
  extension: abs($('ext').value),
  bundles: { base: { mainModule: `${dist}duckdb-base.wasm`, mainWorker: `${dist}duckdb-browser-base.worker.js` } },
});
out.textContent = 'ready';
const run = (label: string, f: () => Promise<unknown>) => async () => {
  const t0 = performance.now();
  try { show(label, await f(), t0); } catch (e) { out.textContent = String(e); }
};
$('sum').onclick = run('total (weighted by coverage)', () => gs.summarize(abs($('path').value), area(), cols()));
$('km').onclick = run('1 km cells', async () =>
  (await gs.rollup(abs($('path').value), area(), cols(), 9)).map(({ geometry, ...r }) => r));
$('geo').onclick = run('GeoJSON downloaded', async () => {
  const fc = await gs.featureCollection(abs($('path').value), area(), cols());
  const a = document.createElement('a');
  a.href = URL.createObjectURL(new Blob([JSON.stringify(fc)], { type: 'application/geo+json' }));
  a.download = 'cells.geojson';
  a.click();
  return { features: fc.features.length };
});
