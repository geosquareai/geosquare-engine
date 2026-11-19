// Browser test: bundle test/web-entry.ts, serve it with the datasets written by test/node.test.ts,
// run it in headless Chromium and compare with the Node results.
//   EXT_WASM=../../build/wasm_eh/geosquare_v2.duckdb_extension.wasm CHROMIUM=<path> node test/web.test.mjs <node test out dir>
import { build } from 'esbuild';
import { chromium } from 'playwright-core';
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { resolve } from 'node:path';

// Wasm and native differ in the last digits of the projection math (~1e-10 relative).
const close = (a, b) => {
  if (typeof a === 'number' && typeof b === 'number') return Math.abs(a - b) <= 1e-9 * Math.max(1, Math.abs(b));
  if (Array.isArray(a) && Array.isArray(b)) return a.length === b.length && a.every((x, i) => close(x, b[i]));
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    const ka = Object.keys(a), kb = Object.keys(b);
    return ka.length === kb.length && ka.every((k) => close(a[k], b[k]));
  }
  return a === b;
};
const data = process.argv[2];
if (!data) throw new Error('usage: node test/web.test.mjs <dir written by test/node.test.ts>');
rmSync('www', { recursive: true, force: true });
mkdirSync('www');
cpSync(data, 'www/data', { recursive: true });
cpSync(resolve(process.env.EXT_WASM ?? '../../build/wasm_eh/geosquare_v2.duckdb_extension.wasm'), 'www/geosquare_v2.duckdb_extension.wasm');
await build({ entryPoints: ['test/web-entry.ts'], bundle: true, format: 'esm', outfile: 'www/app.js', platform: 'browser', logLevel: 'error' });
writeFileSync('www/index.html', '<!doctype html><meta charset="utf-8"><script type="module" src="/www/app.js"></script>');
const port = 8797;
const server = spawn('python3', ['-m', 'http.server', String(port), '--bind', '127.0.0.1'], { stdio: 'ignore' });
await new Promise((r) => setTimeout(r, 1000));
try {
  const browser = await chromium.launch({ executablePath: process.env.CHROMIUM || undefined, args: ['--no-sandbox'] });
  const page = await browser.newPage();
  page.on('pageerror', (e) => console.log('pageerror:', e.message));
  await page.goto(`http://127.0.0.1:${port}/www/index.html`);
  await page.waitForFunction(() => window.__results, null, { timeout: 240000 });
  const got = await page.evaluate(() => window.__results);
  await browser.close();
  if (got.error) throw new Error(got.error);
  const expected = JSON.parse(readFileSync(`${data}/expected.json`, 'utf8'));
  delete expected.columns;
  let ok = true;
  for (const layout of ['flat', 'partitioned', 'iceberg']) {
    const { ms, ...r } = got[layout];
    for (const [k, v] of Object.entries(expected)) {
      const same = close(r[k], v);
      ok &&= same;
      if (!same) console.log(`FAIL ${layout}.${k}\n  wasm ${JSON.stringify(r[k]).slice(0, 300)}\n  node ${JSON.stringify(v).slice(0, 300)}`);
    }
    console.log(`${layout.padEnd(12)} ${Object.keys(expected).length} results, ${ms} ms in the browser`);
  }
  if (!ok) process.exit(1);
  console.log('web OK: DuckDB-Wasm results equal Node for flat, partitioned and iceberg');
} finally {
  server.kill();
}
